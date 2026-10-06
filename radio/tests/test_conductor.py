"""The library's clock in a virtual-time run, driven by a fake conductor.

Each test loads its own copy of libsimradio-sx1262.so with SIM_MESH_TIME=virtual in
the environment (the mode, and the clock profile, are read once per copy),
plays the ether on a UDP socket, and stands in for the host by calling
simradio_idle itself. Nothing moves until the fake conductor says so.
"""

import base64
import ctypes
import json
import os
import queue
import shutil
import socket
import threading
import time

import pytest

from test_model import (ALL_IRQ, CLEAR_IRQ, GET_IRQ, GET_RSSI_INST, HEADER_VALID, PIN_CB,
                        PREAMBLE, RX_DONE, SET_DIO3_TCXO, SET_DIO_IRQ_PARAMS, SET_PACKET_PARAMS,
                        SET_RX, SET_STANDBY, SET_TX, SYNC, TSYM, TX_DONE, WRITE_BUFFER,
                        load_library, toa_seconds, PRE)

NEVER = None
T_JOIN = 5_000_000
EPOCH = 1_790_000_000_000_000
WAKE_CB = ctypes.CFUNCTYPE(None, ctypes.c_void_p)
MOVED_CB = ctypes.CFUNCTYPE(None)


def load_copy(tmp_path, name, env):
    """A private copy of the library, so its globals are this test's alone."""
    lib = load_library()
    path = tmp_path / name
    shutil.copy(lib._name, path)
    saved = {k: os.environ.get(k) for k in env}
    os.environ.update(env)
    try:
        copy = ctypes.CDLL(str(path))
        copy.simradio_virtual()                 # the mode and the epoch are read here
        copy.simradio_node_to_conductor(0)      # and the clock profile here
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
    copy.simradio_station_open.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_char_p]
    copy.simradio_open.argtypes = [ctypes.c_int, PIN_CB, ctypes.c_void_p]
    copy.simradio_open.restype = ctypes.c_void_p
    copy.simradio_transfer.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t,
                                       ctypes.c_char_p]
    copy.simradio_now_us.restype = ctypes.c_int64
    copy.simradio_node_us.restype = ctypes.c_int64
    copy.simradio_epoch_us.restype = ctypes.c_int64
    copy.simradio_node_to_conductor.argtypes = [ctypes.c_int64]
    copy.simradio_node_to_conductor.restype = ctypes.c_int64
    copy.simradio_wake_create.argtypes = [WAKE_CB, ctypes.c_void_p]
    copy.simradio_wake_at.argtypes = [ctypes.c_int, ctypes.c_int64]
    copy.simradio_on_advance.argtypes = [MOVED_CB]
    return copy


