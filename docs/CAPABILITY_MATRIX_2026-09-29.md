# Capability matrix, 29 September 2026

What each simulator we have models, aspect by aspect, written before any of it
is integrated into this fork. It decides what `integration/main` takes from
where: a row goes in when another source models the aspect better than SIMesh
main does and the fork lacks it.

## Sources

| Label | Where, at which commit | What it is |
|---|---|---|
| SIMesh | this repository, main `50e2c31` (upstream reticulous/SIMesh main on 29 September) | the ether (Python), the chip model (C++), the testbed, and SIMesh's own copy of the planner under `planner/` (added in `7cbb06b`) |
| branches | this repository, our ten upstream PR branches on `01d9b1f` (six commits behind main), `demo/integration` `4589d40`, `wip/virtual-time-2026-09-25` `b0b5ddc`, `wip/testbed-scenarios-2026-09-25` `7fb2f9f` | fixes and models written against the ether before main replaced it |
| mesh | mesh `c2ff69d`: `crates/emulator` and `crates/core` | a discrete-event emulator running the real `lora-core` node code in-process |
| planner | planner `f3d897a`, which planner's rewrite of 29 September made `06ff759`; the work published since is `830f221` (its crates are the same at `bd1019d`) | ITU-R propagation and coverage planning (Apache-2.0) |
| reticulum | reticulum `feat/supe` `b2b4302`: `tools/rnscale`, `tools/rncapture`, `tools/simether`, `fw/simesh` with `crates/simesh-hal` and `crates/simesh-radio-sys` | an in-process mesh simulator, the bench capture measurement, a Rust port of SIMesh's older ether, and our stack as a SIMesh station |

Our PR branches:

| Branch | Tip | Upstream PR |
|---|---|---|
| `fix/status-register-ends` | `223e01c` | #1 |
| `feat/shadowing` | `cc1fd56` | #2 |
| `feat/receiver-takes` | `caef684` | #3 |
| `feat/bench-capture` | `1c25929` | #4 |
| `feat/links` | `282f1d1` | #5 |
| `feat/sf-orthogonality` | `6c8e79b` | #6 |
| `feat/crc-band` | `e105862` | #7 |
| `feat/referee` | `abc7140` | #8 |
| `fix/late-listeners` | `5c9d91e` | #9 |
| `fix/preamble-found` | `0c45735` | #10 |

Six of them are stacked, each containing the one before it: shadowing →
receiver-takes → bench-capture → links → sf-orthogonality → crc-band. The other
four stand alone.

Each piece of evidence is `path:line` in the source named at the head of its
bullet, at the commit above. The rows were surveyed read-only. The claims marked
✓ were re-read at the cited lines before this was written.

**Status in the fork** is one of:

- **upstream**: SIMesh main has it.
- **branch**: one of our branches has it.
- **missing**: nobody in the fork has it.

## Summary

| # | Aspect | Better | Status in the fork | Step 3 |
|---|---|---|---|---|
| 1 | Free-space anchor and exponent | SIMesh | upstream | keep; sweep n through the geodata's exponent |
| 2 | ITU-R P.1812-8 terrain | SIMesh (its planner copy) | upstream, on packs | add a `loc_pct` pass-through, default 90 |
| 3 | P.2108 clutter | SIMesh = planner (§3.1 only) | upstream, on packs | keep |
| 4 | Building entry | SIMesh (P.2109 median at an indoor end); planner's measured interior increment a candidate | upstream, on packs | keep; the interior increment is a candidate |
| 5 | Near field | SIMesh | upstream | keep |
| 6 | Antenna height, gain, pattern | SIMesh | upstream | keep |
| 7 | Shadowing | ours (`feat/shadowing`, rnscale) | branch, on the removed ether | re-implement as a layer on the tables, off by default |
| 8 | Per-frame fading, CRC band | ours (`feat/crc-band`) | branch; SIMesh plans the same | port, off by default |
| 9 | Noise figure, thresholds, status registers | SIMesh, plus our SNR clamp | upstream; the SNR clamp on a branch | port the SNR clamp |
| 10 | Co-SF capture | ours for two frames (bench table, late-frame rule); SIMesh for three or more | branch, on the removed ether | re-implement `bench` over SIMesh's summed classes, off by default |
| 11 | Inter-SF rejection | SIMesh | upstream | drop `feat/sf-orthogonality` |
| 12 | Half duplex | SIMesh (rnscale equal) | upstream | keep |
| 13 | Carrier sense, CAD, preamble timing | SIMesh plus two chip fixes of ours | upstream and branches | port `fix/late-listeners` and `fix/preamble-found` |
| 14 | Duty referee per sub-band | SIMesh for duty and e.r.p.; ours for the sensing and collision audits | upstream and branch | port the referee's audits onto `RunView` |
| 15 | Energy | mesh (the only one) | missing | new, low priority |
| 16 | Clock drift, virtual time | SIMesh for time; rnscale for drift | time upstream; drift missing | new: a seeded ±ppm drift per node, off by default |
| 17 | Mobility, churn | SIMesh for churn; mesh for trajectories | churn upstream | trajectories not planned |
| 18 | Traffic | SIMesh's library; mesh's outside interferer | upstream | outside interference is a candidate (row 25) |
| 19 | Metrics | mixed | upstream and branch | the referee's audits (row 14); live against drained in step 4 |
| 20 | Scenario generation, loss matrices | SIMesh (SLT1 tables, planner sidecar) | upstream; our old scenarios need converting | converter for `wip/testbed-scenarios` |
| 21 | Performance | simether and event wake for speed; SIMesh for fidelity | partly upstream | measure in step 4; a wake heap is a candidate |
| 22 | Seeding, determinism | SIMesh, plus our hashed draws | upstream and branches | every new draw hashed from the run seed and stable ids |
| 23 | Shared conformance tests | SIMesh's `ether/test_ether.py` as the reference | upstream | add rnscale's channel cases |
| 24 | BUSY and mode-change time | nobody | missing | not planned |
| 25 | Outside interference | mesh | missing | candidate |

## Rows

### 1. Free-space anchor and exponent

- **SIMesh**
  - The ether reads a pair's loss from a table and corrects it to the frame's own carrier with `20·log10(f/f0)`. Free space is therefore anchored at the real carrier (`ether/ether.py:1080-1097`).
  - Synthetic ground is `FSPL(1 m, f0) + 10·n·log10(max(d, 1 m))`, n = 2.7 by default (`testbed/losses.py:145-170`, `testbed/geodata.py:9-12, 66`, `LOSSTABLE.md:51-70`).
