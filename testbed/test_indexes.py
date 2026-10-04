"""Indexes: reading one, installing its geodata and nodesets by their
sha256, and what is installed saying where it came from."""

import asyncio
import hashlib
import io
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import geodata  # noqa: E402
import indexes  # noqa: E402
import nodeset  # noqa: E402
import store  # noqa: E402

NODESET_TEXT = ("nodes:\n"
                "  a: { id: 1, lat: 0.001, lon: 0.001, height_m: 10, height_from: assumed, "
                "antenna: { type: whip_sma_quarter_wave }, tags: [] }\n"
                "offsets: []\n")


@pytest.fixture
def stores(tmp_path, monkeypatch):
    """Empty geodata and nodeset stores, and no added indexes."""
    monkeypatch.setattr(store, "GEODATA_DIR", str(tmp_path / "geodata"))
    monkeypatch.setattr(store, "NODESETS_DIR", str(tmp_path / "nodesets"))
    monkeypatch.setattr(indexes, "ADDED_FILE", str(tmp_path / "indexes.yaml"))
    return tmp_path


def synthetic_zip():
    out = io.BytesIO()
    gd = geodata.Geodata("flat", geodata.parse({"synthetic": {"extent_m": 5000}}, "t"))
    geodata.export_zip(gd, out)
    return out.getvalue()


def publish(directory, name="demos", geodata_bytes=None, nodeset_bytes=None, sha=None):
    """An index in a directory, with one synthetic geodata and one nodeset
    made for it: the index's address."""
    directory.mkdir(parents=True, exist_ok=True)
    geodata_bytes = geodata_bytes or synthetic_zip()
    nodeset_bytes = nodeset_bytes or NODESET_TEXT.encode()
    (directory / "flat-5.zip").write_bytes(geodata_bytes)
    (directory / "pair.yaml").write_bytes(nodeset_bytes)
    (directory / "index.yaml").write_text(
        "index: %s\ntitle: test ground\n"
        "geodata:\n  - { name: flat-5, url: flat-5.zip, sha256: %s, bytes: %d }\n"
        "nodesets:\n  - { name: pair, url: pair.yaml, sha256: %s, geodata: flat-5, nodes: 1 }\n"
        % (name, sha or hashlib.sha256(geodata_bytes).hexdigest(), len(geodata_bytes),
           hashlib.sha256(nodeset_bytes).hexdigest()))
    return str(directory) + "/"


def run(coro):
    return asyncio.run(coro)


def test_an_index_reads_with_its_urls_relative_to_it(tmp_path):
    got = indexes.parse("index: demos\ndescription: |\n  Two lines\n  of text.\n"
                        "geodata:\n  - { name: g, url: g.zip, sha256: %s }\n"
                        % ("a" * 64), "https://example.org/mesh/index.yaml")
    assert got["geodata"][0]["url"] == "https://example.org/mesh/g.zip"
    assert got["description"] == "Two lines\nof text."
    assert got["nodesets"] == []
    local = indexes.parse("index: demos\nnodesets:\n  - { name: n, url: sub/n.yaml, sha256: %s }\n"
                          % ("b" * 64), str(tmp_path / "index.yaml"))
    assert local["nodesets"][0]["url"] == str(tmp_path / "sub" / "n.yaml")
    assert indexes.resolve(str(tmp_path) + "/") == str(tmp_path / "index.yaml")


@pytest.mark.parametrize("text, why", [
    ("title: no name\n", "names no `index:`"),
    ("index: Bad Name\n", "not a usable index name"),
    ("index: d\ngeodata:\n  - { name: g, url: g.zip, sha256: abc }\n", "64 hex digits"),
    ("index: d\ngeodata:\n  - { name: g, sha256: %s }\n" % ("a" * 64), "names no url"),
    ("index: d\ngeodata:\n  - { name: g, url: a, sha256: %s }\n  - { name: g, url: b, sha256: %s }\n"
     % ("a" * 64, "a" * 64), "two geodata entries"),
    ("index: d\nnodesets:\n  - { name: n, url: n, sha256: %s, nodes: -1 }\n" % ("a" * 64),
     "whole number"),
])
def test_an_index_that_is_not_one_says_why(text, why):
    with pytest.raises(store.StoreError, match=why):
        indexes.parse(text, "here")


