"""The library scripts and the analysis tools share.

    sim_mesh.library    what a script says, top to end, synchronously: script_…
                        (its inputs, includes, log), sim_… (speed, clock,
                        phases, snapshots, pause, stop), and selections,
                        nodes(…) and node(name), with what is done to them
                        (.firmware, .on_first_boot, .exec, .radio,
                        .reticulum.role, .reticulum.lxmf.send, …)
    sim_mesh.select     which nodes: nodes(field=value…), combined with & | - ~
    sim_mesh.driver     what a firmware's driver is, and what sim-mesh hands it
    sim_mesh.traffic    the LXMF traffic driver, on any firmware of category
                        reticulum, and its report
    sim_mesh.sim        the hold on a running simulation the library runs on:
                        its stations, its clock, what is asked of them (async)
    sim_mesh.runner     a script, run: its simulation started, then its report
    sim_mesh.view       a run directory as the analysis tools see it: nodes by id
                        and name, positions in the geodata's metres, each node's
                        radio (the run's globals.py) and role (its tag), and the
                        medium's levels from the run's own loss tables
    sim_mesh.record     the ether's record, line by line
    sim_mesh.reticulum  Reticulum's parts: the category's driver interface, what
                        a frame on the air is (the Reticulum packet, SUPE's
                        frames), and a traffic run's delivery, from what the
                        senders' drivers reported

A script needs only `from sim_mesh import *`: the library's names
(`sim_mesh.library.__all__`). `start`, `attach`, `Sim` and `Selection` are
here at the top too, for the analysis tools and tests that hold a
simulation themselves.

What is generic stays out of a protocol: the record, per-carrier airtime,
link geometry, the loss table and the levels it gives. What a frame means is
a protocol's, found by the station's firmware category through
`protocol_for`.

A **role** is what a station does for the others, one of `ROLES`: a
`transport`, `router` or `repeater` forwards (`FORWARDING`), a `client`
only speaks for itself.
"""

from sim_mesh import reticulum
from sim_mesh.library import *  # noqa: F401,F403 - the library's face
from sim_mesh.library import __all__  # noqa: F401
from sim_mesh.select import Nodes  # noqa: F401
from sim_mesh.sim import Selection, Sim, SimError, attach, start  # noqa: F401

ROLES = ("transport", "router", "repeater", "client")
FORWARDING = ("transport", "router", "repeater")

PROTOCOLS = (reticulum,)


def protocol_for(category):
    """The protocol module that reads stations of this firmware category, or None."""
    for module in PROTOCOLS:
        if category == module.CATEGORY:
            return module
    return None