class Conductor:
    """The ether's side of the clock, for one station."""

    def __init__(self):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.port = self.sock.getsockname()[1]
        self.station = None
        self.seq = 0
        self.t = T_JOIN
        self.inbox = queue.Queue()
        threading.Thread(target=self.drain, daemon=True).start()

    def drain(self):
        while True:
            try:
                data, addr = self.sock.recvfrom(65535)
            except OSError:
                return
            msg = json.loads(data.decode())
            if msg.get("type") == "hello":
                self.station = addr
            self.inbox.put(msg)

    def expect(self, kind, timeout=2.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                msg = self.inbox.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            if msg.get("type") == kind:
                return msg
        raise AssertionError("the station never sent %s" % kind)

    def nothing(self, kind, wait=0.1):
        deadline = time.monotonic() + wait
        while time.monotonic() < deadline:
            try:
                msg = self.inbox.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                return
            assert msg.get("type") != kind, "unexpected %r" % msg

    def send(self, msg):
        self.seq += 1
        msg = dict(msg, seq=self.seq)
        self.sock.sendto(json.dumps(msg).encode(), self.station)
        return self.seq

    def welcome(self):
        return self.send({"type": "welcome", "t": self.t, "mode": "virtual",
                          "rate": None, "epoch": EPOCH, "seed": 1})

    def run(self, t):
        self.t = t
        return self.send({"type": "run", "t": t})


def join(lib, cond, sid=9):
    """Open the station and welcome it. In virtual time station_open returns
    only once the welcome has landed, so it runs beside the conductor."""
    opened = []
    opener = threading.Thread(target=lambda: opened.append(lib.simradio_station_open(
        sid, b"127.0.0.1", ("127.0.0.1:%d" % cond.port).encode())), daemon=True)
    opener.start()
    cond.expect("hello")
    seq = cond.welcome()
    opener.join(5)
    assert opened == [0]
    return seq


def idle(lib, cond):
    lib.simradio_idle()
    return cond.expect("idle")


@pytest.fixture
def virtual(tmp_path):
    lib = load_copy(tmp_path, "libsimradio_virtual.so", {"SIM_MESH_TIME": "virtual"})
    cond = Conductor()
    yield lib, cond
    cond.sock.close()


def test_node_time_is_conductor_time_under_identity(virtual):
    lib, cond = virtual
    assert lib.simradio_virtual() == 1
    seq = join(lib, cond)
    msg = idle(lib, cond)
    assert msg["seq"] == seq and msg["until"] is NEVER
    assert lib.simradio_now_us() == T_JOIN
    assert lib.simradio_node_us() == T_JOIN
    assert lib.simradio_epoch_us() == EPOCH
    time.sleep(0.05)                    # the wall moves; T does not
    assert lib.simradio_node_us() == T_JOIN
    seq = cond.run(T_JOIN + 1234)
    msg = idle(lib, cond)
    assert msg["seq"] == seq
    assert lib.simradio_node_us() == T_JOIN + 1234


def test_idle_is_reported_once_per_grant(virtual):
    lib, cond = virtual
    join(lib, cond)
    idle(lib, cond)
    lib.simradio_idle()
    cond.nothing("idle")


def test_a_run_reaching_a_wake_fires_it(virtual):
    lib, cond = virtual
    join(lib, cond)
    fired = []
    cb = WAKE_CB(lambda arg: fired.append(lib.simradio_node_us()))
    wake = lib.simradio_wake_create(cb, None)
    lib.simradio_wake_at(wake, T_JOIN + 10_000)
    assert idle(lib, cond)["until"] == T_JOIN + 10_000
    cond.run(T_JOIN + 5_000)
    assert idle(lib, cond)["until"] == T_JOIN + 10_000
    assert fired == []
    cond.run(T_JOIN + 10_000)
    assert idle(lib, cond)["until"] is NEVER
    assert fired == [T_JOIN + 10_000]
    cb  # held for the library's sake


def test_the_host_learns_every_move_before_what_is_due_at_it(virtual):
    lib, cond = virtual
    join(lib, cond)
    idle(lib, cond)
    seen = []
    moved = MOVED_CB(lambda: seen.append(("moved", lib.simradio_node_us())))
    lib.simradio_on_advance(moved)
    wake_cb = WAKE_CB(lambda arg: seen.append(("wake", lib.simradio_node_us())))
    wake = lib.simradio_wake_create(wake_cb, None)
    lib.simradio_wake_at(wake, T_JOIN + 250_000)
    assert idle(lib, cond)["until"] == T_JOIN + 250_000
    cond.run(T_JOIN + 70_000)                   # half-way to the wake
    idle(lib, cond)
    assert seen == [("moved", T_JOIN + 70_000)]
    cond.run(T_JOIN + 70_000)                   # T does not move: nothing to learn
    idle(lib, cond)
    assert seen == [("moved", T_JOIN + 70_000)]
    cond.run(T_JOIN + 250_000)
    idle(lib, cond)
    assert seen == [("moved", T_JOIN + 70_000), ("moved", T_JOIN + 250_000),
                    ("wake", T_JOIN + 250_000)]
    lib.simradio_on_advance(MOVED_CB())
    moved, wake_cb  # held for the library's sake


def test_a_run_reaching_a_model_timer_fires_it(virtual):
    lib, cond = virtual
    join(lib, cond)
    idle(lib, cond)
    edges = []
    pin = PIN_CB(lambda ctx, p, level: edges.append(level))
    chip = lib.simradio_open(0, pin, None)

    def frame(*out):
        out = bytes(out)
        reply = ctypes.create_string_buffer(len(out))
        lib.simradio_transfer(chip, out, len(out), reply)
        return reply.raw

    frame(SET_DIO_IRQ_PARAMS, ALL_IRQ >> 8, ALL_IRQ & 0xFF, ALL_IRQ >> 8, ALL_IRQ & 0xFF, 0, 0, 0, 0)
    frame(SET_PACKET_PARAMS, PRE >> 8, PRE & 0xFF, 0x00, 10, 0x01, 0x00)
    frame(WRITE_BUFFER, 0x00, *range(10))
    frame(SET_TX, 0, 0, 0)
    tx = cond.expect("tx")
    assert tx["t0"] == T_JOIN
    toa = int(toa_seconds(10) * 1e6)
    assert tx["t_end"] - tx["t0"] == toa
    msg = idle(lib, cond)                           # the tx retracted the last idle
    assert msg["until"] == T_JOIN + toa
    time.sleep(0.05)
    assert frame(GET_IRQ, 0, 0, 0)[2:] == b"\x00\x00"   # no TX_DONE before its instant
    cond.run(T_JOIN + toa)
    idle(lib, cond)
    assert frame(GET_IRQ, 0, 0, 0)[3] & TX_DONE
    assert edges and edges[-1] == 1

    # A reception: the preamble is found four symbols in, and the sync word
    # and the header land at their instants in T.
    frame(SET_RX, 0xFF, 0xFF, 0xFF)
    idle(lib, cond)
    t0 = cond.t + 1000
    cond.t = t0
    cond.send({"type": "rx_begin", "t": t0, "slot": 0, "id": 5, "t0": t0,
               "t_pre": t0 + 60_000, "t_hdr": t0 + 100_000, "t_end": t0 + 250_000,
               "level": -80})
    found = idle(lib, cond)["until"]
    assert abs(found - (t0 + 4 * TSYM * 1e6)) <= 1
    cond.run(found)
    assert idle(lib, cond)["until"] == t0 + 60_000
    irq = frame(GET_IRQ, 0, 0, 0)
    assert (irq[2] << 8 | irq[3]) & PREAMBLE
    assert not (irq[2] << 8 | irq[3]) & (SYNC | HEADER_VALID)
    cond.run(t0 + 60_000)
    assert idle(lib, cond)["until"] == t0 + 100_000
    irq = frame(GET_IRQ, 0, 0, 0)
    assert (irq[2] << 8 | irq[3]) & SYNC
    assert not (irq[2] << 8 | irq[3]) & HEADER_VALID
    cond.run(t0 + 100_000)
    idle(lib, cond)
    irq = frame(GET_IRQ, 0, 0, 0)
    assert (irq[2] << 8 | irq[3]) & HEADER_VALID
    lib.simradio_close(ctypes.c_void_p(chip))
    pin  # held


def test_a_datagram_is_in_whole_before_a_host_wait_due_at_its_instant_runs(virtual):
    """A frame ends at T, another begins there, the ether says both in one
    datagram, and a host thread waits until T. Its wake runs once both are on
    the chip, after DIO1's rise has been told: it finds RX_DONE up and the
    new frame's energy. Woken as T first moved, it found the chip as far as
    the reader had got, and two runs of one seed could part at such an
    instant, on which of two threads the host ran first."""
    lib, cond = virtual
    join(lib, cond)
    idle(lib, cond)
    seen = []
    pin = PIN_CB(lambda ctx, p, level: seen.append(("pin", level)))
    chip = lib.simradio_open(0, pin, None)

    def frame(*out):
        out = bytes(out)
        reply = ctypes.create_string_buffer(len(out))
        lib.simradio_transfer(chip, out, len(out), reply)
        return reply.raw

    frame(SET_DIO_IRQ_PARAMS, ALL_IRQ >> 8, ALL_IRQ & 0xFF, ALL_IRQ >> 8, ALL_IRQ & 0xFF, 0, 0, 0, 0)
    frame(SET_PACKET_PARAMS, PRE >> 8, PRE & 0xFF, 0x00, 10, 0x01, 0x00)
    frame(SET_RX, 0xFF, 0xFF, 0xFF)                    # continuous: it stays in RX
    idle(lib, cond)
    t0 = cond.t + 1000
    t_end = t0 + 250_000
    cond.t = t0
    cond.send({"type": "rx_begin", "t": t0, "slot": 0, "id": 5, "t0": t0,
               "t_pre": t0 + 60_000, "t_hdr": t0 + 100_000, "t_end": t_end, "level": -80})
    for until in (idle(lib, cond)["until"], t0 + 60_000, t0 + 100_000):
        cond.run(until)
        idle(lib, cond)
    frame(CLEAR_IRQ, ALL_IRQ >> 8, ALL_IRQ & 0xFF)       # DIO1 down
    del seen[:]

    def woke(arg):
        irq = frame(GET_IRQ, 0, 0, 0)
        rssi = -frame(GET_RSSI_INST, 0, 0)[2] / 2
        seen.append(("wake", bool((irq[2] << 8 | irq[3]) & RX_DONE), rssi))
    wake_cb = WAKE_CB(woke)
    wake = lib.simradio_wake_create(wake_cb, None)
    lib.simradio_wake_at(wake, t_end)
    assert idle(lib, cond)["until"] == t_end
    lines = []
    for msg in ({"type": "rx_end", "slot": 0, "id": 5, "verdict": "clean", "rssi": -80, "snr": 7,
                 "payload": base64.b64encode(bytes(10)).decode()},
                {"type": "rx_begin", "slot": 0, "id": 6, "t0": t_end, "t_pre": t_end + 60_000,
                 "t_hdr": t_end + 100_000, "t_end": t_end + 250_000, "level": -70}):
        cond.seq += 1
        lines.append(json.dumps(dict(msg, t=t_end, seq=cond.seq)).encode())
    cond.t = t_end
    cond.sock.sendto(b"\n".join(lines), cond.station)
    assert idle(lib, cond)["seq"] == cond.seq          # both taken, and owed for
    assert seen[-1] == ("wake", True, -70.0)            # the whole instant, and last
    assert seen[:-1] and set(seen[:-1]) == {("pin", 1)}    # DIO1 told before it
    lib.simradio_close(ctypes.c_void_p(chip))
    pin, wake_cb  # held for the library's sake


def transmitting(lib, cond, payload=10):
    """A chip on the station, set up and sent into TX at T = cond.t: the
    chip, its frame helper, and the frame's time on air in µs."""
    lib.simradio_quiet_for_us.argtypes = [ctypes.c_void_p]
    lib.simradio_quiet_for_us.restype = ctypes.c_int64
    pin = PIN_CB(lambda ctx, p, level: None)
    chip = lib.simradio_open(0, pin, None)
    transmitting.pins.append(pin)

    def frame(*out):
        out = bytes(out)
        reply = ctypes.create_string_buffer(len(out))
        lib.simradio_transfer(chip, out, len(out), reply)
        return reply.raw

    frame(SET_DIO_IRQ_PARAMS, ALL_IRQ >> 8, ALL_IRQ & 0xFF, ALL_IRQ >> 8, ALL_IRQ & 0xFF, 0, 0, 0, 0)
    frame(SET_PACKET_PARAMS, PRE >> 8, PRE & 0xFF, 0x00, payload, 0x01, 0x00)
    frame(WRITE_BUFFER, 0x00, *range(payload))
    assert lib.simradio_quiet_for_us(chip) == -1      # standby: nothing on its way
    frame(SET_TX, 0, 0, 0)
    cond.expect("tx")
    return chip, frame, int(toa_seconds(payload) * 1e6)


transmitting.pins = []


def test_a_transmitting_chip_is_quiet_until_tx_done_lands(virtual):
    """simradio_quiet_for_us: while the chip transmits, the node time left
    until TX_DONE can be read; -1 once it has landed and in receive."""
    lib, cond = virtual
    join(lib, cond)
    idle(lib, cond)
    chip, frame, toa = transmitting(lib, cond)
    assert lib.simradio_quiet_for_us(chip) == toa
    idle(lib, cond)
    cond.run(T_JOIN + 1000)
    idle(lib, cond)
    assert lib.simradio_quiet_for_us(chip) == toa - 1000
    cond.run(T_JOIN + toa)
    idle(lib, cond)
    assert frame(GET_IRQ, 0, 0, 0)[3] & TX_DONE
    assert lib.simradio_quiet_for_us(chip) == -1
    frame(SET_RX, 0xFF, 0xFF, 0xFF)
    assert lib.simradio_quiet_for_us(chip) == -1
    lib.simradio_close(ctypes.c_void_p(chip))


def test_on_a_drifting_clock_quiet_ends_at_the_first_node_time_that_sees_tx_done(tmp_path):
    """20 ppm fast: the quiet ends at the first node time whose T has reached
    the frame's end, and not a microsecond sooner or later."""
    lib = load_copy(tmp_path, "libsimradio_quiet.so",
                    {"SIM_MESH_TIME": "virtual",
                     "SIM_MESH_CLOCK_PROFILE": "0:0,1000000000:1000020000"})
    cond = Conductor()
    cond.t = 400_003
    try:
        join(lib, cond)
        idle(lib, cond)
        chip, _, toa = transmitting(lib, cond)
        end = 400_003 + toa
        quiet = lib.simradio_quiet_for_us(chip)
        seen = lib.simradio_node_us() + quiet
        assert lib.simradio_node_to_conductor(seen) >= end
        assert lib.simradio_node_to_conductor(seen - 1) < end
        lib.simradio_close(ctypes.c_void_p(chip))
    finally:
        cond.sock.close()


def test_station_open_returns_only_once_the_welcome_has_set_t(virtual):
    """In virtual time the host goes on at the instant it joined, not at node
    time 0 before the welcome has landed."""
    lib, cond = virtual
    opened = []
    opener = threading.Thread(target=lambda: opened.append(lib.simradio_station_open(
        9, b"127.0.0.1", ("127.0.0.1:%d" % cond.port).encode())), daemon=True)
    opener.start()
    cond.expect("hello")
    time.sleep(0.1)
    assert opened == []                         # still waiting for the welcome
    cond.welcome()
    opener.join(5)
    assert opened == [0]
    assert lib.simradio_node_us() == T_JOIN


def test_the_host_floor_is_said_and_owes_an_idle(virtual):
    """simradio_host_floor: `floor` to the station once it has read a host's
    bytes, which returns only once the ether's run marked `floor` has brought
    it to the run's T (an ordinary run does not pass for it); `floor` to the
    tool once it has answered. Either is the station speaking."""
    lib, cond = virtual
    join(lib, cond)
    idle(lib, cond)
    done = []
    floor = threading.Thread(target=lambda: done.append(lib.simradio_host_floor(1)), daemon=True)
    floor.start()
    assert cond.expect("floor") == {"type": "floor", "sid": 9, "to": "station"}
    cond.run(T_JOIN + 500)                              # not the answer
    time.sleep(0.1)
    assert done == []
    cond.t = T_JOIN + 1000
    cond.send({"type": "run", "t": cond.t, "floor": 1})
    floor.join(3)
    assert done and lib.simradio_node_us() == T_JOIN + 1000
    idle(lib, cond)
    lib.simradio_host_floor(0)
    assert cond.expect("floor") == {"type": "floor", "sid": 9, "to": "tool"}
    idle(lib, cond)


def test_the_link_survives_a_lost_datagram_either_way(virtual):
    lib, cond = virtual
    seq = join(lib, cond)
    first = idle(lib, cond)
    # An idle the ether never heard: said again, a quarter second of wall on.
    started = time.monotonic()
    assert cond.expect("idle", timeout=1.0) == first
    assert 0.2 <= time.monotonic() - started < 0.6

    def raw(n, t):
        cond.sock.sendto(json.dumps({"type": "run", "t": t, "seq": n}).encode(), cond.station)

    # A message the station never got: the next waits, T stays, and an idle
    # for the last one applied asks for it.
    raw(seq + 2, T_JOIN + 2000)
    asked = cond.expect("idle", timeout=1.0)
    assert asked["seq"] == seq and lib.simradio_node_us() == T_JOIN
    # Sent again, in order: applied once each.
    raw(seq + 1, T_JOIN + 1000)
    assert cond.expect("idle", timeout=1.0)["seq"] == seq + 1
    raw(seq + 2, T_JOIN + 2000)
    assert cond.expect("idle", timeout=1.0)["seq"] == seq + 2
    assert lib.simradio_node_us() == T_JOIN + 2000
    raw(seq + 1, T_JOIN + 1000)                     # a duplicate: nothing moves back
    time.sleep(0.05)
    assert lib.simradio_node_us() == T_JOIN + 2000


def test_a_busy_station_reports_idle_anyway(virtual):
    lib, cond = virtual
    seq = join(lib, cond)
    started = time.monotonic()
    msg = cond.expect("idle", timeout=1.0)          # nobody called simradio_idle
    assert msg["seq"] == seq
    assert time.monotonic() - started < 0.5


def test_a_profile_makes_until_come_back_through_the_inverse(tmp_path):
    # Node time runs twice as fast as T for the first second of T, then at
    # T's own rate.
    lib = load_copy(tmp_path, "libsimradio_profile.so",
                    {"SIM_MESH_TIME": "virtual",
                     "SIM_MESH_CLOCK_PROFILE": "0:0,1000000:2000000"})
    cond = Conductor()
    cond.t = 400_000
    try:
        join(lib, cond)
        idle(lib, cond)
        assert lib.simradio_now_us() == 400_000
        assert lib.simradio_node_us() == 800_000
        wake = lib.simradio_wake_create(WAKE_CB(lambda arg: None), None)
        lib.simradio_wake_at(wake, 1_000_000)           # node time
        assert lib.simradio_node_to_conductor(1_000_000) == 500_000
        cond.run(450_000)
        assert idle(lib, cond)["until"] == 500_000       # f⁻¹(1 000 000)
        lib.simradio_wake_at(wake, 2_500_000)
        cond.run(460_000)
        assert idle(lib, cond)["until"] == 1_500_000     # past the last point, slope 1
    finally:
        cond.sock.close()


def test_a_wake_on_a_drifting_clock_fires_once_its_node_time_has_come(tmp_path):
    """A crystal 20 ppm fast: node time runs from T by a slope that is no
    whole ratio. A wake at node time n comes back as the first T whose node
    time has reached n, and fires there, once; a microsecond of T earlier
    the node time is still short of it."""
    lib = load_copy(tmp_path, "libsimradio_drift.so",
                    {"SIM_MESH_TIME": "virtual",
                     "SIM_MESH_CLOCK_PROFILE": "0:0,1000000000:1000020000"})
    cond = Conductor()
    cond.t = 400_000
    fired = []
    try:
        join(lib, cond)
        idle(lib, cond)
        on_wake = WAKE_CB(lambda arg: fired.append(lib.simradio_node_us()))
        wake = lib.simradio_wake_create(on_wake, None)
        n = 123_456_789
        t = lib.simradio_node_to_conductor(n)
        assert (t - 1) * 1_000_020 // 1_000_000 < n <= t * 1_000_020 // 1_000_000
        lib.simradio_wake_at(wake, n)
        cond.run(450_000)
        assert idle(lib, cond)["until"] == t
        cond.run(t - 1)
        idle(lib, cond)
        assert lib.simradio_node_us() < n and not fired
        cond.run(t)
        deadline = time.monotonic() + 2.0     # the link's thread takes the run
        while not fired and time.monotonic() < deadline:
            time.sleep(0.005)
        assert fired == [n]
        assert idle(lib, cond)["until"] is NEVER
    finally:
        cond.sock.close()


def test_a_standby_within_the_tcxo_start_up_stops_it_and_the_next_one_starts_afresh(virtual):
    """STDBY_RC while the TCXO starts stops the reference: the state reads
    STDBY_RC at once, ready then, and the next command that needs the
    oscillator waits a whole start-up of its own, 5 ms from that command."""
    lib, cond = virtual
    join(lib, cond)
    idle(lib, cond)
    pin = PIN_CB(lambda ctx, p, level: None)
    chip = lib.simradio_open(0, pin, None)

    def send(*out):
        out = bytes(out)
        lib.simradio_transfer(chip, out, len(out), ctypes.create_string_buffer(len(out)))

    send(SET_DIO3_TCXO, 0x02, 0x00, 0x01, 0x40)     # 1.8 V, 320 steps of 15.625 us: 5 ms
    t = cond.t
    send(SET_RX, 0xFF, 0xFF, 0xFF)
    state = cond.expect("state")
    assert (state["mode"], state["ready_at"]) == ("FS", t + 5000)
    assert idle(lib, cond)["until"] == t + 5000
    cond.run(t + 2000)                              # 2 ms into the start-up
    idle(lib, cond)
    send(SET_STANDBY, 0x00)
    state = cond.expect("state")
    assert (state["mode"], state["t"], state["ready_at"]) == ("STDBY_RC", t + 2000, t + 2000)
    assert idle(lib, cond)["until"] is NEVER, "the start-up stopped with the reference"
    send(SET_RX, 0xFF, 0xFF, 0xFF)
    state = cond.expect("state")
    assert (state["mode"], state["ready_at"]) == ("FS", t + 7000), "a whole start-up again"
    assert idle(lib, cond)["until"] == t + 7000
    cond.run(t + 7000)
    assert cond.expect("state")["mode"] == "RX"
    lib.simradio_close(ctypes.c_void_p(chip))
    pin  # held for the library's sake
