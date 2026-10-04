"""A pack built from its sources, as a child process the front never waits on.

```
front ── fetch into geodata/.cache/<source>/ ───► source hosts     (sources.py)
front ── planner-job pack-build, JSON on stdin ─► planner-job
planner-job ── one JSON line per step ─────────► front ── geodata_progress ──► every page
planner-job ── geodata/.part-<name>/ ──────────► front: geodata/<name>/, its geodata.yaml
```

A build is a name, a rectangle and a resolution. It fetches what the
rectangle needs of each source (sources.plan chooses the sources and says
what), lays out the compiler's inputs, and runs
`planner-job pack-build` with them as one JSON object on its standard input;
the compiler's steps come back one JSON line each and its diagnostics on
standard error, which go to `geodata/.cache/logs/<name>.log`. The pack is
written under `geodata/.part-<name>/`, its geodata file is written beside it
when it is whole, and only then is the directory renamed into place, so
geodata that exists is geodata that was built.

Berlin's tiles are handed over as a directory of links to just the tiles
this rectangle meets, since the compiler reads every file of the directory
it is given and the cache holds every tile any build fetched.

`row` is what the page is told: {name, state, step, done, total, fetched,
of, error, spec}, state one of fetching, compiling, done, failed, cancelled.
"""

import asyncio
import contextlib
import json
import os
import shutil
import signal
import time

import geodata
import sources
import store

PROGRESS_HZ = 4.0
TAIL_LINES = 20
FETCH_PARALLEL = 4
# Which compiler input a source's files go to, by its format and a layer it
# feeds: the compiler's readers as they are (sourcefile.COMPILER_READS).
INPUTS = {
    ("geotiff", "surface"): "dsm_tiles",
    ("geotiff", "landcover"): "worldcover_tiles",
    ("itu-p1812-maps", "radio-climate"): "itu_maps_dir",
    ("osm-pbf", "roads"): "osm_pbf",
    ("citygml", "buildings"): "lod2_dir",
    ("xyz", "terrain"): "berlin_1m_dir",
    ("xyz", "surface"): "berlin_1m_dir",
    ("csv-grid", "population"): "zensus_csv",
}


