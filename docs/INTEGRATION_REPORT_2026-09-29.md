# Integration report, 29 September 2026

What this fork's `integration/main` takes from our own simulators and tools
into SIMesh, why, how it was checked, and what was left out. The capability
matrix it follows is `docs/CAPABILITY_MATRIX_2026-09-29.md`. The sweep behind
its results ran on 29 and 30 September.

## Summary

- **What went in.** The fixes, models, options and tools below, all on `integration/main`. A model or option is off unless asked for.
- **One seed, one run, and fast.** A virtual-time run keeps every station on the conductor's T:
  - For our stations two runs of one seed are one record at any pace: five runs each of two backbone seeds, two runs of a Berlin seed at 1.2 million record lines each, and a seed paced at 10x against full speed.
  - Nothing the stations say is lost to a full socket buffer. Upstream's ether dropped datagrams in 4 of its 9 mixed-stack Berlin runs.
  - A Berlin run of an hour of run time takes 286 s of wall on average in the grid. It took about 1370 s here before this work, and upstream capped at 10x takes 636 s.
- **Found by the sweep, and fixed.**
  - Our station handed a host tool its turn back early (the reticulum project's 882c8cc).
  - A station could see an instant in parts.
  - simd left a failed request unanswered.
- **Upstream is not repeatable.** One backbone seed delivered 87.5 to 98.6 % over six upstream runs, and 99.4 % on the fork every time. The fork against upstream is therefore a comparison with samples:
  - even on Berlin and on the flat layout;
  - on the densest backbone, 105 messages the fork delivered and upstream did not, against 3, in each of three seeds.
- **The keys, exactly.**
  - The CRC band at 3 dB costs 5 points of delivery on Berlin and 10 on the backbone, as it models.
  - Bench capture adds 1 and 3.
  - Clocks 20 ppm off change little.
- **Mixed stacks.** On the fork our transports delivered 1098 messages that reticulous's did not, against 575. Most of it is where reticulous's transports spent their 100 s of air an hour on the hailing channel and held their transmissions; both stacks stayed within EN 300 220. A mixed run is not yet one record per seed (backlog).

## Sources

- **SIMesh**: upstream main `50e2c31`, which this fork's `main` is.
- **Our upstream PR branches**: ten branches on `01d9b1f`.
- **mesh**: `c2ff69d`.
- **planner**: `bd1019d`. Its crates are those of `830f221`. The survey's `f3d897a` became `06ff759` in planner's rewrite of 29 September.
- **reticulum**: the reticulum project's `tools/rnscale`, `tools/rncapture`, `tools/simether` and `fw/simesh`, at `feat/supe` `b2b4302`. The station of step 4's grids is `fw/simesh` on `perf/sim-quiet` `882c8cc`.

## What went in

Every item is on `integration/main`. A fix changes what an existing scenario
does and has no switch. A model or option is off unless a key or a flag asks
for it; with every key off, an existing geodata or nodeset is read and
written byte for byte as before.

### Fixes

| Commit | What | From |
|---|---|---|
| 1106a99 | PreambleDetected fires 4 symbols into a frame, not at its sync word. Firmware that senses through the demodulator was blind for the whole preamble. | PR #10 |
| b4c2c7a | SNR saturates at +12 dB instead of wrapping in its signed byte, where a link 54 dB over the noise read −10 dB. | PR #1 (the SNR half; the RSSI half is upstream) |
| 1e175b0 | A slot that starts listening mid-frame is judged then. While at least 4 symbols of preamble remain it can lock on; otherwise it is told the frame's energy. A receiver that leaves RX or is retuned mid-frame gets no rx_end, and the record none. | PR #9, and two defects found on the way |
| 0f051d6 | The chip's instantaneous RSSI reads the sum of the frames on the air, as the ether's busy test does, not the strongest one. | defect |
| 0ea5c67 | A run records its medium (noise figure, rule) and seed. The analysis tools had always assumed 6 dB, and a random seed was nowhere to be found again. | defect |
| fab1422 | The reticulum project's station is sent a message once and not waited on; a timeout in virtual time had sent it again. A traffic run now counts its messages from its own logs. | defect, and a new counter |
| 7191383 | The page's Save visible as keeps the offsets it can keep, where it failed on any offset to a node off the geodata. | defect |
| 94c315b | A wake under a clock profile comes at the first T whose node time has reached its deadline. Rounded down both ways, a slope off a whole ratio woke the host a microsecond early, it asked for the same instant again, and a drifting station never came up. | defect, found in step 4 |
| 1a2ab75 | An exception in one message no longer drops the rest of the ether's batch at that instant. | defect |
| b95da20, b0622f8, 9b9add3 | seq.py and compare.py tie a reception to its frame by the ether's own number, not by the last `tx` recorded. That was wrong for 7–9 % of frames when several stations sent at one T, and for every late listener. airtime.py and links.py read real-time records and key on the frame; seq.py reads stamps with their date. | defects |
| f665f8f | compliance.py labels a worst hour in run time, not in a station's own clock. | defect |
| f6e399f, 03ba046, 7c6245a | Node names YAML would misread (`no`, `010`) are written quoted. NaN and infinity are refused where they are read. A malformed `offsets` entry is a StoreError. | defects |
| 99cf5cc | planner-web falls back to the near-field model only under P.1812's 0.25 km floor; any other refusal had come back as a confident near-field figure. | defect |
| e6d0146 | The docs say a renamed node's row is recomputed, as the code does. | docs |
| 7f573db, eef5442, 779034f, 1962382, 81ca479, cbb7e01 | A ground or node figure that is no number is refused by its key. An antenna's angles must be finite. An offset between more than two nodes is refused. A run's edits refuse NaN and infinity, where one had made every later write fail. compare.py reads a stamp with its date. | defects |
| 74e71e9 | The referee counts transmissions begun at one instant, which no carrier sense can prevent, apart. | tool |

### Models and options, off unless asked for

| Commit | Key or flag | What | From |
|---|---|---|---|
| 147d65e | `--crc-margin-db` | The CRC band: a frame m dB over its threshold, m under the band, fails with probability 1 − m/band, one seeded draw per frame and receiver. SIMesh's INTERNALS planned it. | PR #7 |
| 4362f8e | `--bench-capture` | Same-SF collisions as a bench measured them (289 collisions, SF7/125 kHz), over main's summed interference. The lock and both verdicts read one seeded draw. | PR #4 |
| ee375a0 | `--clock-ppm` | Each station's crystal off by a draw within ±ppm, hashed from the seed and its name, through the conductor's clock profile. Virtual time only. | rnscale's ±20 ppm |
| 1e9ab0c | geodata `shadowing_db`, `shadowing_seed` | One static log-normal draw per pair laid over the tables, by node name. | PR #2 |
| bf81004 | nodeset `links:` | A pair's loss stated outright; antennas and offsets still apply. | PR #5 |
| e630c8a, fa256fe | pack geodata `loc_pct` | Tables at a chosen percentage of locations through SIMesh's planner copy, so a median can carry shadowing. | planner's `loc_pct` |

### Tools

| Commit | What | From |
|---|---|---|
| 4864979 | `testbed/referee.py`. Over a finished run's record it reports carrier sense (told, told late, never told, by the window the sender began in), frames nobody was told of, and collisions (hidden senders against senders in earshot). | PR #8's audits |
| 4b27a10 | `testbed/convert_scenario.py`: the scenario format before nodesets into geodata, a nodeset and simd flags, keeping every pair's distance. | new |

### The run's clock, and its speed

Step 4's first grids took up to half an hour a run on the Berlin layout, two
ptys a station, on a host shared with others. Before rerunning them, a
virtual-time run was made to keep every station exactly on the conductor's T,
two runs of one seed the same, and then fast; in that order, because a fast
run that drifts from T is no run. Everything below is a fix or costs nothing
in what a run does: none has a switch.

**One seed, one run.** Two runs of one seed now give the same record, line
for line, and the same delivery, for a run of our stations; a run with
reticulous stations is not there yet (backlog, item 6). Seven ways a run's
outcome depended on how the host scheduled it were found by comparing such
pairs before the grids, and closed; the grids found an eighth (below):

| Commit | What |
|---|---|
| 0bb5e13, fcb590a, cf53137, 0cf5df9 | A host tool and its station take turns with T. The station says when its host door changes hands; T stands while the tool has it; bytes the station reads are taken at the run's T, not at the stale T it was last told. |
| 84824d7, 790015b, 96e5330 | A station is joined to T before anything is asked of it, and what waits for its hello goes on at the hello's T. |
| e6e9e7f, 28972e7 | A script's driver takes turns with T: T stands while the driver has something to do, and goes once everything it sent is under way. |
| 354ea87 | A station is not idle while something is ready for one of its blocked threads. |
| 950b69f | What stations say at one instant is recorded in station order. |
| 3a7515b | A thread waiting on the disk holds the busy watchdog back. |
| ca182dc | Two runs write a cached file through temporaries of their own. |

**Nothing lost.** On the Berlin layout some 170 stations answer one frame's
end at one instant, and the ether's socket buffer (the kernel's, capped by
`net.core.rmem_max`, 208 KB here) cannot hold them all. What does not fit is
dropped: an idle is said again, but a lost `state` or `tx` is one the medium
never heard, and the run with it is not the run. SIMesh's ether heard 444 of
1500 hellos sent while its loop was busy. The fork's conductor takes datagrams
off the socket on a thread of its own (55a013a, 98828d3), and simd reports
any datagram the kernel drops on the ether's socket all the same; every run
of the fork's arms in step 4's grids reported none.

