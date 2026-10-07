"""A script, run: its top to its end, against its simulation.

    sim run SCRIPT --geodata G --nodeset N [--nodeset N2 …] [--name S] [--port P]
                   [--set INPUT=VALUE …]
    sim run SCRIPT (--sim NAME | --resume NAME) [--port P] [--set INPUT=VALUE …]
    sim run SCRIPT --report RUN_DIR

The front runs a script as `python3 -m sim_mesh.runner` when the Scripts tab
says Run, on the Nodes tab's geodata and nodesets, a running simulation or a
paused one, with its output streamed to the page; `sim run` from a shell is
the same. SCRIPT is a path, or the name of one of the store's.

The script is plain Python run from its top (sim_mesh.library): its inputs
take their values from `--set` (the page's choices above the script), and
its declarations (`sim_speed`, `.firmware`, `.on_first_boot`) are collected
until the first thing it does, which starts its simulation with them on
`--geodata` and `--nodeset` (several are merged), or attaches to `--sim`
(or resumes `--resume`) and says its rules to that one. A script that only
declares starts its simulation at its end. The simulation runs on when the
script ends, until something stops it; a script that wants it paused says
`sim_pause()`.

What the script prints and its errors go to the run's `scripts.log` as
well, each line with the run's T, the wall clock and the script's name,
and at a higher log level its commands too (`script_loglevel`).

Then, when the script has a `report(run_dir)` and ran to its end, that
writes the run's `report.md`, and the run's ETSI compliance section
(`compliance.py`) follows it. `--report` writes only that, for a run that
has ended. The process exits 0 when the script ran to its end, 1 when it
raised (the traceback printed), and 2 when it lacked what it needs to run.

A script that lacks an input with no default, or a simulation to run on,
does not run. Under `sim run` (SIM_MESH_SAY_PAGES), the page to choose them
on is said instead, `page: <url>`, the Scripts tab opened on the script with
its world and the inputs that were given; and once a run's simulation is
there, its live map is said the same way. The launcher opens what is said.
"""

import argparse
import inspect
import os
import sys
import traceback
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import compliance                   # noqa: E402 - the path is set just above
import runs as runs_module          # noqa: E402
import script as script_module      # noqa: E402
import store                        # noqa: E402
from sim_mesh import library        # noqa: E402
from sim_mesh import sim as sim_module  # noqa: E402

SAY_PAGES = "SIM_MESH_SAY_PAGES"    # set by `sim run`: say the page to open, `page: <url>`


def script_file(given):
    """A path as it is, or a name of the store's scripts as that script."""
    if os.path.exists(given) or os.sep in given or given.endswith(".py"):
        return given
    return script_module.script_path(given)


def write_report(module, run_dir):
    """The script's `report(run_dir)` as the run's report.md, the run's ETSI
    compliance section after it; whether the script had a report."""
    import asyncio

    report = getattr(module, script_module.REPORT, None)
    if report is None:
        return False
    text = report(run_dir)
    if inspect.isawaitable(text):
        text = asyncio.run(text)
    text = str(text or "").rstrip("\n")
    try:
        tail = compliance.section(run_dir)
    except Exception as err:        # noqa: BLE001 - the script's report still stands
        tail = "## ETSI compliance\n\nNot checked: %s\n" % err
    path = os.path.join(run_dir, runs_module.REPORT_FILE)
    store.write_text(path, (text + "\n\n" if text else "") + tail)
    print("report: %s" % path, flush=True)
    return True


class Logged:
    """A stream that is also the run's scripts.log: each line written, as it
    ends, at the log's lowest level."""

    def __init__(self, stream):
        self.stream = stream
        self.part = ""

    def write(self, text):
        self.stream.write(text)
        self.part += text
        while "\n" in self.part:
            line, self.part = self.part.split("\n", 1)
            library.runtime.log("output", line)
        return len(text)

    def flush(self):
        self.stream.flush()

    def __getattr__(self, name):
        return getattr(self.stream, name)


def page_url(port, **query):
    """The page at the front's port, opened on what `query` says."""
    pairs = [(k, v) for k, vs in query.items()
             for v in (vs if isinstance(vs, (list, tuple)) else [vs]) if v not in (None, "")]
    return "http://localhost:%d/%s" % (port, "?" + urllib.parse.urlencode(pairs) if pairs else "")


