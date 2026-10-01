"""Firmware: names and `_latest`, adding a zip, what is refused, deleting
with a paused run or a snapshot holding it, the pre-built index, the CLI."""

import io
import os
import sys
import zipfile

import pytest
import yaml

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import firmware  # noqa: E402
import stub_firmware  # noqa: E402

ARCH = firmware.machine_arch()


@pytest.fixture
def where(tmp_path, monkeypatch):
    for attr, sub in (("FIRMWARE_DIR", "firmware"), ("RUNS_DIR", "runs"),
                      ("SNAPSHOTS_DIR", "snapshots")):
        (tmp_path / sub).mkdir()
        monkeypatch.setattr(firmware, attr, str(tmp_path / sub))
    return tmp_path


def zip_file(tmp_path, base, version, arch=ARCH, filename=None, **extra):
    path = tmp_path / (filename or "%s_%s_%s.zip" % (base, arch, version))
    path.write_bytes(stub_firmware.zip_bytes(base, version, arch, **extra))
    return str(path)


def add(path):
    import asyncio
    return asyncio.run(firmware.add(path))


def test_names_are_base_arch_and_version():
    assert firmware.parse_name("reticulous-sx1262_aarch64_20260930163128") == \
        ("reticulous-sx1262", "aarch64", "20260930163128")
    assert firmware.parse_name("relay_x86_64_1.2.3-rc.1") == ("relay", "x86_64", "1.2.3-rc.1")
    for bad in ("relay_x86_64", "Relay_x86_64_1.0.0", "relay_x86_64_2026", "relay__1.0.0",
                "relay_x86_64_latest"):
        assert firmware.parse_name(bad) is None, bad
    assert firmware.latest_base("reticulous-sx1262_latest") == "reticulous-sx1262"
    assert firmware.latest_base("relay_x86_64_1.0.0") is None
    order = sorted(["1.10.0", "1.2.0", "1.10.0-rc.1", "0.9.9"], key=firmware.version_key)
    assert order == ["0.9.9", "1.2.0", "1.10.0-rc.1", "1.10.0"]


def test_latest_is_the_newest_of_exactly_that_base(where):
    fw = where / "firmware"
    stub_firmware.install(fw, "relay", "20260101000000")
    stub_firmware.install(fw, "relay", "20260301000000")
    stub_firmware.install(fw, "relay-big", "20270101000000")
    got = firmware.resolve("relay_latest")
    assert got["name"] == "relay_%s_20260301000000" % ARCH and got["category"] == "reticulum"
    assert got["exec"].endswith("station.sh") and got["driver"].endswith("driver.py")
    assert firmware.resolve("relay-big_latest")["version"] == "20270101000000"
    assert firmware.resolve("relay_%s_20260101000000" % ARCH)["version"] == "20260101000000"
    with pytest.raises(firmware.FirmwareError, match="no relay-small firmware"):
        firmware.resolve("relay-small_latest")
    with pytest.raises(firmware.FirmwareError, match="not installed"):
        firmware.resolve("relay_%s_20990101000000" % ARCH)


def test_a_zip_is_installed_under_its_name_whatever_the_file_is_called(where):
    got = add(zip_file(where, "relay-sx1262", "1.0.0", filename="download.zip",
                       title="Relay", radio="sx1262"))
    name = "relay-sx1262_%s_1.0.0" % ARCH
    assert got["name"] == name and got["title"] == "Relay" and got["radio"] == "sx1262"
    assert os.access(got["exec"], os.X_OK)
    assert firmware.read_origin(got["dir"])["source"].endswith("download.zip")
    with pytest.raises(firmware.FirmwareError, match="installed already"):
        add(zip_file(where, "relay-sx1262", "1.0.0", filename="again.zip"))
    assert [r["name"] for r in firmware.listing()] == [name]
    assert not [e for e in os.listdir(where / "firmware") if e.startswith(".")]


