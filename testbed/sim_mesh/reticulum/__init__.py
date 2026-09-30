"""Reticulum's parts of the library:

    sim_mesh.reticulum.frames   what a frame on the air is: the RNode header,
                                the Reticulum packet, SUPE's frames, the power
                                request
    sim_mesh.reticulum.delivery delivery of a traffic run (sim_mesh.traffic), from
                                the senders' logs, each sender by its own
                                station's
    sim_mesh.reticulum.rncfg_delivery   what a station configured with rncfg,
                                the reticulum project's, logs of its sends

A Reticulum station is one whose device is of kind `reticulous` or
`microreticulum`; both frame their packets with the RNode header. Reticulum
has no routers or repeaters of its own; a transport is what forwards.
"""

KIND_TYPES = ("reticulous", "microreticulum")
