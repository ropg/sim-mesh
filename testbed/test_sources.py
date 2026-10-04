"""Building a pack from its sources: the rectangle's grid and what it is
refused for, which tiles and which extract a rectangle needs, the cache's
fetches (resumed, once, a missing file remembered), and a build end to end
against a host and a compiler of the test's own."""

import asyncio
import json
import os
import re
import shutil
import sys
import zipfile

import aiohttp
import pytest
from aiohttp import web

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import geodata  # noqa: E402
import packbuild  # noqa: E402
import sourcefile  # noqa: E402
import sources  # noqa: E402
import store  # noqa: E402

SHIPPED = sourcefile.load((sourcefile.SHIPPED,))

MITTE = [13.38, 52.51, 13.42, 52.53]


def square(lon0, lat0, lon1, lat1):
    return [[lon0, lat0], [lon1, lat0], [lon1, lat1], [lon0, lat1], [lon0, lat0]]


def index():
    """A Geofabrik index with Germany, Brandenburg round Berlin, and Berlin."""
    def feature(id_, ring):
        return {"type": "Feature", "properties": {
            "id": id_, "name": id_.title(),
            "urls": {"pbf": "/geofabrik/%s-latest.osm.pbf" % id_}},
            "geometry": {"type": "MultiPolygon", "coordinates": [[ring]]}}
    return {"features": [feature("germany", square(5.8, 47.2, 15.1, 55.1)),
                         feature("brandenburg", square(11.2, 51.3, 14.8, 53.6)),
                         feature("berlin", square(13.08, 52.33, 13.77, 52.68))]}


FEED = "\n".join('<link href="https://gdi.berlin.de/data/a_lod2/atom/LoD2_%d_%d.zip"/>' % (e, n)
                 for e in range(388, 394) for n in range(5818, 5823))


def test_a_rectangle_is_a_grid_in_the_zone_of_its_centre():
    got = sources.check(MITTE, 30)
    assert got["zone"] == 33 and got["epsg"] == 32633
    nx, ny = got["cells"]
    assert 85 < nx < 95 and 70 < ny < 80
    with pytest.raises(store.StoreError, match="wider than its UTM zone"):
        sources.check([10, 50, 17, 51], 30)
    with pytest.raises(store.StoreError, match="million"):
        sources.check([12, 50, 17, 54], 10)
    with pytest.raises(store.StoreError, match="30 m or 10 m"):
        sources.check(MITTE, 20)
    with pytest.raises(store.StoreError, match="west to east"):
        sources.check([13.42, 52.51, 13.38, 52.53], 30)


def test_tiles_are_named_by_their_south_west_corner_over_the_grids_reach():
    tm = geodata.TransverseMercator(33, True, geodata.WGS84)
    hull = sources.degree_hull([12.96, 52.25, 13.85, 52.79], tm)
    glo30, worldcover = SHIPPED["glo30"], SHIPPED["worldcover"]
    assert [f.name for f in sources.template_files(glo30, hull)] == [
        "Copernicus_DSM_COG_10_N52_00_E012_00_DEM.tif",
        "Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif"]
    assert [f.name for f in sources.template_files(glo30, [-58.5, -34.7, -58.3, -34.5])] == [
        "Copernicus_DSM_COG_10_S35_00_W059_00_DEM.tif"]
    assert [f.name for f in sources.template_files(worldcover, hull)] == [
        "ESA_WorldCover_10m_2021_v200_N51E012_Map.tif"]
    assert sources.template_files(glo30, hull)[0].url.endswith(
        "/Copernicus_DSM_COG_10_N52_00_E012_00_DEM/Copernicus_DSM_COG_10_N52_00_E012_00_DEM.tif")
    # A cached name is read back as the tile it is.
    m = sources.template_pattern(glo30).match("Copernicus_DSM_COG_10_S35_00_W059_00_DEM.tif")
    assert (m.group("ns"), m.group("lat"), m.group("ew"), m.group("lon")) == ("S", "35", "W", "059")


