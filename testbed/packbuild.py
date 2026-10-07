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

import crs
import geodata
import sourcefile
import sources
import store

PROGRESS_HZ = 4.0
TAIL_LINES = 20
FETCH_PARALLEL = 4
# Which compiler input a source's files go to, by its format and a layer it
# feeds (and for a surface GeoTIFF, whether it is the worldwide one the
# compiler splits, or a regional one paired with a terrain).
INPUTS = {
    ("geotiff", "surface", True): "dsm_tiles",
    ("geotiff", "surface", False): "elevation_surface",
    ("geotiff", "terrain", False): "elevation_terrain",
    ("geotiff", "landcover", True): "landcover",
    ("geotiff", "landcover", False): "landcover",
    ("geotiff", "population", False): "population",
    ("itu-p1812-maps", "radio-climate", True): "itu_maps_dir",
    ("osm-pbf", "roads", True): "osm_pbf",
    ("citygml", "buildings", False): "lod2_dir",
    ("cityjson", "buildings", False): "cityjson",
    ("xyz", "terrain", False): "berlin_1m_dir",
    ("xyz", "surface", False): "berlin_1m_dir",
    ("csv-grid", "population", False): "population",
    ("gpkg-grid", "population", False): "population",
    ("inspire-pd-grid", "population", False): "population",
}
# The inputs that take several sources; every other takes one. LoD2 is one
# directory of CityGML the compiler reads whole, so a rectangle across two
# states (Berlin and Potsdam) takes both. Land cover is read worldwide source
# first, a regional one over it where it has a class (NLCD over WorldCover).
MANY = ("berlin_1m_dir", "lod2_dir", "elevation_terrain", "elevation_surface", "landcover")


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
        are read through (INPUTS): a worldwide surface GeoTIFF is the DSM
        tiles, a land cover one is read through its own class table, XYZ
        terrain and surface the 1 m pairs, and so on. An input that takes one
        source is refused two."""
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
                into = INPUTS.get((source.format_type, layer, source.worldwide))
                if into is not None and source_id not in given.setdefault(into, []):
                    given[into].append(source_id)
        for into, ids in given.items():
            if len(ids) > 1 and into not in MANY:
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
            "landcover": [],
            "itu_maps_dir": os.path.dirname(itu[0]) if itu else None,
            "osm_pbf": pbf[0] if pbf else None,
            "osm_buildings": bool(pbf) and "buildings" in reg[pbf_source].layers,
            "lod2_dir": None, "berlin_1m_dir": None, "population": None, "elevation": [],
            "cityjson": None, "threads": 0,
        }
        shutil.rmtree(self.inputs, ignore_errors=True)
        if given.get("lod2_dir"):
            params["lod2_dir"] = self.link_dir(
                "lod2", [p for s in given["lod2_dir"] for p in extracted(s)])
        if given.get("berlin_1m_dir"):
            # Terrain first, as the pairs have always been laid out.
            ids = sorted(given["berlin_1m_dir"], key=lambda s: "terrain" not in reg[s].layers)
            params["berlin_1m_dir"] = self.link_dir(
                "berlin-1m", [p for s in ids for p in extracted(s)])
        params["elevation"] = self.elevation(given, reg, have, extracted)
        tiffs = lambda s: (extracted if s.format.get("members") else have)(s.id)  # noqa: E731
        for source_id in sorted(given.get("landcover", ()), key=lambda s: not reg[s].worldwide):
            s = reg[source_id]
            paths = tiffs(s)
            if paths:
                params["landcover"].append({
                    "tiles": paths, "proj": sourcefile.proj_of(s),
                    "classes": [[int(code), sourcefile.CLUTTER_CLASSES.index(name)]
                                for code, name in s.format["classes"].items()],
                    "source": s.title, "notice": s.notice or ""})
        if one("cityjson"):
            s = reg[one("cityjson")]
            params["cityjson"] = {
                "dir": self.link_dir("cityjson", extracted(s.id)),
                "proj": sourcefile.proj_of(s), "ground": s.format["ground"],
                "roof": s.format["roof"], "source": s.title, "notice": s.notice or ""}
        if one("population"):
            s = reg[one("population")]
            fmt = s.format
            csv = []
            if fmt["type"] == "geotiff":
                raster = tiffs(s)
                if raster:
                    params["population"] = {
                        "raster": raster[0], "proj": sourcefile.proj_of(s),
                        "nodata": None if fmt.get("nodata") is None else float(fmt["nodata"]),
                        "source": s.title, "notice": s.notice or ""}
            elif fmt["type"] == "inspire-pd-grid":
                grids = [self.cache.inspire_grid_csv(f) for f in files[s.id] if self.cache.have(f)]
                if grids:
                    path, system, cell = grids[0]
                    params["population"] = {
                        "csv": path, "proj": crs.proj(system), "delimiter": ",", "x": "x",
                        "y": "y", "value": "value", "cell_m": cell, "source": s.title,
                        "notice": s.notice or ""}
            elif fmt["type"] == "gpkg-grid":
                csv = [self.cache.grid_csv(f, fmt["value"]) for f in files[s.id] if self.cache.have(f)]
                layout = {"delimiter": ",", "x": "x", "y": "y", "value": "value"}
            else:
                csv = extracted(s.id)
                layout = {k: str(fmt[k]) for k in ("delimiter", "x", "y", "value")}
            if csv:
                params["population"] = dict(layout, csv=csv[0], proj=sourcefile.proj_of(s),
                                            cell_m=float(fmt["cell_m"]), source=s.title,
                                            notice=s.notice or "")
        return params

    def elevation(self, given, reg, have, extracted):
        """Regional terrain and surface GeoTIFFs as the compiler's pairs: the
        terrain and the surface sources in one system and with one no-data
        value together, read at a quarter of a cell (in the system's units).
        A terrain with no surface in its system stands alone, in place of the
        split's terrain only (3DEP's in GLO-30's); a surface with no terrain
        is no pair, and said so. A source whose tiles come zipped (Brandenburg's)
        gives the GeoTIFFs in them."""
        by_proj = {}
        for role in ("terrain", "surface"):
            for source_id in given.get("elevation_" + role, ()):
                s = reg[source_id]
                key = (sourcefile.proj_of(s), s.format.get("nodata"))
                pair = by_proj.setdefault(key, {"terrain": [], "surface": [], "units": 1.0})
                pair[role].append(s)
                pair["units"] = crs.per_metre(sourcefile.crs_of(s))
        tiffs = lambda s: (extracted if s.format.get("members") else have)(s.id)  # noqa: E731
        out = []
        for (proj, nodata), pair in by_proj.items():
            if not pair["terrain"]:
                raise store.StoreError("%s has no terrain to pair with in its system"
                                       % pair["surface"][0].title)
            both = pair["terrain"] + pair["surface"]
            out.append({
                "terrain": [p for s in pair["terrain"] for p in tiffs(s)],
                "surface": [p for s in pair["surface"] for p in tiffs(s)],
                "proj": proj, "pixel_m": float(self.spec["res_m"]) / 4.0 * pair["units"],
                "nodata": None if nodata is None else float(nodata),
                "source": " and ".join(s.title for s in both),
                "notice": " / ".join(dict.fromkeys(s.notice or "" for s in both))})
        return out

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
        if code == -signal.SIGKILL:
            raise store.StoreError("the compiler was killed during %s, most likely for want of "
                                   "memory: %s" % (self.row["step"] or "its start", more_memory()))
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


def more_memory():
    """How to give the compiler more memory, by what the front runs in: a
    Podman machine or Docker Desktop's VM on macOS and Windows, which sim
    says in SIM_MESH_ENGINE and SIM_MESH_HOST_OS, else this machine."""
    if not os.environ.get("SIM_MESH_IN_CONTAINER"):
        return "this machine has too little free memory for it; close what else holds memory"
    engine = os.environ.get("SIM_MESH_ENGINE", "")
    if os.environ.get("SIM_MESH_HOST_OS") == "Linux":
        return ("the container has no limit of its own under %s on Linux, so the machine has too "
                "little free memory for it; close what else holds memory" % (engine or "its engine"))
    if engine == "podman":
        return ("give the Podman machine more memory, 4 GB or more (podman machine stop; "
                "podman machine set --memory 4096; podman machine start), then start sim again")
    if engine == "docker":
        return ("give Docker more memory, 4 GB or more (Docker Desktop: Settings › Resources › "
                "Memory), then start sim again")
    return "give the container's engine more memory, 4 GB or more, then start sim again"


def refuse(spec):
    """Why a build of `spec` cannot start, before anything is fetched: a
    StoreError, or None."""
    name = store.check_name(spec.get("name"), "geodata")
    if os.path.exists(geodata.geodata_dir(name)):
        raise store.StoreError("there is already geodata called %r" % name)
    sources.check(spec.get("bbox"), float(spec.get("res_m") or 30))
