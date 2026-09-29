"""The old scenario format converted: positions as the old ether's distances,
links, walls, gains, kinds and first-boot lines carried over, the medium's
flags, and files main's own readers take. Every fixture here is invented."""

import io
import math
import os
import re
import shutil
import sys

import pytest
import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import convert_scenario as cs  # noqa: E402
import geodata  # noqa: E402
import losses  # noqa: E402
import nodeset  # noqa: E402
import script  # noqa: E402
import store  # noqa: E402
from simesh import library  # noqa: E402
from simesh.select import Nodes  # noqa: E402

R = cs.EARTH_RADIUS_M
# The latitude where a degree of longitude is half a degree of latitude.
HALF = math.degrees(math.acos(0.5))
DEVICES = {"reticulous": "reticulous_dev_latest", "berlinmesh": "sergeyculum_local_latest"}


def at(origin, east_m, north_m):
    """[lat, lon] this far east and north of `origin` on the old ether's
    plane: its projection, inverted."""
    lat0, lon0 = origin
    return [lat0 + math.degrees(north_m / R),
            lon0 + math.degrees(east_m / (R * math.cos(math.radians(lat0))))]


def old(nodes, origin=(0.0, 0.0), **extra):
    """An old scenario's mapping, its nodes {name: {id, pos in metres, …}}
    placed on the old ether's plane about `origin`."""
    out = {"origin": list(origin), "physics": {"exponent": 2.7, "noise_figure_db": 6,
                                               "capture_db": 6},
           "setup": [], "nodes": {}}
    for name, node in nodes.items():
        node = dict(node)
        node["pos"] = at(origin, *node["pos"])
        out["nodes"][name] = node
    out.update(extra)
    return out


def xy_of(conv):
    """{node: (x, y)} as main reads the converted nodeset: synthetic metres."""
    written = nodeset.parse(yaml.safe_load(conv["nodeset"]), "converted")
    return {n: geodata.synthetic_xy(node["lat"], node["lon"])
            for n, node in written["nodes"].items()}, written