def test_a_nodeset_brings_its_geodata_and_both_remember_the_index(stores, monkeypatch):
    monkeypatch.setattr(indexes, "OWN_ADDRESS", publish(stores / "site"))
    monkeypatch.setattr(indexes, "OWN_NAME", "demos")
    told = []
    got = run(indexes.install("demos", "nodesets", "pair", None,
                              lambda kind, name, fetched, of: told.append((kind, name))))
    assert got["installed"] == [{"kind": "geodata", "name": "flat-5"},
                                {"kind": "nodesets", "name": "pair"}]
    assert ("geodata", "flat-5") in told and ("nodesets", "pair") in told
    assert geodata.load("flat-5").extent_m == 5000
    assert nodeset.load("pair").nodes["a"]["height_m"] == 10
    assert indexes.geodata_origin("flat-5")["index"] == "demos"
    assert indexes.nodeset_origin("pair")["changed"] is False

    listed = run(indexes.listing(None))
    assert [(e["name"], e["installed"]) for e in listed[0]["geodata"]] == [("flat-5", True)]
    # Installing again is nothing to do.
    run(indexes.install("demos", "geodata", "flat-5", None))

    # An edit here is the nodeset changed, still the index's.
    with open(nodeset.nodeset_path("pair"), "a", encoding="utf-8") as handle:
        handle.write("# edited\n")
    assert indexes.nodeset_origin("pair")["changed"] is True
    status = indexes.status("nodesets", listed[0]["nodesets"][0])
    assert status == {"installed": True, "taken": False, "changed": True}

    # Deleting a nodeset takes its origin with it.
    nodeset.delete("pair")
    assert not os.path.exists(nodeset.origin_path("pair"))


def test_a_name_taken_here_is_refused_and_nothing_replaced(stores, monkeypatch):
    monkeypatch.setattr(indexes, "OWN_ADDRESS", publish(stores / "site"))
    monkeypatch.setattr(indexes, "OWN_NAME", "demos")
    geodata.write(geodata.geodata_path("flat-5"), {"synthetic": {"extent_m": 9000}})
    with pytest.raises(store.StoreError, match="already geodata called flat-5"):
        run(indexes.install("demos", "geodata", "flat-5", None))
    assert geodata.load("flat-5").extent_m == 9000


def test_a_file_that_is_not_the_entry_is_refused(stores, monkeypatch):
    monkeypatch.setattr(indexes, "OWN_ADDRESS", publish(stores / "site", sha="c" * 64))
    monkeypatch.setattr(indexes, "OWN_NAME", "demos")
    with pytest.raises(store.StoreError, match="sha256 differs"):
        run(indexes.install("demos", "geodata", "flat-5", None))
    assert geodata.names() == []
    assert [e for e in os.listdir(store.GEODATA_DIR) if e.startswith(".part-")] == []


def test_indexes_are_added_by_address_and_forgotten_by_name(stores, monkeypatch):
    monkeypatch.setattr(indexes, "OWN_ADDRESS", publish(stores / "own"))
    monkeypatch.setattr(indexes, "OWN_NAME", "demos")
    other = publish(stores / "other", name="others")
    row = run(indexes.add_index(other, None))
    assert row["name"] == "others"
    assert [r["name"] for r in indexes.addresses()] == ["demos", "others"]
    with pytest.raises(store.StoreError, match="already listed"):
        run(indexes.add_index(other, None))
    with pytest.raises(store.StoreError, match="always listed"):
        indexes.remove_index("demos")
    assert indexes.remove_index("others") == ["others"]
    assert [r["name"] for r in indexes.addresses()] == ["demos"]


def test_an_index_that_cannot_be_read_is_a_row_saying_why(stores, monkeypatch):
    monkeypatch.setattr(indexes, "OWN_ADDRESS", str(stores / "nowhere" / "index.yaml"))
    rows = run(indexes.listing(None))
    assert rows[0]["error"] and rows[0]["geodata"] == []


def quiet(_line):
    pass