**Fast.** The conductor of a virtual-time run (every idle, T, every `run`) is
in Rust (78d81d7, `simesh build ether`), in simd's own thread, with the
medium and everything else still in `ether.py` and called at the same points:
its records are those of the Python conductor, which stays, line for line. A
barrier reads only the consoles something was printed on (8282f49, 6abf297),
roles are asked only while a page shows them (3394b2d), and a console need not
be a pty (d4744cc). The stations of our own stack sleep through the engine's
polls that cannot act, woken at the first that can, by a frame or by their
host, with their records unchanged line for line (the reticulum project's
`perf/sim-quiet`, `SIMESH_POLL_SKIP`).

On 28 stations and 34 minutes of run time, one seed, a run took 101.7 s of
wall before any of this and 9.6 s after it; a Berlin run of an hour of run time, about
1370 s before and 212 s after, with no pty at all.

Three more changes cost nothing in what a run does:
- one encoding of each message serves both the record and the wire (e2f3390);
- frame-by-frame page events go only to a page that takes them (b041725);
- a console nothing acts on is read as it comes, not at each instant (bd612f1, `console_acted_on`).

Together they take a Berlin run with 300 s of traffic from 154.5 s of wall to 129.4 s; its record and traffic results are the same through the workload.

A few more are fixes found on the way:
- A script that starts a simulation of its own gives it its rules once its driver has T (6367ed5). Before, its stations ran for as long as attaching took on the host.
- The core counts what the kernel drops on its socket as it reads (7a719c6).
- A station is recorded up at the T it came up, not at the T simd last knew (aa5e5a3).
- A request that cannot be done is answered with its error (f41ea81). simd had told the page and answered nothing, so a script waited on that request for ever. In a virtual-time run T then went on without its driver as fast as it goes, and wrote its record until someone stopped it.