def test_the_smallest_extract_holding_the_rectangle_is_the_one():
    assert sources.smallest_extract(index(), MITTE)["properties"]["id"] == "berlin"
    assert sources.smallest_extract(index(), [13.0, 52.5, 13.2, 52.6])["properties"]["id"] \
        == "brandenburg"
    assert sources.smallest_extract(index(), [4.0, 50, 6.0, 51]) is None
    germany = sources.outline(index(), "germany")
    assert sources.meets(germany, [4.0, 50, 6.0, 51]) and not sources.meets(germany, [0, 0, 1, 1])
    # A rectangle astride a concave outline's notch is not held by it.
    notched = {"type": "Polygon", "coordinates": [[[0, 0], [4, 0], [4, 4], [3, 4], [3, 1],
                                                   [1, 1], [1, 4], [0, 4], [0, 0]]]}
    assert not sources.holds(notched, [0.5, 0.5, 3.5, 3.5])
    assert sources.holds(notched, [0.2, 0.2, 3.8, 0.8])


def test_only_the_feed_tiles_meeting_the_rectangle_are_chosen():
    lod2 = SHIPPED["berlin-lod2"]
    chosen = sources.atom_files(lod2, FEED, MITTE)
    names = sorted(f.name for f in chosen)
    assert names and len(names) < 30 and chosen[0].members == "*.xml"
    x0, y0, x1, y1 = sources.atom_box(lod2, MITTE)
    for f in chosen:
        e, n = (int(v) for v in f.name[5:-4].split("_"))
        assert e < x1 and e + 1 > x0 and n < y1 and n + 1 > y0
    assert sources.atom_files(lod2, FEED, [12.0, 52.5, 12.1, 52.6]) == []


def test_a_listing_and_a_download_service_give_whole_addresses_and_names():
    # Brandenburg's tiles are a directory listing's relative links.
    listing = '<a href="?C=N;O=D">Name</a><a href="dgm_33367-5806.zip">dgm_33367-5806.zip</a>'
    got = sources.atom_files(SHIPPED["brandenburg-dgm1"], listing, [13.0492, 52.3904, 13.0563, 52.3950])
    assert [(f.url, f.name) for f in got] == [
        ("https://data.geobasis-bb.de/geobasis/daten/dgm/tif/dgm_33367-5806.zip",
         "dgm_33367-5806.zip")]
    assert got[0].members == "*.tif"
    # M-V's are a download service's, XML-escaped, the name in the query.
    feed = ('<link href="https://www.geodaten-mv.de/dienste/dgm_download?index=1&amp;dataset=x'
            '&amp;file=dgm1_33_262_5946_2_xyz.zip"/>')
    got = sources.atom_files(SHIPPED["mv-dgm1"], feed, [11.40, 53.62, 11.42, 53.63])
    assert [(f.url, f.name) for f in got] == [
        ("https://www.geodaten-mv.de/dienste/dgm_download?index=1&dataset=x"
         "&file=dgm1_33_262_5946_2_xyz.zip", "dgm1_33_262_5946_2_xyz.zip")]
    assert sources.file_name("https://gdi.berlin.de/x/DGM1_390_5818.zip") == "DGM1_390_5818.zip"


# ---- a host of the test's own --------------------------------------------------

def host_app(root, calls):
    async def any_file(request):
        calls.append((request.method, request.path, request.headers.get("Range"),
                      request.headers.get("User-Agent")))
        path = os.path.join(root, request.path.lstrip("/"))
        if not os.path.isfile(path):
            raise web.HTTPNotFound()
        return web.FileResponse(path)
    app = web.Application()
    app.router.add_route("*", "/{tail:.*}", any_file)
    return app


def running_host(root, check):
    calls = []

    async def go():
        runner = web.AppRunner(host_app(str(root), calls))
        await runner.setup()
        site = web.TCPSite(runner, "127.0.0.1", 0)
        await site.start()
        base = "http://127.0.0.1:%d" % runner.addresses[0][1]
        try:
            async with aiohttp.ClientSession() as session:
                await check(base, session, calls)
        finally:
            await runner.cleanup()
    asyncio.run(go())


