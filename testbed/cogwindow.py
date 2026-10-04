"""A window of a cloud-optimised GeoTIFF: the bytes a rectangle needs, and
only those, fetched into a sparse copy of the file the compiler reads as if
it were whole.

```
front ── GET <file>, Range: bytes=0-262143 ──► host     the header: every image's directory
front ── GET <file>, Range: bytes=<tile>… ───► host     the chunks the box needs, at one level
front: each range written at its own offset into <file> (the rest a hole), and
       <file>.ranges listing what is there
compiler ── CogReader::open_level(<file>, want) ──► the same level, the same chunks
```

A COG keeps every image's directory at its front and the chunks of each
image in order, so the directories and their tag values (the chunk offsets
and byte counts above all) come in one or two requests, and a rectangle at
one level is a run of chunk ranges. The level is the coarsest whose pixel
is no larger than `want` (the full image when none is), the rule the
compiler's `open_level` keeps, so both read the same image. Every image's
directory is fetched, since the compiler walks the chain to choose.

`.ranges` is the byte ranges the copy holds, merged; a chunk is there when
its range is. A copy only ever grows, so a second rectangle beside the
first fetches only its new chunks. Its size on disk is what it holds, its
length the whole file's.
"""

import json
import os
import struct

TYPE_SIZES = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2, 9: 4, 10: 8, 11: 4, 12: 8,
              13: 4, 16: 8, 17: 8, 18: 8}
TAG_WIDTH, TAG_HEIGHT = 256, 257
TAG_TILE_W, TAG_TILE_H, TAG_TILE_OFFSETS, TAG_TILE_COUNTS = 322, 323, 324, 325
TAG_PIXEL_SCALE, TAG_TIEPOINT, TAG_GEOKEYS = 33550, 33922, 34735
GEOKEY_RASTER_TYPE, RASTER_TYPE_POINT = 1025, 2
HEAD_BYTES = 1 << 18
MERGE_GAP = 1 << 14


class WindowError(ValueError):
    pass


def merge(ranges, gap=0):
    """Ranges [(start, length)] sorted and joined where they touch (or lie
    within `gap` of each other)."""
    out = []
    for s, n in sorted(r for r in ranges if r[1] > 0):
        if out and s <= out[-1][0] + out[-1][1] + gap:
            last = out[-1]
            out[-1] = (last[0], max(last[0] + last[1], s + n) - last[0])
        else:
            out.append((s, n))
    return out


def covers(held, start, length):
    """Whether merged ranges `held` hold all of [start, start + length)."""
    for s, n in held:
        if s <= start and start + length <= s + n:
            return True
    return False


class Bytes:
    """Byte ranges of a file as far as they are known, and how to learn more:
    `fetch(start, length)` an awaitable giving those bytes."""

    def __init__(self, fetch):
        self.fetch = fetch
        self.parts = []                 # [(start, bytes)]

    async def get(self, start, length):
        for s, b in self.parts:
            if s <= start and start + length <= s + len(b):
                return b[start - s:start - s + length]
        want = max(length, 1 << 16)
        got = await self.fetch(start, want)
        if len(got) < length:
            raise WindowError("the file ends at %d, before %d" % (start + len(got), start + length))
        self.parts.append((start, got))
        return got[:length]

    def ranges(self):
        return [(s, len(b)) for s, b in self.parts]