**Found by the grids: an instant taken in parts.** Two runs of one seed still parted now and then on a loaded host: one elev seed in six, where a station entered RX 5 ms apart.
- A station's chip library moved its T to a message's instant and then applied the message. Moving T woke the host's waits due there.
- A host thread waiting until T could look at its chip before the frame that ended at T had raised RX_DONE, or after. Which it was depended on how the host scheduled two threads.
- No station ran off T, but which of two orders an instant took was the host's to decide.

The fix has two halves. Each datagram is now applied whole before the host is told of any of it: its due waits, the advance hook and DIO1 all wait (68150ff). And everything the barrier has for a station at one go is sent as one datagram, a message a line, to a station that says in its hello that it takes them (27cb922). On the stack of the grids, five runs each of two elev seeds, and two runs of a Berlin seed (1,231,947 record lines), are each one record.

**Found by the grids: a host's turn given back too early.** The fault was in our station, not in SIMesh.
- The station told SIMesh it was answering its host (the floor, above) before every frame it wrote to the host, including mail pushed to the host as it landed.
- When mail landed while a tool's line still waited for the station's poll, the floor went back to the tool first. T then stood, and the line went unread, until the tool gave up after 8 s of wall.
- Where two such waits ran past the testbed's 10 s, the send failed, the script waited for an answer (f41ea81 above), and T ran away.

The first grids stalled this way in about one run in two, and several were void. The reticulum project's 882c8cc counts only a reply to the host's own frame as an answer.

The grids below ran on 27cb922, with our station at 882c8cc built against its chip library. They replace every earlier grid on this stack.

### Tests

