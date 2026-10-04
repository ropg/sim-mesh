"""The source files: where a pack's ground comes from, as data.

    sim-mesh/sources/sources.yaml             what sim-mesh ships, grouped
    sim-mesh/sources/outlines/<id>.geojson    a regional source's coverage
    testbed/sources.yaml                      a person's own, the same shape
    testbed/sources/outlines/<id>.geojson

```yaml
global:                             # sources with data everywhere
  - { id: glo30, … }
europe:                             # a continent
  DE:                               # a country, by ISO 3166-1 alpha-2
    name: Germany
    sources:
      - { id: berlin-dgm1, … }
```

A source picks one of sim-mesh's methods for finding its files, one way of
fetching them and one format they are in, and fills in their parameters;
it never carries code (README, "Sources"). This module reads the files,
checks every entry whole, and says each one's faults with its file and id;
sources.py plans and fetches from what it reads.

**The compiler reads what it reads.** Until it takes its readers' parameters
from a source, a format's parameters must be the ones its reader assumes:
a `csv-grid` is Zensus's layout, an `xyz` or `citygml` Berlin's, a
landcover `geotiff` WorldCover's classes. A source that asks for anything
else is refused here, rather than read wrongly in a build.

Run as a script it is `sim source`: `check` reads every file and says what
is wrong, `outline ID` writes a source's outline from its own feed, or
from a Geofabrik region (`--geofabrik germany`).
"""

import argparse
import copy
import json
import os
import re
import sys

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import crs as crs_module  # noqa: E402
import store  # noqa: E402

KNOWN_CRS = "EPSG:4326, UTM 326zz/327zz/258zz, 3035, 28992, 7415"
SIM_MESH_ROOT = os.path.dirname(HERE)
SHIPPED = os.path.join(SIM_MESH_ROOT, "sources", "sources.yaml")
OWN = os.path.join(store.SIM_DIR, "sources.yaml")
OUTLINES = "outlines"               # beside a file: outlines/<id>.geojson

GLOBAL = "global"
CONTINENTS = ("africa", "antarctica", "asia", "europe", "north-america", "oceania",
              "south-america")
COUNTRY_RE = re.compile(r"^[A-Z]{2}$")
LAYERS = ("surface", "terrain", "landcover", "buildings", "population", "roads", "places",
          "radio-climate")
CLUTTER_CLASSES = ("open", "water", "low-vegetation", "forest", "suburban", "urban",
                   "dense-urban", "industrial")
COVERAGES = ("worldwide", "outline")
READS = ("whole", "window")
METHODS = {
    # method: (parameters it needs, its addresses, which may each be a list of mirrors)
    "template": (("url", "tile", "crs"), ("url",)),
    "atom": (("feed", "name", "unit_m", "size_m", "crs"), ("feed",)),
    "index": (("index", "index_format", "crs", "url_property"), ("index",)),
    "regions": (("index", "index_format", "file"), ("index",)),
    "file": (("url",), ("url",)),
}
INDEX_FORMATS = {"regions": ("geofabrik",), "index": ("geojson", "flatgeobuf")}
CRS_RE = re.compile(r"^EPSG:(\d+)$")
TILE_FIELD_RE = re.compile(r"\{(ns|ew|lat|lon)(?::0(\d))?\}")
FORMATS = {
    # format: the layers it can feed
    "geotiff": ("surface", "terrain", "landcover"),
    "xyz": ("surface", "terrain"),
    "citygml": ("buildings",),
    "cityjson": ("buildings",),
    "osm-pbf": ("roads", "places", "buildings"),
    "csv-grid": ("population",),
    "gpkg-grid": ("population",),
    "itu-p1812-maps": ("radio-climate",),
}
# A format's own parameters, each needed.
FORMAT_NEEDS = {
    "cityjson": ("crs", "ground", "roof"),
    "csv-grid": ("delimiter", "x", "y", "value", "crs", "cell_m", "members"),
    "gpkg-grid": ("value", "crs", "cell_m", "members"),
}
# What the compiler's readers assume, which a source's parameters must be
# until it takes them from the source.
WORLDCOVER_CLASSES = {10: "forest", 20: "low-vegetation", 30: "low-vegetation", 40: "open",
                      50: "urban", 60: "open", 70: "open", 80: "water", 90: "low-vegetation",
                      95: "low-vegetation", 100: "open"}
