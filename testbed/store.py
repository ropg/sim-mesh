"""Where geodata, nodesets, scripts, loss tables, coverage, runs and snapshots
live, and what a name for any of them may be.

    testbed/geodata/<name>/           the ground: a planner pack, or synthetic ground at 0°, 0°;
                                      geodata.yaml, and the pack's files beside it
    testbed/nodesets/<name>.yaml      which nodes stand where, with their device, role and radio
    testbed/scripts/<name>.py         Python against the sim-mesh library: setup and drivers
    testbed/losses/<geodata>/<nodeset geometry hash>/<band>.bin
                                      derived loss tables, a cache (not kept in git);
                                      <band>-loc<pct>.bin for a pack's stated loc_pct
    testbed/coverage/<geodata>/<key>.bin
                                      one node's coverage raster, a cache (not kept in git)
    testbed/runs/<name>/              one simulation's output (not kept in git)
    testbed/snapshots/<name>/         a moment of a run, to start another from

Every one of these is referred to by name, and a name is also a directory, a
hostname and a proxy label, so one rule serves them all: lower-case letters,
digits and hyphens, starting and ending with a letter or digit, at most 32.
"""

import json
import os
import re

import yaml

SIM_DIR = os.path.dirname(os.path.abspath(__file__))
GEODATA_DIR = os.path.join(SIM_DIR, "geodata")
NODESETS_DIR = os.path.join(SIM_DIR, "nodesets")
SCRIPTS_DIR = os.path.join(SIM_DIR, "scripts")
LOSSES_DIR = os.path.join(SIM_DIR, "losses")
COVERAGE_DIR = os.path.join(SIM_DIR, "coverage")
RUNS_DIR = os.path.join(SIM_DIR, "runs")
SNAPSHOTS_DIR = os.path.join(SIM_DIR, "snapshots")

NAME_RE = re.compile(r"^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$")
# What a plain scalar reads as, by the rules the readers' yaml.safe_load uses.
PLAIN = yaml.resolver.Resolver()
STR_TAG = "tag:yaml.org,2002:str"


class StoreError(Exception):
    """Geodata, a nodeset, a script, a table, a run or a snapshot could not be
    read, written or changed as asked. The message is meant for the page as is."""


def load_yaml(handle):
    """yaml.safe_load of an open file, through libyaml where PyYAML has it.

    The C loader builds with safe_load's own constructor and resolver; only
    its scanner and parser are libyaml's. It reads a city's 840 KB nodeset in
    0.5 s where safe_load takes 2.3. A document libyaml will not read is read
    by safe_load, as before, so a refusal says what safe_load says."""
    text = handle.read()
    loader = getattr(yaml, "CSafeLoader", None)
    if loader is not None:
        try:
            return yaml.load(text, Loader=loader)
        except yaml.YAMLError:
            pass
    return yaml.safe_load(text)


def check_name(name, what="name"):
    """A name that can be a directory, a hostname and a proxy label at once."""
    if not isinstance(name, str) or not NAME_RE.match(name):
        raise StoreError(
            "%r is not a usable %s name: lower-case letters, digits and "
            "hyphens, starting and ending with a letter or digit" % (name, what))
    return name


def slug(text, fallback):
    """The nearest usable name to some free text (an imported node's label).

    Anything that is not a lower-case letter or digit becomes a hyphen, runs
    of them collapse, and the result is cut to the length a name may have.
    Text with nothing usable in it gives `fallback`.
    """
    out = re.sub(r"[^a-z0-9]+", "-", str(text or "").lower()).strip("-")[:32].strip("-")
    return out if NAME_RE.match(out) else fallback


def listing(directory, suffix=".yaml"):
    """Every name with a file of this suffix in a directory, sorted."""
    if not os.path.isdir(directory):
        return []
    return sorted(entry[:-len(suffix)] for entry in os.listdir(directory)
                  if entry.endswith(suffix) and NAME_RE.match(entry[:-len(suffix)]))


def scalar(value):
    """One YAML scalar. JSON's spelling of a string is also YAML's; a string
    that YAML would read back as itself unquoted is written bare, so a file
    stays pleasant to edit by hand."""
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return repr(value)
    if isinstance(value, float):
        value = round(value, 9)
        return repr(int(value)) if value == int(value) and abs(value) < 1e15 else repr(value)
    text = str(value)
    if re.match(r"^[A-Za-z_][A-Za-z0-9_.\-/]*$", text) and text.lower() not in (
            "true", "false", "yes", "no", "on", "off", "null", "y", "n", "~"):
        return text
    return json.dumps(text, ensure_ascii=False)


def name_scalar(name):
    """A name as a YAML scalar: bare, as a nodeset has always written one,
    unless YAML would read it bare as something other than the name, and
    then quoted. A name may be a YAML word or number: bare, `no` and `on`
    read as booleans, `null` as nothing and `010` as the number 8."""
    if NAME_RE.match(name) and PLAIN.resolve(yaml.ScalarNode, name, (True, False)) == STR_TAG:
        return name
    return json.dumps(name, ensure_ascii=False)


def flow(value):
    """A value as one line of YAML: a flow mapping, a flow list or a scalar."""
    if isinstance(value, dict):
        return "{ %s }" % ", ".join("%s: %s" % (scalar(k), flow(v)) for k, v in value.items()) \
            if value else "{}"
    if isinstance(value, (list, tuple)):
        return "[%s]" % ", ".join(flow(v) for v in value)
    return scalar(value)


def write_text(path, text):
    """Write a file whole, through a temporary beside it, so a reader never
    sees half of one; this process's own, so a writer in another does not
    rename it away."""
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    tmp = "%s.%d.tmp" % (path, os.getpid())
    with open(tmp, "w", encoding="utf-8") as handle:
        handle.write(text)
    os.replace(tmp, path)
