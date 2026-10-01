"""The launcher's container engine, with a stand-in that records what it is
asked: the flags a rootless Podman needs and Docker does not, the port floor a
station's web UI needs under either, and the verbs run by the launcher's own
path whatever directory they are typed in."""

import os
import shutil
import stat
import subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LAUNCHER = os.path.join(ROOT, "sim")

STANDIN = """#!/bin/sh
echo "$@" >> "%(log)s"
case "$1" in
    image|info) exit 0 ;;
    ps) echo abc123 ;;
esac
exit 0
"""


# What the launcher runs besides an engine, linked into the stand-in's own
# directory: the launcher sees that directory and nothing else, so it can never
# find a real docker or podman on the machine.
TOOLS = ("bash", "sh", "env", "cksum", "cut", "grep", "id", "uname", "dirname", "cat", "sed",
         "tr", "head", "readlink", "basename")


def standin(tmp_path, name):
    """An engine called `name` that logs every call, one per line."""
    bin_dir = tmp_path / "bin"
    if not bin_dir.exists():
        bin_dir.mkdir()
        for tool in TOOLS:
            found = shutil.which(tool)
            if found:
                (bin_dir / tool).symlink_to(found)
    log = tmp_path / ("%s.log" % name)
    path = bin_dir / name
    path.write_text(STANDIN.replace("#!/bin/sh", "#!%s" % shutil.which("sh")) % {"log": log})
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return bin_dir, log


def launch(args, bin_dir, cwd, **env):
    environ = dict(os.environ, PATH=str(bin_dir))
    for key in ("SIM_MESH_ENGINE", "SIM_MESH_NATIVE", "SIM_MESH_IN_CONTAINER"):
        environ.pop(key, None)
    environ.update(env)
    return subprocess.run([LAUNCHER, *args], cwd=cwd, env=environ, capture_output=True,
                          text=True, timeout=60)


def calls(log, verb):
    return [line.split() for line in log.read_text().splitlines() if line.split()[:1] == [verb]]


def test_podman_keeps_the_callers_id_and_lets_a_station_have_port_80(tmp_path):
    bin_dir, log = standin(tmp_path, "podman")
    done = launch(["firmware", "list"], bin_dir, tmp_path, SIM_MESH_ENGINE="podman")
    assert done.returncode == 0, done.stderr
    [run] = calls(log, "run")
    assert "--sysctl" in run and "net.ipv4.ip_unprivileged_port_start=0" in run
    assert ("--userns=keep-id" in run) == (os.getuid() != 0)
    assert "--user" not in run
    assert run[-3:] == [LAUNCHER, "firmware", "list"]


def test_a_verb_runs_the_launcher_by_its_path_from_the_directory_above(tmp_path):
    bin_dir, log = standin(tmp_path, "podman")
    above = os.path.dirname(ROOT)
    done = launch(["list"], bin_dir, above, SIM_MESH_ENGINE="podman")
    assert done.returncode == 0, done.stderr
    [run] = calls(log, "exec")
    assert run[run.index("-w") + 1] == above
    assert run[-2:] == [LAUNCHER, "list"]


def test_podman_is_the_default_and_docker_is_left_alone(tmp_path):
    bin_dir, log = standin(tmp_path, "podman")
    standin(tmp_path, "docker")
    done = launch(["firmware", "list"], bin_dir, tmp_path)
    assert done.returncode == 0, done.stderr
    assert calls(log, "run")
    assert not (tmp_path / "docker.log").exists()


def test_docker_is_taken_where_there_is_no_podman_and_gets_no_podman_flags(tmp_path):
    bin_dir, log = standin(tmp_path, "docker")
    done = launch(["firmware", "list"], bin_dir, tmp_path)
    assert done.returncode == 0, done.stderr
    [run] = calls(log, "run")
    assert "--userns=keep-id" not in run and "net.ipv4.ip_unprivileged_port_start=0" in run
    assert run[run.index("--user") + 1] == "%d:%d" % (os.getuid(), os.getgid())


def test_with_neither_engine_it_says_so(tmp_path):
    bin_dir, _ = standin(tmp_path, "unused")
    (bin_dir / "unused").unlink()
    done = launch(["firmware", "list"], bin_dir, tmp_path)
    assert done.returncode != 0 and "neither podman nor docker" in done.stderr
