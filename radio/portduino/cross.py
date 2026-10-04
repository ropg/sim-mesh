# A post: build script for a Portduino firmware built for the other of
# aarch64 / x86_64. SIM_MESH_ARCH in the environment names the architecture;
# unset, or the machine's own, it does nothing. Otherwise the program is
# compiled, archived and linked with that architecture's GNU tools
# (`<arch>-linux-gnu-gcc` and the rest, Ubuntu's g++-<arch>-linux-gnu), against
# its libc from the multiarch packages. It is a post: script because
# platform-native picks the host's gcc as its main script runs; the commands
# take the tools by name when they run, so replacing them here still holds.
# By then the project's sources and every library have environments of their
# own, cloned from the global one, so each gets the tools too.
# link.py reads the same variable and links a radio built for that
# architecture.
import os
import platform

Import("env", "projenv")

arch = os.environ.get("SIM_MESH_ARCH") or platform.machine()
if arch != platform.machine():
    prefix = "%s-linux-gnu-" % arch
    tools = dict(CC=prefix + "gcc", CXX=prefix + "g++", AR=prefix + "ar",
                 RANLIB=prefix + "ranlib", OBJCOPY=prefix + "objcopy")
    for each in [env, projenv] + [lb.env for lb in env.GetLibBuilders()]:
        each.Replace(**tools)
