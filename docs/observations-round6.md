# Observations, round 6: validating the B2 fix (states only grow) against a stock mosh-server

Date: 2026-11-03. Build under test: `6dff485` (HEAD: "states only grow, never abandon an in-flight state"; slow start
16 KiB -> 64 KiB, `MIN_STATE_BYTES` 16 KiB, RTO-paced resends of states > 4 KiB, 250 ms loss-RTO floor for the shrink
decision). Same lab as rounds 4 and 5 (`tools/vm-lab/README.md`: server `moche-a`, Debian 13, mosh-server 1.4.0; client
`moche-b`, Ubuntu 24.04, moche release build, stock `mosh-client` 1.4.0). Addresses are placeholders; the control channel is
ssh over the vzNAT `lima0` addresses, so netem on `eth0` (user-v2, the data path) never touched it. Stock mosh is a black
box (no source read; the server was only observed with `strace -f -e trace=process,signal` and captures). No file under
`crates/` was edited. The build in `moche-b` was `cargo build --release -p moche-cli` only (no `cargo test` in the 2 GiB VM).

## Summary

| # | Item | Verdict |
|---|---|---|
| 1 | Source on the lab host fast-forwarded to `6dff485`, tarred into `moche-b`, release build | OK (7 s) |
| 2 | B2 repro loop, 1 MiB paste, 10% loss | **PASS: 10 of 10 completed, sha256 match, 0 server deaths** (round 5: 5 of 5 died). 23-31 s, 3.3-4.9x the compressed payload |
| 2b | Same, 10% loss + 100 ms one-way delay | **PASS: 6 of 6 completed, sha256 match, 0 deaths** (round 5: 1 of 1 died). 59-76 s, 3.6-4.6x |
| 3 | Paste matrix without loss | PASS: cap never shrinks (wire diffs 62-65 KB throughout), cost 1.03-1.11x, 1 MiB none 0.82 s |
| 4 | B1 regression (10% loss, fresh sessions, `RDY` check) | **PASS: 0 freezes in 32 sessions** (round 5: 0 of 65) |
| 5 | Interactivity | echo latency unchanged from round 5; Ctrl-C during a paste = remaining paste time (by construction, ordered stream) |
| 6 | Floods 0% loss (yes 5 MB, seq 1..1e6, 256-colour dump 250x80) | PASS: complete, pane text identical to stock, attributes identical modulo D1 |
| 7 | Ack timing / spurious resends from a lossy client-side capture | no spurious resend found; one weakness noted (our acks ride on lossy big frames), see section 7 |
| 8 | Cleanup | done (netem, iptables, tmux, mosh-servers, captures removed; both VMs running) |

Server aborts: **0 in 16 lossy 1 MiB pastes** (+ 6 extra runs of a scratch variant, section 8) and 0 in every other run.
The `strace -f -e trace=process,signal` wrapper was around every mosh-server of the B2 loop; no `killed by SIGABRT`
appeared and `pgrep` showed the server alive after each run, so no rerun under strace was needed.

## 2. B2 repro loop

Method: `r4.sh`-style driver (rounds 4/5): netem on `eth0` of both VMs; fresh `mosh-server` per run under the strace
wrapper; client `moche --stats --direct <a-u2> <port>` in tmux (200x50); `stty -echo; (5 Hz date ticker) & cat > /tmp/paste.out`;
1,048,509 B of random base64 pasted with `tmux paste-buffer -r`; Ctrl-D and `echo FIN<n>`; completion = `FIN<n>` on screen;
sha256 of `/tmp/paste.out` on the server compared with the source. The driver polls `pgrep -x mosh-server` every 2 s and
would stop on a death. Compressed payload about 797 KB. "Cost" = `--stats bytes_sent` (UDP payload the client handed to
the socket, i.e. before netem) over 797 KB. Note: tcpdump sees packets AFTER the netem qdisc (egress capture is taken at
dequeue), so the capture of a lossy run is smaller than `bytes_sent` (about 10% less); the stats number is the true cost.