def test_a_file_is_fetched_once_resumed_and_a_missing_one_remembered(tmp_path):
    served = tmp_path / "served"
    served.mkdir()
    (served / "big.bin").write_bytes(bytes(range(256)) * 1000)

    async def check(base, session, calls):
        cache = sources.Cache(session, str(tmp_path / "cache"))
        f = sources.File("glo30", base + "/big.bin", "big.bin")
        path = f.path(cache.root)
        os.makedirs(os.path.dirname(path))
        with open(path + ".part", "wb") as out:
            out.write((bytes(range(256)) * 1000)[:1000])
        assert await cache.size(f) == 256000
        assert (await cache.sizes_of([f]))[f] == 255000
        heard = []
        assert await cache.fetch(f, heard.append) == path
        assert open(path, "rb").read() == bytes(range(256)) * 1000
        assert calls[-1][2] == "bytes=1000-" and heard[-1] == 256000
        assert calls[-1][3] == sources.USER_AGENT
        before = len(calls)
        assert await cache.fetch(f) == path and len(calls) == before
        gone = sources.File("glo30", base + "/sea.tif", "sea.tif")
        assert await cache.fetch(gone) is None
        assert await cache.fetch(gone) is None and len(calls) == before + 1
        assert cache.have(gone) is False
    running_host(tmp_path / "served", check)


def test_a_zips_wanted_members_are_extracted_once(tmp_path):
    cache = sources.Cache(None, str(tmp_path))
    f = sources.single_file(SHIPPED["itu"])
    assert f.name == "itu.zip" and f.members == ("DN50.TXT", "N050.TXT")
    os.makedirs(tmp_path / "itu")
    with zipfile.ZipFile(tmp_path / "itu" / "itu.zip", "w") as zf:
        for name in ("ReadMe.doc", "DN50.TXT", "N050.TXT", "LAT.TXT"):
            zf.writestr(name, name)
    got = cache.extracted(f)
    assert sorted(os.path.basename(p) for p in got) == ["DN50.TXT", "N050.TXT"]
    assert sorted(os.listdir(tmp_path / "itu" / "x")) == ["DN50.TXT", "N050.TXT"]
    os.remove(tmp_path / "itu" / "itu.zip")
    assert cache.extracted(f) == got


# ---- a build, end to end ---------------------------------------------------------

FAKE_JOB = r'''#!/usr/bin/env python3
import json, os, sys
params = json.load(sys.stdin)
with open(os.environ["FAKE_JOB_SAW"], "w") as out:
    json.dump(params, out)
if params["name"] == "broken":
    print("pack build: something went wrong", file=sys.stderr)
    print(json.dumps({"error": "the DSM tiles leave holes"}))
    sys.exit(1)
os.makedirs(params["out_dir"], exist_ok=True)
for i, step in enumerate(["terrain", "clutter", "roads"]):
    print(json.dumps({"step": step, "done": i, "total": 3}), flush=True)
manifest = {"name": params["name"], "region": {"bbox": params["bbox"],
            "crs_epsg": 32600 + params["utm_zone"]}, "layers": [], "licenses": []}
path = os.path.join(params["out_dir"], "manifest.json")
with open(path, "w") as out:
    json.dump(manifest, out)
print(json.dumps({"manifest": path}))
'''


def serve_sources(root):
    """The public hosts as a directory: the index, Berlin's feeds, and a file
    for every URL a Mitte build asks for, except the GLO-30 tile east of it
    and Berlin's 1 m tiles."""
    (root / "geofabrik").mkdir(parents=True)
    (root / "index.json").write_text(json.dumps(index()))
    (root / "geofabrik" / "berlin-latest.osm.pbf").write_bytes(b"pbf" * 100)
    (root / "lod2").mkdir()
    with zipfile.ZipFile(root / "lod2" / "LoD2_390_5820.zip", "w") as zf:
        zf.writestr("LoD2_33_390_5820_1_BE.xml", "<CityModel/>")
    with zipfile.ZipFile(root / "zensus.zip", "w") as zf:
        zf.writestr(SHIPPED["zensus"].members(),
                    "GITTER_ID_100m;x_mp_100m;y_mp_100m;Einwohner\n")


