"""`simesh install` puts on a Linux machine what SIMesh's image holds: the
same system packages, the same Node major, the same Reticulum and LXMF. The
two are written separately, the launcher's lists and the Dockerfile's
RUN lines, so this keeps them from drifting apart."""

import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def read(name):
    with open(os.path.join(ROOT, name), encoding="utf-8") as handle:
        return handle.read()


def launcher_list(name):
    """A bash array from the launcher, `NAME=(a b …)`, over several lines."""
    got = re.search(r"^%s=\(([^)]*)\)" % name, read("simesh"), re.M)
    assert got, "no %s in simesh" % name
    return got.group(1).split()


def dockerfile_apt():
    """Every package the Dockerfile's apt-get installs, nodejs aside (it
    comes from NodeSource, which the launcher adds when the system's is too
    old)."""
    packages = set()
    for block in re.findall(r"apt-get install -y --no-install-recommends(.*?);", read("Dockerfile"),
                            re.S):
        packages |= {word for word in block.replace("\\", " ").split() if not word.startswith("-")}
    return packages - {"nodejs"}


def test_the_launcher_installs_every_package_the_image_has():
    apt = set(launcher_list("APT_PACKAGES"))
    assert dockerfile_apt() <= apt, dockerfile_apt() - apt
    # And a venv to put Reticulum and LXMF in, which the image does without.
    assert "python3-venv" in apt


def test_the_launcher_pins_what_the_image_pins():
    pins = re.search(r"pip3 install [^;]*?((?:\S+==\S+\s*)+);", read("Dockerfile"))
    assert pins, "no pinned pip3 install in the Dockerfile"
    assert launcher_list("PYPI_PINS") == pins.group(1).split()


def test_the_launcher_takes_node_from_where_the_image_does():
    major = re.search(r"deb\.nodesource\.com/node_(\d+)\.x", read("Dockerfile")).group(1)
    assert re.search(r"^NODE_MAJOR=%s\b" % major, read("simesh"), re.M)


def test_a_fedora_install_names_the_same_things():
    # Its own names, one for each of the image's (build-essential is gcc, g++
    # and make; gnupg is only for NodeSource's apt key).
    dnf = set(launcher_list("DNF_PACKAGES"))
    for apt_name, dnf_names in {"python3-yaml": ["python3-pyyaml"],
                                "build-essential": ["gcc", "gcc-c++", "make"],
                                "pkg-config": ["pkgconf-pkg-config"],
                                "libstdc++6": ["libstdc++"], "zlib1g": ["zlib"],
                                "libbsd0": ["libbsd"], "procps": ["procps-ng"]}.items():
        assert apt_name in dockerfile_apt() and set(dnf_names) <= dnf, apt_name
    for same in ("ca-certificates", "curl", "git", "python3", "python3-aiohttp", "python3-pytest",
                 "python3-pip", "cmake"):
        assert same in dnf, same