| Suite | Upstream `50e2c31` | `integration/main` before the run's clock and speed (`bd78b37`) | `integration/main` now (`27cb922`) |
|---|---|---|---|
| ether | 52 | 74 | 97, every virtual-time test on both conductors |
| radio | 31 | 36 | 42 |
| testbed | 153, 5 skipped | 216, 5 skipped | 226, 5 skipped |
| planner-web | 17 | 19 | 19 |

## Where ours was better, and by how much

Three layouts, each run with every arm: a flat one of 5 nodes (`mixed`), a
backbone of 28 on a synthetic elevation (`elev`), and a Berlin city of 173
(`city`: 28 transports, 145 households). Each at three path-loss exponents
(2.7, 3.0, 3.3), with and without 7 dB of static shadowing, and three seeds;
every arm of one seed sends the same 360 messages at the same instants, one
every five seconds for 30 minutes after two announce rounds, then drains for
ten. Delivery is counted from each sender's own logs; *live* is a message
whose proof came back during the traffic, *drained* by the end of the drain.
Two arms of one cell pair message by message. Each comparison gives the
messages only one arm delivered, pooled over the cells both ran, with the
exact McNemar p. It also gives how many cells moved each way: a pooled
figure one cell carries is not a result across the sweep.

| Arm | Simulator | Stations | Keys |
|---|---|---|---|
| Up | upstream `50e2c31` | ours, against upstream's chip library | — |
| I0s | this fork | ours | none: the fixes alone |
| Ibs, Ics, Ids | this fork | ours | `--bench-capture`, `--crc-margin-db 3`, `--clock-ppm 20` |
| Bu | upstream `50e2c31` | reticulous transports, ours elsewhere | — |
| B0s | this fork | reticulous transports, ours elsewhere | none |

Every arm runs on the stack above ("The run's clock, and its speed"): stations
without ptys, sleeping through the polls they cannot act at, and for the
fork's arms the conductor in Rust. Our station is the reticulum project's
882c8cc, built against each arm's chip library. In upstream's arms it is
1f3d41a, which differs from 882c8cc only in the floor fix, and that does
nothing there, since upstream's chip library has no floor. Upstream's arms keep upstream's simulator
as it is, with two harness patches that change no run: our station's send is
not waited on, as for the fork (fab1422), and a station's console is a pipe
rather than a pty. They run paced at 10x, because upstream's script driver
waits in wall time, and at full speed T runs away from it (question 10 below);
the fork's arms run as fast as they go, which their determinism allows.
Reticulous (44dedc9, iface-lora e9869e9, rns b789f1b) is built against each
arm's own chip library, so the chip fixes here reach its stations too; it
needs its build image's userland, so those runs are containers of their own,
and it is given a device password and an LXMF identity at its first boot, as
it needs to run Reticulum at all.

### One seed, one result, and upstream's spread

The fork's arms of our stations give one record per seed, whatever the host does:
- Five runs each of two elev seeds (four on their own, one in the grid) and two of a Berlin seed were each one record (above).
- An elev seed paced at 10x gave the same record as at full speed, line for line through the drain (266,281 lines).

Upstream's arm does not. One elev seed (exponent 2.7, no shadowing) gave these drained deliveries on upstream's simulator:

| Run | Pace (a cap) | Drained | Live | Frames in the traffic |
|---|---|---|---|---|
| the grid's | 10x | 87.5 % | 85.8 % | 4044 |
| again | 10x | 96.1 % | 95.8 % | 2148 |
| again | 10x | 95.8 % | 95.6 % | 2275 |
| | 5x | 98.6 % | 97.8 % | 1695 |
| | 2x | 95.6 % | 93.6 % | 2769 |
| | 1x, the wall clock's | 96.1 % | 95.0 % | 2035 |
| the fork's, every run | any | 99.4 % | 97.8 % | 1546 |

The pace is a cap. Upstream's own simulator held about 1.3x during the traffic under a 5x cap on this host, and 0.6x under 1x. Upstream's conductor lets T move on while a station still has work in hand, the defect 354ea87 closes in the fork. So the faster it runs, the more of that work lands late. The 87.5 % run shows what follows:
- 270 frames were deferred on a busy channel and dropped when their retry window closed. The fork's run of that seed dropped 4.
- Messages went without their proofs and were sent again, each resend asking for its path afresh.
- Its traffic phase carried 4044 frames against the fork's 1546, and 2000 of the 4044 answered path requests.

