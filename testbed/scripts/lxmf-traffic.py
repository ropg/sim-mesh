"""LXMF traffic: warm-up announces, an hour of messages, a drain; the record in the run.

Every station runs the firmware chosen above the script (any of category
`reticulum`), is set up by the startup script (its role, its radio, its
nodeset's own setup), and gets an LXMF identity at its first boot. Its
messages go out through its driver's `lxmf.send` verb (sim_mesh.traffic),
and each driver reports how each one ended. `OPTIONS` below are the phases'
settings, over sim_mesh.traffic's defaults; the result, every send with its
route and reply, lands in the run directory as `traffic.json`, the
simulation is paused, and the report is the delivery counted from the run's
events (`delivery.py traffic.json <run>`).
"""
from sim_mesh import *
from sim_mesh import traffic

firmware = script_input("firmware", type=Firmware, category="reticulum",
                        label="Firmware for nodes not otherwise configured")

OPTIONS = {
    "warm_rounds": 3,
    "traffic": 3600.0,
    "every": 5.0,
    "seed": 17,
    "drain": 600.0,
}

sim_speed("max")
nodes().firmware(firmware)
script_include("scripts/startup.py")
# What a station needs to take part: a sender and a recipient each need an
# identity, which the station's name gives it.
nodes().on_first_boot(Node.reticulum.lxmf.create())

traffic.run(OPTIONS)
sim_pause()


def report(run_dir):
    return traffic.report(run_dir)