### 10% loss (both directions)

| run | time (s) | bytes_sent (MB) | ratio | rx_gaps | sha256 | server alive |
|---|---|---|---|---|---|---|
| 1 | 28.3 | 3.24 | 4.1x | 0 | match | yes |
| 2 | 31.5 | 3.13 | 3.9x | 1 | match | yes |
| 3 | 29.5 | 3.58 | 4.5x | 0 | match | yes |
| 4 | 27.7 | 3.87 | 4.9x | 0 | match | yes |
| 5 | 29.6 | 3.44 | 4.3x | 1 | match | yes |
| 6 | 27.2 | 3.26 | 4.1x | 0 | match | yes |
| 7 | 29.7 | 3.16 | 4.0x | 0 | match | yes |
| 8 | 28.9 | 3.31 | 4.2x | 1 | match | yes |
| 9 | 23.3 | 2.80 | 3.5x | 0 | match | yes |
| 10 | 25.2 | 2.66 | 3.3x | 0 | match | yes |
| mean | 28.1 (23.3-31.5) | 3.24 | **4.07x (3.3-4.9)** | | 10/10 | 10/10 |

`send_errors=0 rx_errors=0 queued_bytes=0` in all 10. Ticker lag (downstream freshness) median 134-147 ms, max about 270 ms.

### 10% loss + 100 ms one-way delay (200 ms RTT)

| run | time (s) | bytes_sent (MB) | ratio | sha256 | server alive |
|---|---|---|---|---|---|
| 1 | 67.3 | 3.69 | 4.6x | match | yes |
| 2 | 69.0 | 2.89 | 3.6x | match | yes |
| 3 | 59.2 | 2.87 | 3.6x | match | yes |
| 4 | 60.3 | 2.88 | 3.6x | match | yes |
| 5 | 76.1 | 3.57 | 4.5x | match | yes |
| 6 | 74.0 | 3.07 | 3.9x | match | yes |
| mean | 67.7 (59.2-76.1) | 3.16 | **3.97x (3.6-4.6)** | 6/6 | 6/6 |

### Against the simulation and the earlier rounds

| | time | cost | result |
|---|---|---|---|
| core simulation (prediction) | 40-100 s | 5.7-7.9x | |
| round 4 (`acab026`), 10% loss | 33.3 s | 98.7x | completed |
| round 5 (`0e265a6`), 10% loss | n/a | n/a | **server aborted 5 of 5** |
| **round 6 (`6dff485`), 10% loss** | **28 s** | **4.1x** | completed 10 of 10 |
| round 4, 10% loss + 100 ms | 106.9 s | 63x | completed |
| **round 6, 10% loss + 100 ms** | **68 s** | **4.0x** | completed 6 of 6 |

Real numbers are better than the simulation on both axes (cost 4x vs 5.7-7.9x; time 28 s vs 40-100 s) and 15-25x cheaper
than round 4. The remaining 4x is the expected whole-copy retry cost: a 16 KiB state deflates to about 10 fragments, one
copy survives with probability 0.9^10 = 35%, plus lost acks, so about 3 copies per state (measured 188 big copies for 62
states in the client-side capture, 3.0 per state).

### The B2 invariant is visible on the wire

For the decrypted captures (`pm1_loss10cap`, `pm1_d150`, `pm4_d150`, and the client-side lossy capture) I rebuilt the
cumulative input of every state (`cum[new] = cum[old] + diff length`) and checked: 0 cases of the same `new_num` carrying
different content, and 0 cases of a higher `new_num` with less cumulative input than a lower one (140 client states in the
client-side capture, 83-101 in the others). Round 5's signature (`old=133 new=134` 16 KiB then `old=133 new=135` 8 KiB)
is gone.

## 3. Paste matrix without loss (`KEEPCAP`: server-side capture kept, decoded)

