"""Where a pack's ground comes from: what a rectangle needs of each source,
and the cache it is fetched into. The sources themselves are data
(sourcefile.py reads them); this module knows the methods they name.

```
front ── GET <regions index> ──────────► its host (Geofabrik's)    every region's outline, weekly
front ── GET <atom feed> ──────────────► its host (Berlin's)       a source's tiles, weekly
front ── HEAD <file> ──────────────────► the source's host         its size, once
front ── GET <file>, Range: bytes=<n>- ► the source's host         into the cache, resumed;
                                                                   a mirror when one fails
```

    testbed/geodata/.cache/<source id>/<file>   one source's files; a zip's members in x/
    testbed/geodata/.cache/meta/                what sources list (a regions index as
                                                <id>-<its name>, an atom feed as <id>.atom),
                                                and the MeshCore map's node list; a week old
                                                at most
    testbed/geodata/.cache/sizes.json           each file's size as its host said, by URL

**Finding a rectangle's files**, by the method a source names:

- `template`: the size_deg tiles of EPSG:4326 meeting the rectangle's grid,
  each named by its south-west corner, its address the url with {tile}.
- `atom`: the tiles a feed lists whose square, its corner read off the file
  name in the feed's units, meets the rectangle's grid in the feed's zone.
- `regions`: the smallest region of the index whose outline holds the whole
  rectangle, its `file`.
- `file`: the one file.

A file is cached under its address's own name: a template's or a feed's
tile as its URL names it, a region's as `<region>` and the URL's suffix
(`berlin.osm.pbf`), a single file as the URL names it when that is a plain
file name and else as `<id>` and its suffix (`itu.zip`).

Every build shares the cache, so a second region beside the first fetches
only what is new. A file is fetched once: into `<file>.part`, resumed from
where it stopped by a range request, and renamed into place when whole. A
file its host does not have (GLO-30 has no tile over open sea) leaves a
`<file>.absent` beside it and is not asked for again, unless its source says
a missing file is an error (`missing: error`). An address with mirrors is
tried in order, and the next one when one fails.

**A build takes every source whose coverage meets its rectangle**, and the
compiler takes from each layer's sources by their priority there: Berlin's
own data where the rectangle touches Berlin, GLO-30 and OpenStreetMap's
buildings on the rest, the Zensus grid where it touches Germany (`plan`
says which, and what each is used for).

Every request names sim-mesh in its User-Agent, a 429 or 5xx is retried after
the Retry-After the host gives (or a growing pause), and nothing is fetched
that a build does not need.
"""

import asyncio
import contextlib
import gzip
import hashlib
import html
import json
import math
import os
import re
import shutil
import struct
import time
import urllib.parse
import zipfile

import aiohttp

import cogwindow
import crs
import fgb
import geodata
import sourcefile
import store

CACHE_DIR = os.path.join(store.GEODATA_DIR, ".cache")
USER_AGENT = "sim-mesh (+https://github.com/sim-mesh/sim-mesh)"
CHUNK = 1 << 16
TRIES = 4
BACKOFF_S = 5.0
META_MAX_AGE_S = 7 * 86400          # a feed or an index kept this long unless the source says
HEAD_PARALLEL = 6
MAX_CELLS = 25_000_000              # a grid past this is refused: rasters are held whole
RESOLUTIONS = (30.0, 10.0)
PLAIN_NAME_RE = re.compile(r"^[A-Za-z0-9._-]+$")
WORLD = [-180.0, -85.0, 180.0, 85.0]

# What the cache holds that is no source's, for the page's tooltip on its row.
CACHE_HOLDS = {
    "meta": "What the sources list, fetched at most weekly: Geofabrik's index of extracts and "
            "their outlines, Berlin's tile feeds, the MeshCore map's node list.",
    "osmtiles": "OpenStreetMap's rendered map tiles, for the build view's map; never part of "
                "a pack.",
}
LAYER_TEXT = {"surface": "surface heights", "terrain": "terrain", "landcover": "land cover classes",
              "buildings": "buildings", "population": "population", "roads": "roads",
              "places": "places", "radio-climate": "the radio climate (ΔN and N0)"}


class SourceError(store.StoreError):
    """A source could not be read or fetched; the message is for the page."""


class File:
    """One file of one source: where it comes from (its mirrors after it),
    where it goes in the cache, and for a zip, which of its members a build
    reads (extracted into `x/` beside it)."""

    def __init__(self, source, url, name, members=None, missing="no-data", sha256=None,
                 window=None):
        self.source, self.name, self.members, self.missing = source, name, members, missing
        self.urls = list(url) if isinstance(url, (list, tuple)) else [url]
        self.sha256 = sha256            # what the index says the file is, checked on arrival
        # A window of a cloud-optimised GeoTIFF: {box, want}, the box in the
        # file's system and the pixel to read it at (cogwindow.py). `needs`
        # is the byte ranges it takes, once `Cache.window_needs` knows them.
        self.window = window
        self.needs = None
        self.length = None
        self.head_parts = []

    @property
    def url(self):
        return self.urls[0]

    def path(self, root):
        return os.path.join(root, self.source, self.name)

    def __repr__(self):
        return "File(%s/%s)" % (self.source, self.name)


def registry(sources=None):
    """The sources to plan from: those given, else every source file's."""
    return sources if sources is not None else sourcefile.load()


# ---- the rectangle ---------------------------------------------------------

def utm_zone(lon):
    return min(60, max(1, int(math.floor((lon + 180) / 6)) + 1))