COMPILER_READS = {
    "xyz": {"crs": "EPSG:25833", "spacing_m": 1},
    "citygml": {"lod": 2, "crs": "EPSG:25833"},
    "itu-p1812-maps": {"members": {"delta_n": "DN50.TXT", "n0": "N050.TXT"}},
}


class SourceFault(store.StoreError):
    """A source file that does not hold together; the message names it."""


class Source:
    """One source, read and checked: its entry's fields, where in the file
    it stands, and its outline."""

    def __init__(self, entry, path, continent, country=None, country_name=None):
        self.entry = entry
        self.path = path
        self.continent = continent
        self.country = country
        self.country_name = country_name
        self.outline = None
        for key in ("id", "title", "holds", "licence", "notice", "redistributable", "layers",
                    "resolution_m", "coverage", "find", "read", "format"):
            setattr(self, key, entry.get(key))

    @property
    def worldwide(self):
        return self.coverage == "worldwide"

    @property
    def where(self):
        """Where it stands in the file, for the page: `global`, or
        `Europe › Germany`."""
        if self.continent == GLOBAL:
            return GLOBAL
        return "%s › %s" % (self.continent.replace("-", " ").title(),
                            self.country_name or self.country)

    def addresses(self, key):
        """An address of its `find` and its mirrors, in the order tried."""
        value = self.find[key]
        return [str(v) for v in value] if isinstance(value, list) else [str(value)]

    def address(self, key):
        return self.addresses(key)[0]

    @property
    def format_type(self):
        return self.format["type"]

    def members(self):
        """What a build reads of one of its zips: names, or one `*.ext`."""
        got = self.format.get("members")
        if isinstance(got, dict):
            return tuple(str(v) for v in got.values())
        return got

    def as_dict(self):
        """What the page is told of it."""
        return {"source": self.id, "what": self.title, "holds": self.holds or "",
                "licence": self.licence or "", "layers": dict(self.layers),
                "where": self.where, "worldwide": self.worldwide,
                "own": self.path != SHIPPED}


def _fault(path, ident, sentence):
    return SourceFault("%s: %s%s" % (os.path.relpath(path, SIM_MESH_ROOT)
                                     if path.startswith(SIM_MESH_ROOT) else path,
                                     "%s: " % ident if ident else "", sentence))


def _entries(data, path):
    """Every entry of a file, with where it stands: [(entry, continent,
    country, name)]."""
    if data is None:
        return []
    if not isinstance(data, dict):
        raise _fault(path, None, "a source file is a mapping of `global` and continents")
    out = []
    for key, value in data.items():
        if key == GLOBAL:
            if not isinstance(value, list):
                raise _fault(path, None, "`global` is a list of sources")
            out += [(e, GLOBAL, None, None) for e in value]
            continue
        if key not in CONTINENTS:
            raise _fault(path, None, "%r is neither `global` nor a continent (%s)"
                         % (key, ", ".join(CONTINENTS)))
        if not isinstance(value, dict):
            raise _fault(path, None, "a continent is a mapping of countries")
        for code, country in value.items():
            if not COUNTRY_RE.match(str(code)):
                raise _fault(path, None, "%s: a country is its ISO 3166-1 alpha-2 code, not %r"
                             % (key, code))
            if not isinstance(country, dict) or not isinstance(country.get("sources"), list):
                raise _fault(path, None, "%s %s: a country is {name, sources: [...]}"
                             % (key, code))
            out += [(e, key, str(code), str(country.get("name") or code))
                    for e in country["sources"]]
    return out


