#!/usr/bin/env python3
"""Every Reticulum packet of a run, with every time it went on the air.

    ledger.py RUN [--record PATH] [--from S] [--to S] [--window S] [--json OUT]

Reads the run's `record.tsv` (or --record) and ties every transmission that
starts inside [--from, --to) to the Reticulum packet it carries, the two
halves of a split packet joined, by the packet's own hash: what a Reticulum
node hashes, the packet and destination type of the header and everything
after its addresses, not the hop count and not the transport address. A
packet relayed, or answered by many, is therefore one packet however many
stations put it on the air. Seconds are T in a virtual-time run, and the
wall clock since the record's first line in a real one.

- by kind (`simesh.reticulum.frames.kind_of`): packets, transmissions and
  their airtime, each transmission counted as the packet's first on the
  air, as another station's (a relay, or a second answer), or as a station
  sending again what it has sent already (`repeat`);
- path requests: each request, by its destination and tag, and the stations
  that answered it, a path response for that destination before the next
  request for it or --window seconds after it, whichever is first; per
  request and in all. Every answer costs every station in range its
  airtime, and a requester can use one;
- proofs: per proven packet (a proof is addressed to the hash of the packet
  it proves), the distinct proofs sent and how often they went on the air.

Frames that carry no Reticulum packet (SUPE's, power requests, a bench
`lora tx`) are counted by kind under `not_reticulum`.
"""
import argparse
import base64
import collections
import hashlib
import json
import sys

import referee
from simesh import record as record_module
from simesh.reticulum import frames as rframes
from simesh.view import RunView

TRUNC = rframes.TRUNC_HASH
WINDOW_S = 60.0


def header(raw):
    """(packet type, header 2, destination, context, data) of a Reticulum
    packet's bytes, or None for bytes that are not one."""
    if len(raw) < 19 or raw[0] & 0x80:
        return None
    flags = raw[0]
    hdr2 = bool(flags & 0x40)
    need = 2 + (2 * TRUNC if hdr2 else TRUNC) + 1
    if len(raw) < need:
        return None
    dest = raw[2 + (TRUNC if hdr2 else 0):][:TRUNC]
    return flags & 0x03, hdr2, dest, raw[need - 1], raw[need:]


def packet_hash(raw):
    """The hash a Reticulum node gives a packet: of its header's lower four
    bits (packet and destination type) and everything after its addresses."""
    hdr2 = bool(raw[0] & 0x40)
    return hashlib.sha256(bytes([raw[0] & 0x0F]) + raw[2 + (TRUNC if hdr2 else 0):]).digest()


def transmissions(path):
    """(t µs, sender, bytes, airtime µs) of every transmission in the record,
    on the record's clock, a split packet's halves joined into one at its
    first half's start: the Reticulum packet with the RNode header byte of
    each frame taken off; frames that are no Reticulum packet as they are,
    with None for the bytes' first element's place (see `kinds`)."""
    halves = rframes.Halves()
    first = {}
    stamp0 = record_module.first_stamp(path)
    origin = 0 if stamp0 is None or ":" not in stamp0 else referee.to_us(stamp0)
    for stamp, direction, sid, msg in record_module.lines(path, types=("tx",)):
        if direction != "in":
            continue
        payload = base64.b64decode(msg.get("payload") or "")
        part = halves.part(sid, payload)
        t = referee.to_us(stamp) - origin
        span = max(0, int(msg.get("t_end", 0)) - int(msg.get("t0", 0)))
        if part == 1:
            first[sid] = (t, payload, span)
        elif part == 2:
            if sid in first:
                t1, head, span1 = first.pop(sid)
                yield t1, sid, head[1:] + payload[1:], span1 + span, head
        else:
            yield t, sid, payload[1:], span, payload