def served_sources(tmp_path, base):
    """The shipped source file with every address on the test's host, and
    its outlines beside it: the sources a build is planned from, only those
    the host serves (no test reaches out to a real one)."""
    text = open(sourcefile.SHIPPED, encoding="utf-8").read()
    for old, new in (
            ("https://copernicus-dem-30m.s3.amazonaws.com/{tile}/{tile}.tif", "/glo30/{tile}.tif"),
            ("https://esa-worldcover.s3.eu-central-1.amazonaws.com/v200/2021/map/"
             "ESA_WorldCover_10m_2021_v200_{tile}_Map.tif", "/wc/{tile}.tif"),
            ("https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.1812-8-202509-I!!ZIP-E.zip",
             "/itu.zip"),
            ("https://download.geofabrik.de/index-v1.json", "/index.json"),
            ("https://gdi.berlin.de/data/dgm1/atom/0.atom", "/berlin-dgm1.atom"),
            ("https://gdi.berlin.de/data/bdom/atom/0.atom", "/berlin-bdom.atom"),
            ("https://gdi.berlin.de/data/a_lod2/atom/0.atom", "/berlin-lod2.atom"),
            ("https://www.destatis.de/static/DE/zensus/gitterdaten/"
             "Zensus2022_Bevoelkerungszahl.zip", "/zensus.zip")):
        assert old in text
        text = text.replace(old, base + new)
    here = tmp_path / "sources"
    here.mkdir(exist_ok=True)
    (here / "sources.yaml").write_text(text)
    shutil.copytree(os.path.join(os.path.dirname(sourcefile.SHIPPED), "outlines"),
                    here / "outlines", dirs_exist_ok=True)
    served = ("glo30", "worldcover", "itu", "geofabrik", "berlin-dgm1", "berlin-bdom",
              "berlin-lod2", "zensus")
    return {k: v for k, v in sourcefile.load((str(here / "sources.yaml"),)).items() if k in served}


def test_each_source_says_what_it_is_used_for():
    chosen = [SHIPPED[s] for s in ("glo30", "geofabrik", "berlin-bdom", "berlin-lod2", "zensus",
                                   "itu")]
    inside = sources.used_for(chosen, {s.id: True for s in chosen})
    assert inside["berlin-lod2"] == "buildings" and inside["zensus"] == "population"
    assert inside["glo30"] == "surface heights where Berlin bDOM, 1 m surface has none"
    assert inside["geofabrik"] == \
        "roads; places; buildings where Berlin LoD2 building models has none"
    assert inside["itu"] == "the radio climate (ΔN and N0); kept here"
    across = sources.used_for(chosen, {s.id: s.worldwide for s in chosen})
    assert across["berlin-lod2"] == "buildings inside its outline"
    assert across["glo30"] == "surface heights outside Berlin bDOM, 1 m surface"
    assert across["geofabrik"] == "roads; places; buildings outside Berlin LoD2 building models"
    assert across["zensus"] == "population inside its outline"


def test_the_shipped_sources_hold_together():
    assert list(SHIPPED) == ["glo30", "worldcover", "itu", "geofabrik", "berlin-dgm1",
                             "berlin-bdom", "berlin-lod2", "zensus", "brandenburg-dgm1",
                             "brandenburg-bdom", "brandenburg-lod2", "mv-dgm1", "mv-dom1",
                             "mv-lod2", "ahn-dtm", "ahn-dsm", "3dbag", "cbs-population"]
    for source in SHIPPED.values():
        assert source.worldwide == (source.continent == sourcefile.GLOBAL)
        assert source.worldwide or source.outline["type"] in ("Polygon", "MultiPolygon")
        assert source.redistributable == (source.id != "itu")
    assert SHIPPED["zensus"].where == "Europe › Germany"
    assert SHIPPED["3dbag"].where == "Europe › Netherlands"
    assert sourcefile.proj_of(SHIPPED["ahn-dtm"]).startswith("+proj=sterea")