def test_what_a_zip_is_refused_for(where):
    other = "aarch64" if ARCH != "aarch64" else "x86_64"
    with pytest.raises(firmware.FirmwareError, match="built for %s" % other):
        add(zip_file(where, "relay", "1.0.0", arch=other))
    add(zip_file(where, "relay", "1.0.0"))
    with pytest.raises(firmware.FirmwareError, match="versioned by stamp.*by semver"):
        add(zip_file(where, "relay", "20260101000000"))
    with pytest.raises(firmware.FirmwareError, match="category 'chat'"):
        add(zip_file(where, "chatter", "1.0.0", category="chat"))
    bad = where / "bad.zip"
    with zipfile.ZipFile(bad, "w") as zf:
        zf.writestr("node.yaml", yaml.safe_dump(stub_firmware.node_yaml("evil", "1.0.0")))
        zf.writestr("../escape", "x")
    with pytest.raises(firmware.FirmwareError, match="leaves the firmware"):
        add(str(bad))
    empty = where / "empty.zip"
    with zipfile.ZipFile(empty, "w") as zf:
        zf.writestr("readme", "x")
    with pytest.raises(firmware.FirmwareError, match="no node.yaml"):
        add(str(empty))
    assert sorted(os.listdir(where / "firmware")) == ["relay_%s_1.0.0" % ARCH]


def test_firmware_a_paused_run_or_a_snapshot_holds_is_not_deleted(where):
    fw = where / "firmware"
    old = stub_firmware.install(fw, "relay", "1.0.0")
    new = stub_firmware.install(fw, "relay", "1.1.0")
    spare = stub_firmware.install(fw, "other", "1.0.0")
    run = where / "runs" / "lora"
    (run / "paused").mkdir(parents=True)
    (run / "paused" / "snapshot.yaml").write_text("{}\n")
    (run / "run.yaml").write_text(yaml.safe_dump(
        {"paused": {"t": 5}, "builds": {"relay_latest": {"firmware": old}}}))
    snap = where / "snapshots" / "warm"
    snap.mkdir()
    (snap / "snapshot.yaml").write_text(yaml.safe_dump(
        {"builds": {"relay_latest": {"firmware": new}}}))
    ended = where / "runs" / "done"
    ended.mkdir()
    (ended / "run.yaml").write_text(yaml.safe_dump({"builds": {"x": {"firmware": spare}}}))
    assert firmware.holders() == {old: ["run lora"], new: ["snapshot warm"]}
    names, held = firmware.to_delete("relay")
    assert names == [] and held == {old: ["run lora"], new: ["snapshot warm"]}
    with pytest.raises(firmware.FirmwareError, match="held by run lora"):
        firmware.delete([old])
    assert firmware.to_delete("other") == ([spare], {})
    assert firmware.delete([spare]) == [spare]
    assert sorted(firmware.installed()) == [old, new]


def test_the_cli_lists_and_deletes_with_force(where, capsys):
    fw = where / "firmware"
    stub_firmware.install(fw, "relay", "1.0.0", title="Relay")
    assert firmware.main(["list"]) == 0
    assert "Relay" in capsys.readouterr().out
    assert firmware.main(["delete", "relay"]) == 1          # nothing can confirm here
    assert firmware.installed()
    assert firmware.main(["delete", "-f", "relay"]) == 0
    assert firmware.installed() == {}


def test_the_prebuilt_index_lists_this_machines_zips_with_their_facts():
    other = "aarch64" if ARCH != "aarch64" else "x86_64"
    text = """<html><body><ul>
      <li><a href="relay-sx1262_%s_20260101000000.zip" data-category="reticulum"
             data-radio="sx1262" data-title="Relay">relay</a></li>
      <li><a href="relay-sx1262_%s_20260201000000.zip">relay, newer</a></li>
      <li><a href="relay-sx1262_%s_20260201000000.zip">another machine's</a></li>
      <li><a href="../index.html">up</a></li>
    </ul></body></html>""" % (ARCH, ARCH, other)
    rows = firmware.parse_index(text, "https://sim-mesh.net/firmware/")
    assert [r["version"] for r in rows] == ["20260201000000", "20260101000000"]
    assert rows[1]["title"] == "Relay" and rows[1]["radio"] == "sx1262"
    assert rows[0]["url"] == "https://sim-mesh.net/firmware/relay-sx1262_%s_20260201000000.zip" \
        % ARCH
    assert "category" not in rows[0]


def test_a_zip_can_be_made_in_memory_for_the_page():
    data = stub_firmware.zip_bytes("relay", "1.0.0")
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        assert yaml.safe_load(zf.read("node.yaml"))["base"] == "relay"