- **mesh**
  - `PL0_DB` = 31.2 dB at 1 m, fixed (`crates/emulator/src/propagation.rs:12`). It is right at 868 MHz and off by less than 0.1 dB across 863–870 MHz.
  - n = 2.7 by default (`propagation.rs:40-51`); 44 of the 48 scenario files set 3.3.
  - A pair's n is the mean of its two ends' environment exponents (`propagation.rs:305-311`).
- **reticulum**
  - rnscale uses 31.2 dB + 10·n·log10(d) with n = 3.3, and the anchor does not follow `--freq` (`tools/rnscale/src/geometry.rs:14-18, 46-47, 81-82, 188-189`). At 433 MHz that would be about 6 dB too much loss.
  - simether anchors at the frame's carrier, with n = 2.7 (`tools/simether/src/ether.rs:32-37, 53-55, 426-431`).
- **Verdict:** SIMesh. The step-4 sweep over n = 2.7, 3.0 and 3.3 is a geodata setting and needs no code.

### 2. ITU-R P.1812-8 terrain

- **SIMesh**
  - The front starts SIMesh's `planner-web` once per pack (`testbed/front.py:27, 240, 474`).
  - `testbed/losses.py` asks its `/link.json` about every pair, twice (`losses.py:348-363, 480-484`).
  - The profile method follows §3.2.2 when at least half the samples are on building footprints, otherwise §3.2.1 (`planner/crates/planner-web/src/main.rs:2854-2860`).
  - SIMesh's copy runs the model both ways and gives the pair the mean (`main.rs:2983-2997` ✓). Its comment says the two directions came out up to 14 dB apart.
  - Time 50 %, locations 90 % (`planner/crates/planner-core/src/model.rs:54-55` ✓).
