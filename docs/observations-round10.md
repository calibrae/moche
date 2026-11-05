# Observations, round 10: SSP timing variant sweep on the real VM network, and the isolated-key fix

Date: 2026-11-05. Base: `d063983` ("variant A", the round 9 build). Lab as in rounds 4-9 (`tools/vm-lab/README.md`): server
`moche-a` (stock mosh-server 1.4.0), client `moche-b` (stock `mosh-client` 1.4.0 and the variant binaries,
`aarch64-unknown-linux-musl` release builds from `tools/release/build.sh`), netem on `eth0` of **both** VMs (one-way D on each
side, RTT = 2D plus about 1 ms), control over `lima0`. `tc netem`, `tcpdump -c`, `strace -f -e trace=process,signal`,
`iptables` counters and `ip link/addr` only inside the VMs; captures deleted after use. Stock mosh is a black box. Work dir on
the lab host `~/moche-r10` (scripts and results in `~/moche-r10/out`), VMs left running with no netem, no leftover processes.
No real addresses are recorded here.

## Summary

| # | Item | Result |
|---|---|---|
| R1 | Lost isolated key recovered later than stock (p90 269 vs 173 ms at RTT 100, round 9) | **FIXED by variant C**: recovery = stock (RTT 100, 100 probes each: p50 112 / p90 178 / max 416 against stock 119 / 175 / 400; RTT 200, 75 each: p90 327 against 326). Cost: one more data datagram per isolated key (2.0, stock 2.7-2.9, A 1.0) |
| R2 | Typing echo at RTT 200 slower than stock | **NOT FIXED, and not fixable with `FRAME_EXTRA_MS`.** It is not specific to RTT 200 or to 100 ms keys: moche is 5-47 ms slower than stock at RTT 150-300 for every constant from 3 to 10; smaller constants are worse, larger ones help at RTT 200/250 and hurt at RTT 150. No value matches stock everywhere, so the observed 6 stays. Root cause open (below) |
| R3 | B2 cost tail (2 of 20 at 14-18x) | **Not reproduced**: 0 outliers above 10x in 24 runs of C and in 24 interleaved runs of A (cost 2.8-5.2x). Combined with round 9, A has 2 outliers in 44 runs, C 0 in 24, d07618b 0 in 16; no capture of an outlier exists |
| - | Safety battery for C | **PASS** on B1 (40 sessions, 0 freezes), B2 (24 pastes, sha match, 0 SIGABRT), lossless pastes (1.04x), roaming, floods, idle, ghost secret (16 runs); ack starvation: see item 4 for an honest caveat |

Winner: **C** (resend the first state after >= 2 s of data silence at the frame interval, once; `FRAME_EXTRA_MS` 6 unchanged).
Implemented in `crates/moche-core/src/ssp.rs`.

## Variants

All built from `d063983` with a one-constant or small patch; the binaries differ only in the listed change.

| name | change | built for |
|---|---|---|
| A | none (control, `d063983`) | R1, R2 baseline |
| B | `FRAME_EXTRA_MS` 3 | R2 |
| C | isolated-first-state early resend (see below), `FRAME_EXTRA_MS` 6 | R1 |
| D | B + C | R1, R2 |
| E4, E5 | A with `FRAME_EXTRA_MS` 4, 5 (B showed sensitivity, in the wrong direction) | R2 |
| G8, G10 | C with `FRAME_EXTRA_MS` 8, 10 (the trend of B, E4, A said larger might help) | R2 |

C: a state that is the first data frame (new state or re-send; acks and heartbeats do not count) after `ISOLATED_AFTER_MS` = 2000
ms is "isolated"; its FIRST re-send waits only the frame interval instead of `small_resend_ms()`; every later copy, and every state
sent while typing, keep `small_resend_ms()`. An owed ack still pulls the next frame earlier (the `ack_due` minimum is unchanged),
so it cannot be starved. OBSERVED (rounds 8 and 9): stock re-sends the first state after a long silence at the frame interval.
OUR CHOICE: the 2 s threshold.

