"""The `meshcore` category: what every MeshCore firmware's driver answers.

```
script ── node(n).meshcore.msg(to, text) ──► simd: mid ──► msg(dest, text, mid) ──► station
sender's console ── sent, acknowledged ──► driver ──► msg_status(mid, …)
receiver's console ── the message fetched, mid in its text ──► driver ──► msg_received(mid, …)
```

A firmware of category `meshcore` ships a driver (`sim_mesh.driver`) whose
DRIVER subclasses `MeshcoreDriver` and implements these verbs, beside every
firmware's (`Driver`: name, radio, radio_up, tx_power, diagnostics). Each
takes the station first; a verb a firmware cannot do raises `CommandError`
(`self.cannot(verb)` makes one), and the default here does exactly that. A
script reaches them as `<selection>.meshcore.<verb>`. Their names are
meshcore-cli's commands, and each means what that command means:

    repeat(station, on)                 forwarding others' packets on or off
    advert(station)                     a zero-hop advert
    floodadv(station)                   a flooded advert
    contacts(station)                   [(name, public-key prefix, path length
                                        or None)]: None for a contact reached
                                        by flood
    msg(station, dest, text, mid)       a direct message to the contact `dest`
                                        (its name); `mid` is sim-mesh's id
    chan(station, nb, text, mid)        a message on channel `nb`
    path(station, dest)                 the contact's path, its hops' prefixes,
                                        or None for flood
    reset_path(station, dest)           back to flood for that contact

**What became of a message** is reported against `mid`, by the sender's
driver:

    self.msg_status(station, mid, "sent" | "delivered" | "failed", why=<text>)

which is the event `msg.status`. A direct message ends `delivered` (its
acknowledgement came back) or `failed`; a channel message has no
acknowledgement and ends at `sent`. **A message's arrival** is reported by the
receiver's driver, from what the station fetched:

    self.msg_received(station, mid, text, sender=<prefix> | chan=<nb>)

which is the event `msg.received`. `mid` travels in the message's text, put
there with `tagged(text, mid)` and read back with `untagged(text)`, so the
receiving station's driver knows it with nothing of the sender's. These are
`sim_mesh.driver`'s, shared with the `meshtastic` category, and importable
from here as well.
"""

from sim_mesh.driver import (CommandError, Driver, MID_MARK, RECEIVED_EVENT,  # noqa: F401
                             STATUS_EVENT, STATUSES, UNTAG, tagged, untagged)


class MeshcoreDriver(Driver):
    category = "meshcore"
    VERBS = Driver.VERBS + ("repeat", "advert", "floodadv", "contacts", "msg", "chan",
                            "path", "reset_path")

    async def repeat(self, station, on):
        raise self.cannot("repeat")

    async def advert(self, station):
        raise self.cannot("advert")

    async def floodadv(self, station):
        raise self.cannot("floodadv")

    async def contacts(self, station):
        raise self.cannot("contacts")

    async def msg(self, station, dest, text, mid):
        raise self.cannot("msg")

    async def chan(self, station, nb, text, mid):
        raise self.cannot("chan")

    async def path(self, station, dest):
        raise self.cannot("path")

    async def reset_path(self, station, dest):
        raise self.cannot("reset_path")