def _number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _check(source):
    """A source's faults, the first one raised."""
    e, path, ident = source.entry, source.path, source.entry.get("id")
    if not isinstance(e, dict):
        raise _fault(path, None, "a source is a mapping")
    if not isinstance(ident, str) or not store.NAME_RE.match(ident):
        raise _fault(path, None, "%r is not a usable source id" % (ident,))
    for key in ("title", "licence"):
        if not e.get(key):
            raise _fault(path, ident, "it names no %s" % key)
    if not isinstance(e.get("redistributable"), bool):
        raise _fault(path, ident, "redistributable is true or false")
    if e.get("redistributable") and not e.get("notice"):
        raise _fault(path, ident, "a source a pack may carry gives the notice it carries")
    layers = e.get("layers")
    if not isinstance(layers, dict) or not layers:
        raise _fault(path, ident, "layers is {layer: priority, …}")
    for layer, priority in layers.items():
        if layer not in LAYERS:
            raise _fault(path, ident, "%r is no layer (%s)" % (layer, ", ".join(LAYERS)))
        if not isinstance(priority, int) or isinstance(priority, bool):
            raise _fault(path, ident, "%s's priority is a whole number" % layer)
    if e.get("resolution_m") is not None and not (_number(e["resolution_m"])
                                                  and e["resolution_m"] > 0):
        raise _fault(path, ident, "resolution_m is a number of metres")
    if e.get("coverage") not in COVERAGES:
        raise _fault(path, ident, "coverage is %s" % " or ".join(COVERAGES))
    if (e["coverage"] == "worldwide") != (source.continent == GLOBAL):
        raise _fault(path, ident, "a worldwide source stands under `global`, and only one does")
    _check_find(source)
    if e.get("read") not in READS:
        raise _fault(path, ident, "read is %s" % " or ".join(READS))
    _check_format(source)
    if e["read"] == "window" and (e["format"]["type"] != "geotiff" or e["coverage"] == "worldwide"):
        raise _fault(path, ident, "a window is read of a regional source's cloud-optimised "
                                  "GeoTIFFs")


def _check_find(source):
    e, path, ident = source.entry, source.path, source.entry["id"]
    find = e.get("find")
    if not isinstance(find, dict) or find.get("method") not in METHODS:
        raise _fault(path, ident, "find.method is one of %s" % ", ".join(METHODS))
    needs, addresses = METHODS[find["method"]]
    for key in needs:
        if find.get(key) in (None, "", []):
            raise _fault(path, ident, "find (%s) needs %s" % (find["method"], key))
    for key in addresses:
        for url in source.addresses(key):
            if not re.match(r"^https?://", url):
                raise _fault(path, ident, "find.%s is an http(s) address, not %r" % (key, url))
    if "crs" in find and not crs_module.known(find["crs"]):
        raise _fault(path, ident, "find.crs %s is not a system sim-mesh knows (%s)"
                     % (find["crs"], KNOWN_CRS))
    if find.get("missing", "no-data") not in ("no-data", "error"):
        raise _fault(path, ident, "find.missing is no-data (a file no host has is no data "
                                  "there) or error")
    method = find["method"]
    if method == "template":
        if any("{tile}" not in u for u in source.addresses("url")):
            raise _fault(path, ident, "a template's url holds {tile}")
        size = find.get("size_deg")
        if find["crs"] != "EPSG:4326" or not isinstance(size, int) or isinstance(size, bool) \
                or size < 1:
            raise _fault(path, ident, "a template's tiles are a whole number of degrees "
                                      "(size_deg) of EPSG:4326")
        fields = {m.group(1) for m in TILE_FIELD_RE.finditer(find["tile"])}
        if fields != {"ns", "ew", "lat", "lon"}:
            raise _fault(path, ident, "a template's tile names its corner with {ns}, {lat}, "
                                      "{ew} and {lon}")
    elif method == "atom":
        try:
            pattern = re.compile(find["name"])
        except re.error as err:
            raise _fault(path, ident, "find.name is no pattern: %s" % err) from err
        if not {"x", "y"} <= set(pattern.groupindex):
            raise _fault(path, ident, "find.name has the groups (?P<x>…) and (?P<y>…)")
        if not (_number(find["unit_m"]) and _number(find["size_m"])):
            raise _fault(path, ident, "unit_m and size_m are numbers of metres")
        epsg = int(CRS_RE.match(find["crs"]).group(1))
        if not (32601 <= epsg <= 32660 or 32701 <= epsg <= 32760 or 25801 <= epsg <= 25860):
            raise _fault(path, ident, "an atom feed's tiles are in a UTM zone (326zz, 327zz, "
                                      "258zz)")
    elif method in ("regions", "index"):
        if find["index_format"] not in INDEX_FORMATS[method]:
            raise _fault(path, ident, "a %s source's index_format is one of %s"
                         % (method, ", ".join(INDEX_FORMATS[method])))
    if method == "index":
        try:
            crs_module.plane(find["crs"])
        except store.StoreError as err:
            raise _fault(path, ident, str(err)) from err


