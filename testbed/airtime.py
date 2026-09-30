#!/usr/bin/env python3
"""Airtime, transmit power and losses from a run's ether record.

    airtime.py RUN [--record PATH] [--from S] [--to S] [--calling HZ]
               [--gap S] [--busy] [--roles] [--json OUT]

Reads the run's `record.tsv` (or --record) and counts every transmission
that starts inside [--from, --to): seconds of T in a virtual-time run, of
the wall clock since the record's first line in a real one.

- airtime: total seconds on the air, per station (mean and maximum), and as
  a fraction of the window; split into the calling channel (--calling; by
  default the carrier most of the run's nodes declare) and
  every other carrier, which under SUPE's channel plan are its traffic
  channels; and by what the frame is (`by_kind`, and on the calling channel
  alone `by_kind_calling`), so the announce share can be read off. What a
  frame is belongs to the protocol: in a run with Reticulous stations,
  Reticulum packet type and context or SUPE frame type
  (`sim_mesh.reticulum.frames.kind_of`); otherwise every frame is `frame`;
- transmit power: `power_dbm` of each frame, on the calling channel and on
  the other carriers, weighted by frame and by airtime, with its minimum,
  quartiles and maximum; and exchanges on the other carriers, where an
  exchange is a run of frames on one carrier with less than --gap seconds of
  silence between one frame's end and the next one's start, at reduced
  power when any frame in it went out below the maximum seen;
- losses: receptions that ended in a CRC failure, frames that at least one
  station received and every one of them lost, and frames nobody was in a
  position to receive.

A reception is tied to its transmission by the ether's own number for the
frame, which `rx_begin` and `rx_end` carry and the `tx` line does not, the
way the referee ties them (`referee.Record`, at the run's levels), and not
by when the frame went on the air and what it carried: two frames at one T
can carry the same bytes, and a slot told of a frame mid-way is told it
from then.

--busy adds, per station, the share of the window the calling channel was
occupied where it stands: its own frames and every frame it was told of.
Station names are the run's nodeset's.

--roles splits the airtime by role, each station's as its node declares it
(a node declaring none is a `client`). For each role, the mean
seconds per station on the calling channel, split into announces and path
traffic (announces, path requests and responses, SUPE ANNOUNCE), SUPE HAIL,
the rest of SUPE's frames, and unicast payload (every other frame); on one
traffic channel, a station's traffic-channel seconds divided by the number
of traffic channels any frame in the window used; and in total.
"""
import argparse
import base64
import collections
import json
import sys

import referee
from sim_mesh import record as record_module
from sim_mesh import reticulum
from sim_mesh.reticulum import frames as rframes
from sim_mesh.view import RunView

TOLERANCE_HZ = 125_000 // 4


def quartiles(values):
    v = sorted(values)
    if not v:
        return None
    q = lambda f: v[min(len(v) - 1, int(f * (len(v) - 1) + 0.5))]
    return [v[0], q(0.25), q(0.5), q(0.75), v[-1]]


def union_len(intervals):
    total, end = 0, None
    start = None
    for a, b in sorted(intervals):
        if end is None or a > end:
            if end is not None:
                total += end - start
            start, end = a, b
        elif b > end:
            end = b
    if end is not None:
        total += end - start
    return total


def generic_kind(payload, part):
    return "frame"