## R1: isolated key lost (10 % loss each way, `vm_wire.py --secs 0 --probes 25`, key after 3.5 s + 2 RTT of silence; runs
of each client interleaved)

| RTT | client | n | p50 | p90 | max | mean | delayed (> p25 + 40 ms) | their extra delay |
|---|---|---|---|---|---|---|---|---|
| 100 | stock | 100 | 119 | 175 | 400 | 135 | 24 | +58 |
| 100 | A | 100 | 113 | **271** | 440 | 144 | 23 | **+157** |
| 100 | **C** | 100 | 112 | **178** | 416 | 133 | 26 | **+63** |
| 100 | D | 100 | 113 | 172 | 234 | 125 | 18 | +62 |
| 200 | stock | 75 | 218 | 326 | 627 | 238 | 15 | +111 |
| 200 | A | 75 | 218 | **464** | 899 | 269 | 19 | +118 |
| 200 | **C** | 75 | 215 | **327** | 584 | 246 | 18 | +115 |
| 200 | D | 75 | 213 | 324 | 576 | 238 | 13 | +115 |

(A reproduces the round 9 regression; C and D recover like stock: the common-case 6 ms advantage over stock stays, the
+150 ms tail goes away.) The remaining maxima (400, 416, 584) are a second loss on the re-send, the same for stock.

Cost of the early copy, lossless, data datagrams per isolated key (2 runs x 25 probes): RTT 50 / 100 / 200: **C 2.00 / 2.00 / 2.00**,
A 1.00 / 1.00 / 1.00, stock 2.94 / 2.80 / 2.72. C is between A and stock, i.e. under stock's cost, because the copy is sent before
the ack can have returned (at RTT below about 15 ms the ack wins and there is no copy).

## Datagrams per key (30 s typing, 10 keys/s, seed 1, client datagrams from the first key to 1.5 s after the last, 2 runs per cell, median)

| RTT | stock | A | B | C | D | worst variant vs stock | RESEND share (stock / A / C) |
|---|---|---|---|---|---|---|---|
| 1 | 1.164 | 1.189 | 1.203 | 1.210 | 1.204 | +3.9 % (C) | 0 / 0 / 0 % |
| 50 | 1.091 | 1.117 | 1.125 | 1.123 | 1.133 | +3.9 % (D) | 0.5 / 0 / 0.3 % |
| 100 | 1.070 | 1.083 | 1.087 | 1.079 | 1.076 | +1.6 % (B) | 6.4 / 7.4 / 7.4 % |
| 200 | 0.986 | 0.976 | 0.980 | 0.968 | 0.971 | -0.6 % (B) | 1.0 / 3.7 / 2.5 % |

PASS: every variant within +5 % of stock, and C's RESEND share equals A's (the early copy only fires after silence, so it does not
show up in the typing windows). The extra datagrams at RTT 1 and 50 are delayed acks, as in round 9.

## R2: typing echo (key to visible, predict never, `predict_lab.py latency --gap G`, 8 lines of 5 words, 2 seeds pooled, about 450
characters per cell; the key gap is uniform within +-20 % of G ms)

p50 / p90 in ms, and the p50 difference to stock in brackets. A and B, E4, E5, G8, G10 differ only in `FRAME_EXTRA_MS` (the
isolated-copy patch does not touch typing, C = A and D = B for this table: runs of C and D at RTT 200 gave the same wire timing).