def _check_format(source):
    e, path, ident = source.entry, source.path, source.entry["id"]
    fmt = e.get("format")
    if not isinstance(fmt, dict) or fmt.get("type") not in FORMATS:
        raise _fault(path, ident, "format.type is one of %s" % ", ".join(FORMATS))
    kind = fmt["type"]
    for layer in e["layers"]:
        if layer not in FORMATS[kind]:
            raise _fault(path, ident, "a %s file feeds %s, not %s"
                         % (kind, ", ".join(FORMATS[kind]), layer))
    if kind == "geotiff" and "landcover" in e["layers"]:
        classes = fmt.get("classes")
        if not isinstance(classes, dict) or not classes:
            raise _fault(path, ident, "a landcover geotiff maps its codes: classes {code: class}")
        for code, name in classes.items():
            if name not in CLUTTER_CLASSES:
                raise _fault(path, ident, "%r is no clutter class (%s)"
                             % (name, ", ".join(CLUTTER_CLASSES)))
        if {int(k): v for k, v in classes.items()} != WORLDCOVER_CLASSES:
            raise _fault(path, ident, "the compiler reads land cover as WorldCover's classes "
                                      "only, so far")
    if kind == "geotiff":
        heights = {"surface", "terrain"} & set(e["layers"])
        if heights and len(e["layers"]) > 1:
            raise _fault(path, ident, "a GeoTIFF of heights feeds surface or terrain, one of them")
        if "landcover" in e["layers"] or e["coverage"] == "worldwide":
            # Read through the GLO-30 and WorldCover paths: tiles in degrees.
            if e["find"].get("crs") not in (None, "EPSG:4326"):
                raise _fault(path, ident, "the compiler reads land cover, and a worldwide "
                                          "surface, as GeoTIFF tiles in EPSG:4326, so far")
            if e["coverage"] == "worldwide" and heights != {"surface"} and heights:
                raise _fault(path, ident, "a worldwide GeoTIFF of heights is a surface, the "
                                          "ground the compiler splits")
    for key in FORMAT_NEEDS.get(kind, ()):
        if fmt.get(key) in (None, ""):
            raise _fault(path, ident, "a %s format needs %s" % (kind, key))
    if "crs" in fmt and not crs_module.known(fmt["crs"]):
        raise _fault(path, ident, "format.crs %s is not a system sim-mesh knows (%s)"
                     % (fmt["crs"], KNOWN_CRS))
    if "nodata" in fmt and (kind != "geotiff" or not _number(fmt["nodata"])):
        raise _fault(path, ident, "format.nodata is the number a GeoTIFF of heights writes "
                                  "where it has none")
    if kind == "csv-grid" and len(str(fmt["delimiter"])) != 1:
        raise _fault(path, ident, "a csv-grid's delimiter is one character")
    if kind in ("csv-grid", "gpkg-grid") and not (_number(fmt["cell_m"]) and fmt["cell_m"] > 0):
        raise _fault(path, ident, "cell_m is the grid's cell, in metres")
    if kind == "osm-pbf":
        gives = fmt.get("gives")
        if not isinstance(gives, list) or set(gives) != set(e["layers"]):
            raise _fault(path, ident, "an osm-pbf's gives are the layers it feeds")
    for key, want in COMPILER_READS.get(kind, {}).items():
        if fmt.get(key) != want:
            raise _fault(path, ident, "the compiler reads %s with %s %s only, so far"
                         % (kind, key, json.dumps(want, ensure_ascii=False)))
    if kind in ("xyz", "citygml", "itu-p1812-maps") and not fmt.get("members"):
        raise _fault(path, ident, "format.members names what a build reads of the zip")


