#!/usr/bin/env python3
"""Delivery of an LXMF traffic run, from its output and the run's events.

    delivery.py TRAFFIC.json RUN_DIR [--json OUT]

TRAFFIC.json is what the traffic driver (scripts/lxmf-traffic.py) wrote.
RUN_DIR is the run the traffic went through: the events its stations'
drivers reported (events.jsonl), and its radio graph, which is the run's own
loss tables at the calling channel with each node's declared SF, bandwidth
and power, forwarding through the stations whose role forwards. What is
counted is `sim_mesh.reticulum.delivery`'s.
"""
import argparse
import json
import sys

from sim_mesh.reticulum import delivery


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("traffic")
    ap.add_argument("run_dir")
    ap.add_argument("--json")
    args = ap.parse_args(argv)
    try:
        _, out, rows = delivery.analyse_run(args.traffic, args.run_dir)
    except ValueError as err:
        ap.error(str(err))
    print(json.dumps(out, indent=1))
    if args.json:
        with open(args.json, "w") as f:
            json.dump(dict(out, messages=rows), f, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