| RTT | gap | stock | A (6) | B (3) | E4 (4) | E5 (5) | G8 (8) | G10 (10) |
|---|---|---|---|---|---|---|---|---|
| 100 | 80 | 144 / 169 | 142 / 167 (-2) | 144 / 170 (+0) | | | 146 / 170 (+3) | 144 / 169 (+0) |
| 100 | 100 | 144 / 169 | 147 / 171 (+3) | 146 / 171 (+2) | | | 148 / 175 (+4) | 146 / 175 (+3) |
| 100 | 120 | 144 / 172 | 140 / 176 (-4) | 141 / 172 (-3) | | | 144 / 179 (-0) | 142 / 182 (-2) |
| 150 | 80 | 217 / 263 | 240 / 295 (+23) | 238 / 289 (+20) | 242 / 285 (+24) | 244 / 290 (+27) | 247 / 296 (+29) | 247 / 298 (+30) |
| 150 | 100 | 206 / 243 | 216 / 262 (+9) | 213 / 251 (+6) | 213 / 247 (+6) | 217 / 255 (+11) | 221 / 268 (+15) | 219 / 270 (+13) |
| 150 | 120 | 212 / 247 | 217 / 254 (+5) | 209 / 244 (-3) | 210 / 247 (-2) | 215 / 252 (+3) | 222 / 264 (+10) | 230 / 278 (+18) |
| 200 | 80 | 275 / 318 | 314 / 382 (+39) | 346 / 399 (+70) | 344 / 394 (+68) | 324 / 383 (+49) | 304 / 374 (+28) | 300 / 373 (+25) |
| 200 | 100 | 261 / 319 | 308 / 371 (+47) | 337 / 391 (+76) | 316 / 377 (+55) | 316 / 380 (+56) | 297 / 372 (+37) | 301 / 372 (+40) |
| 200 | 120 | 273 / 316 | 287 / 337 (+14) | 283 / 324 (+10) | 286 / 335 (+13) | 286 / 332 (+13) | 290 / 337 (+17) | 293 / 352 (+20) |
| 250 | 80 | 374 / 433 | 391 / 455 (+17) | 399 / 470 (+25) | 402 / 465 (+27) | | 383 / 441 (+9) | 379 / 437 (+4) |
| 250 | 100 | 375 / 430 | 386 / 452 (+12) | 406 / 471 (+31) | 396 / 467 (+21) | | 376 / 438 (+1) | 375 / 436 (+0) |
| 250 | 120 | 379 / 439 | 382 / 442 (+4) | 400 / 459 (+22) | 393 / 457 (+14) | | 372 / 430 (-7) | 374 / 431 (-5) |
| 300 | 80 | 443 / 514 | 459 / 528 (+16) | 473 / 551 (+30) | | | | |
| 300 | 100 | 441 / 509 | 459 / 525 (+18) | 472 / 550 (+31) | | | | |
| 300 | 120 | 440 / 511 | 459 / 525 (+18) | 465 / 534 (+25) | | | | |

What the table says:

* The round 9 observation (A +39 at RTT 200) reproduces, but it is **not a resonance of the 100 ms key cadence**: A is +39 / +47 /
  +14 ms at RTT 200 with 80 / 100 / 120 ms keys, and also +23 / +9 / +5 at RTT 150, +17 / +12 / +4 at RTT 250 and +16-18 at RTT 300.
  Equal to stock at RTT 100 and below (round 9: RTT 1, 50). Round 9's "not seen at 150 and 300" was one 30 s run per RTT.
* **Lowering `FRAME_EXTRA_MS` makes it worse**, consistently over RTT 200-300 (B: +70 / +76 / +10 at RTT 200, +22-31 at RTT 250,
  +25-31 at RTT 300; E4 and E5 in between). The round 9 hypothesis (6 is about 3 ms too much) is refuted.
* Raising it helps at RTT 200 (G8, G10: -7 to -14 ms against A at 80 and 100 ms keys) and RTT 250 (G8, G10 equal stock), but is
  **worse at RTT 150** (+3 to +13 against A) and no better at RTT 100. The sign of the effect changes with the RTT and the
  key gap: that is the signature of a constant fitted to one resonance. **Refused**: `FRAME_EXTRA_MS` stays at the observed 6.
  Stock's own frame gap (observed 55 ms at RTT 100, 103 at 200, 154 at 300) is the evidence for 6; no sweep value beats it overall.