So a single upstream run per cell is one sample of a spread that depends on the host, and "fixes vs upstream" below measures that as well as the fixes. What it shows is where the fork stood against upstream's samples, not the fixes' effect alone. The comparisons between the fork's own arms (the keys) are exact: each pairs two deterministic records.

### Delivery

Drained delivery, the mean over the seeds of each cell (runs), by layout: the
path-loss exponent (2.7, 3.0, 3.3) and the shadowing (0 or 7 dB). Upstream's
arms run without shadowing, which upstream does not have. Then each
comparison: the messages only one arm delivered, pooled over the cells both
ran, the exact McNemar p, and how many cells moved each way.

**Berlin (`city`, 173 stations)**

| Arm | 2.7, 0 | 2.7, 7 | 3.0, 0 | 3.0, 7 | 3.3, 0 | 3.3, 7 | All |
|---|---|---|---|---|---|---|---|
| Up | 66.0 % (3) | — | 52.0 % (3) | — | 42.0 % (3) | — | 53.4 % (9) |
| I0s | 65.9 % (3) | 66.9 % (3) | 53.9 % (3) | 52.2 % (3) | 42.0 % (3) | 45.9 % (3) | 54.5 % (18) |
| Ibs | 66.5 % (3) | 65.8 % (3) | 53.8 % (3) | 54.6 % (3) | 44.4 % (3) | 48.2 % (3) | 55.6 % (18) |
| Ics | 59.3 % (3) | 63.2 % (3) | 49.3 % (3) | 48.5 % (3) | 36.6 % (3) | 40.6 % (3) | 49.6 % (18) |
| Ids | 66.3 % (3) | 66.2 % (3) | 53.0 % (3) | 53.6 % (3) | 43.1 % (3) | 44.1 % (3) | 54.4 % (18) |
| Bu | void | — | 50.6 % (2) | — | 42.4 % (3) | — | 45.7 % (5) |
| B0s | 49.9 % (3) | 42.5 % (3) | 51.9 % (3) | 46.6 % (3) | 44.6 % (3) | 43.0 % (3) | 46.4 % (18) |

| Comparison | Only the first | Only the second | p | Cells each way |
|---|---|---|---|---|
| Up → I0s: the fixes against upstream | 197 | 216 | 0.38 | I0s 5, Up 3, tied 1 |
| I0s → Ibs: bench capture | 475 | 546 | 0.028 | Ibs 10, I0s 6, tied 2 |
| I0s → Ics: the CRC band, 3 dB | 678 | 360 | 3.5e-23 | I0s 16, Ics 2 |
| I0s → Ids: clocks 20 ppm off | 481 | 475 | 0.87 | 9 each |
| Bu → B0s: reticulous transports, upstream against the fork | 129 | 173 | 0.013 | B0s 4, Bu 1 |
| B0s → I0s: reticulous transports against ours, on the fork | 575 | 1098 | 6.7e-38 | I0s 15, B0s 3 |
| Bu → Up: reticulous transports against ours, on upstream | 162 | 171 | 0.66 | Up 4, Bu 1 |

**The synthetic backbone (`elev`, 28 stations)**

| Arm | 2.7, 0 | 2.7, 7 | 3.0, 0 | 3.0, 7 | 3.3, 0 | 3.3, 7 | All |
|---|---|---|---|---|---|---|---|
| Up | 89.9 % (3) | — | 78.9 % (3) | — | 60.1 % (3) | — | 76.3 % (9) |
| I0s | 99.4 % (3) | 91.9 % (3) | 79.1 % (3) | 89.0 % (3) | 61.2 % (3) | 63.1 % (3) | 80.6 % (18) |
| Ibs | 99.0 % (3) | 98.7 % (3) | 84.4 % (3) | 89.2 % (3) | 63.5 % (3) | 67.4 % (3) | 83.7 % (18) |
| Ics | 89.2 % (3) | 93.7 % (3) | 67.6 % (3) | 71.6 % (3) | 44.8 % (3) | 53.8 % (3) | 70.1 % (18) |
| Ids | 99.0 % (3) | 94.0 % (3) | 81.8 % (3) | 88.7 % (3) | 60.5 % (3) | 66.1 % (3) | 81.7 % (18) |

