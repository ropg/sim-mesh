"""Real time: every node on the chosen firmware, set up by the startup script, and left running.

The simulation runs on the wall clock and keeps running when this script
ends, for the page, the consoles and the stations' web UIs.
"""
from sim_mesh import *

firmware = script_input("firmware", type=Firmware,
                        label="Firmware for nodes not otherwise configured")

sim_speed("real")
nodes().firmware(firmware)
script_include("scripts/startup.py")

nodes().up()