def analyse(args, kind_of=rframes.kind_of, level_at=None):
    lo = int(args.frm * 1e6) if args.frm is not None else None
    hi = int(args.to * 1e6) if args.to is not None else None
    calling = args.calling

    record = referee.Record(args.record, level_at)
    tx_line = {eid: f.line for eid, f in record.by_eid.items()}   # ether frame id -> its `tx`
    frames = []                     # one dict per transmission in the window
    at_line = {}                    # its `tx`'s place among the record's lines -> index into frames
    halves = rframes.Halves()
    busy = collections.defaultdict(list)
    for line, (stamp, direction, s, msg) in enumerate(record_module.lines(args.record)):
        kind = msg.get("type")
        if direction == "in" and kind == "tx":
            t_us = referee.to_us(stamp) - record.origin
            payload = base64.b64decode(msg.get("payload") or "")
            part = halves.part(s, payload)
            if (lo is not None and t_us < lo) or (hi is not None and t_us >= hi):
                continue
            span = max(0, int(msg.get("t_end", 0)) - int(msg.get("t0", 0)))
            f = {"sid": s, "t": t_us, "span": span, "freq": msg.get("freq"),
                 "power": msg.get("power_dbm"), "part": part,
                 "kind": kind_of(payload, part), "rx": 0, "clean": 0, "crc": 0}
            if f["kind"] is None:
                prev = next((g for g in reversed(frames[-64:]) if g["sid"] == s), None)
                f["kind"] = prev["kind"] if prev else "data"
            at_line[line] = len(frames)
            frames.append(f)
            if args.busy and abs(f["freq"] - calling) <= TOLERANCE_HZ:
                busy[s].append((t_us, t_us + span))
        elif direction == "out" and kind in ("rx_begin", "rx_end"):
            i = at_line.get(tx_line.get(msg.get("id")))
            if i is None:
                continue
            f = frames[i]
            if kind == "rx_end":
                f["rx"] += 1
                if msg.get("verdict") == "clean":
                    f["clean"] += 1
                else:
                    f["crc"] += 1
            elif args.busy and not msg.get("cad") and abs(f["freq"] - calling) <= TOLERANCE_HZ:
                # Occupied from when the slot was told, on the record's
                # clock, to the frame's end.
                told = referee.to_us(stamp) - record.origin
                if (lo is None or told >= lo) and (hi is None or told < hi):
                    busy[s].append((told, f["t"] + f["span"]))

    if not frames:
        return frames, 0.0, busy
    window = ((hi if hi is not None else max(f["t"] + f["span"] for f in frames))
              - (lo if lo is not None else min(f["t"] for f in frames))) / 1e6
    return frames, window, busy