| Comparison | Only the first | Only the second | p | Cells each way |
|---|---|---|---|---|
| Up → I0s | 168 | 284 | 5.4e-08 | I0s 5, Up 4 |
| I0s → Ibs | 238 | 440 | 7.4e-15 | Ibs 13, I0s 4, tied 1 |
| I0s → Ics | 922 | 243 | 1.9e-93 | I0s 17, Ics 1 |
| I0s → Ids | 312 | 382 | 0.0088 | Ids 12, I0s 4, tied 2 |

**The flat layout (`mixed`, 5 stations).** Every arm drained 99.8–100 % in every cell. The one exception is the CRC band at exponent 3.3 with shadowing: 92.4 %, and 80 messages lost over the sweep. The comparison with upstream is even drained (4 against 2). By live delivery upstream's arm got 21 messages to the fork's 2, but that is its measure, not its radio: upstream's driver marks the end of the traffic when the host gets to it, 2 to 11 s of T past the plan in these runs, where the fork's lands on it (question 10). Proofs that come back in those seconds count as live on upstream only.

### What the sweep says

**The fork's keys** are exact comparisons, each a pair of records the seed alone decides.
- **The CRC band** costs 4.9 points of drained delivery on Berlin and 10.5 on the backbone, in 16 and 17 cells of 18. That is the band doing what it models: a frame within 3 dB of its threshold is lost with probability 1 − m/3.
- **Bench capture**, the bench's same-SF capture rule, gives 3.1 points on the backbone (13 cells for, 4 against) and 1.1 on Berlin (p 0.028, 10 cells for, 6 against).
- **Clocks 20 ppm off** change nothing on Berlin, and give the backbone 1.1 points (12 cells for, 4 against, p 0.009).

**The fork against upstream** measures upstream's spread as well (above).
- **Berlin:** even (216 against 197, p 0.38).
- **Flat layout:** even, drained.
- **Backbone:** 284 against 168 for the fork, pooled, but all of it at exponent 2.7. There the fork delivered 105 messages upstream did not and upstream 3 the other way, in each of three seeds. At 3.0 and 3.3 the count was 179 against 165, two cells for the fork and four for upstream.

The seed of the table above is one of those three. Its upstream runs span 87.5 to 98.6 % against the fork's 99.4 %, so on the densest backbone the fork is above every upstream run seen. Which of the fixes does it, and how much of it is upstream's clock, this sweep cannot say. The fork changed its chip library and its simulator's protocol together, so neither half runs with upstream's other half.

**Mixed stacks**, reticulous transports with our stations elsewhere, on Berlin:
- **Upstream's arm lost datagrams in 4 of its 9 runs**, each of them void: 1642, 936 and 1613 at exponent 2.7, which is every run there, and 44 at 3.0. The fork's lost none, in this arm or any other.
- **Reticulous transports on the fork against on upstream:** 173 against 129 messages for the fork (p 0.013, 4 cells of 5).
- **Our transports against reticulous ones:** on the fork, 1098 against 575 for ours (15 cells of 18). 83 % of the net difference lies at exponent 2.7, where upstream's runs were void, so upstream has no figure there. At 3.3 without shadowing, reticulous's transports delivered a little more (127 against 99). On upstream, over the cells it has, the two are even (171 against 162).
- **Where the gap comes from.** In the dense cells' first-seed runs, 16 and 17 of reticulous's 28 transports spent their hailing channel's 100 s of air an hour and held their transmissions, with 353 and 402 warnings a run. At exponent 3.3 only 2 and 5 did. Both stacks stayed within EN 300 220 in the two dense-cell runs checked (`compliance.py`). Reticulous's cap is polite spectrum access's figure, where the band's 10 % duty cycle would allow 360 s. Whether a transport should hold at 100 s is its design's call; this sweep shows what the cap costs in delivery where traffic is dense.
- **Samples of a spread.** A mixed run is not yet one record per seed: four runs of one seed delivered 41.7 to 48.1 % (backlog, item 6). So its cells are samples of a spread too, though a narrower one than upstream's.

**Speed**, mean wall per run:

| Arm | Berlin, an hour of run time | Backbone, 34 minutes | Flat, 34 minutes |
|---|---|---|---|
| Up, capped at 10x | 636 s | 482 s | 787 s |
| I0s, as fast as it goes | 286 s | 56 s | 8 s |
| Ids (drifting clocks) | 435 s | 65 s | 8 s |
| Bu, capped at 10x | 929 s | — | — |
| B0s | 508 s | — | — |

## What was left out, and why

