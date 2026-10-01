"""Reticulum's parts of the library:

    sim_mesh.reticulum.driver   the `reticulum` category's driver interface: what
                                every Reticulum firmware's driver answers
    sim_mesh.reticulum.frames   what a frame on the air is: the RNode header,
                                the Reticulum packet, SUPE's frames, the power
                                request
    sim_mesh.reticulum.delivery delivery of a traffic run (sim_mesh.traffic), from
                                what the senders' drivers reported

A Reticulum station is one whose firmware is of category `reticulum`; its
frames are read as RNode-framed Reticulum packets. Reticulum has no routers
or repeaters of its own; a transport is what forwards.
"""

CATEGORY = "reticulum"
