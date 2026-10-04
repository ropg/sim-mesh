"""MeshCore's parts of the library:

    sim_mesh.meshcore.driver    the `meshcore` category's driver interface: what
                                every MeshCore firmware's driver answers

A MeshCore station is one whose firmware is of category `meshcore`: a
companion, which a person drives through meshcore-cli, a repeater or a room
server. Its verbs carry meshcore-cli's own command names and mean what those
commands mean.
"""

CATEGORY = "meshcore"
