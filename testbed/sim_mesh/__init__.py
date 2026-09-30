"""The library scripts and the analysis tools share.

    sim_mesh.library    what a script says, top to end, synchronously: time(),
                        firmware(), on_first_boot(), exec(), the meta commands
                        (announce, max_tx_pwr, send_msg), the run's clock,
                        snapshots, moves, pause and stop
    sim_mesh.select     which nodes: nodes(field=value…), combined with & | - ~
    sim_mesh.traffic    the LXMF traffic driver, on any firmware with the
                        meta commands, and its report
    sim_mesh.sim        the hold on a running simulation the library runs on:
                        its stations, its clock, what is asked of them (async)
    sim_mesh.runner     a script, run: its simulation started, then its report
    sim_mesh.view       a run directory as the analysis tools see it: nodes by id
                        and name, positions in the geodata's metres, each node's
                        radio (the run's globals.py) and role (its tag), and the
                        medium's levels from the run's own loss tables
    sim_mesh.record     the ether's record, line by line
    sim_mesh.reticulum  Reticulum's parts: what a frame on the air is (the
                        Reticulum packet, SUPE's frames), and a traffic run's
                        delivery, from Reticulous's logs

A script needs only `from sim_mesh import *`: the library's names
(`sim_mesh.library.__all__`). `start`, `attach`, `Sim` and `Selection` are
here at the top too, for the analysis tools and tests that hold a
simulation themselves.

What is generic stays out of a protocol: the record, per-carrier airtime,
link geometry, the loss table and the levels it gives. What a frame means is
a protocol's, found by the station's kind (its device's `kind`) through
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


def protocol_for(kind_type):
    """The protocol module that reads stations of this kind type, or None."""
    for module in PROTOCOLS:
        if kind_type in module.KIND_TYPES:
            return module
    return None