- **planner**
  - It carries the same P.1812-8 implementation (`crates/planner-propag/src/p1812/`, module map at `mod.rs:7-14`), with validity checks at `mod.rs:126-151`.
  - It is validated black-box against Py1812: 108 of 108 vectors within 0.1 dB (`README.md:17`).
  - Its `/link.json` runs one direction.
  - The query is `deny_unknown_fields` with no `erp_dbm`, `loc_pct` or `time_pct` (`crates/planner-web/src/main.rs:2435-2458` ✓). `loc_pct` is a committed model parameter, default 90 (`crates/planner-core/src/model.rs`), that the query does not expose. Since `830f221` the query also takes `erp_dbm` (a budget each way from the e.r.p. cap and the receiving antenna's gain toward the sender) and an antenna, azimuth and tilt at each end (`crates/planner-web/src/main.rs:3169-3232` at `bd1019d`); the loss is still computed once, one way.
- **mesh, reticulum:** no terrain. mesh adds 3 dB per 50 m of height difference and checks no line of sight (`crates/emulator/src/propagation.rs:335-344`).
- **Verdict:** SIMesh, whose planner copy is ahead of planner on reciprocity.
  - The tables are 90 %-of-locations values. The shadowing layer (row 7) must start from a median, or location variability is counted twice. P.1812's own term at 90 % is at most about 2.5 dB (1.28·σ_L, with σ_L ≈ 1.96 dB at w_a = 100 m; `planner/crates/planner-propag/src/p1812/location.rs:8-35`).
  - Step 3 adds an optional `loc_pct` to SIMesh's sidecar and table cache, defaulting to 90 so every existing table stays the same. The same change is proposed for planner below.

### 3. ITU-R P.2108 clutter

- **SIMesh** (its planner copy) implements §3.1, the height-gain terminal correction.
  - R and the street width are measured from LoD2 footprints along the path's bearing.
  - Each end is raised to R for P.1812, and then A_h is added (`planner/crates/planner-web/src/main.rs:581-634, 2938-2981`; `planner/crates/planner-propag/src/p2108.rs:94-146`).
- **planner:** the same §3.1 code in `/link.json` (`crates/planner-web/src/main.rs:2793-2852`). It is not applied to the backbone pairs.
- **mesh:** a fixed per-node `clutter_loss_db`, with presets indoor 15, basement 25, forest 10 and dense_urban 3 dB (`crates/emulator/src/propagation.rs:139-144, 185-195`).
- **Nobody** implements §3.2, the statistical clutter loss.
- **Verdict:** SIMesh. On synthetic ground no clutter term exists. A per-node loss after mesh's presets would be a crude stand-in and is not planned.

### 4. Building entry

- **SIMesh** (its planner copy) treats an antenna inside a building footprint and below its roof as indoor.
  - That end pays the ITU-R P.2109-2 median (traditional building, p = 0.5, 0° elevation) instead of its A_h, and its own building is taken out of the profile (`planner/crates/planner-web/src/main.rs:715-740, 2744-2751, 2959-2962`).
  - The median is about 14 dB at 868 MHz (`planner/crates/planner-core/src/entry_loss.rs:9-11`).
- **planner:** has P.2109-2, Table 1, with its σ (`crates/planner-core/src/entry_loss.rs:46-54, 188-220`), but only the web census uses it (`crates/planner-web/src/main.rs:3486-3489`).
- **mesh:** the clutter constants of row 3.
- **Measurement.** The one Berlin measurement named for this work is 35 indoor links, 28–511 m: P.1812 optimistic by a median 19.6 dB, and a fitted multi-wall law with a leave-one-out RMSE of 7.4 dB.
  - Published at `830f221`: planner-core's `multiwall.rs` and `linkray.rs`, and the write-up in `TODO.md:933-949` at `bd1019d`.
    - The fitted law is `38.2 + 25.2·log10 d + 1.26·interior_m`, interior metres capped at 25. Leave-one-out it is 7.4 dB against 7.9 dB for distance alone, and the wall count could not be told from the interior metres on those links (`crates/planner-web/src/main.rs:1719-1735`).
    - Planner uses only its interior term, as an increment on its P.1812 street raster for an outdoor node and an indoor home, never better than the building's best wall (`crates/planner-coverage/src/indoor.rs:215-223`), and not in `/link.json`. The file warns against using the law for a region it has not seen (`multiwall.rs:28-34`).
    - Reconciling that increment with P.2109's traditional-stock median of about 14 dB is planner's own open item (`TODO.md:20-21`).
  - The survey logs hold a receiver's home position. Neither they nor anything derived from them comes into this fork.
  - A multi-wall fitter with no data beside it is in reticulum (`tools/bench/multiwall.py` at `2783599`). It compares log-distance, FSPL + wall losses, and both, by leave-one-out error.
- **Verdict:** SIMesh, with planner's interior increment a candidate for an indoor end on a pack, behind a key: 1.26 dB per metre of building between the antenna and the wall its path leaves through, capped at 25 m, in place of the P.2109 median. The sidecar already finds an antenna inside a footprint; the metres to the exit wall along the bearing would be new. No pack is at hand to test it on, so it is not planned in this round.

### 5. Near field

- **SIMesh**
  - Under 20 m: free space at f0, flagged in the table (`testbed/losses.py:105, 472-473`).
  - From 20 to 250 m, where P.1812 refuses: its planner copy gives free space plus a P.526 knife edge over the real roofs. It is averaged both ways, never below free space, and adds A_h or entry loss (`planner/crates/planner-propag/src/near_field.rs:1-30`; `planner/crates/planner-web/src/main.rs:3004-3033`; flagged at `testbed/losses.py:428-436`).
- **planner:** the same model (`crates/planner-propag/src/near_field.rs:42-113`). The backbone pairs get NaN under 250 m instead (`crates/planner-coverage/src/gaps.rs:1271-1281`).
- **mesh:** none. It answers (Ptx, SNR 20) under 0.1 m and extrapolates below 31.2 dB between 0.1 and 1 m (`crates/emulator/src/propagation.rs:299-301`).
- **reticulum:** a 1 m floor.
- **Verdict:** SIMesh.

### 6. Antenna height, gain, pattern

- **SIMesh**
  - The nodeset gives each node:
    - `height_m`, default 2.0 m (`testbed/nodeset.py:69`);
    - `max_dbm`, where above 22 dBm the node gets a GC1109 front end (`testbed/boards.py:27-35`);
    - `antenna: {type, azimuth_deg, elevation_deg}` from a catalogue with a pattern (`testbed/antennas.py:6-9, 90-132`).
  - Each antenna is pointed along the 3-D line between the tips, with k = 4/3 (`antennas.py:135-152`).
  - The default whip is 2.0 dBi, with a 75° vertical beamwidth, 10° tilt and an 18 dB floor (`testbed/antennas/catalogue.yaml:35-42`).
  - Gains are layered on the tables at hand-over (`testbed/losses.py:190-222`).
  - Heights enter the loss only on packs.
- **mesh**
  - Height gain is 20·log10(h/1.5) per end with no cap: +28.4 dB for a 40 m tower (`crates/emulator/src/propagation.rs:323-333`).
  - Gain is one omni figure (`propagation.rs:315-317`).
  - Conducted power is capped at 14 dBm, but e.i.r.p. is not capped (`crates/core/src/node.rs:1959, 3078`).
- **planner:** a height per end, an isotropic gain per end, and e.r.p. ceilings (`crates/planner-core/src/preset.rs:89-93, 159-183`; `crates/planner-coverage/src/environment.rs:782, 809-815`).
- **reticulum:** rnscale has one power for every node and no gain or height (`tools/rnscale/src/geometry.rs:32-33`). simether has an isotropic `gain_db` per station (`tools/simether/src/ether.rs:123-128`).
- **Verdict:** SIMesh. The open item of 24 September (per-node antenna height and gain) is now upstream. mesh's uncapped height gain is not ported.

### 7. Shadowing

- **SIMesh:** none (`ether/INTERNALS.md:405-407`). What comes close:
  - P.1812's 90 %-of-locations term, deterministic and at most about 2.5 dB (row 2);
  - hand-written per-pair `offsets` (`testbed/losses.py:278-295`).
- **branches:** `feat/shadowing` draws one standard normal per unordered pair, from SHA-256 of the seed and both station numbers. It is fixed for the run, scaled by `physics.shadowing_db`, and off by default (`ether/ether.py:134, 339` on `cc1fd56`), with 5 tests. It was written against the distance formula main has removed.
- **reticulum:** rnscale draws N(0, 7 dB) per pair, symmetric and once per run, with no correlation (`tools/rnscale/src/geometry.rs:145-149, 186-193`; test at `:377`).
- **mesh:**
  - σ is 6 dB by default and 7 dB in the scenarios.
  - With `fading_corr` c = 0, the default, it is redrawn for every frame at every receiver.
  - With c > 0 it is σ·(√c·z_slow + √(1−c)·z_fast), where z_slow is a symmetric hash of both ends snapped to a 25 m grid (`crates/emulator/src/propagation.rs:26-37, 359-373, 407-432`). No scenario sets c.
  - The code's own comment calls the per-frame default "an idealization that systematically favors retry/anycast schemes at knife-edge link margins" (`propagation.rs:26-29` ✓).
- **Correlation between links**, for example two links sharing an end or a building: nobody models it. mesh's "correlated" is correlated in time, not across links.
- **Verdict:** ours. A static draw per pair is the standard log-normal model, and it is the one our campaign numbers used (σ = 7 dB, a literature value).
- **Step 3:** re-implement as a layer on the tables at hand-over, next to the offsets, keyed by node name and seeded from the run seed.
  - It is off by default, and a table built from a pack must be a median (row 2) when it is on.

### 8. Per-frame fading, CRC band

- **SIMesh:** neither (`ether/README.md:113-115`; `ether/INTERNALS.md:403-412`). The same design is planned: `crc_margin_db`, 3 dB, a probability falling linearly, drawn from the run's seed (`INTERNALS.md:1596-1598`).
- **branches:** `feat/crc-band` applies it to a frame judged clean whose margin m over its SF's threshold is under the band. The frame fails with p = 1 − max(m, 0)/band, one hashed draw per frame and receiver (`ether/ether.py:201, 756-758` on `e105862`). There are 3 tests. It is off by default; the campaign used 2 dB.
- **mesh:** the fast part of its fading (row 7). Decoding is a hard cut made when the frame is sent (`crates/emulator/src/world.rs:2077`; `crates/emulator/src/propagation.rs:478-481`).
- **reticulum:** a cliff at sensitivity in both rnscale and simether.
- **Verdict:** ours, which is SIMesh's own plan. Step 3 ports it as a hook in `deliver_end` after a clean verdict. Fast fading of the level itself, mesh's z_fast, would be a separate key and is not planned.

### 9. Noise figure, thresholds, status registers

- **SIMesh**
  - The noise floor is −174 + 10·log10(BW) + NF, with NF 6 dB from a simd flag.
  - SNR thresholds run from SF5 −2.5 to SF12 −20 dB, so sensitivity at SF7/125 kHz is −124.5 dBm.
  - Decisions use unrounded levels (`ether/ether.py:84-94, 1105-1118, 1520-1523`).
  - The chip holds RSSI to its register's ends (`radio/src/model.cpp:250` ✓).
  - SNR still wraps: the chip casts `(int8_t)(snr·4)` (`model.cpp:843`), and the ether sends SNR unclamped (`ether/ether.py:1523`). Any link more than about 32 dB above the noise, which at 125 kHz means stronger than about −85 dBm, reads as a weak one. Our SUPE picks its rate from that SNR (reticulum `crates/reticulum-lora-radio/src/iface.rs:3122`).
- **branches:** `fix/status-register-ends` clamps SNR to −32…+12 dB, where a LoRa receiver's estimate saturates (`radio/src/model.cpp:172-180` on `223e01c`). There are 3 tests. Its RSSI half is upstream now, with a register maximum of 254.
- **mesh**
  - SNR leaves out the noise figure: it is about 6 dB optimistic and clamped to ±30 dB (`crates/emulator/src/propagation.rs:389-392`).
  - Decoding uses a per-SF sensitivity table that has the NF inside it (`propagation.rs:456-471`).
  - RSSI is truncated toward zero, which is up to 1 dB optimistic (`propagation.rs:387`).
- **reticulum:** rnscale and simether use SIMesh's formula and 6 dB. rnscale clamps SNR to −20…+12 dB (`tools/rnscale/src/geometry.rs:49-51, 90-99, 277-283`).
- **Verdict:** SIMesh, plus our SNR clamp.

### 10. Co-SF capture

- **SIMesh**
  - **Lock:** a decodable frame takes the receiver if the receiver is idle, or if it leads the frame being received by 6 dB. This can happen anywhere in the earlier frame's air. The earlier frame is then lost (`ether/ether.py:1376-1412`).
  - **Verdict:** a frame must lead the summed same-SF power in every stretch of its air by 6 dB (`ether.py:1440-1489`), which is Semtech's figure rather than Croce's measured 0–1 dB (`ether.py:96-103`).
  - `--pairwise` is the older rule, one interferer at a time (`ether.py:1414-1436, 1491-1509`).
- **branches:** `feat/bench-capture`, `capture_model: bench`, off by default (`ether/ether.py:166, 188, 640-672, 774` on `1c25929`), with 9 tests.
  - Within 1.2 dB, both frames are lost with p = 9/39; otherwise a coin picks the survivor.
  - Stronger by 1.2 to 2.7 dB, a frame survives with p = 119/136, rising in a straight line to certain at 6.1 dB. The weaker frame never survives.
  - A frame that starts after the first one's preamble is never received, and the first survives only if it is the stronger.
- **reticulum**
  - **The measurement** is `tools/rncapture/README.md:37-54`, of 2026-09-17: SX1262 and LR2021 senders, SX1262 and nRF52840 receivers, SF7/125 kHz, 289 collisions.
  - **What it covers:**
    - Listen-before-talk stayed on, so the frames started within about 8 ms of each other (`:69-74`).
    - The late-start rule rests on 12 of 1,500 trials at 30–35 ms, all about 2 dB apart (`:80-88`).
    - It covers one SF, one bandwidth and one frame length.
  - **rnscale** applies the same table per listener, and frames too weak to decode still interfere (`tools/rnscale/src/medium.rs:75-134, 531-569, 620-635`).
- **mesh**
  - Capture needs +1 dB SIR, Croce's Table II diagonal. There is no timing rule: any overlap counts, and each interferer is checked alone, not summed (`crates/emulator/src/world.rs:130-148, 2186-2229`).
  - Its three docs say "no capture" or "6 dB" (`docs/EMULATOR.md:183`, `docs/RQ_EMULATOR_VERIFICATION.md:18`, `docs/ASSUMPTIONS_AUDIT_2026-04-18.md:300-306`).
- **Verdict:** ours for two frames at SF7, where it is measured; SIMesh's summed classes for three or more frames. `feat/receiver-takes` is superseded by the ether's own lock and is dropped.
  - **Where the two part:** a later frame 6 dB or more stronger. SIMesh lets it take the receiver mid-payload. The bench saw late stronger frames destroy both, but only at about 2 dB, where SIMesh loses both too. No measurement covers 6 dB and more. This is an open question.
- **Step 3:** re-implement `bench` as a third rule inside `arrive` and the verdict, over the summed class: Δ is the frame against the sum. For two frames that is the bench table exactly. It stays off by default.

### 11. Inter-SF rejection

- **SIMesh:** Croce et al. 2018, Table II (SX1272), with a correct SF7 row `[1, −8, −9, −9, −9, −9]` (`ether/ether.py:105-122` ✓).
  - The diagonal is replaced by 6 dB, and SF5 and SF6 take SF7's row and column (`:197-206`).
  - Interference is summed per SF class (`:1481-1488`).
  - Any overlapping channel counts at full power (`ether/INTERNALS.md:413-416`).
- **mesh:** the same table, but its SF7 row is `[1, −8, −18, −8, −8, −9]` (`crates/emulator/src/world.rs:139` ✓). That is a transcription error: an SF7 frame survives an SF9 interferer 9 dB too easily.
- **branches:** `feat/sf-orthogonality` has the same values, per interferer. It is superseded.
- **reticulum:** rnscale is SF7 only. simether has no orthogonality at all, which is pessimistic (`tools/simether/src/ether.rs:26-31, 227-230`).
- **Verdict:** SIMesh. Drop our branch.

### 12. Half duplex

- **SIMesh:** a station that is sending is skipped as a receiver and loses its locks. A receiver that starts sending gets `crc` (`ether/ether.py:1304-1314, 1453-1457, 1469-1470`; `radio/src/model.cpp:775-776`). There is no turnaround time (row 24).
- **mesh:** it leaks. `transmit()` returns at once and the node re-enters RX in the same poll (`crates/emulator/src/virtual_radio.rs:79-98`). Arrival checks `receiving` only at the frame's end (`crates/emulator/src/world.rs:2326-2329`). A frame that overlaps the node's own transmission, even completely, is delivered.
- **reticulum:** rnscale misses any frame that overlaps the node's own sending and holds 7 frames in its queue (`tools/rnscale/src/medium.rs:49-54, 344-365, 446-452`). simether skips a station whose `tx_until` is past the frame's start (`tools/simether/src/ether.rs:1061-1071`).
- **Verdict:** SIMesh, equal to rnscale.

### 13. Carrier sense, CAD, preamble timing

- **SIMesh**
  - **Sense threshold:** 10·log10(BW / 1 kHz) − 117 + 15 dBm, which is −81.03 dBm at 125 kHz, per ETSI EN 300 220-1 §5.21.2 (`ether/ether.py:124-134` ✓, `:209-212`).
  - **When energy is told:** only at a frame's start, to slots already in RX or CAD, when the summed in-band power reaches the threshold (`ether.py:1362-1399`). A frame that starts while a slot is in neither mode is never told to it (`ether.py:1318-1320`).
  - **CAD is ideal:** `cadDetPeak` and `cadDetMin` are stored but never used (`radio/src/model.cpp:738-758, 853-870`).
  - **RSSI_INST** reads the strongest frame, while the ether's busy test is the sum (`model.cpp:701-707, 885-891` ✓).
  - **PREAMBLE_DETECTED** fires with SYNC_WORD_VALID at the end of the sync word, so the firmware is blind for the whole preamble: 46 ms at SF8 with 18 symbols (`model.cpp:820-822, 907`).
- **branches**
  - `fix/late-listeners`: a slot that enters RX or CAD is told the energy of every frame already on the air that it could hear. The chip raises only RSSI and CAD from that (`ether/ether.py:451-477`, `radio/src/ether_link.cpp:88-96`, `radio/src/model.cpp:782, 797` on `5c9d91e`), with 4 tests.
  - `fix/preamble-found`: PREAMBLE_DETECTED fires 4 symbols into a frame. That is the bench's blind window: about 4 ms at SF7/125 kHz, three boards, 150 trials (`radio/src/model.cpp:88, 265, 747` on `0c45735`), with 1 new test and 2 changed.
- **reticulum**
  - rnscale runs the firmware's own access: up to six looks, receiver first, then a 10 ms CAD (`tools/rnscale/src/sim.rs:117-127, 236-276`).
  - It has a 4 ms blind window (`tools/rnscale/src/medium.rs:29-37, 575-581`), fitted by replaying the bench jam: 7.8 % met and 90 % parked, against the boards' 6.7 % and 85 % (`tools/rnscale/src/main.rs:918-966`).
- **mesh**
  - CCA is busy when the inbox is non-empty or any transmission is above the current SF's sensitivity, about −124 dBm.
  - CAD is perfect and evaluated at the next 100 ms poll (`crates/emulator/src/world.rs:1640-1682`).
  - `read_rssi` returns a constant −120 dBm (`crates/emulator/src/virtual_radio.rs:114-116`).
- **−75 dBm** is nobody's sense threshold, and in mesh it is only the interferer's default level. Both firmwares use −81 dBm at 125 kHz:
  - reticulum's plan (`crates/reticulum-rnode-proto/src/lib.rs:1611-1617, 1852-1856`) reads EN 300 220-2 Table 17 as 15 dB over the sensitivity limit below 100 mW e.r.p. and 11 dB over it from 100 to 500 mW, which is −85 dBm at 125 kHz;
  - reticulous iface-lora has `CSMA_CCA_DBM_125K` (`esp-idf/src/lora_csma.h:43` at `e9869e9`).
- **Verdict:** SIMesh's threshold and summing, plus our two chip fixes. Both are ported in step 3.
  - Summing RSSI_INST in the chip is a candidate: a small change, but one that changes behaviour, so it would need a key.

### 14. Duty referee per sub-band

- **SIMesh:** `testbed/compliance.py`, run after every script run into `report.md` (`testbed/simesh/runner.py:21-22, 53-66`).
  - It judges each node's worst sliding hour against EN 300 220-2 Annex B, e.g. 865–868 MHz at 25 mW e.r.p. and 1 % (`compliance.py:53-66`).
  - e.r.p. = power + antenna peak gain − 2.15 dB (`:153`).
  - Polite spectrum access is checked when a node is over its duty cycle: at most 100 s per hour in any 200 kHz, frames at most 1 s, and at least 100 ms between them (`:41-46, 148-177`).
  - Listen-before-talk is assumed, since the record does not show it. Nothing is enforced during a run (`ether/INTERNALS.md:417-419`).
- **branches:** `feat/referee` (`testbed/referee.py` on `abc7140`, 5 tests).
  - **Duty section:** its band table puts 863–865 MHz at 1 % (0.1 % is right), treats the alarm sub-bands as usable, and checks no power. It is superseded by `compliance.py`.
  - **Audits, which nothing on main does:**
    - carrier sense: every transmission that began over an audible frame, told / told late / never told, by window (blind, preamble, payload);
    - frames nobody was told of;
    - collisions, split into hidden senders and senders in earshot.
  - They are built on the removed scenario API (`build_air`, `:188`) and assume ISO stamps, where virtual time writes T.
- **reticulum:** the EU868 plan, from BNetzA Vfg. 91/2025 and EN 300 220-2 (`crates/reticulum-rnode-proto/src/lib.rs:1428-1509`), with `srd_rule` at `:1706-1783`.
  - Its PSA terms also include at most 4 s per dialogue and a CCA of at least 160 µs (`:1330-1342`). `compliance.py` does not check those two.
- **mesh**
  - The core enforces a per-sub-band ledger (`crates/core/src/duty.rs:33-79, 219-237`), but anything not in its table is SRD 1 %, so 863–865 MHz is judged at 1 %.
  - The emulator's watchdog uses one permille for every band and only counts (`crates/emulator/src/metrics.rs:202-310`).
- **Verdict:** SIMesh for duty and e.r.p. It matches the plan in the brief: 14 dBm e.r.p., 100 s/h per 200 kHz, 1 % on 865–868. Ours for the audits, which step 3 ports onto `RunView.medium()` (`testbed/simesh/view.py:138`). The "told late" split needs row 13's late listeners.

### 15. Energy

- **mesh** (`crates/emulator/src/scenario.rs:146-203`; `crates/emulator/src/metrics.rs:55-132`):
  - TX is 120 mA × airtime, plus a TCXO warm-up per transmission.
  - RX is (11 mA + TCXO idle) × all elapsed time, sending included.
  - The battery is 2600 mAh.
  - `sleep_ma` is parsed and never used, and a node's own `battery_pct` is set once (`crates/emulator/src/virtual_node.rs:202`).
- **Nobody else** models energy. SIMesh lists sleep and power management as not modelled (`INTERNALS.md:1557`).
- **Verdict:** mesh, the only one. SIMesh records every slot's mode changes, so a post-run tool over `record.tsv` could charge RX, CAD, standby and TX by mode, which mesh cannot. New, low priority, with the currents as parameters.

### 16. Clock drift, virtual time

- **SIMesh**
  - **Virtual time:** the ether conducts. T moves only when every station is idle (`ether/ether.py:243-273, 631-716`), and the time shim answers the C library's clocks (`radio/shim/simclock.c:219-230`).
  - **Drift:** only a piecewise-linear `SIMESH_CLOCK_PROFILE` in a kind's environment. The host clock follows it and the radio's timers stay on T (`radio/src/conductor.cpp:61-105, 321-322`; `STATION.md:22`). Only a test uses it (`radio/tests/test_conductor.py:300-305`).
- **reticulum**
  - rnscale draws a drift uniform in ±20 ppm per node (`tools/rnscale/src/main.rs:35-48, 575-576`).
  - simether's conductor keeps its wakes in a heap (`tools/simether/src/ether.rs:614-632, 658-710`).
- **mesh:** an opt-in skew of ±P ppm plus a boot offset, seeded from the CLI seed or 42, not the scenario seed (`crates/emulator/src/world.rs:1477-1510`; `crates/emulator/src/main.rs:501`).
- **branches:** `wip/virtual-time` ran simether as simd's ether through `simd_remote.py`, over the geometric API main has removed. It is superseded.
- **Verdict:** SIMesh for time, rnscale for the drift model.
  - Step 3 adds a seeded drift per node, uniform in ±ppm and off by default, through the existing profile mechanism.
  - 20 ppm is 72 ms an hour. That moves how the periodic timers of different nodes line up, such as announces, but not a single exchange, which lasts seconds. Its effect on delivery is expected to be small, and step 4 measures it.

### 17. Mobility, churn

- **SIMesh**
  - Moves are discrete, from a script or the page, each recomputing a row and column of the table (`testbed/simesh/library.py:416-419`; `testbed/simd.py:517-565, 1110-1127`).
  - Churn: resets, factory resets, staggered reset-all, and nodes added or removed at runtime (`testbed/simd.py:1095-1244`; `testbed/stations.py:474-479`).
- **mesh:** random walk, linear and patrol trajectories at 1 Hz (`crates/emulator/src/scenario.rs:322-358`; `crates/emulator/src/virtual_node.rs:245-321`). There is no bounding box, and the heading seed ignores the scenario seed (`crates/emulator/src/world.rs:1872`).
- **reticulum:** rnscale kills and joins nodes at set hours (`tools/rnscale/src/main.rs:186-190, 670-687`).
- **Verdict:** SIMesh for churn, with real process restarts; mesh for trajectories. Trajectories are not planned: our deployments are fixed repeaters, and on a pack every move costs two sidecar requests per pair.

### 18. Traffic

- **SIMesh:** `scripts/lxmf-traffic.py` over `testbed/simesh/traffic.py`.
  - An LXMF message every 5 s for an hour.
  - Pairs drawn uniformly with `Random(17)`.
  - Three size classes, weighted 167 : 76 : 39.
  - A 600 s drain (`traffic.py:1-96`; `scripts/lxmf-traffic.py:15-31`).
- **reticulum**
  - `tools/simcampaign` drives SIMesh from outside, with seeded plans that repeat CPython's draws and paired scoring. It stays in reticulum.
  - rnscale has page fetches, LXMF messages and hosts (`tools/rnscale/src/client.rs:17-65`).
- **mesh**
  - Fixed-interval flows with a demand envelope (`crates/emulator/src/world.rs:971-994, 1061-1096`). Priority is parsed and ignored (`:2470-2472`).
  - An outside interferer: Poisson, SF8, the same level at every node, −75 dBm by default (`:878-966`).
- **Verdict:** SIMesh's library for the fork. mesh's outside interferer is row 25.

### 19. Metrics

- **SIMesh:**
  - airtime by station and kind (`testbed/airtime.py`);
  - usable links against the table (`testbed/links.py`);
  - LXMF delivery from the senders' logs (`testbed/simesh/reticulum/delivery.py`);
  - `compare.py`, `seq.py` and `compliance.py`.
- **branches:** the referee's audits (row 14).
- **reticulum**
  - rnscale counts losses split into in earshot and hidden, plus topology figures (`tools/rnscale/src/medium.rs:169-230`; `tools/rnscale/src/main.rs:430-477, 829-889`).
  - simcampaign records fates and McNemar pairs.
- **mesh:** delivered/intended both at the end of traffic ("live") and after the drain, latency percentiles, retry lineage, echo audits, and an analytic reachability oracle (`crates/emulator/src/metrics.rs:1104-1123, 1177-1513`; `crates/emulator/src/world.rs:2628-2860`).
- **Verdict:** mixed.
  - The fork gains the referee's audits (row 14).
  - Step 4 reports delivered/intended live and drained from the senders' logs and T. That needs no new model.

### 20. Scenario generation, loss matrices

- **SIMesh**
  - **Nodesets** are imported from planner's optimiser `sites.csv` (all at an assumed 2 m), from a deployed-network CSV, or from a MeshCore or PotatoMesh map (`testbed/nodeset.py:603-669`).
  - **SLT1 tables** state every ordered pair's loss per band. Measured cells carry `FLAG_MEASURED` (`ether/slt.py:34`; `LOSSTABLE.md:1-49`).
  - Tables are cached per geometry, and a new nodeset reuses the nearest cached table (`testbed/losses.py:554-640`).
  - Packs are built from public sources by `planner-job pack-build` (`INTERNALS.md:597-660`).
- **planner:** no pairwise export.
  - `site_pair_loss` is published at `830f221` (`crates/planner-coverage/src/gaps.rs:1287`): P.1812 from one end to the other, whatever the budget, with `LinkParams` as its inputs, so `loc_pct`, `time_pct` and frequency are the caller's. It is one direction, gives nothing under 250 m, adds no P.2108, and only `planner optimize` calls it.
  - The nearest code is the private `backbone_graph`, which discards pairs over their budget and gives NaN under 250 m (`crates/planner-coverage/src/gaps.rs:1236-1404`).
- **branches**
  - `feat/links` is superseded by SLT1.
  - `wip/testbed-scenarios` holds 25 files in the removed scenario format. The Berlin layouts carry about 3.1k `links:` each. They need converting to a nodeset, a geodata and a table.
- **mesh:** honeycomb generators, a polygon clip, map fetches and LLM generation (`crates/emulator/src/scengen.rs:173-483`; `crates/emulator/src/main.rs:1108-1211`). There is no loss input: RSSI is always analytic (`crates/emulator/src/world.rs:2062-2075`).
- **Licences.** The MeshCore import path is upstream, but scraped MeshCore coordinates carry no licence (planner's own notice says so: `crates/planner-pack/src/nodes.rs:159-165`) and are not used here.
- **Verdict:** SIMesh.
  - The open item of 24 September, planner's P.1812 losses into `links:`, is closed upstream by the sidecar and SLT1.
  - What remains is a converter for our old scenarios (step 3), and a loss-matrix export in planner that would replace two sidecar requests per pair (a proposal, below).

### 21. Performance

- **SIMesh**
  - Time is event-driven, but every barrier scans every station (`ether/ether.py:631-639`); a heap is on its to-do list (`INTERNALS.md:1599-1602`).
  - One simd event loop is the limit at about 100 stations (`INTERNALS.md:160-163`).
  - Parallel simulations run through the front (`testbed/front.py:254-256`).
  - Pack tables cost about 2.5 ms per request, two requests per pair, answered one at a time (`testbed/losses.py:57-61`).
- **reticulum**
  - **simether:** cached pair losses, a nearest-first walk, a wake heap and several sockets (`tools/simether/src/ether.rs:1034-1058, 1167-1186`).
  - **Measured on 25 September**, 173 stations, `--time max`, announces only:
    - the Python ether of the time with 25 ms slices ran at about 1.5× real time;
    - simether with event-driven stations ran at 6.3–6.5× (`tools/simether/README.md:70-90`).
  - **The station's event wake v2** (`fw/simesh/src/engine.rs:179-246`, commit `0deab93`) sleeps until DIO1 rises.
  - simether ports SIMesh's ether as of `96d4ab9` and has no tables.
- **mesh, rnscale:** in-process, and far faster for protocol sweeps, but with no firmware on a chip model. mesh polls every node every 100 ms, in lock step (`crates/emulator/src/world.rs:451, 1862-1866`).
- **Verdict:** SIMesh's architecture keeps real firmware in the loop, which neither in-process simulator has. For speed, step 4 measures first. A wake heap in the barrier is SIMesh's own to-do and must keep station-id order at equal T.

### 22. Seeding, determinism

- **SIMesh**
  - `--seed` is random by default. It goes into `welcome` and, in virtual runs only, into `SIMESH_SEED`, from which the shim seeds each node's `getrandom` (`ether/ether.py:471-473, 501`; `radio/shim/simclock.c:41-48, 1341-1395`).
  - Messages at one T are taken in station-id order (`ether.py:718-729`).
  - `/dev/urandom` and the wall-time guards stay outside the seed (`INTERNALS.md:1467-1476`).
- **branches:** shadowing, bench capture and the CRC band draw from SHA-256 hashes of the seed and stable ids, so a draw does not depend on event order. That is also what pairs arms with common random numbers.
- **reticulum:** rnscale is bit-identical per seed (`tools/rnscale/src/main.rs:223, 539, 572-587`). The station derives a SHA-256 stream from `SIMESH_SEED` and its id (`fw/simesh/src/platform.rs:51-124`).
- **mesh:** deterministic, except that node RNGs are `seed + i + 1`, so consecutive seeds share streams (`crates/emulator/src/world.rs:339`). Mobility, skew and an environment variable also sit outside the seed (`world.rs:470-473, 1872`).
- **Verdict:** SIMesh, with every new draw taken our way.

### 23. Shared conformance tests

- **SIMesh:** at `50e2c31` in this workspace: `ether/test_ether.py` 52 passed; `radio/tests` 31 passed; `testbed` 153 passed and 5 skipped (pack tests without a pack).
- **reticulum**
  - simether's `conformance/conftest.py` swaps `test_ether.Bench` for simether over placements, gains and walls: the API of `96d4ab9`. Main's `Bench` is built on links, so the harness no longer fits.
  - rnscale has 12 channel tests (`tools/rnscale/src/medium.rs:797-990`), among them the bench table at both ends, late stronger and weaker frames, and the lock.
- **mesh:** the 22 library unit tests do not build. Four `PropagationConfig` literals lack the two fields added in `cdd4fb8` (E0063; `crates/emulator/src/propagation.rs:513-516, 599-602, 631-634, 660-663` ✓, fields at `:26-37`).
- **planner:** 511 tests. The P.1812 oracle is skipped without its locally generated vectors (`crates/planner-propag/tests/oracle.rs:115-146`).
- **Verdict:** SIMesh's `test_ether.py` is the shared reference. Step 3 adds rnscale's channel cases there, behind the bench key.

### 24. BUSY and mode-change time

- **SIMesh:** BUSY is never raised, and every command completes at once (`radio/src/model.cpp:392, 509-511`). Mode-change times are on its to-do list (`INTERNALS.md:1576-1584`).
- **Nobody else** models them. Our station once lost 60 ms of T per mode change to a BUSY pulse, since fixed (`crates/simesh-hal/src/lib.rs:423-438`).
- **Not planned.**

### 25. Outside interference

- **mesh:** a Poisson interferer, SF8, 100 ms × U[0.5, 1.5], at the same level at every node (`crates/emulator/src/world.rs:878-966`).
- **Nobody else** has one. The band is shared with other SRD users, so this is an airtime effect. A better model would be a phantom station with a position, heard through the tables like any other.
- **Candidate**, after the rows above.

## Defects found on the way

### SIMesh main

These are drafted for Rop in the report, not filed.

1. RSSI_INST reads the strongest frame while the ether's busy test sums (row 13).
2. The idle RSSI is −110 dBm (`radio/src/model.h:48`), while the ether's noise floor at 125 kHz and NF 6 dB is −117 dBm.
3. SNR wraps above about 32 dB (row 9). `fix/status-register-ends` fixes it.
4. A frame that starts while a slot is outside RX and CAD is never told to it (row 13). `fix/late-listeners` fixes it.
5. PREAMBLE_DETECTED fires at the sync word (row 13). `fix/preamble-found` fixes it.
6. A later frame 6 dB or more stronger takes the receiver mid-payload. No measurement covers that (row 10).
7. The analysis tools read a run's `physics` (`testbed/simesh/view.py:143`), which simd never writes. They assume NF 6 dB whatever the run used.
8. `testbed/losses.py:31-37` and `LOSSTABLE.md:31-34` say cells are not reciprocal, while the sidecar averages both directions, as `INTERNALS.md:692-699` says. `INTERNALS.md:1537-1538` says there is no antenna pattern; the testbed has one.
9. The ether tells a slot of energy only from −81 dBm, the CCA threshold of a transmitter under 100 mW (`ether/ether.py:124-134`). A firmware whose threshold is lower cannot see the frames in between. A real chip's RSSI reads down to its noise, and the firmware applies its own threshold. EN 300 220-2 as reticulum reads it puts that threshold at −85 dBm from 100 to 500 mW, which 869.4–869.65 MHz allows.

### Our branches

- All ten were written against the distance-formula ether, which main has replaced. The stacked six conflict in 17 to 39 hunks each.
- The four chip commits (#1, #3, #9, #10) note that iface-lora carries its own copy of the chip model and needs the same change. That is no longer so: since iface-lora e651bca (25 September) its host build links SIMesh's own chip library (`radio/`), and the in-tree copy is gone. A chip fix in this fork's `radio/` therefore reaches reticulous stations built against it, and iface-lora's own PR branches #2–#5, written against the removed copy, are superseded. The same note in three `integration/medium` commit messages (1106a99, b4c2c7a, 0f051d6) is wrong for the same reason.

### mesh

This is what mesh would need; mesh is read-only here.

1. Half duplex leaks (row 12).
2. The SF7 row of the Croce table is mistyped (row 11).
3. The library unit tests do not build (row 23).
4. Transmissions are pruned 500 ms after they end (`crates/emulator/src/world.rs:2140` ✓), but a collision is judged at the victim's end over its whole air. An interferer that ended more than 0.5 s before the victim ended has already been pruned, so its collision is missed. The comment at `world.rs:182-184` describes a different rule. This affects frames over 0.5 s: SF10/125 kHz with 50 bytes is already 0.74 s.
5. Nodes are polled in lock step every 100 ms, and the interval cannot be configured (`crates/emulator/src/world.rs:451, 1862-1866`).
6. Capture has no timing rule, no summing, RSSI truncation and no noise figure (rows 9, 10).
7. Fading is independent per frame by default (row 7).
8. 863–865 MHz is judged at 1 % (row 14).
9. The Meshtastic baseline is tagged MediumFast but runs SF10/BW250, which is MediumSlow (`crates/core/src/node.rs:1263-1264`).
10. `docs/EMULATOR.md`, `docs/RQ_EMULATOR_VERIFICATION.md` and `docs/ASSUMPTIONS_AUDIT_2026-04-18.md` contradict the code on capture, inter-SF interference, sensitivity, the poll interval and RX during TX.
11. There is no licence file. `Cargo.toml:18` declares MIT OR Apache-2.0 for the workspace, but no crate inherits it.

### planner

These are proposals; planner is read-only here.

1. **A loss-matrix export.** It should give:
   - both directions of every pair, whatever its budget;
   - the near-field model under 250 m;
   - P.2108 at both ends, and the profile method and decimation of `/link.json`;
   - `loc_pct`, `time_pct` and frequency as inputs;
   - model and near-field flags per cell.

   SIMesh's `testbed/losses.py` is already its consumer. It would replace two requests per pair. The unpublished `site_pair_loss` may be where this starts; publishing it is the planner owners' call.
2. **`/link.json` gets `loc_pct` and `time_pct`.** Both are model parameters, and the query exposes neither. `erp_dbm` is unpublished (row 2).
3. **SIMesh's planner copy has moved on:** both-way means, and P.2109 at an indoor end. The two should be reconciled.
4. **`sensitivity.db_per_m_*`** compares a loss that includes A_h against a P.1812-only probe (`crates/planner-web/src/main.rs:2954-2963`). It looks inflated.
5. **Housekeeping:**
   - `README.md:20` quotes coverage figures that `HANDOFF.md:69-72` declares void.
   - `fw/sense` declares MIT with no licence file.
   - The page never shows the pack's licences (`crates/planner-web/src/index.html:3526-3530`).

### reticulum

These are ours, fixed on `feat/supe` outside this task.

- `SIMESH_STAND_DOWN` expects `close-siblings`, while rnscale calls that rule `close`.
- simether's README points to a `tests/` directory that does not exist, and calls event wake a local patch.

## Order for step 3

Most consequential first. Each item is one topic, with tests, off by default unless it is a fix.

1. **The PR branches.** Port the four that apply to main:
   - `fix/preamble-found`;
   - `fix/status-register-ends`, the SNR half;
   - `fix/late-listeners`, on main's energy rule;
   - `feat/crc-band`, as a hook in `deliver_end`.

   The other six are recorded in the report:
   - `feat/shadowing`, `feat/bench-capture` and `feat/referee` are re-implemented in items 2, 3 and 4;
   - `feat/receiver-takes`, `feat/sf-orthogonality` and `feat/links` are superseded and dropped.
2. **Shadowing** as a layer on the tables, and `loc_pct` for pack tables. The σ sweep needs it, and it decides who hears whom.
3. **Bench capture** over summed classes, with the late-frame rule and rnscale's channel cases as shared tests. Collisions are the airtime link-breaker.
4. **The referee's audits** on `RunView`: carrier sense, frames nobody was told of, and collisions, hidden or in earshot.
5. **A converter** from the old scenario format to nodesets and tables, for step 4's layouts.
6. **A seeded clock drift per node.**
7. **Candidates, if time allows:**
   - energy (row 15);
   - outside interference (row 25);
   - summed RSSI in the chip (row 13);
   - a wake heap (row 21).

## Open questions

- **Fixes and the off-by-default rule.** The chip fixes (`fix/preamble-found`, the SNR clamp, `fix/late-listeners`) change what existing scenarios do. That is their point, and upstream they are unconditional. Here they get switches, off by default, until the fork's maintainer decides otherwise.
- **The late stronger frame (row 10).** No measurement covers a late frame 6 dB or more stronger. That is a bench question for Rop and us.