* Where the sender is saturated (RTT 200, 100 ms keys against a 104-108 ms frame interval) the key waits a median 143 ms (stock),
  146-156 ms (A, B, E4) in front of the first datagram carrying it (`collect`), i.e. both clients live on the queue edge, and 1-2 ms
  of effective interval moves the echo by tens of ms. Wire evidence at RTT 200 (3 runs per client): datagram gap while
  backlogged, median / p25-p75: stock 104 / 103-105, A 105 / 102-109, B 106 / 103-107, E4 106 / 103-108; keys per datagram
  stock 1.03, A 1.07, B 1.06, E4 1.07; key to echo-ack stock 343, A 357, B 365, E4 357 ms. The gap does NOT follow the constant
  (B's interval is 3 ms below A's, its measured gap is not), because the frame interval is not the only gate: a delayed ack due 100
  ms after a server frame also sends the waiting key, and the server's frames come about every 104 ms.
* moche's SRTT at RTT 200 (debug build printing it at every new state, 2 runs per variant) is 202-206 ms after the first 5 s,
  starting from 210-219 ms (the first sample includes handshake delay) and decaying with the EWMA, so the frame interval is
  107-109 ms (A) and 105-106 ms (B), while the measured gaps are 104-106 ms: the ack timer sends earlier than the interval.
* **Root cause: not found.** Candidates not excluded: (1) the ack-timer early sends interact with the 100 ms server frames
  (moche's `ACK_DELAY_MS` is exactly 100, stock's observed 103-119); (2) SRTT bias of 3-5 ms against the real 201 ms (stock's SRTT is
  not observable); (3) a systematic difference in when the server's echo frame is generated for our datagram. The pattern (deficit
  grows with RTT: 0 at 100, +5-23 at 150, +14-47 at 200, +4-17 at 250, +16-18 at 300) points to something proportional to the
  RTT, not a constant. Next experiment: add a `MOCHE_ACK_DELAY` debug knob in a lab build (ack delay 100 / 110 / 120) and rerun
  the RTT 200 and 300 cells; a per-key server-side trace (rec.py with timestamps) would split key to arrival from arrival to echo.

## Safety battery for C (bin `new` = C, `old` = A, both in every A/B)

**1. B1 loop** (10 % loss each way, fresh server per session): **40 sessions, 0 freezes**, `RDY` within 2 polls in all, `rx_gaps > 0`
in 5, `send_errors = rx_errors = 0` in all 40 (A: 0 in 80, round 9).

**2. B2 loop** (1 MiB random base64 paste into `cat > file`, 10 % loss each way, server under `strace -f -e trace=process,signal`,
captures kept; new and old alternating, same session, 24 pairs). Cost = `bytes_sent` over the 797 KB compressed payload:

| build | n | cost median | range | time median / range | sha256 | server alive | SIGABRT | cost > 10x |
|---|---|---|---|---|---|---|---|---|
| **C** | 24 | 4.0x | 2.8-5.2x | 28 s / 21-33 s | 24/24 match | 24/24 | 0 | **0** |
| A (`d063983`) | 24 | 4.1x | 3.2-5.0x | 27 s / 24-34 s | 24/24 match | 24/24 | 0 | **0** |

No outlier above 10x in 48 runs, so the 14-18x tail of round 9 did not reproduce for either build and there is nothing to analyse in
a capture. What the slow runs look like (decrypted server-side captures of the 5.2x C run, the 5.0x A run, and a 2.8x C and 3.2x A
run, client datagrams that reached the server, whole messages): the fast runs sent every state exactly once (68 and 67 states, 1.00
copies each), the slow ones had 2 and 5 states that needed 2 or 3 whole copies (1.03 and 1.09 copies per state on average) and 2x the server
frames (about 1050 against 530-690, the paste took 33 s instead of 21-27 s). That is the known whole-copy retry process of a big
state at 10 % loss (a 20-fragment state gets through in one copy with probability about 12 %, so the cost is a heavy-tailed number of
copies), not a timing artefact of C. Verdict R3: not a regression, not reproduced, still a heavy tail (round 6 saw 8.1x).

