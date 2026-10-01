"""Stand-in firmware for the tests: a station that is a shell script, and a
`reticulum` driver that writes down every line and verb it is given in the
station's `lines` file."""

import io
import os
import stat
import zipfile

import yaml

import firmware as firmware_module

DRIVER = '''
import os
from sim_mesh.reticulum.driver import CommandError, ReticulumDriver


def note(station, line):
    with open(os.path.join(station.dir, "lines"), "a") as out:
        out.write(line + "\\n")
    return "did %s" % line


class Stub(ReticulumDriver):
    async def wait_up(self, station, timeout):
        return True

    async def run(self, station, line, timeout=None):
        return note(station, line)

    def configured(self, station):
        return os.path.exists(os.path.join(station.dir, "lines"))

    async def name(self, station, name):
        note(station, "name %s %d" % (name, station.node_id))

    async def radio(self, station, **figures):
        note(station, "radio " + " ".join("%s=%s" % kv for kv in sorted(figures.items())))

    async def radio_up(self, station):
        note(station, "radio up")

    async def role(self, station, role):
        note(station, "role %s" % role)

    async def lxmf_announce(self, station, name=None):
        return note(station, "announce")

    async def lxmf_create(self, station, name):
        note(station, "lxmf create %s" % name)
        named = os.path.join(station.dir, "identities")
        held = [line.split() for line in open(named)] if os.path.exists(named) else []
        if not any(each == name for each, _ in held):
            addr = "%030x%02x" % (station.node_id, len(held))
            with open(named, "a") as out:
                out.write("%s %s\\n" % (name, addr))
            return addr
        return dict(held)[name]

    async def lxmf_identities(self, station):
        named = os.path.join(station.dir, "identities")
        held = [tuple(line.split()) for line in open(named)] if os.path.exists(named) else []
        return held or [(station.name, "%032x" % station.node_id)]

    async def lxmf_send(self, station, dest, text, mid, sender=None):
        note(station, "send %s %s %s" % (sender or "-", dest, text))
        # Its own id, said by the station before the send returns, as a
        # firmware's log can: held until coupled, then reported under mid.
        self.lxmf_native(station, "n-" + text, "delivered")
        self.lxmf_couple(station, mid, "n-" + text)

    async def current_role(self, station):
        return "client"


DRIVER = Stub
'''

STATION = "#!/bin/sh\nexec sleep 60\n"


def node_yaml(base, version, arch=None, **extra):
    return dict({"base": base, "arch": arch or firmware_module.machine_arch(),
                 "version": version, "category": "reticulum", "exec": "station.sh",
                 "driver": "driver.py"}, **extra)


def zip_bytes(base, version, arch=None, driver=DRIVER, **extra):
    """A firmware zip, as bytes."""
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as zf:
        zf.writestr("node.yaml", yaml.safe_dump(node_yaml(base, version, arch, **extra)))
        info = zipfile.ZipInfo("station.sh")
        info.external_attr = (0o755 | stat.S_IFREG) << 16
        zf.writestr(info, STATION)
        zf.writestr("driver.py", driver)
    return out.getvalue()


def install(firmware_dir, base, version, driver=DRIVER, **extra):
    """A stand-in firmware installed straight into `firmware_dir`: its name."""
    name = "%s_%s_%s" % (base, firmware_module.machine_arch(), version)
    where = os.path.join(str(firmware_dir), name)
    os.makedirs(where)
    with open(os.path.join(where, "node.yaml"), "w") as f:
        yaml.safe_dump(node_yaml(base, version, **extra), f)
    with open(os.path.join(where, "station.sh"), "w") as f:
        f.write(STATION)
    os.chmod(os.path.join(where, "station.sh"), 0o755)
    with open(os.path.join(where, "driver.py"), "w") as f:
        f.write(driver)
    return name
