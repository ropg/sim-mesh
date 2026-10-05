"""The Portduino idle (radio/portduino/idle.cpp), in a stand-in of its own.

idle_standin.cpp is compiled with idle.cpp by the host's g++, once with the
link wraps a firmware that watches its sockets is linked with, and once
without any, as the firmwares that do not are; each runs on the wall clock,
without the shim, and prints a line a check.
"""

import os
import shutil
import subprocess

import pytest

from test_model import BUILD, RADIO

WRAPS = ["-Wl,--wrap=bind", "-Wl,--wrap=listen", "-Wl,--wrap=accept", "-Wl,--wrap=close"]
SOURCES = [os.path.join(RADIO, "tests", "idle_standin.cpp"),
           os.path.join(RADIO, "portduino", "idle.cpp")]


def build(name, flags):
    out = os.path.join(BUILD, name)
    if (not os.path.exists(out)
            or any(os.path.getmtime(out) < os.path.getmtime(src) for src in SOURCES)):
        cxx = shutil.which("g++") or pytest.fail("no g++ to build the idle stand-in")
        os.makedirs(BUILD, exist_ok=True)
        subprocess.run([cxx, "-O1", *SOURCES, *flags, "-lpthread", "-o", out], check=True)
    return out


def run(program, *args):
    env = dict(os.environ, SIM_MESH_BIND_ADDR="127.0.0.2")
    for key in ("LD_PRELOAD", "SIM_MESH_TIME"):
        env.pop(key, None)
    done = subprocess.run([program, *args], env=env, capture_output=True, text=True, timeout=30)
    return [line.split(" ", 2) for line in done.stdout.splitlines()]


def test_the_idle_ends_on_a_watched_socket_a_wake_or_its_time():
    lines = run(build("idle_standin", WRAPS))
    failed = [" ".join(line[1:]) for line in lines if line[0] != "ok"]
    assert not failed
    assert [line[1] for line in lines] == [
        "zero", "timeout", "wake", "bind", "listener", "readable", "radio", "wake-watched",
        "hangup", "hangup-once", "close", "closed-listener"]


def test_a_firmware_linked_without_the_wraps_links_and_idles_as_before():
    lines = run(build("idle_standin_nowrap", []), "nowrap")
    assert [line[0] for line in lines] == ["ok"] * 4, lines
    assert [line[1] for line in lines] == ["zero", "timeout", "wake", "bind"]