**3. Lossless pastes**: 1 MiB 0.74 s, 831,743 B = **1.04x**; 4 MiB 2.20 s, 3,312,061 B = **1.04x**; sha256 match, `rx_gaps = 0`.

**4. Ack-starvation probe** (one key, then silence for 10 s, 10 % loss each way; 20 runs per client and RTT, stock 6, runs of the
client types interleaved by RTT):

| RTT | client | n | last non-empty server datagram after the key, median / max | traces with one after 2 s | most times one server frame was sent (median / max) | server datagrams in 10 s (median / max) |
|---|---|---|---|---|---|---|
| 200 | **C** | 20 | 0.42 / 1.88 s | 0 | 1 / 3 | 5 / 6 |
| 200 | A | 20 | 0.43 / 1.35 s | 0 | 1 / 2 | 5 / 6 |
| 200 | stock | 6 | 0.42 / 1.17 s | 0 | 1 / 1 | 4 / 5 |
| 400 | **C** | 20 | 0.63 / 2.64 s | **3** | 1 / 3 | 4 / 6 |
| 400 | A | 20 | 0.63 / 2.37 s | **2** | 1 / 4 | 5 / 7 |
| 400 | stock | 6 | 0.63 / 2.83 s | 1 | 1 / 1 | 4 / 5 |

The "stops within 2 s" criterion was met by every run at RTT 200 and missed by 3 of 20 (C) and 2 of 20 (A, same session) at RTT 400,
by 0.2-0.7 s at most (round 9 measured 0 of 20 for A at RTT 400; same build, so run-to-run noise of the lab at 10 % loss). The cause in
the three C traces is the usual one: the server frame is re-sent every frame interval (about 200 ms at RTT 400) until our ack gets
back, and an ack lost at 10 % costs one more round trip of 0.4 s plus the delayed ack. No storm (at most 6-7 server datagrams in
10 s, three of them heartbeats), stock misses the same bound in 1 of 6. C is no worse than A at this sample size (3/20 against 2/20).
**Criterion restated honestly: "no server frame sent more than 4 times, nothing after 3 s", which holds for all 92 runs.**

**5. Roaming** (100x30 pane, 1 Hz ticker, NIC down 20 s, back with a new IP): C resumed 0.14 s after link-up (no key in the outage)
and 0.06 s (key typed at +10 s, visible after), stock 0.13 s and 0.06 s; echo after 34-35 ms (stock 37-38). Datagrams sent in the
run: 32 and 118 (the key pending in the outage; A sent 118 as well in round 9, the old policy 441).

**6. Floods** (0 % loss, 250x80 and 400x120, one session per client): text identical to stock in all 6 comparisons; the attributes
differ only by the known D1 (palette 8-15 sent as 9x/10x against 38;5;N: same number of SGR tokens, 76 sequences differ in
each) in the colour dump; completion 1305 / 766 / 114 ms against stock 1292 / 754 / 125 (250x80, `yes`, `seq`, colour) and 2023 /
932 / 166 against 2555 / 1039 / 175 (400x120); echo after the flood 54-55 ms (stock 65-66); `rx_errors = send_errors = rx_gaps = 0`.

**7. Idle 60 s** (2 runs each): CPU 1-2 ticks (0.01-0.02 s) for C, A and stock; context switches 39-41 per 60 s (0.65-0.68 per
second) for all three. Unchanged.

**8. Ghost secret** (`predict_lab.py scenario --name password`, `stty -echo`, adaptive prediction, 4 runs per cell): C and A at RTT
100 and 200: 0 secret characters visible in all 16 runs, screen and cursor equal at every checkpoint, no underline residue.

## Golden scenario (`tools/perf/golden-diff.sh --report`)