- **PR #3, a receiver taken off one frame by another** (`feat/receiver-takes`): main's ether owns the lock since `a339076`.
- **PR #6, other spreading factors apart** (`feat/sf-orthogonality`): main applies Croce's Table II, summed per class and with the SF7 row right, always.
- **PR #5 as written**: SLT1 tables supersede it. What main lacked, a loss stated outright, came back as the nodeset key.
- **PR #8's duty section**: `compliance.py` holds each node to EN 300 220-2 Annex B, e.r.p. and polite spectrum access. Ours put 863–865 MHz at 1 % and checked no power.
- **`wip/virtual-time`** (simd_remote and the external simether): it talks to the geometric ether main removed. simether would need loss tables first.
- **iface-lora's PRs #2–#5**: since iface-lora e651bca its host build links SIMesh's chip library, so the chip fixes above reach reticulous stations built against this fork. Three `integration/medium` commit messages (1106a99, b4c2c7a, 0f051d6) wrongly say iface-lora needs the same change.
- **From mesh**:
  - its capture (+1 dB, no timing rule, per interferer);
  - its SNR without a noise figure;
  - its 100 ms polling grid.

  SIMesh's are better. Its energy model and outside interferer are candidates (below).
- **From planner**:
  - The multi-wall law as a whole-path model: planner's own file warns against using it for a region it has not seen.
  - Its interior increment (1.26 dB per metre of building, capped at 25 m) is a candidate for indoor ends on packs. No pack was at hand to test it.

## Candidates for a next round

1. Planner's interior increment for an indoor antenna on a pack, in place of the P.2109 median, behind a key.
2. Planner's `site_pair_loss` as SIMesh's pair-table path: both directions, `loc_pct`, `time_pct` and frequency as inputs, and no two HTTP requests per pair.
3. Per-node power from the sub-band's e.r.p. cap (planner's `conducted_dbm_for_erp`), so a run is compliant by construction.
4. An energy tool over the record, after mesh's model but charged by mode (RX, CAD, standby, TX by power), which only SIMesh's record allows.
5. An outside interferer: a foreign station on the shared band, heard through the tables like any node.
6. mesh's analytic reachability as a ceiling a protocol's delivery can be held against.
7. Fast per-frame fading, a key of its own on top of the static shadowing.

## Backlog: defects found and not yet fixed

Items 1 to 5 and 7 are robustness against what a file, an import or a
page's message may hold: each was confirmed by reading or running the code,
and none touches a well-formed run. Item 6 is the sweep's own open finding.

