"""Delivery of LXMF messages sent from stations configured with rncfg: the
reticulum project's station, fw/simesh (the kind SIMesh calls `sergeyculum`).

Such a station logs its LXMF sends under the `lxmf` tag
(crates/reticulum-node/src/engine.rs in the reticulum project):

    sent <n> B to <dest8> …                       a message goes out
    no proof yet for the message to <dest8> …     it is sent again
    the message to <dest8> was delivered …        its proof has come back
    no proof for the message to <dest8> after …   it is given up on
    nobody answered for <dest8> — the held …      no path came: never sent

where <dest8> is the first eight hex digits of the recipient's delivery
destination. No message id is logged, so a send of the traffic driver's is
matched to the first message its sender logged to that recipient from the
instant the send was due, within MATCH_S, in order. A message is delivered
when its proof came back, which is the stack's own word that the recipient
has it. The instant a send was due is the driver's schedule (its phase
`traffic_start` and the send's `at`), not when the tool answered, which for a
tool that waits for the proof is long after the message went out; latency
runs from it too.

A station's log is stamped with seconds since its process started, one boot
section per start (its `simesh …: station N in` line at 0). Each section is
placed in T at the station's `hello` in the run's record, the k-th last
section at the k-th last hello, which is where the station started to within
the milliseconds between its start and its hello.
"""

import collections
import os
import re

from simesh import record

# SIMesh's kind for the reticulum project's station.
KIND_TYPES = ("sergeyculum",)
MATCH_S = 90.0      # how long after it was due a send may first go out
EARLY_S = 2.0       # how early: the schedule's instant is the driver's, to a second or so


BOOT = re.compile(r"^\s*0\.0+ \[\w+\] simesh .*: station \d+ in ")
LINE = re.compile(r"^\s*(\d+\.\d+) \[\w+\] \[(\w+)\] (.*)$")
EVENTS = (
    ("sent", re.compile(r"^sent \d+ B to ([0-9a-f]{8})")),
    ("retry", re.compile(r"^no proof yet for the message to ([0-9a-f]{8})")),
    ("delivered", re.compile(r"^the message to ([0-9a-f]{8}) was delivered")),
    ("gave up", re.compile(r"^no proof for the message to ([0-9a-f]{8})")),
    ("no path", re.compile(r"^nobody answered for ([0-9a-f]{8})")),
)


def hellos(record_path):
    """station id -> the T, in seconds, of each of its hellos, in order."""
    out = collections.defaultdict(list)
    for stamp, direction, sid, msg in record.lines(record_path):
        if direction == "in" and msg.get("type") == "hello":
            out[sid].append(record.parse_time(stamp))
    return out


def events(log_path, starts):
    """(T, event, dest8) of every LXMF event in a station's log, its boot
    sections placed at `starts`, the T of the station's hellos."""
    with open(log_path, encoding="utf-8", errors="replace") as handle:
        text = handle.read().splitlines()
    boots = [i for i, line in enumerate(text) if BOOT.match(line)]
    if not boots or not starts:
        return []
    mine = boots[-len(starts):]
    starts = starts[-len(mine):]
    out = []
    for k, first in enumerate(mine):
        end = mine[k + 1] if k + 1 < len(mine) else len(text)
        for line in text[first:end]:
            m = LINE.match(line)
            if not m or m.group(2) != "lxmf":
                continue
            for name, pattern in EVENTS:
                e = pattern.match(m.group(3))
                if e:
                    out.append((starts[k] + float(m.group(1)), name, e.group(1)))
                    break
    out.sort()
    return out


def messages(evs):
    """The messages a station logged, by recipient: dest8 -> [{sent, delivered,
    end}], and the recipients it gave up asking a path for: dest8 -> [T].

    A proof or a giving-up closes the latest message still open to that
    recipient, as the reticulum project's campaign scorer reads the same
    lines (tools/simcampaign/src/score.rs): with no id in the log, the one
    most recently sent is the one a proof most likely answers."""
    sent = collections.defaultdict(list)
    no_path = collections.defaultdict(list)
    open_ = {}
    for t, name, dest in evs:
        if name == "sent":
            sent[dest].append({"sent": t, "delivered": None, "end": None})
            open_[dest] = sent[dest][-1]
        elif name == "no path":
            no_path[dest].append(t)
        elif name in ("delivered", "gave up") and dest in open_:
            message = open_.pop(dest)
            message["end"] = name
            if name == "delivered":
                message["delivered"] = t
    return sent, no_path


def due(drive, send):
    """The T, in seconds, a send of the driver's was due: its phase
    `traffic_start` plus the plan's one second of lead and the send's `at`;
    the T its tool answered at when the result has no such phase."""
    for phase in drive.get("phases") or []:
        if phase[0] == "traffic_start" and send.get("at") is not None:
            return phase[2] / 1e6 + 1.0 + float(send["at"])
    return (send.get("t_sent") or 0) / 1e6


def read_logs(run_dir, sends, drive, sids):
    """{(sender, marker): {'delivered': T or None, 'last': what became of it}}
    for the driver's `sends` from stations of these kinds, matched to their
    logs. `drive` is the driver's result, `sids` name -> station id."""
    dests = drive.get("dests") or {}
    starts = hellos(record.path_in(run_dir))
    by_sender = collections.defaultdict(list)
    for s in sends:
        by_sender[s["src"]].append(s)
    out = {}
    for name, own in by_sender.items():
        path = os.path.join(run_dir, "nodes", name, "log")
        logged, no_path = {}, {}
        if name in sids and os.path.isfile(path):
            logged, no_path = messages(events(path, starts.get(sids[name], [])))
        taken = collections.defaultdict(int)
        for s in sorted(own, key=lambda s: due(drive, s)):
            dest = (dests.get(s["dst"]) or "")[:8]
            at = due(drive, s)
            rec = out[(name, s["marker"])] = {"delivered": None, "last": None}
            if not dest:
                rec["last"] = "no destination for the recipient"
                continue
            queue = logged.get(dest, [])
            i = taken[dest]
            while i < len(queue) and queue[i]["sent"] < at - EARLY_S:
                i += 1
            if i < len(queue) and queue[i]["sent"] <= at + MATCH_S:
                taken[dest] = i + 1
                rec["delivered"] = queue[i]["delivered"]
                rec["last"] = queue[i]["end"] or "sent, no proof by the end"
            elif any(at - EARLY_S <= t <= at + MATCH_S for t in no_path.get(dest, [])):
                rec["last"] = "no path: nobody answered"
            else:
                rec["last"] = "not sent: %s" % (s.get("reply") or s.get("error") or "no log line")
    return out