@pytest.mark.parametrize("change, why", [
    (lambda e: e.update(id="Bad Id"), "usable source id"),
    (lambda e: e.update(layers={"sky": 1}), "no layer"),
    (lambda e: e.update(coverage="worldwide"), "worldwide source stands under `global`"),
    (lambda e: e["find"].update(method="ftp"), "find.method"),
    (lambda e: e["find"].pop("feed"), "needs feed"),
    (lambda e: e["find"].update(name="(\\d+)"), "(?P<x>"),
    (lambda e: e.update(read="stream"), "read is whole or window"),
    (lambda e: e.update(read="window"), "a window is read of a regional source's cloud-optimised"),
    (lambda e: e["format"].update(crs="EPSG:4326"), "the compiler reads xyz"),
    (lambda e: e["format"].update(nodata=-9999), "format.nodata is the number a GeoTIFF"),
    (lambda e: e.update(notice=None), "gives the notice"),
])
def test_a_source_that_does_not_hold_together_is_refused_saying_why(tmp_path, change, why):
    import copy
    import yaml
    entry = copy.deepcopy(SHIPPED["berlin-dgm1"].entry)
    change(entry)
    (tmp_path / "outlines").mkdir()
    shutil.copy(os.path.join(os.path.dirname(sourcefile.SHIPPED), "outlines",
                             "berlin-dgm1.geojson"), tmp_path / "outlines" / "berlin-dgm1.geojson")
    (tmp_path / "sources.yaml").write_text(yaml.safe_dump(
        {"europe": {"DE": {"name": "Germany", "sources": [entry]}}}, allow_unicode=True))
    with pytest.raises(store.StoreError, match=re.escape(why)):
        sourcefile.read_file(str(tmp_path / "sources.yaml"))


def test_a_persons_source_may_not_take_a_shipped_id_nor_lack_its_outline(tmp_path):
    import yaml
    own = tmp_path / "sources.yaml"
    own.write_text(yaml.safe_dump({"europe": {"DE": {"name": "Germany", "sources": [
        SHIPPED["zensus"].entry]}}}, allow_unicode=True))
    with pytest.raises(store.StoreError, match="outline"):
        sourcefile.load((sourcefile.SHIPPED, str(own)))
    (tmp_path / "outlines").mkdir()
    shutil.copy(os.path.join(os.path.dirname(sourcefile.SHIPPED), "outlines", "zensus.geojson"),
                tmp_path / "outlines" / "zensus.geojson")
    with pytest.raises(store.StoreError, match="sim-mesh's"):
        sourcefile.load((sourcefile.SHIPPED, str(own)))