| profile | size | time | client bytes (= tcpdump, no loss) | ratio vs compressed | round 4 / 5 | sha256 |
|---|---|---|---|---|---|---|
| none | 1 MiB | **0.82 s** | 828,692 (1.04x of 797 KB) | 1.04x | r4 0.91 s 2.44x; r5 0.85 s 1.04x | match |
| delay 150 ms (300 ms RTT) | 1 MiB | **7.5 s** | 881,649 | 1.11x | r4 7.95 s 3.2x; r5 22.7 s 2.03x | match |
| none | 4 MiB | **2.83 s** | 3,309,769 (3.19 MB compressed) | 1.04x | r4 3.35 s 2.3x; r5 6.7 s 1.51x | match |
| delay 150 ms | 4 MiB | 26.4 s | 3,360,470 | 1.05x | not measured before | match |

The round 5 regressions (cap halving without loss: 4 MiB 1.5x and 6.7 s; 150 ms 2.0x and 22.7 s) are gone: the shrink
trigger now needs a full loss RTO, and latency alone never fires it. Ticker lag during the pastes: 17 ms median (none, 1 MiB),
305 ms (150 ms), 332 ms (4 MiB, 150 ms).

State sizes sent, from the decrypted server-side captures (raw input bytes per state, diffs from `ana.py`-style decoding):

* 1 MiB, none: `16390, 16390, 29199, 31758, 65532, 62487, 63508, 65035 ...` then 61-65 KB until the tail (`51216`). Slow start
  (16 -> 32 -> 64 KiB) is visible; 19 big states, each sent once.
* 4 MiB, none: `16390, 16390, 30227, 31752, 65532, ...` then about 65 KB for all remaining 63 states, last `43394`; each once.
* 1 MiB / 4 MiB, 150 ms: the same ramp, then 61-65.5 KB; two states needed a second copy. The cap never fell back.
* 10% loss: sizes stay at the floor, `12-16 KiB` (diffs of 12,298-16,396 bytes) with occasional doublings to about 30 KiB
  (`29196, 30732, 31756, 31757`) that fall back at the next loss.

## 4. B1 regression (10% loss, fresh sessions)

`rp5.sh`/`rpb.sh` unchanged: fresh server, `moche --stats`, wait 3 s, type `echo RDY""1`, wait up to 20 s for `RDY1`.

| client | sessions | freezes | `RDY1` after | `rx_gaps>0` runs |
|---|---|---|---|---|
| moche `6dff485` | 32 | **0** | <= 0.4 s in all 32 | 1 (`rx_gaps=1`) |
| (round 5) moche `0e265a6` | 65 | 0 | | 5 |
| (round 4) moche `acab026` | 28 | 4 | | |

`send_errors=0 rx_errors=0` in all 32.

## 5. Interactivity

Typing echo latency, no paste (`latb.sh`: `send-keys -l tok`, 5 ms poll, time to `tok` on screen):

| profile | moche min / median / p90 / max (ms) | stock | round 5 moche |
|---|---|---|---|
| none (20 samples) | 19 / 28 / 30 / 32 | 13 / 17 / 19 / 21 | 19 / 27 / 29 / 37 |
| loss 10% (40 samples) | 19 / 29 / 50 / 79 | 13 / 18 / 36 / 77 | 17 / 27 / 48 / 98 |

moche stays about 10 ms above stock at the median (the 8 ms collect wait), unchanged from round 5. `rx_gaps=0
send_errors=0` in both runs.

Ctrl-C typed 3.0 s after the paste started (`ipb.sh`; 1 MiB paste; prompt-back time measured from the keypress):