def say_page(url):
    """The page this run is to be watched on, for the launcher to open."""
    if os.environ.get(SAY_PAGES):
        print("page: %s" % url, flush=True)


def missing(args, path):
    """What the script lacks to run from here: its inputs without a value,
    and its simulation when none is named. Empty when it can run."""
    out = ["input %s" % row["name"]
           for row in script_module.describe(path).get("inputs") or ()
           if row["name"] not in library.runtime.given and row.get("default") is None]
    if not args.sim and not args.resume and not (args.geodata and args.nodeset):
        out.append("a simulation (--sim, --resume, or --geodata and --nodeset)")
    return out


def run(args):
    path = os.path.abspath(script_file(args.script))
    if args.report:
        module = script_module.module_of(path)
        if not write_report(module, os.path.abspath(args.report)):
            print("%s has no report(run_dir)" % args.script, flush=True)
            return 2
        return 0
    name = os.path.splitext(os.path.basename(path))[0]
    in_store = os.path.dirname(path) == os.path.abspath(store.SCRIPTS_DIR)
    lacking = missing(args, path)
    if lacking:
        # The rest is chosen on the Scripts tab, the script open on what was given.
        if in_store and os.environ.get(SAY_PAGES):
            say_page(page_url(args.port, script=name, geodata=args.geodata,
                              nodeset=args.nodeset or [],
                              **{"set." + k: v for k, v in library.runtime.given.items()}))
            print("%s needs %s: choose it on the Scripts tab" % (name, ", ".join(lacking)),
                  flush=True)
            return 2
        print("! %s needs %s" % (name, ", ".join(lacking)), flush=True)
        return 2
    library.runtime.on_held = lambda sim: say_page(page_url(args.port, sim=sim.name))
    library.runtime.configure(
        script_name=name, geodata=args.geodata, nodesets=args.nodeset or None, name=args.name,
        build=args.build, build_tag=args.build_tag, port=args.port, sim=args.sim,
        resume=args.resume,
        script=name if in_store else None)
    sys.stdout, sys.stderr = Logged(sys.stdout), Logged(sys.stderr)
    try:
        module = script_module.run_file(path, name)
        sim = library.runtime.held()          # a script that only declared starts it now
        print("simulation %s" % sim.name, flush=True)
        run_dir = sim.run_dir
    finally:
        library.runtime.close()
    if run_dir:
        write_report(module, run_dir)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(prog="sim run", description=__doc__.split("\n")[0])
    ap.add_argument("script")
    ap.add_argument("--sim", help="the running simulation to run it on")
    ap.add_argument("--resume", metavar="SIM",
                    help="the paused simulation to resume and run it on, at the script's "
                         "speed (real unless it says otherwise)")
    ap.add_argument("--geodata")
    ap.add_argument("--nodeset", action="append",
                    help="a nodeset, given once or more; several are merged")
    ap.add_argument("--name", help="the new simulation's name")
    ap.add_argument("--build", help="an installed firmware every node given one runs instead")
    ap.add_argument("--build-tag", help="with --build: only the nodes carrying this tag")
    ap.add_argument("--port", type=int, default=sim_module.DEFAULT_PORT)
    ap.add_argument("--report", metavar="RUN_DIR",
                    help="only write that run's report, with the script's report(run_dir)")
    ap.add_argument("--set", action="append", default=[], metavar="INPUT=VALUE",
                    help="a value for one of the script's inputs")
    args = ap.parse_args(argv)
    for given in args.set:
        name, sep, value = given.partition("=")
        if not sep or not name.strip():
            ap.error("--set takes INPUT=VALUE, not %r" % given)
        library.runtime.given[name.strip()] = value.strip()
    try:
        return run(args)
    except (store.StoreError, sim_module.SimError, library.ScriptError) as err:
        print("! %s" % err, flush=True)
        return 1
    except KeyboardInterrupt:
        return 130
    except Exception:                       # noqa: BLE001 - the script's own, shown whole
        traceback.print_exc()
        sys.stderr.flush()
        return 1


if __name__ == "__main__":
    sys.exit(main())
