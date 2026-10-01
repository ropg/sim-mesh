# The radio's header for this library's source, and the program linked with
# the radio's shared library by name (build/libsimradio-sx1262.so, from
# `sim` as it starts), which sim-mesh provides the station at run time.
import inspect
import os

Import("env")

here = os.path.dirname(os.path.realpath(inspect.getframeinfo(inspect.currentframe()).filename))
radio = os.path.dirname(here)
env.Append(CPPPATH=[os.path.join(radio, "include")])

program = DefaultEnvironment()
program.Append(LIBPATH=[os.path.join(radio, "build")],
               LIBS=["simradio-sx1262", "pthread", "dl"])
