"""Meshtastic check: a line of three stations, their node lists, a direct and a channel message, a traceroute, a setting after setup.

Every node runs the chosen Meshtastic firmware and is set up by the startup
script. The first node sends the second a direct message and the second
sends one on the channel; the first traces its route to the third; the third
is given a hop limit after it is up, which restarts it. The report reads
what the drivers recorded (the run's events.jsonl).
"""
from sim_mesh import *
import json
import os

firmware = script_input("firmware", type=Firmware, category="meshtastic",
                        label="Firmware for nodes not otherwise configured")
speed = script_input("speed", type=str, default="max", label="Time: max or real")

sim_speed(speed)
nodes().firmware(firmware)
script_include("scripts/startup.py")

up = nodes().up()
first, second, third = up[:3]
print("up at T %.1f s: %s" % (sim_now(), ", ".join(up)))

sim_wait(60)        # a station sends a NodeInfo at most once a minute
for round in range(2):  # twice: a frame can be lost (a receiver resets its AGC once a minute)
    for name in up:
        node(name).meshtastic.nodeinfo()
        sim_wait(15)
    sim_wait(30 if round else 20)
for name in up:
    print("%s hears %s" % (name, node(name).meshtastic.nodes()[name]))

mid = node(first).meshtastic.sendtext("hello there", to=second)[first]
print("direct %s -> %s: %s" % (first, second, mid))
sim_wait(60)
mid = node(second).meshtastic.sendtext("hello all")[second]
print("channel from %s: %s" % (second, mid))
sim_wait(60)

try:
    print("traceroute %s -> %s: %s" % (first, third,
                                       node(first).meshtastic.traceroute(third)[first]))
except ScriptError as err:
    print("traceroute %s -> %s failed: %s" % (first, third, err))

node(third).meshtastic.hop_limit(4)
print("%s took its hop limit and is %s at T %.1f s"
      % (third, node(third).facts()[third]["status"], sim_now()))
sim_stop()


def report(run_dir):
    with open(os.path.join(run_dir, "events.jsonl")) as handle:
        events = [json.loads(line) for line in handle]
    lines = ["| T s | node | event | mid | status / from |", "|---|---|---|---|---|"]
    for e in events:
        if e.get("event", "").startswith("msg."):
            lines.append("| %.3f | %s | %s | %s | %s |" % (
                e.get("t", 0) / 1e6 if isinstance(e.get("t"), (int, float)) else 0,
                e.get("node"), e["event"], e.get("mid"),
                e.get("status") or e.get("sender") or "chan %s" % e.get("chan")))
    return "\n".join(["# Meshtastic check", ""] + lines) + "\n"
