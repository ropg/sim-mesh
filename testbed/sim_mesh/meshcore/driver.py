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
receiving station's driver knows it with nothing of the sender's.
"""

import re

from sim_mesh.driver import CommandError, Driver  # noqa: F401 - a driver's imports

STATUSES = ("sent", "delivered", "failed")
STATUS_EVENT = "msg.status"
RECEIVED_EVENT = "msg.received"
MID_MARK = " #"
UNTAG = re.compile(r"^(.*?) #([A-Za-z0-9_.-]+)\s*$", re.S)


def tagged(text, mid):
    """A message's text with sim-mesh's id at its end."""
    return "%s%s%s" % (text, MID_MARK, mid)


def untagged(text):
    """(text, mid) out of a tagged text; (text, None) when it carries none."""
    found = UNTAG.match(text or "")
    if not found:
        return text, None
    return found.group(1), found.group(2)


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

    # ---- what became of a message ----------------------------------------

    def msg_status(self, station, mid, status, why=None):
        """Report a message's status under sim-mesh's id."""
        fields = {"mid": mid, "status": status}
        if why:
            fields["why"] = why
        station.report(STATUS_EVENT, **fields)

    def msg_received(self, station, mid, text, sender=None, chan=None):
        """Report a message the station received: from a contact (its
        public-key prefix) or on a channel."""
        fields = {"mid": mid, "text": text}
        if sender is not None:
            fields["sender"] = sender
        if chan is not None:
            fields["chan"] = chan
        station.report(RECEIVED_EVENT, **fields)