| profile | Ctrl-C to prompt | paste start to prompt | marker echo after prompt | bytes_sent | dst size |
|---|---|---|---|---|---|
| delay 150 ms, run 1 | 4.43 s | 7.5 s | 320 ms (about 1 RTT) | 0.83 MB | 1,048,047 |
| delay 150 ms, run 2 | 4.51 s | 7.5 s | 324 ms | 0.83 MB | 1,045,429 |
| loss 10%, run 1 | 36.8 s | 39.8 s | 35 ms | 6.48 MB (8.1x) | 1,044,505 |
| loss 10%, run 2 | 28.9 s | 31.9 s | 37 ms | 4.22 MB (5.3x) | 1,045,429 |
| loss 10%, run 3 | 26.5 s | 29.5 s | 36 ms | 3.26 MB (4.1x) | 1,044,505 |
| loss 10%, run 4 | 27.0 s | 30.1 s | 35 ms | 3.12 MB (3.9x) | 1,044,890 |

The Ctrl-C is behind the whole pasted block in the ordered input stream, so it takes effect when the paste has
drained (7.5 s at 150 ms, the same as an uninterrupted paste; 27-37 s at 10% loss). That is by construction and unchanged since
round 4, but nothing is added by the new rule "an in-flight state is not abandoned": the Ctrl-C does not wait for more than the
paste. Round 5 (150 ms): 5.3 s; now 4.4-4.5 s. Small commands after the drain: 327-358 ms at 150 ms (1 RTT = 300 ms), 27-63 ms
at 10% loss: normal latency, no queueing behind old data. (The harness's per-keystroke echo check times out in this
step because `stty -echo` is still set after Ctrl-C; that is not a bug.) The first loss-10% Ctrl-C run is the slowest
paste of the whole round (39.8 s, 8.1x): an outlier against the 10 runs of section 2 (23-31 s, max 4.9x); the
other three were normal. One sample, not explained.

## 6. Floods, 0% loss (250x80, one session per client)

| step | moche done | stock done | text identical | attributes |
|---|---|---|---|---|
| `yes \| head -c 5000000` | 1359 ms | 1314 ms | yes | identical |
| `seq 1 1000000` | 769 ms | 772 ms | yes | identical |
| 256-colour dump | 124 ms | 123 ms | yes | identical after normalising the known D1 (palette 8-15) |

`rx_errors=0 send_errors=0`, no out-of-sync, `AFTER` echoes in 61 ms (moche) / 59 ms (stock). Round 5 (0%): 1319 / 723 / 112 ms.

## 7. Ack timing from a lossy capture (client-side, 10% loss, 1 MiB paste, decrypted)

Client-side `tcpdump -ni eth0 udp` (what survived netem in both directions), one run (`pm1_loss10cb`: 23.2 s, 3.2x, sha256
match), analysed per datagram group (one fragment id = one copy).

| measure | value |
|---|---|
| server states first received -> our first delivered frame carrying an ack >= that state | n=253, median 111 ms, p90 755 ms, max 1981 ms |
| our big state first send -> server ack seen at the client | n=62, median 69 ms, p90 78 ms, max 274 ms |
| our small frames (keystroke/ack) first send -> server ack | n=77, median 105 ms, p90 253 ms, max 1165 ms |
| gaps between consecutive copies of big states | min 20 ms, p10 62, median 150, p90 155, max 161 ms (none above 200 ms) |
| big copies sent AFTER a server frame acking them had reached the client | **0** of 188 |
| big copies per state | 188 copies for 62 states = 3.0 (1/0.9^10 whole-copy retries plus lost acks predict about 3) |

* Server ack hold toward our states: a big state is acked a median 69 ms (p90 78 ms) after its first send, with an RTT
  near 0: so the server holds the ack about 70 ms (its delayed-ack timer), well under the 250 ms loss-RTO floor.
  Therefore no resend of a healthy state is triggered by ack delay: resends come every about 150 ms (RTO plus ack delay,
  clamped), but a resend only happens when no ack arrived, and 126 of the 188 copies were repeats of states whose
  earlier copies were genuinely incomplete (a fragment lost) or whose ack was lost. I did not find a datagram that
  re-sent an already-acked or duplicate `new_num` after the ack was visible to the client.