def report(frames, window, busy, args, names, roles=None):
    """The figures. `names` is station id -> name; `roles` station id -> role,
    for --roles."""
    calling = args.calling
    on_call = lambda f: abs(f["freq"] - calling) <= TOLERANCE_HZ
    out = {"window_s": window, "frames": len(frames), "calling_hz": calling}
    stations = sorted(set(names) | {f["sid"] for f in frames})
    n_st = len(stations) or 1

    per = collections.defaultdict(lambda: [0, 0])     # sid -> [calling us, other us]
    for f in frames:
        per[f["sid"]][0 if on_call(f) else 1] += f["span"]
    tot_call = sum(v[0] for v in per.values()) / 1e6
    tot_other = sum(v[1] for v in per.values()) / 1e6
    each = {s: (per[s][0] + per[s][1]) / 1e6 for s in stations}
    top = max(each.items(), key=lambda x: x[1]) if each else (None, 0)
    out["airtime"] = {
        "total_s": tot_call + tot_other, "calling_s": tot_call, "traffic_channels_s": tot_other,
        "stations": n_st,
        "per_station_mean_s": (tot_call + tot_other) / n_st,
        "per_station_mean_calling_s": tot_call / n_st,
        "per_station_mean_traffic_s": tot_other / n_st,
        "per_station_max_s": top[1], "per_station_max_name": names.get(top[0], top[0]),
        "per_station_mean_fraction": (tot_call + tot_other) / n_st / window if window else None,
        "per_station_max_fraction": top[1] / window if window else None,
    }
    freqs = collections.Counter()
    for f in frames:
        freqs[round(f["freq"] / 1e5) / 10] += f["span"]
    out["by_carrier_mhz"] = {"%.1f" % k: v / 1e6 for k, v in sorted(freqs.items())}
    kinds = collections.defaultdict(lambda: [0, 0])
    for f in frames:
        kinds[f["kind"]][0] += 1
        kinds[f["kind"]][1] += f["span"]
    out["by_kind"] = {k: {"frames": v[0], "s": v[1] / 1e6}
                      for k, v in sorted(kinds.items(), key=lambda x: -x[1][1])}
    on_calling = collections.defaultdict(lambda: [0, 0])
    for f in frames:
        if on_call(f):
            on_calling[f["kind"]][0] += 1
            on_calling[f["kind"]][1] += f["span"]
    out["by_kind_calling"] = {k: {"frames": v[0], "s": v[1] / 1e6}
                              for k, v in sorted(on_calling.items(), key=lambda x: -x[1][1])}
    ann = sum(v[1] for k, v in kinds.items() if rframes.is_announce_kind(k))
    out["announce_s"] = ann / 1e6
    out["other_s"] = (sum(v[1] for v in kinds.values()) - ann) / 1e6

    def power(sel):
        fs = [f for f in frames if sel(f) and f["power"] is not None]
        if not fs:
            return None
        air = sum(f["span"] for f in fs)
        top = max(g["power"] for g in fs)
        return {"frames": len(fs),
                "mean_by_frame": sum(f["power"] for f in fs) / len(fs),
                "mean_by_airtime": sum(f["power"] * f["span"] for f in fs) / air if air else None,
                "min_q1_median_q3_max": quartiles([f["power"] for f in fs]),
                "below_max_frames": sum(1 for f in fs if f["power"] < top)}
    out["power_calling"] = power(on_call)
    out["power_traffic_channels"] = power(lambda f: not on_call(f))

    other = sorted((f for f in frames if not on_call(f)), key=lambda f: (f["freq"], f["t"]))
    top_power = max((f["power"] for f in frames if f["power"] is not None), default=None)
    ex, cur, last = [], None, None
    for f in other:
        if cur is None or f["freq"] != cur["freq"] or f["t"] - last > args.gap * 1e6:
            cur = {"freq": f["freq"], "frames": 0, "reduced": False, "stations": set()}
            ex.append(cur)
        cur["frames"] += 1
        cur["stations"].add(f["sid"])
        cur["reduced"] |= f["power"] is not None and f["power"] < top_power
        last = f["t"] + f["span"]
    out["exchanges"] = {"count": len(ex), "reduced_power": sum(1 for e in ex if e["reduced"]),
                        "frames_mean": sum(e["frames"] for e in ex) / len(ex) if ex else None}

    heard = [f for f in frames if f["rx"]]
    out["losses"] = {
        "receptions": sum(f["rx"] for f in frames),
        "receptions_clean": sum(f["clean"] for f in frames),
        "receptions_crc": sum(f["crc"] for f in frames),
        "frames_received_somewhere": len(heard),
        "frames_lost_at_every_receiver": sum(1 for f in heard if not f["clean"]),
        "frames_nobody_received": len(frames) - len(heard),
    }
    if roles is not None:
        chans = {f["freq"] for f in frames if not on_call(f)}
        acc = collections.defaultdict(lambda: collections.Counter())
        for f in frames:
            if on_call(f):
                acc[f["sid"]][rframes.calling_class(f["kind"])] += f["span"]
            else:
                acc[f["sid"]]["traffic"] += f["span"]
        out["roles"] = {"traffic_channels": len(chans)}
        for role in sorted(set(roles.values())):
            members = [s for s in stations if roles.get(s) == role]
            n = len(members) or 1
            mean = lambda key: sum(acc[s][key] for s in members) / 1e6 / n
            calling_s = {c: mean(c) for c in rframes.CALLING_CLASSES}
            traffic_s = mean("traffic")
            out["roles"][role] = {
                "stations": len(members),
                "calling_s": sum(calling_s.values()),
                "calling_by_class_s": calling_s,
                "per_traffic_channel_s": traffic_s / len(chans) if chans else None,
                "traffic_channels_s": traffic_s,
                "total_s": sum(calling_s.values()) + traffic_s,
            }
    if busy:
        share = {names.get(s, s): union_len(v) / 1e6 / window for s, v in busy.items()}
        vals = sorted(share.values())
        out["busy_calling"] = {"min_q1_median_q3_max": quartiles(vals),
                               "mean": sum(vals) / len(vals), "top": sorted(
                                   share.items(), key=lambda x: -x[1])[:5]}
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("run", help="the run directory")
    ap.add_argument("--record", help="the record to read (default: the run's record.tsv)")
    ap.add_argument("--from", dest="frm", type=float)
    ap.add_argument("--to", type=float)
    ap.add_argument("--calling", type=int,
                    help="the calling channel in Hz (default: the run's most declared carrier)")
    ap.add_argument("--gap", type=float, default=1.0,
                    help="seconds of silence that end an exchange on a traffic channel")
    ap.add_argument("--busy", action="store_true")
    ap.add_argument("--roles", action="store_true", help="airtime per station role")
    ap.add_argument("--json")
    args = ap.parse_args(argv)
    view = RunView(args.run)
    args.record = args.record or view.record_path
    if args.calling is None:
        args.calling = view.calling_hz()
    kind_of = rframes.kind_of if reticulum in view.protocols() else generic_kind
    frames, window, busy = analyse(args, kind_of, referee.Air(view.medium()).level)
    out = report(frames, window, busy, args, view.names,
                 view.roles() if args.roles else None)
    text = json.dumps(out, indent=1, default=list)
    if args.json:
        with open(args.json, "w") as f:
            f.write(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
