"""The `meshtastic` category: what every Meshtastic firmware's driver answers.

```
script ── node(n).meshtastic.sendtext(text, to) ──► simd: mid ──► sendtext(text, mid, dest) ──► station
sender's console ── queued, routing answer ──► driver ──► msg_status(mid, …)
receiver's console ── the text received, mid in it ──► driver ──► msg_received(mid, …)
```

A firmware of category `meshtastic` ships a driver (`sim_mesh.driver`) whose
DRIVER subclasses `MeshtasticDriver` and implements these verbs, beside every
firmware's (`Driver`: name, radio, radio_up, tx_power, diagnostics). Each
takes the station first; a verb a firmware cannot do raises `CommandError`
(`self.cannot(verb)` makes one), and the default here does exactly that. A
script reaches them as `<selection>.meshtastic.<verb>`. Their names are the
Meshtastic CLI's options, and each means what that option means:

    role(station, role)                 its device role, Meshtastic's in lower
                                        case (ROLES)
    hop_limit(station, n)               the hops a packet it originates may
                                        take, 0-7
    sendtext(station, text, mid, dest=None, ch_index=0, want_ack=True)
                                        a text message to the node named
                                        `dest`, or on channel `ch_index` when
                                        `dest` is None; `mid` is sim-mesh's id
    traceroute(station, dest)           {route, snr_towards, route_back,
                                        snr_back}, hops as node names where
                                        known
    nodes(station)                      [(name, id, hops_away, snr, last_heard)]
    nodeinfo(station)                   a NodeInfo broadcast now

**What became of a message** is reported against `mid` with `msg_status`
and `msg_received` (`sim_mesh.driver`), as the `meshcore` category does: a
direct message ends `delivered` (its acknowledgement came back) or `failed`;
a channel message ends at `sent`. `mid` travels in the text (`tagged`,
`untagged`).
"""

from sim_mesh.driver import (CommandError, Driver, RECEIVED_EVENT,  # noqa: F401
                             STATUS_EVENT, STATUSES, tagged, untagged)

ROLES = ("client", "client_mute", "client_hidden", "client_base", "router", "router_late",
         "tracker", "sensor", "tak", "tak_tracker", "lost_and_found")
ROUTER_ROLES = ("router", "router_late")


class MeshtasticDriver(Driver):
    category = "meshtastic"
    VERBS = Driver.VERBS + ("role", "hop_limit", "sendtext", "traceroute", "nodes", "nodeinfo")

    async def role(self, station, role):
        raise self.cannot("role")

    async def hop_limit(self, station, n):
        raise self.cannot("hop_limit")

    async def sendtext(self, station, text, mid, dest=None, ch_index=0, want_ack=True):
        raise self.cannot("sendtext")

    async def traceroute(self, station, dest):
        raise self.cannot("traceroute")

    async def nodes(self, station):
        raise self.cannot("nodes")

    async def nodeinfo(self, station):
        raise self.cannot("nodeinfo")