async def structure(data):
    """Every image of a GeoTIFF: [{width, height, tile_w, tile_h, offsets,
    counts}], the georeferencing of the first, and the byte ranges its
    directories and their values take."""
    head = await data.get(0, 16)
    order = {b"II": "<", b"MM": ">"}.get(head[:2])
    if order is None:
        raise WindowError("not a TIFF")
    magic = struct.unpack(order + "H", head[2:4])[0]
    big = magic == 43
    if magic not in (42, 43):
        raise WindowError("not a TIFF (magic %d)" % magic)
    used = [(0, 16 if big else 8)]
    nxt = struct.unpack(order + ("Q" if big else "I"), head[8:16] if big else head[4:8])[0]
    images, geo = [], None
    seen = set()
    while nxt and nxt not in seen:
        seen.add(nxt)
        count_size, entry_size, ptr = (8, 20, 8) if big else (2, 12, 4)
        n = struct.unpack(order + ("Q" if big else "H"), await data.get(nxt, count_size))[0]
        raw = await data.get(nxt + count_size, n * entry_size + ptr)
        used.append((nxt, count_size + n * entry_size + ptr))
        tags = {}
        for i in range(n):
            e = raw[i * entry_size:(i + 1) * entry_size]
            tag, typ = struct.unpack(order + "HH", e[:4])
            cnt = struct.unpack(order + ("Q" if big else "I"), e[4:12] if big else e[4:8])[0]
            size = TYPE_SIZES.get(typ, 1) * cnt
            inline = e[12:20] if big else e[8:12]
            if size <= len(inline):
                value = inline[:size]
            else:
                off = struct.unpack(order + ("Q" if big else "I"), inline)[0]
                value = await data.get(off, size)
                used.append((off, size))
            tags[tag] = (typ, cnt, value)
        nxt = struct.unpack(order + ("Q" if big else "I"), raw[n * entry_size:n * entry_size + ptr])[0]

        def nums(tag, default=None):
            if tag not in tags:
                return default
            typ, cnt, value = tags[tag]
            fmt = {3: "H", 4: "I", 16: "Q", 11: "f", 12: "d", 8: "h", 9: "i", 17: "q"}.get(typ)
            if fmt is None:
                return default
            return list(struct.unpack(order + fmt * cnt, value))
        if TAG_TILE_OFFSETS not in tags:
            raise WindowError("not tiled: a window reads a tiled GeoTIFF")
        images.append({"width": nums(TAG_WIDTH)[0], "height": nums(TAG_HEIGHT)[0],
                       "tile_w": nums(TAG_TILE_W)[0], "tile_h": nums(TAG_TILE_H)[0],
                       "offsets": nums(TAG_TILE_OFFSETS), "counts": nums(TAG_TILE_COUNTS)})
        if geo is None:
            scale, tie = nums(TAG_PIXEL_SCALE), nums(TAG_TIEPOINT)
            if not scale or not tie or len(tie) < 6:
                raise WindowError("no georeferencing (pixel scale and tiepoint)")
            keys = nums(TAG_GEOKEYS, [])
            point = any(keys[k] == GEOKEY_RASTER_TYPE and keys[k + 3] == RASTER_TYPE_POINT
                        for k in range(4, len(keys) - 3, 4))
            # The outer corner of pixel (0, 0).
            cx = tie[3] - tie[0] * scale[0] - (0.5 * scale[0] if point else 0.0)
            cy = tie[4] + tie[1] * scale[1] + (0.5 * scale[1] if point else 0.0)
            geo = {"corner": (cx, cy), "pixel": (scale[0], scale[1])}
    if not images:
        raise WindowError("no image")
    return {"images": images, "geo": geo, "used": merge(used)}


def level_for(st, want):
    """The image whose pixel suits `want`: the coarsest no larger, else the
    full image (CogReader::open_level's rule)."""
    full = st["images"][0]
    best = 0
    for i, im in enumerate(st["images"][1:], 1):
        if st["geo"]["pixel"][0] * full["width"] / im["width"] > want + 1e-9:
            break
        best = i
    return best


def chunks_for(st, level, box):
    """The chunks of an image a box [x0, y0, x1, y1] (in the file's system)
    meets: [(offset, count)], empty chunks (count 0) left out."""
    full, im = st["images"][0], st["images"][level]
    px = st["geo"]["pixel"][0] * full["width"] / im["width"]
    py = st["geo"]["pixel"][1] * full["height"] / im["height"]
    cx, cy = st["geo"]["corner"]
    x0, y0, x1, y1 = box
    c0 = max(0, int((x0 - cx) // px))
    c1 = min(im["width"] - 1, int((x1 - cx) // px))
    r0 = max(0, int((cy - y1) // py))
    r1 = min(im["height"] - 1, int((cy - y0) // py))
    if c0 > c1 or r0 > r1:
        return []
    across = -(-im["width"] // im["tile_w"])
    out = []
    for tr in range(r0 // im["tile_h"], r1 // im["tile_h"] + 1):
        for tc in range(c0 // im["tile_w"], c1 // im["tile_w"] + 1):
            k = tr * across + tc
            if k < len(im["offsets"]) and im["counts"][k]:
                out.append((im["offsets"][k], im["counts"][k]))
    return out


def held(path):
    """The ranges a sparse copy holds, and the whole file's length."""
    try:
        with open(path + ".ranges", encoding="utf-8") as handle:
            got = json.load(handle)
        return [tuple(r) for r in got["ranges"]], int(got["length"])
    except (OSError, ValueError, KeyError):
        return [], None


def local_reader(path):
    async def fetch(start, length):
        with open(path, "rb") as handle:
            handle.seek(start)
            return handle.read(length)
    return fetch


def write(path, length, parts, ranges):
    """Byte parts [(start, bytes)] into the sparse copy at their offsets,
    and its `.ranges` grown by them."""
    mode = "r+b" if os.path.exists(path) else "w+b"
    with open(path, mode) as out:
        if out.seek(0, os.SEEK_END) < length:
            out.truncate(length)
        for start, b in parts:
            out.seek(start)
            out.write(b)
    tmp = path + ".ranges.part"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump({"length": length, "ranges": merge(ranges)}, handle)
    os.replace(tmp, path + ".ranges")
