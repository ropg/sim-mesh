# The radio's header for this library's source, and the program linked with
# the radio's shared library by name, which sim-mesh provides the station at
# run time. For the machine's own architecture that is build/libsimradio-sx1262.so,
# from `sim` as it starts. For the other one (SIM_MESH_ARCH, as cross.py reads
# it), the radio is compiled here with that architecture's g++ into
# build.linux-<arch>/, and the program links that copy.
import inspect
import os
import platform
import subprocess

Import("env")

here = os.path.dirname(os.path.realpath(inspect.getframeinfo(inspect.currentframe()).filename))
radio = os.path.dirname(here)
env.Append(CPPPATH=[os.path.join(radio, "include")])

arch = os.environ.get("SIM_MESH_ARCH") or platform.machine()
build = os.path.join(radio, "build")
if arch != platform.machine():
    build = os.path.join(radio, "build.linux-%s" % arch)
    prefix = "%s-linux-gnu-" % arch
    subprocess.run(["cmake", "-S", radio, "-B", build,
                    "-DCMAKE_C_COMPILER=%sgcc" % prefix,
                    "-DCMAKE_CXX_COMPILER=%sg++" % prefix], check=True,
                   stdout=subprocess.DEVNULL)
    subprocess.run(["cmake", "--build", build, "--target", "simradio-sx1262"], check=True,
                   stdout=subprocess.DEVNULL)

program = DefaultEnvironment()
program.Append(LIBPATH=[build], LIBS=["simradio-sx1262", "pthread", "dl"])