1. **A run's offset edit truncates.** `simd.py` `do_nodeset_offset` reads an offset's `between` as its first two names, so a third is dropped and a string "ab" is taken as nodes a and b. A file's offsets refuse both since 1962382.
2. **An OverflowError closes the page's socket.** `handle()` does not catch it, and `ws_page` has no catch-all. `do_plan`'s `int(phase["until"])` with `Infinity` or `1e999` gets there.
3. **The CSV imports read `nan` and `inf` as numbers** (`nodeset.py` `_number`, and the imports' JSON `height_m` in `front.py`).
   - A NaN latitude imports, and the write then fails.
   - An infinity leaves the request unanswered.
   - A NaN transmit power silently becomes the board's maximum −9.
4. **A failed edit can leave part of itself behind.** `Nodeset.add_node` cleans up only on a StoreError, so a TypeError leaves the node in. `set_node`, and simd's move, apply fields in order, so a refused later field leaves the earlier ones applied, unmarked.
5. **Synthetic `exponent` and `extent_m` are not held to nine decimals** as the newer keys are. A hand-written 2.71828182846 reads back as 2.718281828 in a run's copy, and its table check fails.
6. **A mixed run is not one record per seed.**
   - Two runs of one Berlin seed with reticulous transports, even with one fixed epoch, part where a reticulous station transmits in one run and another one, or none, in the other (at T 4957 s in one pair, 5029 s in the other).
   - Four runs of that seed delivered between 41.7 % and 48.1 %. Runs of our stations alone are each one record.
   - Not yet traced. The first suspects are the order in which a reticulous station's tasks draw from its seeded randomness, and how its FreeRTOS tasks interleave at a tick.
7. **Minor:**
   - an offset from a node to itself is accepted;
   - `id: 1.5` reads as 1;
   - `height_m: yes` reads as 1.0;
   - `after` and `stagger` take Infinity;
   - `do_levels` passes a NaN frequency on to the ether.

## Open questions for Rop

Drafted here, not sent; whether and how to send them is the fork owner's call.

1. **A late stronger frame.** Main lets a frame 6 dB or more stronger take a receiver anywhere in an earlier frame's air. The bench saw a late frame about 2 dB stronger spoil both (six of six), where main's rule loses both as well. No bench covers 6 dB and more. Is the capture anywhere in the payload deliberate, or should the lock close once the earlier frame's header is in?
2. **Energy below the sense threshold.** The ether tells a slot of energy only from −81 dBm, a transmitter's CCA threshold under 100 mW. A firmware whose threshold is lower cannot see the frames in between; EN 300 220-2 as the reticulum project reads it puts the threshold at −85 dBm from 100 to 500 mW, which 869.4–869.65 MHz allows. A real receiver's RSSI reads down to its noise. Tell every in-band frame to a listening slot and let the chip and firmware decide?
3. **The idle floor.** The chip reads −110 dBm idle; the ether's noise at 125 kHz and 6 dB NF is −117 dBm. Align them, with the noise figure in the welcome?
4. **CAD** is ideal: `cadDetPeak` and `cadDetMin` are stored and never used, and a CAD finds any frame it was told of, energy included. Worth a detection curve?
5. **Defaults for the new options.** INTERNALS planned the CRC band at 3 dB; here it is off unless given. Bench capture is measured at SF7/125 kHz only. Which should become defaults, and on what evidence?
6. **Shadowing is drawn by node name, not station id**, so a nodeset keeps its ground when ids change; our old branch drew by id. Agree?
7. **The fixes here that upstream has too**: the SNR wrap, the preamble timing, late listeners, the summed RSSI, the abandoned receptions, the run's medium, the kind's blocking send, the conductor's rounding. Each would be a PR of its own, with its test, if the fork owner chooses to send them.
8. **The upstream defects found and fixed in the fork** (the table above) likewise.
9. **Lost datagrams.** The ether's socket buffer overflows when a hundred stations or more answer at one instant, and the kernel drops what does not fit, a `tx` among them. A reader thread of its own, or at least a warning when the ether's socket drops anything, would keep large runs honest. In step 4, upstream's ether dropped datagrams in 4 of its 9 mixed-stack Berlin runs. The fork has both (above).
10. **A driver that waits in wall time.** At `--time max`, a script's driver that polls the clock while a run warms up lets T run away (tens of hours of T in a warm-up of minutes); a paced run hides it. The fork's driver takes turns with T. Paced, the driver still marks its phases when the host gets to it: in step 4 upstream's traffic ended 2 to 11 s of T past the plan.
11. **A request that fails is not answered.** `simd.handle` tells the page of a CommandError, a StoreError or a malformed message, and answers nothing. A script's request that fails that way (a `message` whose recipient's address cannot be asked, say) is waited on for ever. The fork answers it with its error (f41ea81), and the library raises SimError.
12. **One seed, several results.** On upstream's simulator one elev seed gave between 87.5 % and 98.6 % of its messages delivered, run to run and pace to pace, where the fork gives one record at any pace. The spread comes from T moving on while a station still has work in hand (354ea87 here), a host tool's bytes taken at a stale T, and the rest of "One seed, one run". The chip library's instant taken whole (68150ff) holds as well for any station that links it. Would these be welcome upstream, all together or one at a time?

## What planner and mesh would need

**planner**

- A loss-matrix export: every pair both ways, whatever its budget, with the near field under 250 m and P.2108 at both ends, and `loc_pct`, `time_pct` and frequency as inputs. `site_pair_loss` is the start: one direction, no near field, no P.2108.
- `/link.json` exposing `loc_pct` and `time_pct`.
- Reconciling with SIMesh's copy, which averages both directions and charges P.2109 at an indoor end.
- `sensitivity.db_per_m_*` compares a loss including A_h against a P.1812-only probe.
- `README.md:20` quotes void coverage figures.

**mesh**

- A node receives frames that overlap its own transmission (half duplex leaks).
- The SF7 row of the Croce table is mistyped: `[1, −8, −18, −8, −8, −9]` should be `[1, −8, −9, −9, −9, −9]`.
- The library unit tests do not build (E0063 since `cdd4fb8`).
- Transmissions are pruned 500 ms after they end, which misses collisions on frames longer than 0.5 s.
- 863–865 MHz is judged at 1 %.
- The docs contradict the code on capture.
- There is no licence file.