def test_a_build_fetches_what_it_needs_and_hands_the_compiler_its_inputs(tmp_path, monkeypatch):
    served = tmp_path / "served"
    serve_sources(served)
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path / "geodata"))
    ground = tmp_path / "geodata"
    job = tmp_path / "planner-job"
    job.write_text(FAKE_JOB)
    job.chmod(0o755)
    saw = tmp_path / "saw.json"
    monkeypatch.setenv("FAKE_JOB_SAW", str(saw))

    async def check(base, session, calls):
        reg = served_sources(tmp_path, base)
        (served / "glo30").mkdir()
        (served / "glo30" / "Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif").write_bytes(b"dem")
        (served / "wc").mkdir()
        (served / "wc" / "N51E012.tif").write_bytes(b"wc")
        with zipfile.ZipFile(served / "itu.zip", "w") as zf:
            zf.writestr("DN50.TXT", "dn")
            zf.writestr("N050.TXT", "n0")
        idx = index()
        for feature in idx["features"]:
            feature["properties"]["urls"]["pbf"] = base + feature["properties"]["urls"]["pbf"]
        (served / "index.json").write_text(json.dumps(idx))
        for source in ("berlin-dgm1", "berlin-bdom", "berlin-lod2"):
            (served / (source + ".atom")).write_text(
                FEED.replace("https://gdi.berlin.de/data/a_lod2/atom", base + "/" + source[7:]))
        cache = sources.Cache(session, str(tmp_path / "cache"))

        planned = await sources.plan(cache, {"bbox": MITTE, "res_m": 30}, sources=reg)
        rows = {r["source"]: r for r in planned["sources"]}
        assert rows["geofabrik"]["to_fetch"] == 300 and rows["glo30"]["to_fetch"] == 3
        assert planned["extract"]["id"] == "berlin"
        assert rows["berlin-lod2"]["used_for"] == "buildings"
        assert rows["geofabrik"]["used_for"] == \
            "roads; places; buildings where Berlin LoD2 building models has none"
        assert rows["zensus"]["used_for"] == "population"
        potsdam = await sources.plan(cache, {"bbox": [12.9, 52.35, 13.0, 52.42], "res_m": 30},
                                     sources=reg)
        rows = {r["source"]: r for r in potsdam["sources"]}
        assert potsdam["extract"]["id"] == "brandenburg"
        assert sorted(rows) == ["geofabrik", "glo30", "itu", "worldcover", "zensus"]
        assert rows["geofabrik"]["used_for"] == "roads; places; buildings"
        paris = {"bbox": [2.0, 48.8, 2.1, 48.9], "res_m": 30}
        with pytest.raises(store.StoreError, match="no region of"):
            await sources.plan(cache, paris, sources=reg)

        said = []
        spec = {"name": "mitte", "bbox": MITTE, "res_m": 30}
        packbuild.refuse(spec)
        build = packbuild.Build(cache, spec, str(job), said.append, sources_=reg)
        await build.start()
        assert build.row["state"] == "done", build.row["error"]
        params = json.loads(saw.read_text())
        assert params["osm_buildings"] and params["utm_zone"] == 33
        assert os.path.basename(params["lod2_dir"]) == "lod2"
        assert params["berlin_1m_dir"] and params["elevation"] == [] and params["cityjson"] is None
        # Zensus's grid as a population input, its layout and system from its source.
        population = params["population"]
        assert population["csv"].endswith("Zensus2022_Bevoelkerungszahl_100m-Gitter.csv")
        assert (population["delimiter"], population["x"], population["value"], population["cell_m"]) \
            == (";", "x_mp_100m", "Einwohner", 100.0)
        assert population["proj"].startswith("+proj=laea +lat_0=52 +lon_0=10")
        assert params["osm_pbf"].endswith("geofabrik/berlin.osm.pbf")
        assert os.path.basename(params["itu_maps_dir"]) == "x"
        assert [os.path.basename(p) for p in params["dsm_tiles"]] == [
            "Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif"]
        states = [r["state"] for r in said]
        assert states[0] == "fetching" and "compiling" in states and states[-1] == "done"
        assert [r["step"] for r in said if r["state"] == "compiling"][1:4] == \
            ["terrain", "clutter", "roads"]
        gd = geodata.load("mitte")
        assert gd.is_pack and gd.pack_dir == str(ground / "mitte")
        assert (ground / "mitte" / "geodata.yaml").read_text().startswith("# built from sources")
        assert os.listdir(ground) == ["mitte"]
        with pytest.raises(store.StoreError, match="already"):
            packbuild.refuse(spec)

        fetched = len(calls)
        build = packbuild.Build(cache, dict(spec, name="broken"), str(job), said.append,
                                sources_=reg)
        await build.start()
        assert build.row["state"] == "failed"
        assert build.row["error"] == "the DSM tiles leave holes"
        assert "something went wrong" in open(build.log_path).read()
        assert not os.path.exists(geodata.geodata_path("broken"))
        # The second build over the same ground fetched nothing new.
        assert not [c for c in calls[fetched:] if c[0] == "GET" and "glo30" in c[1]]

        build = packbuild.Build(cache, dict(spec, name="gone"), str(job), said.append,
                                sources_=reg)
        task = build.start()
        await asyncio.sleep(0)
        build.cancel()
        await task
        assert build.row["state"] == "cancelled"
        assert os.listdir(ground) == ["mitte"]
    running_host(served, check)


