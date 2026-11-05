# Observations, round 9: the round 8 resend/collect policy against a real stock mosh-server (VM lab)

Date: 2026-11-04/05. Build under test: `d063983` ("ssp: resend small unacked states after ~110 ms, not every frame slot; no
collect delay; frame interval SRTT/2+6"), `aarch64-unknown-linux-musl` release built with `tools/release/build.sh` from a fresh
clone on the lab host (`moche 0.1.0 (d063983, ...)`). The previous commit `d07618b` was built the same way and run **in the same
session, same day, same VMs** as "old" for every A/B. Lab as in rounds 4-7 (`tools/vm-lab/README.md`): server `moche-a` (Debian 13,
stock mosh-server 1.4.0), client `moche-b` (Ubuntu 24.04, stock `mosh-client` 1.4.0 and the two moche binaries). Netem on `eth0` of
**both** VMs (one-way D on each side, so RTT = 2D plus about 1 ms of base; "RTT 100" = `delay 50ms` on each VM), control over the
vzNAT `lima0` addresses. `tc netem` (delay, loss), `tcpdump -c` (deleted after use), `strace -f -e trace=process,signal` on the
server in the B2 loop, `iptables` counters and `ip link/addr` for roaming; nothing else. Stock mosh is a black box (no source
read). `crates/**` untouched, nothing committed. Addresses are not recorded here.

## Summary

| # | Item | Verdict |
|---|---|---|
| 1 | Wire count A/B, RTT 1/50/100/150/200/300, 30 s typing at 10 keys/s | **PASS**: new within 3.5 % of stock at every RTT (+3.5, +1.9, 0.0, -0.4, -1.7, -0.3 %); old was 2.70x stock's count at RTT 50 and 1.76x at RTT 100. RESEND class under 5 % of keys everywhere except RTT 100 where stock itself is 7.8 % (new 7.8 %) |
| 2 | Echo latency, predict never (key to visible) | **PASS at RTT 1/50/100** (new = stock +-3 ms, 9-13 ms better than old); **REGRESSION at RTT 200: new 303 ms, stock 264, old 267** (+39 ms, reproduced on the wire at +30 ms, not seen at RTT 150 and 300) |
| 3 | Typing at 10 % loss each way, RTT 50 and 100 | **PASS for delivery** (every key reached the server: 922/922 in each cell; no freeze: longest gap between server data frames 0.35 s). **Tail regression**: an isolated last key that is lost is recovered ~100 ms later than stock or old (p90 269 ms against 173 / 186 at RTT 100, 75 probes per client) |
| 4 | B1 loop, 10 % loss, fresh sessions | **PASS: 0 freezes in 80 sessions** (32 + 48), `RDY` within 0.4 s in all, `rx_gaps>0` in 5, `send_errors=rx_errors=0` |
| 5 | B2 loop, 1 MiB paste, 10 % loss (+100 ms) | **PASS for correctness**: 9 + 6 + 8 + 16 runs (new 10 % loss: 20, old: 16, 10 % loss + 100 ms: 3), sha256 match in all, 0 SIGABRT, 0 server deaths. Cost: median 4.2x for both builds; new had 2 outliers in 20 (18.5x, 14.4x), old 0 in 16 (not significant, p about 0.3) |
| 6 | Lossless pastes | **PASS**: 1 MiB 0.94 s / 1.04x, 4 MiB 2.94 s / 1.04x, sha256 match (rounds 6/7: 0.8-1.0 s, 2.7-2.8 s) |
| 7 | Floods at 250x80 and 400x120 | **PASS**: pane text identical to stock in all 6 comparisons, attributes identical after the known D1 normalisation, completion never more than 2 % slower than stock (400x120 `seq`: 987 against 1203 ms) |
| 8 | Ack starvation probe, one key then silence, RTT 200/400 at 10 % loss | **PASS**: 40 of 40 runs stopped within 2 s (max 1.12 s at RTT 200, 1.86 s at RTT 400, no server frame sent more than 3 times); stock needed up to 6.35 s |
| 9 | Roaming: NIC down 20 s, back with a new IP | **PASS**: typing and screen resumed 0.14 s after link-up (stock 0.14 s), with and without a key typed during the outage |
| 10 | Prediction with the new timing, CPU per key | **PASS**: no ghost secret (adaptive), contradictions retracted, converged = stock; CPU per key 0.40 ms (old 0.60, stock 0.94), wakeups 29/s (old 46/s, stock 45/s) |
| 11 | Idle 60 s | **PASS**: 0.00-0.01 s CPU, 0.67 wakeups/s, identical to old and stock |

Whether to ship: see "Verdict" at the end. Short form: safe (no freeze, no abort, no starvation, roaming fine, 40 % fewer wakeups and
less CPU), the regression it fixes is large (1.9-3.0x datagrams while typing at RTT 50-100), but it introduced three measurable
costs that I would tune before shipping: lost-isolated-key recovery (+~100 ms), a +30 ms echo latency at RTT 200, and a possible
heavier cost tail in the lossy paste.

## Method

Everything runs from the lab host with scripts kept next to this repo (`tools/perf/vm_wire.py`, `vm_wire_run.sh`,
`vm_wire_report.py`, `rec.py`, plus the round 6 drivers `rp5.sh`/`rpb.sh`, `r4.sh`/`pb.sh`, `fld.sh`/`fl.sh`, `nm.sh` which stay on
the lab host):

* **Wire runs** (items 1, 3, 8, 10 CPU, 11): `vm_wire.py` inside the client VM starts `tcpdump -ni eth0 -c N` on the data path
  (the client edge: client datagrams are captured before the netem qdisc, server datagrams after it), runs the client on a pty that
  the script owns, types random lowercase letters with 80-120 ms gaps (seeded) and writes the key times; the pcap is converted to
  the `ts C|S hex` trace format of `delay_relay.py` and decoded with the session key by `trace_decode` and `wire_analyze.py`
  (same classes as round 8: NEWKEY, RESEND, ACK, EMPTY; BURST was 0 everywhere in the VM lab, unlike the Mac). Per-key = client
  datagrams from the first key to 1.5 s after the last, divided by keys (296 per 30 s run).
* **Server-side key recorder** (`rec.py`, item 3): the mosh-server runs a raw-tty recorder that logs and echoes every byte, so
  "every key delivered" is a count on the server and the client still sees an echo.
* **Echo latency** (item 2) and **prediction scenarios** (item 10): `predict_lab.py` run from the lab host instead of from the client
  VM (a generated copy, every client-VM command over ssh, tmux control pipe over ssh; the VMs cannot ssh to each other without a
  key, and I did not install one). The stock client was run with `MOSH_PREDICTION_DISPLAY=never` for the latency runs (without it
  its own prediction shows the character at once: 3 ms, which is not the quantity measured here); in the scenario runs stock keeps
  its default prediction, as the reference for the moche adaptive mode. The ssh pipe adds about 1 ms to every client sample and
  equally to all clients.
* Pastes, floods, B1: the round 6/7 drivers unchanged except for the binary path.

## 1. Wire count A/B (30 s, 296 keys, seed 1; 3 runs per cell, 2 at RTT 150 and 300)

Client datagrams per key, median of the runs (each run in brackets for the middle RTTs):

| RTT ms | stock | old (`d07618b`, never) | **new (never)** | new (adaptive) | new vs stock |
|---|---|---|---|---|---|
| 1 | 1.166 (1.166 1.172 1.162) | 1.057 | **1.206** (1.209 1.206 1.196) | 1.203 | +3.4 % |
| 50 | 1.101 (1.101 1.078 1.105) | **2.970** (2.963 2.970 2.986) | **1.122** (1.122 1.128 1.115) | 1.115 | +1.9 % |
| 100 | 1.081 (1.054 1.081 1.084) | **1.899** (1.895 1.909 1.899) | **1.081** (1.081 1.061 1.088) | 1.078 | 0.0 % |
| 150 | 1.050 | 1.293 | **1.046** | | -0.4 % |
| 200 | 0.990 (0.993 0.990 0.990), 0.980 | 0.997, 0.984 | **0.973** (0.973 0.966 0.976), 0.963 | 0.970 | -1.7 % |
| 300 | 0.665 | 0.667 | **0.663** | | -0.3 % |

RESEND class (same `new_num` sent again) as a share of keys, median:

| RTT ms | stock | old | new |
|---|---|---|---|
| 1 | 0.0 % | 0.0 % | 0.0 % |
| 50 | 0.3 % | **196.6 %** | 0.0 % |
| 100 | 7.8 % | **89.5 %** | 7.8 % |
| 150 | 4.7 % | 28.9 % | 4.5 % |
| 200 | 1.7 % / 0.7 % | 6.8 % / 4.7 % | 3.7 % / 2.8 % |
| 300 | 0.7 % | 0.7 % | 0.7 % |

Standalone acks per run (median): RTT 1: stock 49, old 17, **new 61**; RTT 50: stock 28, old 1, **new 36**; RTT 100 and above: 1-3 for all.
So at low RTT the new policy sends about 4 % more datagrams than stock, all of them delayed acks (stock piggybacks slightly more
often); well inside the 10 % target. The "RESEND under 5 %" target is met except at RTT 100, where stock itself resends on 7.8 % of
keys (identical for new): the target should read "no more than stock".

Timing, RTT 100, run 2 (client edge): key to first datagram carrying it, median 52 ms stock / 50 ms new (frame-slot bound, 56 ms),
resend gap median 106 / 107 ms (stock 99-147, new 102-149), key to server echo-ack 182 / 182 ms; the whole difference to old is the
RESEND column. At RTT 1 the key goes out after 0 ms (new) against 2 ms (stock) and 8 ms (old, the removed collect delay).

## 2. Echo latency, key to visible, predict never (`predict_lab.py latency`, 8 lines of 5 words per run, 2 seeds, pooled characters)

| RTT ms | client | chars | min | p50 | p90 | max | per-run p50 |
|---|---|---|---|---|---|---|---|
| 1 | stock | 490 | 12 | 16 | 38 | 48 | 15 16 |
| 1 | old | 490 | 14 | 30 | 34 | 49 | 30 30 |
| 1 | **new** | 490 | 12 | **17** | 38 | 49 | 17 17 |
| 50 | stock | 489 | 62 | 74 | 85 | 108 | 74 74 |
| 50 | old | 490 | 70 | 84 | 91 | 101 | 83 85 |
| 50 | **new** | 490 | 59 | **75** | 87 | 122 | 75 75 |
| 100 | stock | 474 | 111 | 146 | 172 | 192 | 146 147 |
| 100 | old | 472 | 118 | 157 | 182 | 459 | 157 156 |
| 100 | **new** | 474 | 112 | **147** | 174 | 205 | 148 146 |
| 200 | stock | 453 | 4 | 264 | 318 | 361 | 270 259 |
| 200 | old | 449 | 2 | 267 | 325 | 391 | 265 270 |
| 200 | **new** | 451 | 2 | **303** | 375 | 438 | 301 305 |

(Characters never seen are the typed spaces, trimmed by `capture-pane`; 16-41 per cell, equal across clients.)

* RTT 1/50/100: new is within 1 ms of stock at the median and 9-13 ms better than old (old: the 8 ms collect delay at RTT 1, the
  frame-slot wait behind a pointless resend at RTT 50 and 100). **PASS.**
* **RTT 200: new is 39 ms slower than stock and 36 ms slower than old, in both seeds (301/305 against 270/259).** Wire-level
  confirmation with the 30 s typing runs (key to server echo-ack at the client edge, medians of 2-3 runs each): RTT 200 stock
  332-347 ms, old 352-354, new 362-366 (+15 to +30 ms); RTT 150: stock 250, old 290, new 252; RTT 300: stock 486, old 488, new 442.
  So it is specific to RTT 200, where the frame interval (about 103-106 ms) sits right on the 10 keys/s typing cadence (100 ms): the
  sender is just overloaded, keys are batched (1.08 keys per datagram for new, 1.04 for stock) and every key waits nearly a slot. The
  inter-frame gap of new-state datagrams at RTT 200: stock median 103 (p75 105), old 103 (104), **new 106 (p75 110)**; at RTT 300 all
  three are 154. A frame interval 3 ms longer than stock's at this resonance costs tens of ms. See the tuning proposals.

## 3. Typing at 10 % loss each way (RTT 50 and 100), 30 s + 8 isolated probe keys, 3 runs per cell

Server-side recorder: keys delivered / typed; probe = a key typed after >= 3.5 s of silence, time from the key to the first server
data datagram (the echo; includes any loss recovery); "typing key to server" is the same for every typing key.

| RTT | client | keys delivered | probe p50 | probe p90 | probe max (n=24) | typing key to server p50 / p90 | longest gap between server data frames while typing |
|---|---|---|---|---|---|---|---|
| 50 | stock | 922/922 | 68 | 109 | 126 | 27 / 71 | 0.28 s |
| 50 | old | 922/922 | 72 | 121 | 246 | 28 / 69 | 0.24 s |
| 50 | **new** | 922/922 | 63 | 67 | 120 | 25 / 66 | 0.35 s |
| 100 | stock | 923/923 | 117 | 124 | 186 | 33 / 105 | 0.33 s |
| 100 | old | 922/922 | 123 | 175 | 183 | 32 / 79 | 0.29 s |
| 100 | **new** | 922/922 | 112 | 271 | **420** | 37 / 110 | 0.27 s |

Every key reached the server, no freeze (no gap above 0.35 s), no probe went unanswered in 8 s. The 24-probe cells are too small
for the tail, so 75 isolated probes per client at RTT 100 (3 runs x 25) and 50 at RTT 200 (2 x 25) were added:

| RTT | client | n | p50 | p90 | max | mean | delayed probes (> p25 + 40 ms) | their extra delay (median) |
|---|---|---|---|---|---|---|---|---|
| 100 | stock | 75 | 119 | 173 | 353 | 131 | 17 (23 %) | +58 ms |
| 100 | old | 75 | 122 | 186 | 283 | 141 | 19 (25 %) | +60 ms |
| 100 | **new** | 75 | **113** | **269** | **448** | 146 | 15 (20 %) | **+157 ms** |
| 200 | stock | 50 | 219 | 329 | 540 | 240 | 7 (14 %) | +114 ms |
| 200 | old | 50 | 222 | 328 | 536 | 248 | 11 (22 %) | +105 ms |
| 200 | **new** | 50 | 216 | **463** | 581 | 253 | 10 (20 %) | **+185 ms** |

Sorted new probes at RTT 100: 113 x 22, then 156 161 171 176 180 266 267 269 270 274 279 431 431 448 448; stock: 119 x 21, then 168-185,
353. In the common case (no loss) new is 6 ms faster than stock (no collect delay); when the key or the reply is lost, new recovers
after the small-resend pause `max(110, min(SRTT + 50, 250))` = 150-160 ms at RTT 100 and 250 ms at RTT 200, while stock (and old, at the
frame slot) re-send an isolated first key at the frame interval (56 ms at RTT 100: round 8 "post-silence early re-send"). Net cost:
about +100 ms for the ~20 % of isolated keys that need a resend at 10 % loss each way, i.e. +15 ms mean at RTT 100 and +13 ms at RTT 200
against stock. Not a freeze, but it is the cost foreseen in round 8 risk 1 and 8, now measured.

(The `tailok` column in the raw files, "last 30 typed characters found in the client's output", fails for stock because mosh-client
repaints with cursor movements between rows; it is not used.)

## 4. B1 loop (10 % loss each way, fresh server per session, `echo RDY""<i>`, up to 20 s)

| run set | sessions | freezes | RDY seen after | `rx_gaps > 0` | `send_errors` / `rx_errors` |
|---|---|---|---|---|---|
| first | 32 | **0** | 2nd poll (0.4 s) in all | 3 | 0 / 0 |
| second | 48 | **0** | 2nd poll in all | 2 | 0 / 0 |

Total 80 sessions, 0 freezes (round 7: 0 of 32; round 4, the bug: 4 of 28). The re-ack path fired in 5 sessions, each completed.

## 5. B2 loop (1 MiB random base64 paste into `cat > file`, 10 % loss each way, server under `strace -f -e trace=process,signal`)

Cost = `--stats bytes_sent` over the 797 KB compressed payload (the round 7 convention). All runs: sha256 match, server alive,
`killed by SIGABRT` count 0.

| set | run | time (s) | bytes_sent (MB) | cost |
|---|---|---|---|---|
| new, 10 % loss | 1 | 28.2 | 2.93 | 3.7x |
| | 2 | 27.8 | 2.96 | 3.7x |
| | 3 | 59.0 | **14.71** | **18.5x** |
| | 4 | 32.7 | 4.27 | 5.4x |
| | 5 | 22.7 | 2.68 | 3.4x |
| | 6 | 27.6 | 3.28 | 4.1x |
| new, 10 % loss, second batch | 1-5 | 23.2 28.5 28.1 26.1 28.2 | 2.46 3.18 3.33 3.15 3.68 | 3.1 4.0 4.2 3.9 4.6x |
| | 6 | 50.0 | **11.44** | **14.4x** |
| old, 10 % loss, same day | 1-8 | 31.9 26.0 27.5 29.1 27.3 29.0 28.7 21.8 | 3.66 4.13 3.40 2.96 3.04 3.35 3.45 2.55 | 4.6 5.2 4.3 3.7 3.8 4.2 4.3 3.2x |
| new, 10 % loss + 100 ms one way | 1 | 58.1 | 2.67 | 3.3x |
| | 2 | 68.0 | 3.24 | 4.1x |
| | 3 | 86.9 | 4.68 | 5.9x |

Interleaved A/B, same session (new, old alternating, 10 % loss, 8 pairs):

| build | cost per run (x of 797 KB) | time (s) | median cost |
|---|---|---|---|
| new | 4.2 4.2 4.9 4.5 4.6 4.9 4.5 3.7 | 27.1 33.0 31.8 29.8 28.9 33.2 28.5 26.3 | 4.5x |
| old | 4.3 4.5 4.3 3.9 3.6 3.7 3.7 4.1 | 25.9 35.6 26.7 25.2 23.0 27.0 28.1 27.7 | 4.1x |

All 16: sha256 match, server alive, 0 SIGABRT. Over all 10 %-loss runs: new 20 runs (median 4.2x, range 3.1-5.4x plus the 14.4x and 18.5x
outliers), old 16 runs (median 4.2x, range 3.2-5.2x). New is about 0.3x costlier on the median in the interleaved pairs (4.5 against 4.1)
and 1-2 s slower per paste, which is within what the 110 ms resend pause could explain for the small frames around the big states, but also
within the lab noise.

Round 7 (`bf5951d`) for comparison: 10 % loss 24.5-33.8 s, 3.6-5.1x; 10 % + 100 ms 63.7-84.1 s, 3.9-7.6x. The cost is the
whole-copy retry process of a big state with its heavy tail (round 6 saw one 8.1x). The new build has two outliers in its first 12 runs (18.5x,
14.4x), more than any earlier round; The interleaved batch and the 8 old runs did not reproduce them (new 8 runs 3.7-4.9x), so with 2 of 20 against 0 of 16 I cannot tell a regression
from the known heavy tail; no capture of the two runs was kept. No server death, no mismatch in any run, which is the property under test.

## 6. Lossless pastes (`--stats`)

| size | path | time | client bytes | cost | sha256 |
|---|---|---|---|---|---|
| 1 MiB | RTT 1 | 0.94 s | 832,224 | 1.04x | match |
| 4 MiB | RTT 1 | 2.94 s | 3,310,800 | 1.04x | match |
| 1 MiB | one-way 150 ms | 7.3 s | 829,549 | 1.04x | match |
| 4 MiB | one-way 150 ms | 25.3 s | 3,310,188 | 1.04x | match |
| 1 MiB | stock client, RTT 1 | 0.24 s | | | match |

Round 6/7 (RTT 1): 0.82-1.04 s and 2.67-2.83 s, 1.04x. Same cost per byte, `rx_gaps=0`.

## 7. Floods (0 % loss, one session per client, text and escape-sequence panes compared)

| size | step | moche done | stock done | text | attributes |
|---|---|---|---|---|---|
| 250x80 | `yes \| head -c 5000000` | 1388 ms | 1364 ms | identical | identical |
| 250x80 | `seq 1 1000000` | 762 ms | 767 ms | identical | identical |
| 250x80 | 256-colour dump | 112 ms | 114 ms | identical | identical after the known D1 (palette 8-15) |
| 400x120 | `yes` | 1943 ms | 1982 ms | identical | identical |
| 400x120 | `seq` | 987 ms | 1203 ms | identical | identical |
| 400x120 | colour dump | 164 ms | 175 ms | identical | identical after D1 |

Echo after the flood 55-63 ms (moche) / 63-66 ms (stock); `rx_errors=0 send_errors=0 rx_gaps=0`.

## 8. Ack starvation probe (one key, then nothing for 12.5 s; RTT 200 and 400, 10 % loss each way)

New: 20 runs per RTT; stock: 8 per RTT (one RTT 200 stock trace is empty, a capture glitch, counted as -1). "Last non-empty server
datagram" is the last server datagram with a non-empty diff (data or echo ack) after the key, i.e. the moment the server stopped
re-sending because our ack arrived.

| RTT | client | n | last non-empty server datagram after the key, median / max | runs with one after 2 s | most times one server frame was sent (median / max) | server datagrams in 10 s (median / max) | client datagrams in 10 s |
|---|---|---|---|---|---|---|---|
| 200 | **new** | 20 | 0.40 s / **1.12 s** | **0** | 1 / 3 | 5 / 6 | 6 |
| 200 | stock | 8 | 0.40 / 6.35 | 1 | 1 / 11 | 5 / 13 | 6 |
| 400 | **new** | 20 | 0.63 s / **1.86 s** | **0** | 1 / 2 | 5 / 5 | 7 |
| 400 | stock | 8 | 0.63 / 3.75 | 1 | 1 / 5 | 5 / 7 | 6 |

PASS: 0 of 40 new runs let the server go on past 2 s, no storm (at most 6 server datagrams in 10 s, three of them heartbeats), and the
new build is better than stock in the tail (stock's outliers are lost acks that stock re-sends late).

Long idle after a key (RTT 100, no loss, 20 s, 3 runs each): server heartbeat every 3.0 s in all three clients; the client answers
each heartbeat with an ack after 0.12 s (stock), 0.09 s (old) and 0.13-0.15 s (new): sane, 3 client + 3 server datagrams per 10 s. The
last non-empty server datagram after the key: 0.17 s (new), 0.18 (old), 0.21 (stock).

## 9. Roaming (100x30 pane, 1 Hz ticker on the server, `ip link set eth0 down`, 20 s, address flushed and replaced by a new IP in the same
subnet, link up; control over `lima0`, a watchdog at +110 s restores link and address)

| client | key typed during the outage | status line shown during the outage | resumed (screen current) after link-up | key typed in outage visible | typed after: echo | datagrams sent in the run |
|---|---|---|---|---|---|---|
| new | no | yes | **0.14 s** | | 28 ms | 32 |
| new | yes (+10 s) | yes | **0.15 s** | yes | 35 ms | 118 |
| old | no | yes | 0.14 s | | 25 ms | 31 |
| old | yes | no | 0.01 s | yes | 34 ms | 441 |
| stock | no | yes | 0.15 s | | 36 ms | |
| stock | yes | yes | 0.14 s | yes | 27 ms | |

PASS (target 1-2 s). The old build sent 441 datagrams in the run with a key pending during the outage (it retried at every frame
slot into the dead link), the new one 118: the longer small-resend pause also helps there.

## 10. Prediction with the new timing, CPU per key

Scenarios (`predict_lab.py scenario`, one run per cell, tmux-captured client pane against the server's pane; adaptive moche against
stock's default prediction):

| scenario | RTT | client | text and cursor equal at every checkpoint | ghost secret chars | other |
|---|---|---|---|---|---|
| password (`stty -echo; read`) | 100 | **new adaptive** | yes | **0** | |
| | 100 | old adaptive | yes | 0 | |
| | 100 | stock | yes | 0 | |
| | 100 | new `always` | yes | 2 chars for 0.19 s | the documented cost of `always` |
| | 200 | new adaptive / old / stock | yes | 0 / 0 / 0 | |
| | 200 | new `always` | yes | 3 chars for 0.28 s | |
| vim insert/normal (`vim_insert_normal`) | 100 | new / stock | yes / yes | | insert echo 2 ms, underlined share 0.93 / 0.80, converged in 504 / 499 ms |
| | 200 | new / stock | yes / yes | | 3 ms, 0.93 / 0.77, 500 / 499 ms |
| vim real alt-screen (`vim_plain`) | 100, 200 | new / stock | `normal_done` checkpoint differs in both (test artefact, equal for stock) | NORMAL mode: 0 underlined cells | |
| contradiction (`transform2`: echo then upper-case) | 100 | new / stock | yes / yes | | 1 retraction each, lowercase visible 0.12 / 0.19 s |
| | 200 | new / stock | yes / yes | | 1 retraction each, 0.38 / 0.30 s |

No residue (underlines left on screen), convergence times equal to stock, no ghost secrets in adaptive mode. (One run per cell: a
sanity subset, not a statistic.)

CPU per key and wakeups (RTT 100, 30 s of typing, 296 keys, 3 runs; `/proc` ticks of the client process, context switches over all threads):

| client | CPU ms per key | context switches per second |
|---|---|---|
| **new** | **0.44 0.41 0.34** | **28.8 29.1 29.2** |
| old | 0.68 0.61 0.51 | 46.3 46.1 46.1 |
| stock | 0.95 0.91 0.95 | 45.3 45.1 45.5 |

New is not worse: about a third less CPU than old and 37 % fewer wakeups (the removed resends).

## 11. Idle, 60 s after the first screen (3 runs each)

| client | CPU seconds | wakeups per second |
|---|---|---|
| new | 0.01 0.00 0.00 | 0.67 0.67 0.62 |
| old | 0.01 0.00 0.01 | 0.68 0.68 0.70 |
| stock | 0.01 0.01 0.00 | 0.67 0.68 0.67 |

Unchanged (heartbeat every 3 s).

## Regressions, minimal repros, hypotheses

1. **Lost isolated key is recovered later than stock or old.** Repro: netem `loss 10 %` on both VMs, RTT 100, type one key after 3.5 s
   of silence, time to the echo (`vm_wire.py --secs 0 --probes 25`, report `vm_wire_report.py loss`). new p90 269 / max 448 ms against stock
   173 / 353. Cause: for the first send after a silence stock re-sends at the frame interval (round 8), new waits
   `max(110 ms, min(SRTT + 50, 250))`. Cost +~100 ms on ~20 % of isolated keys at 10 % loss each way.
2. **Echo latency at RTT 200 +30..39 ms vs stock and old** at 10 keys/s typing (resonance of the 106 ms frame interval with the 100 ms
   key cadence). Repro: netem `delay 100ms` on both VMs, `vm_wire.py --secs 30` (wire: `collect` 156 against 144 ms, echo-ack
   362-366 against 332-347) or `predict_lab.py latency` (p50 303 against 264). Not seen at RTT 150 and 300. Hypothesis: the
   new-state frame gap is 106 ms (p75 110) against stock's 103 (p75 105), i.e. FRAME_EXTRA_MS 6 is about 3 ms too much for this stock
   version/lab, and SRTT noise adds a few more at this resonance.
3. **B2 cost tail**: 2 of 12 new runs at 14-18x, not seen in 8 old runs of the same day nor in earlier rounds (max 8.1x). Not reproduced in 8 interleaved new runs (3.7-4.9x) or 8 old runs; 2/20 against 0/16 is not significant (p about 0.3). Needs a capture-enabled
   rerun (`KEEPCAP=1 r4.sh`) of ~30 runs per build to settle; no repro recipe beyond the loop itself.
4. A harmless observation: new sends about 4 % more datagrams than stock at RTT 1 (delayed acks that stock piggybacks), 12 more
   standalone acks per 296 keys.

## Verdict

**Safe to ship functionally; recommend two small tunings first; do not revert.**

* What it fixes is real and large on a real server: while typing, the old policy sent 2.70x (RTT 50) and 1.76x (RTT 100) the datagrams
  of the stock client (1.3x at RTT 150), the new one is within 3.5 % of stock at every RTT from 1 to 300 ms; the removed collect delay
  and resend-behind-the-slot make echo 9-13 ms faster than old at RTT 1/50/100 (equal to stock), and CPU per key falls from 0.60 (old)
  to 0.40 ms, wakeups from 46 to 29 per second (stock: 0.94 ms, 45).
* What it must not break, did not break: no freeze in 80 B1 sessions, no server abort or hash mismatch in any of 39 lossy pastes,
  1.04x on clean pastes, floods identical to stock, the server stops re-sending within 1.9 s even at RTT 400 with 10 % loss (stock
  needs up to 6.4 s), roaming resumes in 0.14 s, prediction unchanged, idle unchanged.
* Measured costs (all expected from the round 8 risk list, now quantified):
  1. Lost isolated key: +100 ms on the ~20 % that need a resend at 10 % loss each way (mean +15 ms at RTT 100). Proposal (untested,
     `crates/**` not touched): re-send the *first* state sent after >= 2 s of silence at the frame interval, as stock does, then fall back to
     `small_resend_ms()` for the later copies; this costs about one extra datagram per isolated key (stock sends 2.8-3.4 per isolated
     key, new 2.0-2.2). Cheaper variant: `SMALL_RESEND_MARGIN_MS` 50 to 20 (RTT 100: 150 to 120 ms).
  2. RTT 200 typing latency +30..39 ms: `FRAME_EXTRA_MS` 6 to 3 (stock's measured gap is 103 ms at RTT 200 and 154 at RTT 300 in this lab; ours
     106 and 154). Untested; re-run item 2 at RTT 150/200/250 after the change.
  3. B2 cost tail: not shown to be real (see above); watch it in the next B2 loop rather than act.
* Remaining risk if shipped as is: only the two latency items above, both on lossy or RTT-200 paths, both bounded (no loss of data,
  no freeze).

## Not verified

* Jitter, reordering, duplication, asymmetric loss, loss other than 10 %, RTT above 400 ms, other mosh-server versions (1.4.0 only).
* The ack-starvation probe was not run for the old build (not needed for the verdict); stock n is 8 per RTT, with one lost trace.
* Ctrl-C responsiveness during a lossy paste (round 5 item 4) was not re-run.
* Predictive-echo scenarios are single runs per cell, and the stock `mosh-client` was run with its default prediction there but with
  prediction off for the latency table.
* Why `vim_plain` fails its NORMAL-mode checkpoint for both clients (pre-existing harness artefact).
* The root cause of the RTT 200 latency (frame-interval resonance is the hypothesis; no variant build was made, `crates/**` untouched).
* The B2 tail: cause of the 18x and 14x runs (no capture kept), and whether it is the new timing at all.
* iOS, the Swift side, NAT rebinding without a link flap.