def proj_of(source):
    """The proj string a source's files are read in by the compiler: its
    format's system, else its finding method's."""
    return crs_module.proj(source.format.get("crs") or source.find.get("crs") or "EPSG:4326")


def _load_outline(source):
    """A regional source's outline: outlines/<id>.geojson beside its file,
    a Polygon or MultiPolygon in degrees (a Feature or a one-feature
    collection of one too)."""
    path = os.path.join(os.path.dirname(source.path), OUTLINES, source.id + ".geojson")
    try:
        with open(path, encoding="utf-8") as handle:
            got = json.load(handle)
    except FileNotFoundError as err:
        raise _fault(source.path, source.id, "its outline %s is not there"
                     % os.path.join(OUTLINES, source.id + ".geojson")) from err
    except (OSError, ValueError) as err:
        raise _fault(source.path, source.id, "its outline is no GeoJSON: %s" % err) from err
    if isinstance(got, dict) and got.get("type") == "FeatureCollection":
        features = got.get("features") or []
        got = features[0] if len(features) == 1 else None
    if isinstance(got, dict) and got.get("type") == "Feature":
        got = got.get("geometry")
    if not isinstance(got, dict) or got.get("type") not in ("Polygon", "MultiPolygon"):
        raise _fault(source.path, source.id, "its outline is a Polygon or MultiPolygon")
    polygons = [got["coordinates"]] if got["type"] == "Polygon" else got["coordinates"]
    for polygon in polygons:
        for ring in polygon:
            if len(ring) < 4 or ring[0] != ring[-1] or not all(
                    len(p) >= 2 and -180 <= p[0] <= 180 and -90 <= p[1] <= 90 for p in ring):
                raise _fault(source.path, source.id, "its outline has a ring that is not closed "
                                                     "or not in degrees")
    return {"type": got["type"], "coordinates": got["coordinates"]}


def read_file(path, required=False):
    """The sources of one file, checked: [Source]. A file not there is none
    unless `required`."""
    try:
        with open(path, encoding="utf-8") as handle:
            data = yaml.safe_load(handle)
    except FileNotFoundError:
        if required:
            raise _fault(path, None, "not there") from None
        return []
    except (OSError, yaml.YAMLError) as err:
        raise _fault(path, None, str(err)) from err
    out = []
    for entry, continent, country, name in _entries(data, path):
        source = Source(entry if isinstance(entry, dict) else {}, path, continent, country, name)
        if not isinstance(entry, dict):
            raise _fault(path, None, "a source is a mapping")
        _check(source)
        if source.coverage == "outline":
            source.outline = _load_outline(source)
        out.append(source)
    return out


_cache = {}


def load(paths=None):
    """Every source known here, by id, in file order: sim-mesh's own, then
    the person's. An id twice is refused, a person's taking a shipped one's
    included: another address for the same data is a mirror, in its entry.
    A file is read again only when it changes."""
    paths = paths or (SHIPPED, OWN)
    key = []
    for path in paths:
        try:
            key.append((path, os.path.getmtime(path)))
        except OSError:
            key.append((path, None))
    key = tuple(key)
    if key in _cache:
        return _cache[key]
    out = {}
    for i, path in enumerate(paths):
        for source in read_file(path, required=i == 0 and path == SHIPPED):
            if source.id in out:
                raise _fault(path, source.id, "that id is %s's already" % (
                    "sim-mesh's" if out[source.id].path == SHIPPED else "another source"))
            out[source.id] = source
    _cache.clear()
    _cache[key] = out
    return out