def analyse(path, lo=None, hi=None, window_s=WINDOW_S):
    """The ledger of the record at `path`, keyed by station id."""
    kinds = collections.defaultdict(lambda: {
        "packets": 0, "transmissions": 0, "airtime_s": 0.0, "first": 0, "other_station": 0,
        "other_station_airtime_s": 0.0, "repeat": 0, "repeat_airtime_s": 0.0})
    not_reticulum = collections.defaultdict(lambda: {"frames": 0, "airtime_s": 0.0})
    sent = {}                       # packet hash -> stations that have sent it
    requests = collections.OrderedDict()    # (destination, tag) -> request
    answers = collections.defaultdict(list)  # destination -> [(t, sid, airtime)]
    proofs = collections.defaultdict(lambda: collections.Counter())
    total = 0.0
    for t, sid, raw, span, frame in transmissions(path):
        if (lo is not None and t < lo) or (hi is not None and t >= hi):
            continue
        air = span / 1e6
        total += air
        parsed = header(raw)
        kind = rframes.kind_of(frame, 0)
        if parsed is None or kind in (None, "other") or kind.startswith("SUPE") \
                or kind in ("power request", "empty"):
            not_reticulum[kind or "other"]["frames"] += 1
            not_reticulum[kind or "other"]["airtime_s"] += air
            continue
        ptype, _hdr2, dest, _ctx, data = parsed
        h = packet_hash(raw)
        row = kinds[kind]
        row["transmissions"] += 1
        row["airtime_s"] += air
        senders = sent.get(h)
        if senders is None:
            sent[h] = {sid}
            row["packets"] += 1
            row["first"] += 1
        elif sid in senders:
            row["repeat"] += 1
            row["repeat_airtime_s"] += air
        else:
            senders.add(sid)
            row["other_station"] += 1
            row["other_station_airtime_s"] += air
        if kind == "path request" and len(data) >= TRUNC:
            key = (data[:TRUNC], data[-TRUNC:] if len(data) >= 2 * TRUNC else data[TRUNC:])
            if key not in requests:
                requests[key] = {"t": t, "requester": sid, "copies": 0}
            requests[key]["copies"] += 1
        elif kind == "path response":
            answers[dest].append((t, sid, air))
        elif ptype == 3 and kind == "proof":
            proofs[dest][h] += 1
    return {
        "airtime_s": total,
        "by_kind": {k: dict(v) for k, v in sorted(kinds.items(), key=lambda kv: -kv[1]["airtime_s"])},
        "not_reticulum": {k: dict(v) for k, v in sorted(not_reticulum.items())},
        "path_requests": path_requests(requests, answers, window_s),
        "proofs": {
            "proven_packets": len(proofs),
            "proofs": sum(len(c) for c in proofs.values()),
            "transmissions": sum(sum(c.values()) for c in proofs.values()),
            "proven_more_than_once": sum(1 for c in proofs.values() if len(c) > 1),
        },
    }


def path_requests(requests, answers, window_s):
    """Each request with the answers it drew: a path response for its
    destination from its first transmission until the next request for that
    destination or `window_s` later, whichever is first."""
    by_dest = collections.defaultdict(list)
    for (dest, _tag), r in requests.items():
        by_dest[dest].append(r["t"])
    rows = []
    taken = set()
    for (dest, _tag), r in sorted(requests.items(), key=lambda kv: kv[1]["t"]):
        later = [t for t in by_dest[dest] if t > r["t"]]
        end = min(min(later, default=float("inf")), r["t"] + window_s * 1e6)
        got = [(i, a) for i, a in enumerate(answers.get(dest, [])) if r["t"] <= a[0] < end]
        taken.update((dest, i) for i, _a in got)
        rows.append({
            "t_s": r["t"] / 1e6, "destination": dest.hex()[:8], "requester": r["requester"],
            "copies": r["copies"], "responders": sorted({a[1] for _i, a in got}),
            "responses": len(got), "airtime_s": sum(a[2] for _i, a in got)})
    unasked = sum(1 for dest, a in answers.items() for i in range(len(a)) if (dest, i) not in taken)
    n = len(rows)
    return {
        "requests": n,
        "answered": sum(1 for r in rows if r["responses"]),
        "responders_mean": sum(len(r["responders"]) for r in rows) / n if n else None,
        "responders_max": max((len(r["responders"]) for r in rows), default=None),
        "responses": sum(r["responses"] for r in rows),
        "response_airtime_s": sum(r["airtime_s"] for r in rows),
        "responses_outside_a_window": unasked,
        "per_request": rows,
    }


def named(out, names):
    """The ledger with station ids as the run's node names."""
    for r in out["path_requests"]["per_request"]:
        r["requester"] = names.get(r["requester"], r["requester"])
        r["responders"] = [names.get(s, s) for s in r["responders"]]
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("run", help="the run directory")
    ap.add_argument("--record", help="the record to read (default: the run's record.tsv)")
    ap.add_argument("--from", dest="frm", type=float)
    ap.add_argument("--to", type=float)
    ap.add_argument("--window", type=float, default=WINDOW_S,
                    help="seconds after a path request that its answers may come in")
    ap.add_argument("--json")
    args = ap.parse_args(argv)
    view = RunView(args.run)
    out = analyse(args.record or view.record_path,
                  int(args.frm * 1e6) if args.frm is not None else None,
                  int(args.to * 1e6) if args.to is not None else None, args.window)
    text = json.dumps(named(out, view.names), indent=1)
    if args.json:
        with open(args.json, "w") as f:
            f.write(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