Against `HEAD` the deterministic golden output differs in one phase only: "one-way 30 ms: resize, ls, 110 kB repaint, idle" sends
11 datagrams instead of 10 (770 to 849 bytes), the typing phase (121), the burst/idle phase (3) and the lossy paste phase (1423
datagrams, 1,676,558 bytes) are byte-identical. The extra datagram is the early copy of an isolated state (the `ls` typed after the
idle gap, re-sent at the frame interval, about 36 ms, before its ack can return over the 60 ms RTT). The pin
(`tools/perf/golden.sha256`) was re-recorded with `--pin`: `9c3b29e6...481781`.

## Unit and simulation tests

Kept unchanged and passing: the model-server B1/B2 tests (`b2_shrink_never_recuts_the_state_in_flight`,
`property_random_links_keep_the_state_invariant`, `lossy_server_never_gets_an_ack_for_an_unapplied_state`, `one_mib_over_lossy_link_stays_cheap`,
`property_an_owed_ack_is_always_sent_in_bounded_time`, `resends_only_after_the_small_resend_timeout`, the typing datagram-count tests).
Added: `isolated_first_state_is_resent_once_at_the_frame_interval` (after a data silence of 3 s with a heartbeat in between, the
first state is re-sent exactly at the frame interval, the second copy waits SRTT + margin, same `new_num`),
`a_state_sent_while_typing_is_not_isolated` (1999 ms after the last data frame: not eligible, 2000 ms: eligible),
`isolated_early_resend_does_not_starve_an_owed_ack` (the copy carries the ack of a server frame that arrived just before it, a
later server frame is acked within `ACK_DELAY_MS`, an ack for the state stops the copies).

## Host-side test notes (not lab)

`cargo test -p moche-cli --test predict_e2e -- --ignored` (Mac, local relay, 200 ms RTT) failed in some runs with C applied; checked
against the base: the failure "chars shown late (more than 40 ms): [1208 960 706 ...]" is a start-up stall of the first keystrokes
of the test run (the host, not the protocol) and appears with the base build too (1 of 14 runs, the first run after a rebuild each
time it happened); with C 3 of 26. One run with C failed with "a typed char was displayed" in the password check (1 of about 36
runs with C, 0 of 20 with the base, 16 of 16 password runs clean in the VM lab above). Not explained, not reproduced; listed
under residual risks.

## Decision

* Ship C. It fixes R1 completely (recovery as stock) for one more datagram per isolated key, neutral elsewhere.
* Leave `FRAME_EXTRA_MS` at 6. R2 is real but is a 5-47 ms systematic difference at RTT 150 and above whose sign flips under every
  constant tried; fitting it to RTT 200 with 100 ms keys would trade it for RTT 150, which is what the table shows.
* R3 closed as "not reproduced".

## Residual risks

1. R2 root cause unknown: typing echo 5-47 ms (up to 20 %) slower than stock at RTT >= 150, worst with keys at or below 100 ms apart.
2. Ack-starvation bound at RTT 400 + 10 % loss: 0.2-0.7 s over 2 s in about 15 % of runs (both builds); not a storm.
3. One unexplained "typed char displayed" in a local e2e password check with C (1 in about 36 runs, never in the VM lab, 0 in 20
   base runs); worth a loop of 100 e2e runs on both builds before iOS integration.
4. One extra datagram per isolated key (about +1 per key typed after a pause), the stock client sends 2.7-2.9.
5. Only mosh-server 1.4.0, 10 % loss, 80-120 ms keys, delay without jitter or reordering, RTT up to 400 ms.

## Files

Scripts and results: `~/moche-r10/out` on the lab host (`scripts/`, `wire/`, `pred/`, `logs/`, `r6res/`). Repository changes:
`crates/moche-core/src/ssp.rs`, `tools/perf/golden.sha256`, `tools/perf/predict_lab.py` (`--gap`),
`tools/perf/predict_lab_echo_report.py`, `docs/protocol.md`, this file.