def dist(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


# A mixed scenario: a gateway and a relay of the default kind (reticulous),
# two households of the rncfg kind, a wall, gains, links, and first-boot
# lines in both languages.
MIXED = old({
    "gw": {"id": 1, "pos": (0, 0), "gain_db": 3},
    "r1": {"id": 2, "pos": (900, 0),
           "setup": ["set s.rnsd.transport_enabled 1", "set s.lora.0.tx_power 17"]},
    "h1": {"id": 5, "kind": "berlinmesh", "pos": (900, 1200), "gain_db": -10,
           "setup": ["transport on"]},
    "h2": {"id": 7, "kind": "berlinmesh", "pos": (-400, 300), "gain_db": -8,
           "setup": ["announce interval set 900"]},
}, physics={"exponent": 3.1, "noise_figure_db": 7, "capture_db": 6, "shadowing_db": 5,
            "shadowing_seed": 3, "capture_model": "bench", "sf_orthogonality": "croce",
            "crc_band_db": 1.5},
    kinds={"reticulous": {"elf": "../x/reticulous.elf"},
           "berlinmesh": {"elf": "../y/simesh",
                          "setup": ["name set {name}",
                                    "set --freq-hz 869525000 --sf 8 --bw-hz 125000 --cr 5 "
                                    "--txpower-dbm 20"]}},
    setup=["hostname {name}", "lora up", "lora 0 freq 869.525", "lora 0 sf 8", "lora 0 bw 125",
           "lora 0 txp 14", "lxmf create {name}"],
    obstructions=[{"between": ["gw", "h1"], "db": 10}],
    links=[{"between": ["gw", "r1"], "loss_db": 101.5}, {"between": ["r1", "h2"], "loss_db": 120},
           {"between": ["r1", "gw"], "loss_db": 99}, {"between": ["gw", "ghost"], "loss_db": 80}])


def test_positions_keep_every_distance_the_old_ether_had():
    # A 3-4-5 triangle and a fourth node, where a degree of longitude is half
    # a degree of latitude on the old plane.
    data = old({"a": {"id": 1, "pos": (0, 0)}, "b": {"id": 2, "pos": (3000, 0)},
                "c": {"id": 3, "pos": (0, 4000)},
                "d": {"id": 4, "pos": (-1200, 2500), "height_m": 15}},
               origin=(HALF, 0.0))
    conv = cs.convert(data, DEVICES, "tri")
    xy, written = xy_of(conv)
    # A height the file states is kept; the others stand at main's default.
    assert [written["nodes"][n]["height_m"] for n in "abcd"] == [2, 2, 2, 15]
    assert abs(dist(xy["a"], xy["b"]) - 3000) < 1e-3
    assert abs(dist(xy["a"], xy["c"]) - 4000) < 1e-3
    assert abs(dist(xy["b"], xy["c"]) - 5000) < 1e-3
    assert abs(dist(xy["a"], xy["d"]) - math.hypot(1200, 2500)) < 1e-3
    # Centred on the centroid, on synthetic ground's square.
    assert abs(sum(x for x, _ in xy.values())) < 1e-3
    assert abs(sum(y for _, y in xy.values())) < 1e-3
    check = conv["check"]
    assert check["ok"] and check["pairs"] == 6 and check["worst_m"] < cs.CHECK_TOLERANCE_M
    # A few kilometres about the origin, the old plane is the great circle's to
    # a small fraction of a percent: no warning.
    assert check["great_circle_worst"] < 1e-3
    assert not any("great circle" in w for w in conv["warnings"])
    # A node a centimetre off is caught.
    _, written = xy_of(conv)
    written["nodes"]["d"]["lat"] += 0.01 / geodata.M_PER_DEGREE
    old_xy = {n: cs.old_metres(data["origin"], *node["pos"]) for n, node in data["nodes"].items()}
    moved = cs.check_distances(cs.read_old(data, "tri"), old_xy, written["nodes"])
    assert not moved["ok"] and abs(moved["worst_m"] - 0.01) < 2e-3 and "d" in moved["worst_pair"]


def test_the_old_ethers_plane_is_kept_where_it_was_wrong():
    # A file that never set its origin put its nodes on a plane scaled for
    # the equator, where the ground's degree of longitude is half as long:
    # every east-west distance the old runs had was twice the ground's. The
    # conversion keeps the old runs' distances.
    data = old({"a": {"id": 1, "pos": (0, 0)}, "b": {"id": 2, "pos": (0, 0)}})
    data["nodes"]["a"]["pos"] = [HALF, 0.0]
    data["nodes"]["b"]["pos"] = [HALF, math.degrees(1000 / R)]
    conv = cs.convert(data, DEVICES, "wide")
    xy, _ = xy_of(conv)
    assert abs(dist(xy["a"], xy["b"]) - 1000) < 1e-3          # the old plane's
    true = cs.great_circle_m(data["nodes"]["a"]["pos"], data["nodes"]["b"]["pos"])
    assert abs(true - 500) < 0.1                                # the ground's
    assert any("off the great circle" in w for w in conv["warnings"])


def test_links_walls_gains_kinds_and_first_boot_lines_carry_over():
    conv = cs.convert(MIXED, {"reticulous": "reticulous_dev_latest",
                              "berlinmesh": "sergeyculum_local_latest"}, "mixed")
    _, written = xy_of(conv)
    nodes = written["nodes"]
    assert {n: node["id"] for n, node in nodes.items()} == {"gw": 1, "r1": 2, "h1": 5, "h2": 7}
    for node in nodes.values():
        assert node["antenna"] == {"type": "rubber_duck"}
        assert node["height_m"] == 2.0 and node["height_from"] == "assumed"
    # Each node's kind as a tag, and the role its lines gave it.
    assert {n: node["tags"] for n, node in nodes.items()} == {
        "gw": ["reticulous"], "r1": ["reticulous", "transport"],
        "h1": ["berlinmesh", "transport"], "h2": ["berlinmesh"]}
    # The transmit power: the scenario's line for its default kind, a node's
    # own line after it, the rncfg kind's line.
    assert {n: node.get("max_dbm") for n, node in nodes.items()} == {
        "gw": 14, "r1": 17, "h1": 20, "h2": 20}
    # A pair's wall and its ends' gains in one offset: what the old ether
    # added to the pair's loss.
    offsets = {frozenset(o["between"]): o["db"] for o in written["offsets"]}
    assert offsets == {frozenset(("gw", "h1")): 10 - 3 + 10, frozenset(("gw", "h2")): -3 + 8,
                       frozenset(("gw", "r1")): -3, frozenset(("h1", "h2")): 18,
                       frozenset(("h1", "r1")): 10, frozenset(("h2", "r1")): 8}
    # Links as stated: a pair once, its last loss, the one naming no node left out.
    raw = yaml.safe_load(conv["nodeset"])
    assert [(sorted(l["between"]), l["loss_db"]) for l in raw["links"]] == [
        (["gw", "r1"], 99), (["h2", "r1"], 120)]
    assert all(l["note"] == cs.LINK_NOTE for l in raw["links"])
    assert any("restate a pair" in w for w in conv["warnings"])
    assert any("name a node the file lacks" in w for w in conv["warnings"])
    # The devices by tag, and the lines nothing else says, to the same nodes.
    setup = conv["setup"]
    assert 'firmware(nodes(tag="reticulous"), "reticulous_dev_latest")' in setup
    assert 'firmware(nodes(tag="berlinmesh"), "sergeyculum_local_latest")' in setup
    assert 'on_first_boot(nodes(tag="reticulous"), """\n    lxmf create {name}\n""")' in setup
    assert 'on_first_boot("h2", """\n    announce interval set 900\n""")' in setup
    for said in ("hostname", "lora", "name set", "transport", "set s.", "set --"):
        assert "\n    " + said not in setup
    # The ground and the medium.
    ground = yaml.safe_load(conv["geodata"])
    assert ground["synthetic"]["exponent"] == 3.1
    assert (ground["shadowing_db"], ground["shadowing_seed"]) == (5, 3)
    assert conv["medium"] == "--noise-figure 7 --bench-capture --crc-margin-db 1.5\n"
    assert any("globals.py in freq_mhz, sf" in w for w in conv["warnings"])
    assert not any("differ among the nodes" in w for w in conv["warnings"])
    s = conv["summary"]
    assert (s["nodes"], s["links"], s["walls"], s["offsets"], s["transport"]) == (4, 2, 1, 6, 2)


def test_the_setup_puts_each_node_on_its_kinds_device_after_a_scripts_own(runtime, monkeypatch,
                                                                         tmp_path):
    conv = cs.convert(MIXED, DEVICES, "mixed")
    shutil.copytree(store.SCRIPTS_DIR, str(tmp_path / "scripts"))
    (tmp_path / "nodesets").mkdir()
    (tmp_path / "nodesets" / "mixed.py").write_text(conv["setup"])
    monkeypatch.setattr(library, "TESTBED_DIR", str(tmp_path))
    monkeypatch.syspath_prepend(store.SCRIPTS_DIR)
    runtime.configure(geodata="plain", nodesets=["mixed"])
    # A script's declarations, as realtime.py makes them.
    library.firmware("all", "reticulous_stable_latest")
    library.include("scripts/startup.py")
    _, written = xy_of(conv)
    ran = {}
    for name, node in written["nodes"].items():
        facts = {"name": name, "tags": node["tags"]}
        ran[name] = [rule["firmware"] for rule in runtime.firmware_rules
                     if Nodes(rule["which"]).matches(facts)][-1]
    assert ran == {"gw": DEVICES["reticulous"], "r1": DEVICES["reticulous"],
                   "h1": DEVICES["berlinmesh"], "h2": DEVICES["berlinmesh"]}
    # What each is told at its first boot: startup.py's role (from the tag)
    # and radio, the old lines nothing else says, then the radio started.
    told = {}
    for name, node in written["nodes"].items():
        facts = {"name": name, "tags": node["tags"]}
        told[name] = [line["intent"] if isinstance(line, dict) else line
                      for rule in runtime.first_boot_rules if Nodes(rule["which"]).matches(facts)
                      for line in rule["lines"]]
    assert told == {"gw": ["radio", "lxmf create {name}", "radio_up"],
                    "r1": ["role", "radio", "lxmf create {name}", "radio_up"],
                    "h1": ["role", "radio", "radio_up"],
                    "h2": ["radio", "announce interval set 900", "radio_up"]}


def test_a_kind_the_mapping_lacks_is_refused_and_so_is_a_device_that_is_no_name(tmp_path, capsys):
    with pytest.raises(cs.ConvertError, match="no device for kind berlinmesh.*2 node"):
        cs.convert(MIXED, {"reticulous": "reticulous_dev_latest"}, "mixed")
    with pytest.raises(cs.ConvertError, match="a device is"):
        cs.convert(MIXED, dict(DEVICES, berlinmesh="some build"), "mixed")
    # A name that is a device's but no build here yet is converted, and said.
    assert cs.check_device("nosuch_local_latest") == (
        None, "device nosuch_local_latest is not here yet (no devices/local/nosuch_local.yaml)")
    path = tmp_path / "mixed.yaml"
    # The old default kind is the first the file names: keep its order.
    path.write_text(yaml.safe_dump(MIXED, sort_keys=False))
    assert cs.main([str(path), "--out", str(tmp_path / "out"),
                    "--device", "reticulous=reticulous_dev_latest"]) == 1
    assert "no device for kind berlinmesh" in capsys.readouterr().err
    assert not (tmp_path / "out").exists()


def test_main_reads_what_is_written_and_prices_every_pair_as_the_old_ether(tmp_path, monkeypatch):
    path = tmp_path / "mixed.yaml"
    # The old default kind is the first the file names: keep its order.
    path.write_text(yaml.safe_dump(MIXED, sort_keys=False))
    out = tmp_path / "out"
    assert cs.main([str(path), "--out", str(out), "--device", "reticulous=reticulous_dev_latest",
                    "--device", "berlinmesh=sergeyculum_local_latest"]) == 0
    # Main's own readers take the files as they are: the links and the
    # shadowing are keys they leave to the layers that read them.
    ns = nodeset.open_path(str(out / "nodesets" / "mixed.yaml"), "mixed")
    gd = geodata.read(str(out / "geodata" / "mixed.yaml"))
    assert sorted(ns.nodes) == ["gw", "h1", "h2", "r1"] and not gd.is_pack
    assert ns.node("gw")["tags"] == ["reticulous"] and ns.node("h2")["tags"] == ["berlinmesh"]
    assert all(gd.holds(node["lat"], node["lon"]) for node in ns.nodes.values())
    raw = yaml.safe_load((out / "nodesets" / "mixed.yaml").read_text())
    assert len(raw["links"]) == 2 and "shadowing_db" in yaml.safe_load(
        (out / "geodata" / "mixed.yaml").read_text())
    script.parse((out / "nodesets" / "mixed.py").read_text(), "mixed.py")
    assert (out / "medium.txt").read_text().startswith("--noise-figure 7 --bench-capture")
    assert "\nWarnings: " in (out / "conversion.txt").read_text()
    # Every pair without a stated link, priced by main's table with the
    # antennas and offsets on it, is what the old ether priced it at:
    # FSPL(1 m) + 10·n·log10(d) + wall - G_a - G_b.
    table = losses.medium_tables({"868": losses.synthetic_table(gd, ns, "868")}, gd, ns)["868"]
    linked = {frozenset(l["between"]) for l in raw["links"]}
    walls = {frozenset(w["between"]): w["db"] for w in MIXED["obstructions"]}
    xy = {n: cs.old_metres(MIXED["origin"], *node["pos"]) for n, node in MIXED["nodes"].items()}
    freq = 869.525e6
    priced = 0
    for a in ns.nodes:
        for b in ns.nodes:
            if a == b or frozenset((a, b)) in linked:
                continue
            ga, gb = (MIXED["nodes"][n].get("gain_db", 0) for n in (a, b))
            before = (20 * math.log10(4 * math.pi * freq / 299_792_458.0)
                      + 10 * 3.1 * math.log10(dist(xy[a], xy[b]))
                      + walls.get(frozenset((a, b)), 0) - ga - gb)
            assert abs(table.at(a, b, freq) - before) < 1e-3, (a, b)
            priced += 1
    assert priced == 8
    # Written once; again only with --force.
    assert cs.main([str(path), "--out", str(out), "--device", "reticulous=reticulous_dev_latest",
                    "--device", "berlinmesh=sergeyculum_local_latest"]) == 1
    monkeypatch.setattr(sys, "stdin", io.StringIO(path.read_text()))
    assert cs.main(["-", "--name", "mixed", "--force", "--out", str(out),
                    "--device", "reticulous=reticulous_dev_latest",
                    "--device", "berlinmesh=sergeyculum_local_latest"]) == 0


def test_the_medium_flags_say_what_the_old_physics_did():
    def flags(**physics):
        warnings = []
        got = cs.medium_flags(dict(cs.OLD_PHYSICS, **physics), [], warnings)
        return " ".join(got), warnings

    # The old default: a frame survives each audible interferer by the
    # capture margin, whatever its spreading factor.
    assert flags() == ("--noise-figure 6 --pairwise", [])
    got, warnings = flags(capture_db=8, sf_orthogonality="croce", crc_band_db=2)
    assert got == "--noise-figure 6 --pairwise --crc-margin-db 2"
    assert [re.split(r"[ :]", w)[0] for w in warnings] == ["capture_db", "sf_orthogonality"]
    got, warnings = flags(capture_model="bench", noise_figure_db=4.5)
    assert got == "--noise-figure 4.5 --bench-capture"
    assert [re.split(r"[ :]", w)[0] for w in warnings] == ["sf_orthogonality", "capture_model"]
    with pytest.raises(cs.ConvertError, match="capture_model"):
        cs.convert(old({"a": {"id": 1, "pos": (0, 0)}}, physics={"capture_model": "coin"}),
                   DEVICES, "x")


def test_first_boot_lines_in_each_old_kinds_language():
    rl, rn = cs.reticulous_line, cs.rncfg_line
    assert rl("hostname {name}") == [("said", "name")] and rl("lora up") == [("said", "radio_up")]
    assert rl("lora 0 sync 0x12") == [("radio", {"sync": 0x12})]
    assert rl("lora 0 freq 868.1") == [("radio", {"freq_mhz": 868.1})]
    assert rl("set s.rnsd.transport_enabled 0") == [("role", "client")]
    assert rl("lora 1 txp 10") == [("other", "lora 1 txp 10")]      # another slot
    assert rl("lora 0 sf eight") == [("other", "lora 0 sf eight")]
    assert rn("transport off") == [("role", "client")]
    assert rn("set --sf 9 --bw-hz 250000 --beacon 1") == [
        ("radio", {"sf": 9, "bw_khz": 250.0}), ("other", "set --beacon 1")]
    # Two kinds on two spreading factors: main has one radio for every node.
    data = dict(MIXED, kinds=dict(MIXED["kinds"], berlinmesh={"setup": ["set --sf 7"]}))
    warnings = cs.convert(data, DEVICES, "x")["warnings"]
    assert any(w.startswith("the old radios differ among the nodes in sf;") for w in warnings)
    # A kind of a type the conversion has no language for keeps every line.
    data = old({"a": {"id": 1, "pos": (0, 0), "setup": ["transport on"]}},
               kinds={"other": {"type": "somefw"}})
    conv = cs.convert(data, {"other": "reticulous_dev_latest"}, "x")
    assert 'on_first_boot("a", """\n    transport on\n""")' in conv["setup"]
    assert "transport" not in yaml.safe_load(conv["nodeset"])["nodes"]["a"]["tags"]


@pytest.fixture
def runtime(monkeypatch):
    """A fresh library runtime, with no simulation to reach."""
    fresh = library.Runtime()
    monkeypatch.setattr(library, "runtime", fresh)
    monkeypatch.delenv("SIMESH_SIM", raising=False)
    return fresh
