"""The coordinate systems a source may be in: what planning, the map and
the compiler need of each.

    EPSG:4326              degrees
    EPSG:326zz, 327zz      UTM on WGS84, north and south
    EPSG:258zz             UTM on ETRS89 (Berlin's data)
    EPSG:3035              ETRS89 / LAEA Europe (Zensus's grid)
    EPSG:28992             Amersfoort / RD New (the Netherlands' AHN, 3DBAG, CBS)
    EPSG:7415              RD New with NAP heights (3DBAG's CityJSON): RD New across

`proj(crs)` is the proj string the compiler projects with, exactly.
`plane(crs)` is what planning projects with here, both ways between
degrees and the system's metres: Krüger's series for UTM (geodata.py), and
for RD New the published polynomial between RD and WGS84 coordinates
(Schreutelkamp and Strang van Hees, 2001), good to about a metre across the
Netherlands, which is all that choosing tiles and drawing a map asks for.
"""

import re

import geodata
import store

EPSG_RE = re.compile(r"^EPSG:(\d+)$")
RD_NEW = ("+proj=sterea +lat_0=52.15616055555555 +lon_0=5.38763888888889 +k=0.9999079 "
          "+x_0=155000 +y_0=463000 +ellps=bessel "
          "+towgs84=565.417,50.3319,465.552,-0.398957,0.343988,-1.8774,4.0725 +units=m +no_defs")
FIXED = {
    4326: "+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs",
    3035: "+proj=laea +lat_0=52 +lon_0=10 +x_0=4321000 +y_0=3210000 +ellps=GRS80 +units=m +no_defs",
    28992: RD_NEW,
    7415: RD_NEW,
}


def epsg(crs):
    m = EPSG_RE.match(str(crs))
    return int(m.group(1)) if m else None


def known(crs):
    code = epsg(crs)
    return code is not None and (code in FIXED or _utm(code) is not None)


def _utm(code):
    for base, south, ellps in ((32600, False, "WGS84"), (32700, True, "WGS84"),
                               (25800, False, "GRS80")):
        if 1 <= code - base <= 60:
            return code - base, south, ellps
    return None


def proj(crs):
    """The proj string of a known system, as the compiler reads it."""
    code = epsg(crs)
    if code in FIXED:
        return FIXED[code]
    utm = _utm(code) if code is not None else None
    if utm is None:
        raise store.StoreError("%s is a system sim-mesh does not know" % crs)
    zone, south, ellps = utm
    return "+proj=utm +zone=%d%s +ellps=%s %s+units=m +no_defs" % (
        zone, " +south" if south else "", ellps,
        "+datum=WGS84 " if ellps == "WGS84" else "+towgs84=0,0,0,0,0,0,0 ")


class Degrees:
    def forward(self, lat, lon):
        return lon, lat

    def inverse(self, x, y):
        return y, x


class RdNew:
    """RD New both ways, by the published polynomials."""

    LAT0, LON0 = 52.15517440, 5.38720621
    X0, Y0 = 155000.0, 463000.0
    TO_LAT = ((0, 1, 3235.65389), (2, 0, -32.58297), (0, 2, -0.24750), (2, 1, -0.84978),
              (0, 3, -0.06550), (2, 2, -0.01709), (1, 0, -0.00738), (4, 0, 0.00530),
              (2, 3, -0.00039), (4, 1, 0.00033), (1, 1, -0.00012))
    TO_LON = ((1, 0, 5260.52916), (1, 1, 105.94684), (1, 2, 2.45656), (3, 0, -0.81885),
              (1, 3, 0.05594), (3, 1, -0.05607), (0, 1, 0.01199), (3, 2, -0.00256),
              (1, 4, 0.00128), (0, 2, 0.00022), (2, 0, -0.00022), (5, 0, 0.00026))
    TO_X = ((0, 1, 190094.945), (1, 1, -11832.228), (2, 1, -114.221), (0, 3, -32.391),
            (1, 0, -0.705), (3, 1, -2.340), (1, 3, -0.608), (0, 2, -0.008), (2, 3, 0.148))
    TO_Y = ((1, 0, 309056.544), (0, 2, 3638.893), (2, 0, 73.077), (1, 2, -157.984),
            (3, 0, 59.788), (0, 1, 0.433), (2, 2, -6.439), (1, 1, -0.032), (0, 4, 0.092),
            (1, 4, -0.054))

    def forward(self, lat, lon):
        dp, dl = 0.36 * (lat - self.LAT0), 0.36 * (lon - self.LON0)
        x = sum(c * dp ** p * dl ** q for p, q, c in self.TO_X)
        y = sum(c * dp ** p * dl ** q for p, q, c in self.TO_Y)
        return self.X0 + x, self.Y0 + y

    def inverse(self, x, y):
        dx, dy = (x - self.X0) * 1e-5, (y - self.Y0) * 1e-5
        lat = sum(c * dx ** p * dy ** q for p, q, c in self.TO_LAT)
        lon = sum(c * dx ** p * dy ** q for p, q, c in self.TO_LON)
        return self.LAT0 + lat / 3600.0, self.LON0 + lon / 3600.0


def plane(crs):
    """forward(lat, lon) → (x, y) and inverse(x, y) → (lat, lon) for a known
    system."""
    code = epsg(crs)
    if code == 4326:
        return Degrees()
    if code in (28992, 7415):
        return RdNew()
    if code is not None and _utm(code) is not None:
        return geodata.TransverseMercator(*geodata.utm_zone_of(code))
    raise store.StoreError("planning has no projection for %s" % crs)


def box_in(crs, bbox, margin=0.0):
    """A rectangle [lon0, lat0, lon1, lat1] as the box it reaches in a
    system's units, from its edges sampled, `margin` added each side."""
    lon0, lat0, lon1, lat1 = bbox
    p = plane(crs)
    pts = []
    for i in range(9):
        f = i / 8.0
        for lat, lon in ((lat0, lon0 + f * (lon1 - lon0)), (lat1, lon0 + f * (lon1 - lon0)),
                         (lat0 + f * (lat1 - lat0), lon0), (lat0 + f * (lat1 - lat0), lon1)):
            pts.append(p.forward(lat, lon))
    return [min(x for x, _ in pts) - margin, min(y for _, y in pts) - margin,
            max(x for x, _ in pts) + margin, max(y for _, y in pts) + margin]


def ring_to_degrees(crs, ring):
    """A ring in a system's units as [[lon, lat], …]."""
    p = plane(crs)
    out = []
    for x, y in ring:
        lat, lon = p.inverse(x, y)
        out.append([round(lon, 6), round(lat, 6)])
    return out