class StandInCache:
    """What source_map asks of the cache, from fixtures: its root, the index
    and a Berlin feed of two tiles."""

    def __init__(self, root):
        self.root = str(root)

    async def regions_index(self, source):
        return index()

    async def feed(self, source):
        return ('<a href="https://gdi.berlin.de/x/DGM1_390_5818.zip"/>'
                '<a href="https://gdi.berlin.de/x/DGM1_392_5818.zip"/>')

    async def index_features(self, source):
        raise store.StoreError("no index in this test")


def test_a_sources_area_and_its_cache_come_from_names_feeds_and_outlines(tmp_path):
    for source, name in (("glo30", "Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif"),
                         ("glo30", "Copernicus_DSM_COG_10_S34_00_W071_00_DEM.tif.part"),
                         ("worldcover", "ESA_WorldCover_10m_2021_v200_N51E012_Map.tif"),
                         ("berlin-dgm1", "DGM1_390_5818.zip"), ("berlin-dgm1", "DGM1_390_5818.zip.x"),
                         ("geofabrik", "berlin.osm.pbf")):
        (tmp_path / source).mkdir(exist_ok=True)
        (tmp_path / source / name).write_bytes(b"x")
    cache = StandInCache(tmp_path)

    glo = asyncio.run(sources.source_map(cache, "glo30", SHIPPED))
    assert glo["worldwide"] and glo["cached_files"] == 1
    assert glo["cached"]["coordinates"] == [sources._box(13, 52, 14, 53)]
    cover = asyncio.run(sources.source_map(cache, "worldcover", SHIPPED))
    assert cover["cached"]["coordinates"] == [sources._box(12, 51, 15, 54)]
    dgm = asyncio.run(sources.source_map(cache, "berlin-dgm1", SHIPPED))
    # Its data is where its feed has a tile, not the whole of its outline.
    assert not dgm["worldwide"] and len(dgm["covers"]["coordinates"]) == 2
    assert dgm["covers_from"] == "the 2 tiles its feed lists"
    assert len(dgm["cached"]["coordinates"]) == 1 and dgm["cached_files"] == 1
    osm = asyncio.run(sources.source_map(cache, "geofabrik", SHIPPED))
    assert osm["cached"]["coordinates"] == [[square(13.08, 52.33, 13.77, 52.68)]]

    # A point in the cached Berlin tile, and one in France.
    lat, lon = sources.zone_of("EPSG:25833").inverse(390500, 5818500)
    here = asyncio.run(sources.sources_at(cache, lon, lat, SHIPPED))
    assert here["berlin-dgm1"] == {"has": True, "cached": True, "error": None}
    assert here["berlin-lod2"]["has"] and not here["berlin-lod2"]["cached"]
    assert here["glo30"]["cached"] and here["zensus"]["has"]
    # Inside Berlin but on no tile its feeds list: no Berlin data there.
    lat, lon = sources.zone_of("EPSG:25833").inverse(386500, 5818500)
    beside = asyncio.run(sources.sources_at(cache, lon, lat, SHIPPED))
    assert not beside["berlin-dgm1"]["has"] and beside["zensus"]["has"]
    away = asyncio.run(sources.sources_at(cache, 2.35, 48.85, SHIPPED))
    assert [s for s, v in away.items() if v["has"]] == ["glo30", "worldcover", "itu", "geofabrik"]
    assert not any(v["cached"] for v in away.values())


def test_a_file_comes_from_a_mirror_when_the_first_address_fails(tmp_path):
    served = tmp_path / "served"
    (served / "second").mkdir(parents=True)
    (served / "second" / "tile.tif").write_bytes(b"tile")

    async def check(base, session, calls):
        cache = sources.Cache(session, str(tmp_path / "cache"))
        f = sources.File("glo30", [base + "/first/tile.tif", base + "/second/tile.tif"],
                         "tile.tif")
        assert await cache.fetch(f) == f.path(cache.root)
        assert [c[1] for c in calls] == ["/first/tile.tif", "/second/tile.tif"]
        gone = sources.File("glo30", [base + "/a/x.tif", base + "/b/x.tif"], "x.tif",
                            missing="error")
        with pytest.raises(store.StoreError, match="404"):
            await cache.fetch(gone)
    running_host(served, check)