* The 250 ms loss-RTO floor is never reached by the server's ack time (max 274 ms, in the one state whose first copy
  lost a fragment), so the shrink condition (>= 3 sends and a full loss RTO without ack) fired rarely: the cap stays at the
  floor anyway at 10% loss (section 3).
* Weakness: our own ack to a server state has a p90 of 755 ms and a max of nearly 2 s while a big state is in flight.
  The ack rides on the big frames, which lose a fragment 65% of the time at 10% loss, and a big state is repeated only about
  every 150 ms; the server was sending only its 5 Hz ticker here, so it did not suffer, but with heavier server output the
  server would hold more unacked states and re-diff from an older base for longer. A one-fragment pure ack datagram sent
  next to the big copy would cut this. Not a correctness bug. (The measure is "time to the first frame of ours that
  reached the capture whole and carries the ack", so it also contains our frames that lost fragments after the capture
  point; read it as an upper bound of the effective delay.)

## 8. MIN_STATE_BYTES 16 KiB vs 8 KiB (scratch variant, not in the repo)

The same release build with `MIN_STATE_BYTES` = 8 KiB only, built from a throw-away copy in `moche-b` (deleted), 6 runs at 10% loss:

| variant | n | time (s) | cost (ratio vs 797 KB) | completed / sha256 |
|---|---|---|---|---|
| 16 KiB (`6dff485`) | 10 | 28.1 (23.3-31.5) | 4.07x (3.3-4.9) | 10/10 |
| 8 KiB scratch | 6 | 28.7 (27.2-30.0) | 3.22x (2.4-4.5) | 6/6, no death |

Per run for 8 KiB (MB): 2.88, 1.91, 3.56, 2.19, 2.68, 2.20. The two ranges overlap; the mean cost is about 20% lower and the
time about the same. That is a hint, not proof: the run-to-run spread (2.4x to 4.9x) is as large as the effect. Verdict:
**keep 16 KiB** for now; an 8 KiB floor is a candidate for a modest gain at 10% loss, worth a 20-run comparison and a
check on a lossier link (20-30%), where it should help more (fragments per copy matter exponentially). 16 KiB is
fine for correctness and was what the simulation assumed.

## Bugs found

None new. Observations that are not bugs: (a) our ack delay under a big in-flight state (section 7); (b) the Ctrl-C waits behind
the paste by design; (c) one slow outlier paste (39.8 s, 8.1x) in the interactivity run.

## Not verified

* Longer or lossier links (20-30% loss), asymmetric loss, reorder, duplication; pastes of 4 MiB under loss (not run); a 1 MiB
  paste at loss below 10%.
* Statistical weight of the 8 KiB vs 16 KiB comparison (6 vs 10 runs).
* Spurious server-side duplicates (a copy that reaches the server after its ack was sent) cannot be seen from a post-netem
  capture; only "sent after the client saw the ack" (0) was measured.
* The cause of the stock server's abort in round 5 remains an inference from the trace; the fix removes the trigger and 22
  lossy pastes did not reproduce it, but the server's actual invariant was not read.
* `cargo test -p moche-core` in the VM (OOM on one test in round 5; not run this round); the suite is exercised on the Mac.
* Roaming and network-change tests were not repeated (out of scope for this round).

## Scripts

Not in the repo; on the lab host in `~/moche-vmlab/r6`: `r4.sh` (round 5 driver plus `pgrep` death detection, strace
check, optional `KEEPCAP=1` capture copy), `pb.sh`, `nm.sh`, `msw.sh` (strace wrapper for the server), `chainB2.sh`
(10 + 6 runs), `chainRest.sh` (matrix, B1 loop, latency, Ctrl-C, floods), `chain3.sh` (client-side lossy capture), `chain4.sh`
(Ctrl-C repeats), `chain5.sh` (8 KiB variant), `sum.sh`, `ana2.py` (per-copy decoder), `copies.py`, `ackan.py`, `sizes.py`,
`mono.py` (the B2 invariant checker), `res.*.txt` raw outputs. A venv with `cryptography` is there for the decoders.