# ---- outlines --------------------------------------------------------------------

def tile_quads(source, tiles):
    """An atom source's tiles as polygons in degrees: [(x, y)] corners in its
    feed's units."""
    import geodata
    find = source.find
    zone = geodata.TransverseMercator(*geodata.utm_zone_of(int(find["crs"].split(":")[1])))
    unit, size = float(find["unit_m"]), float(find["size_m"])
    out = []
    for x, y in tiles:
        x0, y0 = x * unit, y * unit
        ring = []
        for cx, cy in ((x0, y0), (x0 + size, y0), (x0 + size, y0 + size), (x0, y0 + size),
                       (x0, y0)):
            lat, lon = zone.inverse(cx, cy)
            ring.append([round(lon, 6), round(lat, 6)])
        out.append([ring])
    return out


def write_outline(source, geometry, attribution=None):
    """A source's outline written beside its file."""
    path = os.path.join(os.path.dirname(source.path), OUTLINES, source.id + ".geojson")
    feature = {"type": "Feature", "properties": {"source": source.id}, "geometry": geometry}
    if attribution:
        feature["properties"]["attribution"] = attribution
    store.write_text(path, json.dumps(feature, separators=(",", ":")) + "\n")
    return path


def _raw_sources(path):
    """A file's entries as written, unchecked: for making the outlines the
    check wants."""
    try:
        with open(path, encoding="utf-8") as handle:
            data = yaml.safe_load(handle)
    except FileNotFoundError:
        return []
    return [Source(copy.deepcopy(entry), path, continent, country, name)
            for entry, continent, country, name in _entries(data, path)
            if isinstance(entry, dict)]


def main(argv=None):
    import asyncio
    import aiohttp
    import sources as sources_module

    ap = argparse.ArgumentParser(prog="sim source", description="the source files")
    sub = ap.add_subparsers(dest="verb", required=True)
    sub.add_parser("check", help="read every source file and say what is wrong")
    sub.add_parser("list", help="every source, where it stands and what it feeds")
    p = sub.add_parser("outline", help="write a source's outline: an atom feed's tiles, or a "
                                       "Geofabrik region's outline")
    p.add_argument("id")
    p.add_argument("--geofabrik", metavar="REGION", help="the outline of this Geofabrik region")
    args = ap.parse_args(argv)
    try:
        if args.verb == "check":
            got = load()
            print("%d sources, all whole" % len(got))
        elif args.verb == "list":
            for s in load().values():
                print("%-16s %-22s %-40s %s" % (s.id, s.where, s.title, ", ".join(
                    "%s %d" % kv for kv in s.layers.items())))
        else:
            raw = _raw_sources(SHIPPED) + _raw_sources(OWN)
            source = next((s for s in raw if s.id == args.id), None)
            if source is None:
                raise SourceFault("no source %s in %s or %s" % (args.id, SHIPPED, OWN))

            async def make():
                async with aiohttp.ClientSession() as session:
                    cache = sources_module.Cache(session)
                    if args.geofabrik:
                        index = await cache.regions_index(next(
                            s for s in raw if (s.find or {}).get("method") == "regions"
                            and (s.find or {}).get("index_format") == "geofabrik"))
                        geometry = sources_module.outline(index, args.geofabrik)
                        if geometry is None:
                            raise SourceFault("Geofabrik has no region %s" % args.geofabrik)
                        return geometry, ("outline © OpenStreetMap contributors, ODbL 1.0, "
                                          "by Geofabrik")
                    if source.find.get("method") != "atom":
                        raise SourceFault("%s finds no tiles to outline: give --geofabrik REGION"
                                          % source.id)
                    tiles = sources_module.atom_tiles(source, await cache.feed(source))
                    return {"type": "MultiPolygon",
                            "coordinates": tile_quads(source, [(x, y) for _u, x, y in tiles])}, None
            geometry, attribution = asyncio.run(make())
            print("wrote %s" % write_outline(source, geometry, attribution))
    except store.StoreError as err:
        print("sim source: %s" % err, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
