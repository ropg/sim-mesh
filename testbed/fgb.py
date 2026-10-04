"""FlatGeobuf, read: the features of a file, each its properties and its
outer rings, as an index of tiles lists them (3DBAG's tile_index.fgb).

    magic (8)  header size (u32)  header (flatbuffer)  [packed R-tree]  features…
    feature: size (u32)  Feature (flatbuffer): geometry, properties

A flatbuffer table is read where it lies, through its vtable; only the
fields an index needs are: the header's columns and feature count, each
feature's geometry (its xy and ring ends, and the parts of a multi-geometry)
and its properties, which are (u16 column, value) pairs encoded by the
column's type.
"""

import struct

MAGIC = b"fgb\x03"
NODE_ITEM_BYTES = 40
STRING, JSON = 11, 12
SCALARS = {0: "<b", 1: "<B", 2: "<?", 3: "<h", 4: "<H", 5: "<i", 6: "<I", 7: "<q", 8: "<Q",
           9: "<f", 10: "<d"}


class FgbError(ValueError):
    pass


class Table:
    """A flatbuffer table at `pos` in `buf`."""

    def __init__(self, buf, pos):
        self.buf, self.pos = buf, pos
        vt = pos - struct.unpack_from("<i", buf, pos)[0]
        self.vt_len = struct.unpack_from("<H", buf, vt)[0]
        self.vt = vt

    def _field(self, i):
        o = 4 + 2 * i
        if o >= self.vt_len:
            return 0
        return struct.unpack_from("<H", self.buf, self.vt + o)[0]

    def scalar(self, i, fmt, default=0):
        off = self._field(i)
        return struct.unpack_from(fmt, self.buf, self.pos + off)[0] if off else default

    def _ref(self, i):
        off = self._field(i)
        if not off:
            return None
        at = self.pos + off
        return at + struct.unpack_from("<I", self.buf, at)[0]

    def string(self, i):
        at = self._ref(i)
        if at is None:
            return None
        n = struct.unpack_from("<I", self.buf, at)[0]
        return self.buf[at + 4:at + 4 + n].decode("utf-8", "replace")

    def vector(self, i, fmt):
        at = self._ref(i)
        if at is None:
            return []
        n = struct.unpack_from("<I", self.buf, at)[0]
        return list(struct.unpack_from("<%d%s" % (n, fmt), self.buf, at + 4))

    def bytes(self, i):
        at = self._ref(i)
        if at is None:
            return b""
        n = struct.unpack_from("<I", self.buf, at)[0]
        return self.buf[at + 4:at + 4 + n]

    def tables(self, i):
        at = self._ref(i)
        if at is None:
            return []
        n = struct.unpack_from("<I", self.buf, at)[0]
        out = []
        for k in range(n):
            p = at + 4 + 4 * k
            out.append(Table(self.buf, p + struct.unpack_from("<I", self.buf, p)[0]))
        return out


def _root(buf, pos):
    return Table(buf, pos + struct.unpack_from("<I", buf, pos)[0])


def _index_bytes(count, node_size):
    """The packed Hilbert R-tree's size: every level's nodes, 40 bytes each."""
    if node_size < 2 or count == 0:
        return 0
    n, nodes = count, count
    while n != 1:
        n = -(-n // node_size)
        nodes += n
    return nodes * NODE_ITEM_BYTES


def _rings(geom):
    """A geometry's outer rings as [(x, y)…]: a polygon's first ring, each
    part's for a multi-geometry."""
    parts = geom.tables(7)
    if parts:
        return [r for p in parts for r in _rings(p)]
    xy = geom.vector(1, "d")
    if not xy:
        return []
    ends = geom.vector(0, "I") or [len(xy) // 2]
    first = ends[0]
    return [list(zip(xy[0:2 * first:2], xy[1:2 * first:2]))]


def _properties(raw, columns):
    out, at = {}, 0
    while at + 2 <= len(raw):
        col = struct.unpack_from("<H", raw, at)[0]
        at += 2
        if col >= len(columns):
            raise FgbError("a property names column %d of %d" % (col, len(columns)))
        name, kind = columns[col]
        if kind in SCALARS:
            fmt = SCALARS[kind]
            out[name] = struct.unpack_from(fmt, raw, at)[0]
            at += struct.calcsize(fmt)
        else:
            n = struct.unpack_from("<I", raw, at)[0]
            value = raw[at + 4:at + 4 + n]
            out[name] = value.decode("utf-8", "replace") if kind in (STRING, JSON) else value
            at += 4 + n
    return out


def read(buf):
    """Every feature of a FlatGeobuf file's bytes: [(properties, rings)]."""
    if buf[:4] != MAGIC:
        raise FgbError("not FlatGeobuf")
    size = struct.unpack_from("<I", buf, 8)[0]
    header = _root(buf, 12)
    columns = [(c.string(0), c.scalar(1, "<B")) for c in header.tables(7)]
    count = header.scalar(8, "<Q")
    node_size = header.scalar(9, "<H", 16)
    at = 12 + size + _index_bytes(count, node_size)
    out = []
    while at + 4 <= len(buf):
        n = struct.unpack_from("<I", buf, at)[0]
        feature = _root(buf, at + 4)
        geom_at = feature._ref(0)           # a table, where its offset points
        rings = _rings(Table(buf, geom_at)) if geom_at is not None else []
        out.append((_properties(feature.bytes(1), columns), rings))
        at += 4 + n
    if count and len(out) != count:
        raise FgbError("%d features read, the header says %d" % (len(out), count))
    return out