def test_publishing_beside_an_index_installs_back_as_the_same_bytes(stores, monkeypatch):
    geodata.write(geodata.geodata_path("flat"), {"synthetic": {"extent_m": 7000}})
    os.makedirs(store.NODESETS_DIR)
    with open(nodeset.nodeset_path("pair"), "w", encoding="utf-8") as handle:
        handle.write(NODESET_TEXT)
    site = stores / "site"
    site.mkdir()
    (site / "index.yaml").write_text("# our own\nindex: ours\ntitle: Ours\n"
                                     "description: |\n  Two\n  lines.\ngeodata: []\nnodesets: []\n")
    entry = run(indexes.publish("geodata", "flat", str(site), None, entry_name="flat-7",
                                title="Flat, 7 km", tags=["flat"], say=quiet))
    assert entry["url"] == "flat-7.zip" and (site / "flat-7.zip").stat().st_size == entry["bytes"]
    run(indexes.publish("nodesets", "pair", str(site / "index.yaml"), None,
                        geodata_for="flat-7", say=quiet))
    text = (site / "index.yaml").read_text()
    assert text.startswith("# our own\nindex: ours\ntitle: Ours\ndescription: |\n  Two\n  lines.\n")
    got = indexes.parse(text, str(site / "index.yaml"))
    assert [e["name"] for e in got["geodata"]] == ["flat-7"]
    assert got["nodesets"][0]["geodata"] == "flat-7" and got["nodesets"][0]["nodes"] == 1
    assert got["nodesets"][0]["sha256"] == hashlib.sha256(NODESET_TEXT.encode()).hexdigest()
    with pytest.raises(store.StoreError, match="already"):
        run(indexes.publish("geodata", "flat", str(site), None, entry_name="flat-7", say=quiet))

    # What was published installs elsewhere as what it was.
    monkeypatch.setattr(store, "GEODATA_DIR", str(stores / "elsewhere"))
    monkeypatch.setattr(indexes, "OWN_ADDRESS", str(site) + "/")
    monkeypatch.setattr(indexes, "OWN_NAME", "ours")
    run(indexes.install("ours", "geodata", "flat-7", None))
    assert geodata.load("flat-7").extent_m == 7000


def test_publishing_to_a_release_uploads_once_and_never_replaces(stores, monkeypatch):
    from aiohttp import web
    import aiohttp

    releases, uploads = {}, []

    async def tag(request):
        rel = releases.get(request.match_info["tag"])
        return web.json_response(rel) if rel else web.json_response({}, status=404)

    async def make(request):
        assert request.headers["Authorization"] == "Bearer secret"
        body = await request.json()
        rel = {"id": 1, "assets": [], "upload_url": "%s/upload/1{?name,label}" % base[0]}
        releases[body["tag_name"]] = rel
        return web.json_response(rel, status=201)

    async def upload(request):
        name, data = request.query["name"], await request.read()
        uploads.append((name, data))
        releases["examples"]["assets"].append({"name": name})
        return web.json_response({"browser_download_url": "https://dl/%s" % name}, status=201)

    base = [None]
    geodata.write(geodata.geodata_path("flat"), {"synthetic": {"extent_m": 7000}})
    site = stores / "site"
    site.mkdir()
    (site / "index.yaml").write_text("index: ours\nrelease: someone/repo:examples\n"
                                     "geodata: []\nnodesets: []\n")
    monkeypatch.setenv("GH_TOKEN", "secret")

    async def go():
        app = web.Application()
        app.router.add_get("/repos/someone/repo/releases/tags/{tag}", tag)
        app.router.add_post("/repos/someone/repo/releases", make)
        app.router.add_post("/upload/1", upload)
        runner = web.AppRunner(app)
        await runner.setup()
        site_ = web.TCPSite(runner, "127.0.0.1", 0)
        await site_.start()
        base[0] = "http://127.0.0.1:%d" % runner.addresses[0][1]
        monkeypatch.setattr(indexes, "GITHUB_API", base[0])
        try:
            async with aiohttp.ClientSession() as session:
                entry = await indexes.publish("geodata", "flat", str(site), session, say=quiet)
                assert entry["url"] == "https://dl/flat.zip"
                assert hashlib.sha256(uploads[0][1]).hexdigest() == entry["sha256"]
                # A second entry of that file name is refused by the release too.
                (site / "index.yaml").write_text("index: ours\nrelease: someone/repo:examples\n"
                                                 "geodata: []\nnodesets: []\n")
                with pytest.raises(store.StoreError, match="already has flat.zip"):
                    await indexes.publish("geodata", "flat", str(site), session, say=quiet)
        finally:
            await runner.cleanup()
    run(go())
    assert len(uploads) == 1 and not list(site.glob(".publish-*"))