class Build:
    def __init__(self, cache, spec, binary, say, preexec_fn=None, sources_=None):
        self.cache = cache
        self.sources = sources_             # the sources to plan from; None: the source files'
        self.binary = binary
        self.say = say
        self.preexec_fn = preexec_fn
        self.name = spec["name"]
        self.spec = {k: spec.get(k) for k in ("bbox", "res_m")}
        self.planned = None
        self.part = os.path.join(store.GEODATA_DIR, geodata.PART_PREFIX + self.name)
        self.inputs = self.part + ".inputs"
        self.log_path = os.path.join(self.cache.root, "logs", self.name + ".log")
        self.process = None
        self.task = None
        self.tail = []
        self.said_at = 0.0
        self.row = {"name": self.name, "state": "fetching", "step": "planning", "done": 0,
                    "total": 0, "fetched": 0, "of": None, "error": None, "spec": self.spec}

    @property
    def running(self):
        return self.row["state"] in ("fetching", "compiling")

    def tell(self, force=False, **fields):
        """Change the row and tell every page, at most PROGRESS_HZ times a
        second unless `force` (a new step or state)."""
        self.row.update(fields)
        now = time.monotonic()
        if force or now - self.said_at >= 1.0 / PROGRESS_HZ:
            self.said_at = now
            self.say(dict(self.row))

    def start(self):
        self.task = asyncio.ensure_future(self.run())
        return self.task

    def cancel(self):
        if self.task is not None and not self.task.done():
            self.task.cancel()

    async def run(self):
        try:
            planned = self.planned = await sources.plan(self.cache, self.spec, sources=self.sources)
            await self.fetch_all(planned)
            self.tell(True, state="compiling", step="unpacking", done=0, total=0)
            params = await asyncio.to_thread(self.params, planned, self.sources)
            await self.compile(params)
            self.finish()
            self.tell(True, state="done", step=None)
        except asyncio.CancelledError:
            await self.stop_process()
            self.tell(True, state="cancelled", step=None)
        except (store.StoreError, OSError, ValueError) as err:
            await self.stop_process()
            self.tell(True, state="failed", step=None, error=str(err))
        finally:
            shutil.rmtree(self.part, ignore_errors=True)
            shutil.rmtree(self.inputs, ignore_errors=True)

    async def fetch_all(self, planned):
        """Every file the build needs into the cache, a few at a time."""
        todo = [f for fs in planned["files"].values() for f in fs if self.cache.have(f) is None]
        known = [r["to_fetch"] for r in planned["sources"]]
        titles = {r["source"]: r["title"] for r in planned["sources"]}
        of = None if None in known else sum(known)
        progress = {}
        self.tell(True, step="downloading", done=0, total=len(todo), fetched=0, of=of)
        gate = asyncio.Semaphore(FETCH_PARALLEL)
        finished = [0]

        async def one(f):
            async with gate:
                start = self.cache.fetched_bytes(f)
                title = titles.get(f.source, f.source)

                def heard(got):
                    progress[f.name] = got - start
                    self.tell(step="downloading %s" % title, fetched=sum(progress.values()))
                await self.cache.fetch(f, heard)
                finished[0] += 1
                self.tell(step="downloading %s" % title, done=finished[0])
        await asyncio.gather(*(one(f) for f in todo))

    def link_dir(self, name, paths):
        """A directory of links to `paths`, for an input the compiler reads whole."""
        into = os.path.join(self.inputs, name)
        os.makedirs(into, exist_ok=True)
        for path in paths:
            os.symlink(path, os.path.join(into, os.path.basename(path)))
        return into

    def params(self, planned, sources_=None):
        """The compiler's input, from the files now in the cache, the zips
        among them extracted: on a worker thread, as a city's tiles are
        gigabytes.

        Each chosen source goes to the compiler input its format and layer
        are read through (INPUTS): a surface GeoTIFF is the DSM tiles, a
        land cover one WorldCover's, XYZ terrain and surface the 1 m pairs,
        and so on. An input that takes one source is refused two."""
        files = planned["files"]
        reg = sources.registry(sources_)
        have = lambda source: [f.path(self.cache.root) for f in files.get(source, ())  # noqa: E731
                               if self.cache.have(f)]
        extracted = lambda source: [p for f in files.get(source, ())  # noqa: E731
                                    if self.cache.have(f) for p in self.cache.extracted(f)]
        given = {}
        for source_id in files:
            source = reg[source_id]
            for layer in source.layers:
                into = INPUTS.get((source.format_type, layer))
                if into is not None and source_id not in given.setdefault(into, []):
                    given[into].append(source_id)
        for into, ids in given.items():
            if len(ids) > 1 and into not in ("berlin_1m_dir",):
                raise store.StoreError("the compiler takes one source for %s, and the rectangle "
                                       "has %s" % (into, ", ".join(ids)))
        one = lambda into: given.get(into, [None])[0]  # noqa: E731
        dsm = have(one("dsm_tiles")) if one("dsm_tiles") else []
        if not dsm:
            raise store.StoreError("no surface tile lies under this rectangle (open sea?)")
        itu = extracted(one("itu_maps_dir")) if one("itu_maps_dir") else []
        pbf_source = one("osm_pbf")
        pbf = have(pbf_source) if pbf_source else []
        params = {
            "name": self.name, "out_dir": self.part,
            "bbox": [float(v) for v in self.spec["bbox"]], "res_m": float(self.spec["res_m"]),
            "utm_zone": planned["grid"]["zone"],
            "dsm_tiles": dsm,
            "worldcover_tiles": have(one("worldcover_tiles")) if one("worldcover_tiles") else [],
            "itu_maps_dir": os.path.dirname(itu[0]) if itu else None,
            "osm_pbf": pbf[0] if pbf else None,
            "osm_buildings": bool(pbf) and "buildings" in reg[pbf_source].layers,
            "lod2_dir": None, "berlin_1m_dir": None, "zensus_csv": None, "threads": 0,
        }
        shutil.rmtree(self.inputs, ignore_errors=True)
        if one("lod2_dir"):
            params["lod2_dir"] = self.link_dir("lod2", extracted(one("lod2_dir")))
        if given.get("berlin_1m_dir"):
            # Terrain first, as the pairs have always been laid out.
            ids = sorted(given["berlin_1m_dir"], key=lambda s: "terrain" not in reg[s].layers)
            params["berlin_1m_dir"] = self.link_dir(
                "berlin-1m", [p for s in ids for p in extracted(s)])
        if one("zensus_csv"):
            csv = extracted(one("zensus_csv"))
            params["zensus_csv"] = csv[0] if csv else None
        return params

    async def compile(self, params):
        shutil.rmtree(self.part, ignore_errors=True)
        os.makedirs(os.path.dirname(self.part), exist_ok=True)
        os.makedirs(os.path.dirname(self.log_path), exist_ok=True)
        self.process = await asyncio.create_subprocess_exec(
            self.binary, "pack-build", stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            start_new_session=True, preexec_fn=self.preexec_fn)
        self.process.stdin.write(json.dumps(params).encode("utf-8"))
        await self.process.stdin.drain()
        self.process.stdin.close()
        errors = asyncio.ensure_future(self.read_stderr())
        manifest, error = None, None
        async for raw in self.process.stdout:
            try:
                line = json.loads(raw)
            except ValueError:
                continue
            if "step" in line:
                self.tell(line["step"] != self.row["step"] or line.get("done") != self.row["done"],
                          step=line["step"],
                          done=line.get("done", 0), total=line.get("total", 0))
            elif "manifest" in line:
                manifest = line["manifest"]
            elif "error" in line:
                error = line["error"]
        code = await self.process.wait()
        await errors
        self.process = None
        if code != 0 or manifest is None:
            raise store.StoreError(error or "the compiler ended with %d: %s"
                                   % (code, " / ".join(self.tail[-3:]) or "nothing said"))

    async def read_stderr(self):
        with open(self.log_path, "ab") as log:
            async for raw in self.process.stderr:
                log.write(raw)
                line = raw.decode("utf-8", "replace").rstrip()
                if line:
                    self.tail = (self.tail + [line])[-TAIL_LINES:]

    async def stop_process(self):
        process = self.process
        if process is None or process.returncode is not None:
            return
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(process.pid, signal.SIGTERM)
        with contextlib.suppress(asyncio.TimeoutError, asyncio.CancelledError):
            await asyncio.wait_for(process.wait(), 10)
        if process.returncode is None:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(process.pid, signal.SIGKILL)

    def finish(self):
        """The geodata file beside the pack, and the directory into place."""
        dest = geodata.geodata_dir(self.name)
        if os.path.exists(dest):
            raise store.StoreError("there is already geodata called %r" % self.name)
        spec = self.spec
        geodata.write(os.path.join(self.part, geodata.GEODATA_FILE),
                      {geodata.PACK: geodata.OWN_PACK},
                      "built from sources: %s at %g m\n%s"
                      % (", ".join("%.5f" % v for v in spec["bbox"]), float(spec["res_m"]),
                         "\n".join("%s: %s" % (r["title"], r["used_for"])
                                   for r in self.planned["sources"])))
        os.rename(self.part, dest)
        geodata.load(self.name)


def refuse(spec):
    """Why a build of `spec` cannot start, before anything is fetched: a
    StoreError, or None."""
    name = store.check_name(spec.get("name"), "geodata")
    if os.path.exists(geodata.geodata_dir(name)):
        raise store.StoreError("there is already geodata called %r" % name)
    sources.check(spec.get("bbox"), float(spec.get("res_m") or 30))