def utm_hull(bbox, tm):
    """The rectangle's grid in a zone, as the compiler lays it: the extent of
    its four corners projected, (x0, y0, x1, y1) in metres."""
    lon0, lat0, lon1, lat1 = bbox
    xy = [tm.forward(lat, lon) for lon, lat in ((lon0, lat0), (lon0, lat1),
                                                  (lon1, lat0), (lon1, lat1))]
    return (min(p[0] for p in xy), min(p[1] for p in xy),
            max(p[0] for p in xy), max(p[1] for p in xy))


def degree_hull(bbox, tm):
    """The degrees the rectangle's grid reaches: its UTM hull's corners back
    in degrees. The grid bows past the rectangle with meridian convergence,
    so a tile source is asked for this, not for the rectangle."""
    x0, y0, x1, y1 = utm_hull(bbox, tm)
    ll = [tm.inverse(x, y) for x, y in ((x0, y0), (x0, y1), (x1, y0), (x1, y1))]
    return [min(p[1] for p in ll), min(p[0] for p in ll),
            max(p[1] for p in ll), max(p[0] for p in ll)]


def check(bbox, res_m):
    """The grid a rectangle makes at a resolution: {zone, epsg, cells: [nx,
    ny]}, or a StoreError with the sentence saying why it is refused."""
    try:
        lon0, lat0, lon1, lat1 = (float(v) for v in bbox)
    except (TypeError, ValueError) as err:
        raise store.StoreError("a rectangle is four numbers, west, south, east, north") from err
    if not (-180 <= lon0 < lon1 <= 180 and -80 <= lat0 < lat1 <= 84):
        raise store.StoreError("the rectangle is not west to east and south to north, "
                               "inside 80° S to 84° N")
    if float(res_m) not in RESOLUTIONS:
        raise store.StoreError("the resolution is 30 m or 10 m")
    zone = utm_zone((lon0 + lon1) / 2)
    if lon1 - lon0 > 6:
        raise store.StoreError("the rectangle is %.1f° wide, wider than its UTM zone (%d, 6°)"
                               % (lon1 - lon0, zone))
    tm = geodata.TransverseMercator(zone, True, geodata.WGS84)
    x0, y0, x1, y1 = utm_hull([lon0, lat0, lon1, lat1], tm)
    nx, ny = max(2, round((x1 - x0) / res_m)), max(2, round((y1 - y0) / res_m))
    if nx * ny > MAX_CELLS:
        raise store.StoreError("the grid would be %d × %d cells at %g m, more than %d million: "
                               "a smaller rectangle, or 30 m"
                               % (nx, ny, res_m, MAX_CELLS // 1_000_000))
    return {"zone": zone, "epsg": 32600 + zone, "cells": [nx, ny],
            "size_km": [round((x1 - x0) / 1000, 2), round((y1 - y0) / 1000, 2)]}


def zone_of(crs):
    """The transverse Mercator of a UTM `EPSG:<code>`."""
    return geodata.TransverseMercator(*geodata.utm_zone_of(int(str(crs).split(":")[1])))


# ---- finding: template -----------------------------------------------------------

def tile_name(pattern, lat, lon):
    """A template's tile named by its south-west corner: {ns} and {ew} the
    hemispheres, {lat} and {lon} the whole degrees without sign, `:0n`
    padding them to n digits."""
    def field(m):
        what, pad = m.group(1), m.group(2)
        if what == "ns":
            return "N" if lat >= 0 else "S"
        if what == "ew":
            return "E" if lon >= 0 else "W"
        value = abs(lat if what == "lat" else lon)
        return "%0*d" % (int(pad), value) if pad else "%d" % value
    return sourcefile.TILE_FIELD_RE.sub(field, pattern)


def template_files(source, hull):
    """The tiles meeting the degrees `hull` [lon0, lat0, lon1, lat1]."""
    size = source.find["size_deg"]
    lon0, lat0, lon1, lat1 = hull
    out = []
    for lat in range(math.floor(lat0 / size) * size, math.floor((lat1 - 1e-9) / size) * size + 1,
                     size):
        for lon in range(math.floor(lon0 / size) * size,
                         math.floor((lon1 - 1e-9) / size) * size + 1, size):
            tile = tile_name(source.find["tile"], lat, lon)
            urls = [u.replace("{tile}", tile) for u in source.addresses("url")]
            out.append(File(source.id, urls, urls[0].rsplit("/", 1)[-1], missing=_missing(source)))
    return out


def template_pattern(source):
    """A template's cached file names as a pattern, its corner's groups ns,
    lat, ew, lon."""
    name = source.address("url").rsplit("/", 1)[-1].replace("{tile}", source.find["tile"])
    out, at = "", 0
    for m in sourcefile.TILE_FIELD_RE.finditer(name):
        out += re.escape(name[at:m.start()])
        what, pad = m.group(1), m.group(2)
        out += "(?P<%s>[%s])" % (what, "NS" if what == "ns" else "EW") if what in ("ns", "ew") \
            else "(?P<%s>\\d%s)" % (what, "{%s}" % pad if pad else "+")
        at = m.end()
    return re.compile("^" + out + re.escape(name[at:]) + "$")


# ---- finding: atom -----------------------------------------------------------------

HREF_RE = re.compile(r'href="([^"]+)"')


def file_name(url):
    """The name a tile is kept under: the last part of its address, or for
    a download service's address (M-V's `dgm_download?…&file=<name>`) the
    last value in its query that is a file name."""
    parts = urllib.parse.urlsplit(url)
    named = [v for _k, v in urllib.parse.parse_qsl(parts.query) if "." in v and "/" not in v]
    return named[-1] if named else parts.path.rsplit("/", 1)[-1]


def atom_tiles(source, feed_text):
    """A feed's tiles: [(url, x, y)], the corner in the feed's units. A
    link is read as XML writes it (M-V's carry `&amp;`), and one relative
    to the feed's address (a directory listing's, Brandenburg's) is made
    whole."""
    name = re.compile(source.find["name"])
    base = source.address("feed")
    out = []
    for href in HREF_RE.findall(feed_text):
        url = urllib.parse.urljoin(base, html.unescape(href))
        m = name.search(url)
        if m:
            out.append((url, int(m.group("x")), int(m.group("y"))))
    return out


def atom_box(source, bbox):
    """The rectangle's grid in a feed's zone and units."""
    unit = float(source.find["unit_m"])
    x0, y0, x1, y1 = utm_hull(bbox, zone_of(source.find["crs"]))
    return x0 / unit, y0 / unit, x1 / unit, y1 / unit


def atom_files(source, feed_text, bbox):
    """The tiles of a feed that meet the rectangle's grid."""
    x0, y0, x1, y1 = atom_box(source, bbox)
    size = float(source.find["size_m"]) / float(source.find["unit_m"])
    return [File(source.id, url, file_name(url), source.members(), missing=_missing(source))
            for url, e, n in atom_tiles(source, feed_text)
            if e < x1 and e + size > x0 and n < y1 and n + size > y0]


# ---- finding: regions and single files -------------------------------------------

def region_file(source, feature):
    """A region's file: its `file` of the index, cached as `<region>` and the
    URL's suffix."""
    props = feature["properties"]
    url = props["urls"][source.find["file"]]
    base = url.rsplit("/", 1)[-1]
    return File(source.id, url, props["id"] + ("." + base.split(".", 1)[1] if "." in base else ""),
                source.members(), missing=_missing(source))


# ---- finding: an index of footprints -----------------------------------------------

def read_index(source, raw):
    """An index's features: [(properties, outer rings in its system)], from
    GeoJSON (AHN's kaartbladindex) or FlatGeobuf (3DBAG's tile_index)."""
    if source.find["index_format"] == "flatgeobuf":
        try:
            return fgb.read(raw)
        except (fgb.FgbError, struct.error) as err:
            raise SourceError("%s's index is not FlatGeobuf: %s" % (source.title, err)) from err
    try:
        doc = json.loads(raw.decode("utf-8-sig"))
    except ValueError as err:
        raise SourceError("%s's index is not GeoJSON: %s" % (source.title, err)) from err
    out = []
    for feature in doc.get("features") or ():
        rings = [[tuple(p[:2]) for p in polygon[0]] for polygon in _rings(feature.get("geometry"))]
        out.append((feature.get("properties") or {}, rings))
    return out


def _ring_box(rings):
    xs = [x for ring in rings for x, _ in ring]
    ys = [y for ring in rings for _, y in ring]
    return (min(xs), min(ys), max(xs), max(ys)) if xs else None


def window_want(source, res_m):
    """The pixel a window reads a source at for a pack's resolution: a
    quarter of a cell, so each cell takes a few samples a side."""
    return float(res_m) / 4.0 if source.read == "window" else None


def index_files(source, features, bbox, res_m=30.0):
    """The files of an index whose footprint meets the rectangle's grid in
    the index's system; for a source read as a window, each with the box it
    needs of its file."""
    box = crs.box_in(source.find["crs"], bbox, margin=4 * float(res_m))
    want = window_want(source, res_m)
    out = []
    for props, rings in features:
        b = _ring_box(rings)
        if b is None or b[0] > box[2] or b[2] < box[0] or b[1] > box[3] or b[3] < box[1]:
            continue
        url = props.get(source.find["url_property"])
        if not url:
            continue
        sha = props.get(source.find.get("sha256_property")) if source.find.get("sha256_property") \
            else None
        out.append(File(source.id, url, url.rsplit("/", 1)[-1], source.members(),
                        missing=_missing(source), sha256=sha,
                        window={"box": box, "want": want} if want else None))
    return out


def single_file(source):
    urls = source.addresses("url")
    base = urls[0].rsplit("/", 1)[-1]
    name = base if PLAIN_NAME_RE.match(base) else \
        source.id + ("." + base.rsplit(".", 1)[1] if "." in base else "")
    return File(source.id, urls, name, source.members(), missing=_missing(source))


def _missing(source):
    """What a file no host has means: no data there (open sea for GLO-30),
    unless the source says it is an error."""
    return source.find.get("missing", "no-data")


# ---- outlines --------------------------------------------------------------

def _rings(geometry):
    """A GeoJSON (Multi)Polygon as a list of polygons, each [outer, holes…]."""
    if not geometry:
        return []
    if geometry.get("type") == "Polygon":
        return [geometry["coordinates"]]
    if geometry.get("type") == "MultiPolygon":
        return geometry["coordinates"]
    return []


def _in_ring(lon, lat, ring):
    inside = False
    j = len(ring) - 1
    for i in range(len(ring)):
        xi, yi = ring[i][0], ring[i][1]
        xj, yj = ring[j][0], ring[j][1]
        if (yi > lat) != (yj > lat) and lon < (xj - xi) * (lat - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside


def _in_polygon(lon, lat, polygon):
    return _in_ring(lon, lat, polygon[0]) and not any(_in_ring(lon, lat, h) for h in polygon[1:])


def _crosses(a, b, c, d):
    """Whether segments ab and cd cross."""
    def side(p, q, r):
        return (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    return (side(a, b, c) * side(a, b, d) < 0) and (side(c, d, a) * side(c, d, b) < 0)


def _box_edges(bbox):
    lon0, lat0, lon1, lat1 = bbox
    corners = [(lon0, lat0), (lon1, lat0), (lon1, lat1), (lon0, lat1)]
    return corners, [(corners[i], corners[(i + 1) % 4]) for i in range(4)]


def _edge_crosses_box(polygon, edges):
    for ring in polygon:
        for i in range(len(ring) - 1):
            p, q = ring[i], ring[i + 1]
            if any(_crosses(p, q, a, b) for a, b in edges):
                return True
    return False


def holds(geometry, bbox):
    """Whether an outline holds the whole rectangle."""
    corners, edges = _box_edges(bbox)
    for polygon in _rings(geometry):
        if all(_in_polygon(lon, lat, polygon) for lon, lat in corners) \
                and not _edge_crosses_box(polygon, edges):
            return True
    return False


def meets(geometry, bbox):
    """Whether an outline and the rectangle share any ground."""
    corners, edges = _box_edges(bbox)
    lon0, lat0, lon1, lat1 = bbox
    for polygon in _rings(geometry):
        if any(_in_polygon(lon, lat, polygon) for lon, lat in corners):
            return True
        if any(lon0 <= p[0] <= lon1 and lat0 <= p[1] <= lat1 for p in polygon[0]):
            return True
        if _edge_crosses_box(polygon, edges):
            return True
    return False


def holds_point(geometry, lon, lat):
    return any(_in_polygon(lon, lat, polygon) for polygon in _rings(geometry))


def area(geometry):
    """An outline's area in square degrees, outer rings only: enough to say
    which of two outlines is the smaller."""
    total = 0.0
    for polygon in _rings(geometry):
        ring = polygon[0]
        total += abs(sum(ring[i][0] * ring[i + 1][1] - ring[i + 1][0] * ring[i][1]
                         for i in range(len(ring) - 1))) / 2
    return total


def smallest_extract(index, bbox, file="pbf"):
    """The region of a Geofabrik index whose outline holds the rectangle and
    is the smallest: its index feature, or None."""
    best = None
    for feature in index.get("features") or ():
        props = feature.get("properties") or {}
        if not (props.get("urls") or {}).get(file):
            continue
        geometry = feature.get("geometry")
        if holds(geometry, bbox):
            size = area(geometry)
            if best is None or size < best[0]:
                best = (size, feature)
    return best[1] if best else None


def outline(index, extract_id):
    for feature in index.get("features") or ():
        if (feature.get("properties") or {}).get("id") == extract_id:
            return feature.get("geometry")
    return None


# ---- the cache -------------------------------------------------------------

class Cache:
    """The download cache, and the one session every fetch goes through."""

    def __init__(self, session, root=None):
        self.session = session
        self.root = root or CACHE_DIR
        self.sizes_path = os.path.join(self.root, "sizes.json")
        self.sizes = {}
        with contextlib.suppress(OSError, ValueError):
            with open(self.sizes_path, encoding="utf-8") as handle:
                self.sizes = json.load(handle)
        self.meta_locks = {}
        self.features = {}              # source id -> ((index path, mtime), features)

    def have(self, f):
        """True when fetched, False when its host has none, None when not yet.
        A window is fetched when its copy holds every range it needs."""
        path = f.path(self.root)
        if f.window is not None:
            if f.needs is None or not os.path.isfile(path):
                return None
            held, _length = cogwindow.held(path)
            return True if all(cogwindow.covers(held, s, n) for s, n in f.needs) else None
        if os.path.isfile(path):
            return True
        if os.path.isfile(path + ".absent"):
            return False
        return None

    def fetched_bytes(self, f):
        """How much of a file is here: whole, or its `.part` so far."""
        path = f.path(self.root)
        for each in (path, path + ".part"):
            with contextlib.suppress(OSError):
                return os.path.getsize(each)
        return 0

    async def request(self, method, url, headers=None):
        """A response from the host, retried politely on 429 and 5xx."""
        headers = dict(headers or {}, **{"User-Agent": USER_AGENT})
        pause = BACKOFF_S
        for attempt in range(TRIES):
            try:
                resp = await self.session.request(method, url, headers=headers,
                                                  timeout=aiohttp.ClientTimeout(
                                                      total=None, sock_connect=30, sock_read=120))
            except (aiohttp.ClientError, asyncio.TimeoutError) as err:
                if attempt == TRIES - 1:
                    raise SourceError("%s: %s" % (url, err or type(err).__name__)) from err
                await asyncio.sleep(pause)
                pause *= 2
                continue
            if resp.status == 429 or resp.status >= 500:
                wait = resp.headers.get("Retry-After")
                resp.release()
                if attempt == TRIES - 1:
                    raise SourceError("%s answers %d" % (url, resp.status))
                await asyncio.sleep(float(wait) if wait and wait.isdigit() else pause)
                pause *= 2
                continue
            return resp
        raise SourceError("%s: no answer" % url)

    async def size(self, f):
        """A file's size in bytes as its host says, asked once; None when
        the host does not say."""
        if f.url in self.sizes:
            return self.sizes[f.url]
        got, error = None, None
        for url in f.urls:
            try:
                resp = await self.request("HEAD", url)
            except SourceError as err:
                error = err
                continue
            try:
                got = None if resp.status == 404 else resp.content_length
            finally:
                resp.release()
            break
        else:
            raise error
        self.sizes[f.url] = got
        with contextlib.suppress(OSError):
            store.write_text(self.sizes_path, json.dumps(self.sizes, indent=0, sort_keys=True))
        return got

    async def sizes_of(self, files):
        """{file: bytes still to fetch, or None when unknown} for files not here.
        A window's is the ranges it needs that its copy does not hold."""
        todo = [f for f in files if self.have(f) is None]
        gate = asyncio.Semaphore(HEAD_PARALLEL)

        async def one(f):
            if f.window is not None:
                try:
                    await self.window_needs(f)
                except SourceError:
                    return f, None
                held, _length = cogwindow.held(f.path(self.root))
                return f, sum(n for s, n in f.needs if not cogwindow.covers(held, s, n))
            async with gate:
                try:
                    whole = await self.size(f)
                except SourceError:
                    whole = None
                return f, None if whole is None else max(0, whole - self.fetched_bytes(f))
        return dict(await asyncio.gather(*(one(f) for f in todo)))

    async def fetch(self, f, progress=None):
        """A file into the cache, resumed where a `.part` of it stopped, from
        its address or the next mirror when one fails: its path, or None
        when no host has it and its source says that is no data.
        `progress(bytes)` hears the bytes it holds as they arrive."""
        if f.window is not None:
            return await self.fetch_window(f, progress)
        have = self.have(f)
        path = f.path(self.root)
        if have is not None:
            return path if have else None
        os.makedirs(os.path.dirname(path), exist_ok=True)
        error, missing = None, 0
        for url in f.urls:
            try:
                got = await self._fetch_from(f, url, path, progress)
            except SourceError as err:
                error = err
                continue
            if got is not None:
                return got
            missing += 1
        if missing == len(f.urls):
            if f.missing != "no-data":
                raise SourceError("%s answers 404" % f.url)
            store.write_text(path + ".absent", "%s\n" % f.url)
            return None
        raise error

    async def _fetch_from(self, f, url, path, progress):
        part = path + ".part"
        start = self.fetched_bytes(f)
        resp = await self.request("GET", url, {"Range": "bytes=%d-" % start} if start else None)
        try:
            if resp.status == 404:
                return None
            if resp.status == 416 and start:
                os.replace(part, path)
                return path
            if resp.status not in (200, 206):
                raise SourceError("%s answers %d" % (url, resp.status))
            mode = "ab" if resp.status == 206 else "wb"
            got = start if resp.status == 206 else 0
            with open(part, mode) as out:
                async for chunk in resp.content.iter_chunked(CHUNK):
                    out.write(chunk)
                    got += len(chunk)
                    if progress:
                        progress(got)
        except (aiohttp.ClientError, asyncio.TimeoutError) as err:
            raise SourceError("%s: %s" % (url, err or type(err).__name__)) from err
        finally:
            resp.release()
        if f.sha256:
            digest = hashlib.sha256()
            with open(part, "rb") as handle:
                for chunk in iter(lambda: handle.read(CHUNK * 16), b""):
                    digest.update(chunk)
            if digest.hexdigest() != f.sha256.lower():
                os.remove(part)
                raise SourceError("%s is not the file its index lists: its sha256 differs" % url)
        os.replace(part, path)
        return path

    # ---- windows of cloud-optimised GeoTIFFs --------------------------------

    async def _range(self, url, start, length):
        """Bytes [start, start + length) of a file, and its whole length."""
        resp = await self.request("GET", url, {"Range": "bytes=%d-%d" % (start, start + length - 1)})
        try:
            if resp.status == 404:
                raise SourceError("%s answers 404" % url)
            if resp.status not in (200, 206):
                raise SourceError("%s answers %d" % (url, resp.status))
            total = None
            m = re.match(r"bytes \d+-\d+/(\d+)", resp.headers.get("Content-Range", ""))
            if m:
                total = int(m.group(1))
            if resp.status == 200:
                raise SourceError("%s does not serve ranges, which a window needs" % url)
            body = await resp.read()
        except (aiohttp.ClientError, asyncio.TimeoutError) as err:
            raise SourceError("%s: %s" % (url, err or type(err).__name__)) from err
        finally:
            resp.release()
        return body, total

    async def window_needs(self, f):
        """The byte ranges a window takes (every image's directory and the
        chunks its box meets at its level), from its copy's header when the
        copy holds it, else fetched; and the file's length."""
        if f.needs is not None:
            return f.needs
        path = f.path(self.root)
        held, length = cogwindow.held(path)
        local = bool(held) and os.path.isfile(path)
        if local:
            data = cogwindow.Bytes(cogwindow.local_reader(path))
        else:
            box = {"total": None}

            async def remote(start, n):
                body, total = await self._range(f.url, start, max(n, cogwindow.HEAD_BYTES))
                box["total"] = total or box["total"]
                return body
            data = cogwindow.Bytes(remote)
        try:
            st = await cogwindow.structure(data)
        except (cogwindow.WindowError, struct.error) as err:
            raise SourceError("%s: %s" % (f.url, err)) from err
        level = cogwindow.level_for(st, f.window["want"])
        f.needs = st["used"] + cogwindow.chunks_for(st, level, f.window["box"])
        f.length = length if local else box["total"]
        if not local:
            f.head_parts = list(data.parts)
        if f.length is None:
            raise SourceError("%s did not say its length" % f.url)
        return f.needs

    async def fetch_window(self, f, progress=None):
        """A window's ranges into its sparse copy, those it already holds
        left alone: its path."""
        await self.window_needs(f)
        path = f.path(self.root)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        held, _length = cogwindow.held(path)
        missing = cogwindow.merge([(s, n) for s, n in f.needs
                                   if not cogwindow.covers(held, s, n)], gap=cogwindow.MERGE_GAP)
        parts = list(f.head_parts)
        got = 0
        for start, n in missing:
            body, _total = await self._range(f.url, start, n)
            parts.append((start, body))
            got += len(body)
            if progress:
                progress(got)
        if parts:
            await asyncio.to_thread(cogwindow.write, path, f.length, parts,
                                    held + [(s, len(b)) for s, b in parts])
        f.head_parts = []
        return path

    def extracted(self, f):
        """The members a build reads of a fetched zip, extracted into `x/`
        beside it (once): their paths. `members` is names, or one `*.ext`."""
        path = f.path(self.root)
        into = os.path.join(os.path.dirname(path), "x")
        done = path + ".x"
        if os.path.isfile(done):
            with open(done, encoding="utf-8") as handle:
                return [os.path.join(into, n) for n in handle.read().split("\n") if n]
        if f.name.endswith(".gz"):
            # One file, gzipped (3DBAG's CityJSON tiles): it, decompressed.
            base = os.path.basename(f.name[:-3])
            os.makedirs(into, exist_ok=True)
            try:
                with gzip.open(path, "rb") as src, open(os.path.join(into, base) + ".part", "wb") as dst:
                    shutil.copyfileobj(src, dst, CHUNK)
            except (OSError, EOFError) as err:
                with contextlib.suppress(OSError):
                    os.remove(path)
                raise SourceError("%s is not gzip, and is fetched again next time: %s"
                                  % (f.url, err)) from err
            os.replace(os.path.join(into, base) + ".part", os.path.join(into, base))
            store.write_text(done, base + "\n")
            return [os.path.join(into, base)]
        names = []
        try:
            with zipfile.ZipFile(path) as zf:
                for info in zf.infolist():
                    base = os.path.basename(info.filename)
                    if info.is_dir() or not base or not self.wanted(f, base):
                        continue
                    os.makedirs(into, exist_ok=True)
                    with zf.open(info) as src, open(os.path.join(into, base) + ".part", "wb") as dst:
                        shutil.copyfileobj(src, dst, CHUNK)
                    os.replace(os.path.join(into, base) + ".part", os.path.join(into, base))
                    names.append(base)
        except zipfile.BadZipFile as err:
            with contextlib.suppress(OSError):
                os.remove(path)
            raise SourceError("%s is not a zip, and is fetched again next time: %s"
                              % (f.url, err)) from err
        store.write_text(done, "".join(n + "\n" for n in names))
        return [os.path.join(into, n) for n in names]

    def grid_csv(self, f, value):
        """A GeoPackage grid (CBS's 100 m squares) as the CSV the compiler's
        grid reader takes, `x,y,value`: each cell's centre, from its
        geometry's envelope, and its `value` column, written once beside the
        GeoPackage. Its path."""
        gpkg = next((p for p in self.extracted(f) if p.endswith(".gpkg")), None)
        if gpkg is None:
            raise SourceError("%s holds no GeoPackage" % f.url)
        out = gpkg[:-len(".gpkg")] + "." + re.sub(r"[^A-Za-z0-9_]", "_", value) + ".csv"
        if os.path.isfile(out):
            return out
        import sqlite3
        db = sqlite3.connect("file:%s?mode=ro" % gpkg, uri=True)
        try:
            table, column = db.execute(
                "select table_name, column_name from gpkg_geometry_columns").fetchone()
            rows = db.execute('select "%s", "%s" from "%s"' % (column, value, table))
            with open(out + ".part", "w", encoding="utf-8") as handle:
                handle.write("x,y,value\n")
                for geom, v in rows:
                    box = _gpkg_envelope(geom)
                    if box is None or v is None:
                        continue
                    handle.write("%.3f,%.3f,%s\n" % ((box[0] + box[1]) / 2, (box[2] + box[3]) / 2, v))
        except sqlite3.Error as err:
            raise SourceError("%s: %s" % (gpkg, err)) from err
        finally:
            db.close()
        os.replace(out + ".part", out)
        return out

    @staticmethod
    def wanted(f, base):
        members = f.members if isinstance(f.members, (list, tuple)) else (f.members,)
        for m in members:
            if m.startswith("*") and base.lower().endswith(m[1:].lower()):
                return True
            if base == m:
                return True
        return False

    async def meta(self, name, url, max_age_s=META_MAX_AGE_S):
        """A file saying what a source has (a regions index, a feed), fetched
        when missing or older than `max_age_s`: its text."""
        path = await self.meta_file(name, url, max_age_s)
        with open(path, encoding="utf-8-sig") as handle:
            return handle.read()

    async def meta_file(self, name, url, max_age_s=META_MAX_AGE_S):
        """As `meta`, the file's path: kept `max_age_s`, and the copy there
        is served when a fetch fails. `url` may be a list, its mirrors."""
        path = os.path.join(self.root, "meta", name)
        lock = self.meta_locks.setdefault(name, asyncio.Lock())
        async with lock:
            fresh = os.path.isfile(path) and time.time() - os.path.getmtime(path) < max_age_s
            if not fresh:
                error = None
                for each in (url if isinstance(url, (list, tuple)) else [url]):
                    try:
                        resp = await self.request("GET", each)
                        try:
                            if resp.status != 200:
                                raise SourceError("%s answers %d" % (each, resp.status))
                            body = await resp.read()
                        finally:
                            resp.release()
                    except SourceError as err:
                        error = err
                        continue
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    with open(path + ".part", "wb") as out:
                        out.write(body)
                    os.replace(path + ".part", path)
                    error = None
                    break
                if error is not None and not os.path.isfile(path):
                    raise error
            return path

    def meta_age(self, name):
        """When a meta file was fetched, in Unix seconds, or None."""
        with contextlib.suppress(OSError):
            return os.path.getmtime(os.path.join(self.root, "meta", name))
        return None

    async def regions_index(self, source):
        """A regions source's index, parsed."""
        name = "%s-%s" % (source.id, source.address("index").rsplit("/", 1)[-1])
        text = await self.meta(name, source.addresses("index"), _max_age(source))
        try:
            return json.loads(text)
        except ValueError as err:
            raise SourceError("%s's index is not JSON: %s" % (source.title, err)) from err

    async def feed(self, source):
        """An atom source's feed, its text."""
        return await self.meta(source.id + ".atom", source.addresses("feed"), _max_age(source))

    async def index_features(self, source):
        """An index source's features, [(properties, rings)], read again
        only when its index is fetched again."""
        name = "%s-%s" % (source.id, source.address("index").rsplit("/", 1)[-1])
        path = await self.meta_file(name, source.addresses("index"), _max_age(source))
        key = (path, os.path.getmtime(path))
        cached = self.features.get(source.id)
        if cached is None or cached[0] != key:
            with open(path, "rb") as handle:
                raw = handle.read()
            cached = (key, await asyncio.to_thread(read_index, source, raw))
            self.features[source.id] = cached
        return cached[1]


def _gpkg_envelope(blob):
    """A GeoPackage geometry's envelope (minx, maxx, miny, maxy) from its
    header, or from its WKB polygon's points when the header carries none."""
    if not blob or blob[:2] != b"GP":
        return None
    flags = blob[3]
    order = "<" if flags & 1 else ">"
    kind = (flags >> 1) & 7
    if kind:
        return struct.unpack_from(order + "4d", blob, 8)
    # No envelope: the WKB after the 8-byte header, a (multi)polygon's points.
    wkb = blob[8:]
    xs, ys = [], []

    def geometry(at):
        o = "<" if wkb[at] == 1 else ">"
        kind = struct.unpack_from(o + "I", wkb, at + 1)[0] % 1000
        at += 5
        if kind == 6:                   # multipolygon: its polygons, each with its own header
            parts = struct.unpack_from(o + "I", wkb, at)[0]
            at += 4
            for _ in range(parts):
                at = geometry(at)
            return at
        if kind != 3:
            raise ValueError("a grid cell is a polygon")
        rings = struct.unpack_from(o + "I", wkb, at)[0]
        at += 4
        for _ in range(rings):
            n = struct.unpack_from(o + "I", wkb, at)[0]
            at += 4
            for _ in range(n):
                x, y = struct.unpack_from(o + "2d", wkb, at)
                xs.append(x)
                ys.append(y)
                at += 16
        return at
    try:
        geometry(0)
    except (ValueError, struct.error):
        return None
    return (min(xs), max(xs), min(ys), max(ys)) if xs else None


def _max_age(source):
    days = source.find.get("refresh_days")
    return float(days) * 86400 if days else META_MAX_AGE_S


# ---- where each source has data, and what of it is cached ------------------------

def _box(lon0, lat0, lon1, lat1):
    return [[[lon0, lat0], [lon1, lat0], [lon1, lat1], [lon0, lat1], [lon0, lat0]]]


def _cached_names(root, source):
    """The files of a source in the cache, a window's copy among them: no
    `.part`, no `.absent`, no `.x` (a zip's members extracted), no `.ranges`
    (what a window's copy holds), not the extracted `x/`."""
    base = os.path.join(root, source)
    if not os.path.isdir(base):
        return []
    return sorted(name for name in os.listdir(base)
                  if os.path.isfile(os.path.join(base, name))
                  and not name.endswith((".part", ".absent", ".x", ".ranges")))


async def source_map(cache, source_id, sources=None):
    """Where one source has data and what of it is in the cache, for the
    page's map, as GeoJSON MultiPolygons in degrees:

        {source, worldwide, covers: geometry | null, covers_from, cached:
         geometry | null, cached_files, error}

    A worldwide source's `covers` is null; an atom source's is the tiles its
    feed lists, where its data is (its outline when the feed cannot be had);
    any other's is its outline. What is cached is read off the file names,
    as its method names them: a
    template's tile its south-west corner, a feed's tile its corner in the
    feed's units, a region's file its region, whose outline the index gives,
    and a single file the whole coverage. A source that needs its index to
    say this and cannot have it says so in `error`."""
    source = registry(sources)[source_id]
    names = _cached_names(cache.root, source_id)
    out = {"source": source_id, "worldwide": source.worldwide,
           "covers": None if source.worldwide else source.outline,
           "covers_from": "the whole world" if source.worldwide else "its outline",
           "cached": None, "cached_files": len(names), "error": None}
    method = source.find["method"]
    cached = []
    try:
        if method == "template":
            pattern, size = template_pattern(source), source.find["size_deg"]
            for name in names:
                m = pattern.match(name)
                if m:
                    lat = int(m.group("lat")) * (-1 if m.group("ns") == "S" else 1)
                    lon = int(m.group("lon")) * (-1 if m.group("ew") == "W" else 1)
                    cached.append(_box(lon, lat, lon + size, lat + size))
        elif method == "atom":
            # Its data is where its feed has a tile: inside its outline, and
            # no more of it than that.
            try:
                listed = atom_tiles(source, await cache.feed(source))
            except store.StoreError as err:
                out["error"] = "its feed could not be read, so its outline is drawn: %s" % err
            else:
                out["covers"] = {"type": "MultiPolygon", "coordinates": sourcefile.tile_quads(
                    source, [(x, y) for _url, x, y in listed])}
                out["covers_from"] = "the %d tiles its feed lists" % len(listed)
            pattern = re.compile(source.find["name"])
            tiles = [(int(m.group("x")), int(m.group("y")))
                     for m in (pattern.search(name) for name in names) if m]
            cached = sourcefile.tile_quads(source, tiles)
        elif method == "index":
            # Its data is where its index has a file, as for a feed.
            try:
                features = await cache.index_features(source)
            except store.StoreError as err:
                out["error"] = "its index could not be read, so its outline is drawn: %s" % err
            else:
                system = source.find["crs"]
                out["covers"] = {"type": "MultiPolygon", "coordinates": [
                    [crs.ring_to_degrees(system, ring)] for _p, rings in features for ring in rings]}
                out["covers_from"] = "the %d files its index lists" % len(features)
                here = set(names)
                prop = source.find["url_property"]
                for props, rings in features:
                    url = str(props.get(prop) or "")
                    if url.rsplit("/", 1)[-1] in here:
                        cached += [[crs.ring_to_degrees(system, ring)] for ring in rings]
        elif method == "regions":
            out["covers_from"] = "the whole world, in regions whose outlines its index gives"
            if names:
                index = await cache.regions_index(source)
                for name in names:
                    cached += _rings(outline(index, name.split(".", 1)[0]))
        elif names:
            cached = _rings(source.outline) if source.outline else [_box(*WORLD)]
    except store.StoreError as err:
        out["error"] = str(err)
    if cached:
        out["cached"] = {"type": "MultiPolygon", "coordinates": cached}
    return out


async def sources_at(cache, lon, lat, sources=None):
    """Which sources have data at a point, and whether what covers it is in
    the cache: {source: {has, cached, error}}. A worldwide source has data
    everywhere; any other where `source_map` says it covers."""
    out = {}
    reg = registry(sources)
    for source_id, source in reg.items():
        area_ = await source_map(cache, source_id, reg)
        has = source.worldwide or holds_point(area_["covers"], lon, lat)
        out[source_id] = {"has": has, "cached": has and holds_point(area_["cached"], lon, lat),
                          "error": area_["error"]}
    return out


def areas(sources=None):
    """The outlines of the sources that do not cover the world, for the
    build view's map: [{source, title, outline}]."""
    return [{"source": s.id, "title": s.title, "outline": s.outline}
            for s in registry(sources).values() if not s.worldwide]


# ---- what a build needs ------------------------------------------------------

def used_for(chosen, whole):
    """What each chosen source is used for: {id: one line}. `chosen` is the
    sources the rectangle takes, `whole` which of them cover all of it. In
    each layer a source's part is the rectangle less what sources of a
    higher priority there cover."""
    out = {}
    for source in chosen:
        parts = []
        for layer, priority in source.layers.items():
            over = [s for s in chosen if s.layers.get(layer, -1) > priority]
            text = LAYER_TEXT[layer]
            if any(whole[s.id] for s in over):
                text += " where %s has none" % next(s.title for s in over if whole[s.id])
            elif over:
                text += " outside %s" % " and ".join(s.title for s in over)
            elif not whole[source.id]:
                text += " inside its outline"
            parts.append(text)
        if not source.redistributable:
            parts[-1] += "; kept here"
        out[source.id] = "; ".join(parts)
    return out


async def plan(cache, spec, sizes=True, sources=None):
    """What a build of `spec` ({bbox, res_m}) takes: the grid, the sources
    whose coverage meets the rectangle with what each is used for, and each
    one's files with what is still to fetch.

    {grid, extract, sources: [{source, title, licence, used_for, files,
    cached, to_fetch}], files: {source: [File]}}; to_fetch is None where a
    host does not say a size. `extract` is the region a regions source took.
    """
    reg = registry(sources)
    bbox = [float(v) for v in spec["bbox"]]
    grid = check(bbox, float(spec.get("res_m", 30)))
    tm = geodata.TransverseMercator(grid["zone"], True, geodata.WGS84)
    hull = degree_hull(bbox, tm)
    files, chosen, whole, extract = {}, [], {}, None
    for source in reg.values():
        if not source.worldwide and not meets(source.outline, bbox):
            continue
        method = source.find["method"]
        if method == "template":
            found = template_files(source, hull)
        elif method == "atom":
            found = atom_files(source, await cache.feed(source), bbox)
        elif method == "index":
            found = index_files(source, await cache.index_features(source), hull,
                                spec.get("res_m", 30))
        elif method == "regions":
            feature = smallest_extract(await cache.regions_index(source), bbox,
                                       source.find["file"])
            if feature is None:
                raise store.StoreError("no region of %s holds the whole rectangle" % source.title)
            found = [region_file(source, feature)]
            extract = {"id": feature["properties"]["id"],
                       "name": feature["properties"].get("name")}
        else:
            found = [single_file(source)]
        if not found:
            continue
        files[source.id] = found
        chosen.append(source)
        whole[source.id] = source.worldwide or holds(source.outline, bbox)
    uses = used_for(chosen, whole)
    todo = await cache.sizes_of([f for fs in files.values() for f in fs]) if sizes else {}
    rows = []
    for source in chosen:
        fs = files[source.id]
        pending = [todo.get(f) for f in fs if cache.have(f) is None]
        rows.append({"source": source.id, "title": source.title, "licence": source.licence,
                     "used_for": uses[source.id], "files": len(fs),
                     "cached": sum(1 for f in fs if cache.have(f) is not None),
                     "to_fetch": None if None in pending else sum(pending)})
    return {"grid": grid, "extract": extract, "sources": rows, "files": files}
