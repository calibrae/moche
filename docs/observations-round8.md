# Observations, round 8: why moche sent more client datagrams than the stock client while typing, and the fix

Date: 2026-11-04. Base: `d07618b`. Stock `mosh-client` / `mosh-server` 1.4.0 (Homebrew), observed as a black box: its datagrams
decrypted with the session key this harness itself holds (the throwaway `MOSH_KEY`), timing taken at the relay. Nothing of
mosh's source was read. Everything ran on one Mac over loopback through the delay relay; no remote host, no netem, so this is
**not** the VM-lab validation (that stays to do, plan at the end).

## Summary

| Question | Answer |
|---|---|
| Why 1.5 to 2.6x the stock datagrams while typing at 10 keys/s? | moche **re-sent every unacked key state at the frame slot** (about SRTT/2, 20 ms at RTT 10, 55 ms at RTT 100) while the ack only came back about RTT + 40 ms after the key. Stock does not: while typing it re-sent a state only about 100-112 ms after its last send, and only 3 % of the time, because the next key supersedes the state first. |
| Second effect | Each such re-send refreshes "last send", so a key typed right after it **waited a frame slot** (median key-to-datagram 39 ms at RTT 100 against 3 ms for stock), plus the fixed 8 ms collection delay. |
| Not the cause | Acks. moche's delayed ack (100 ms after the first unacked server frame, piggybacked on key frames) matches stock's. moche sent 1 to 2 standalone acks per run, stock 1 to 29 (it sends one when nothing else goes out). Heartbeats, the echo-ack-only frames and the server side are equal. |
| Fix | `COLLECT_MS` 8 to 0; a small unacked state is re-sent after `max(frame slot, 110 ms, min(SRTT + 50 ms, 250 ms))` instead of the frame slot; frame interval `SRTT/2 + 6 ms` (what `protocol.md` already said, the code had left out the +6). About 40 lines in `ssp.rs`. |
| Result (local relay, 30 s, 296 keys) | RTT 10/50/100/200: stock 1.02-1.06 / 1.06-1.07 / 0.97-1.03 / 0.71-0.83 datagrams per key; moche before 1.80 / 2.58 / 1.46 / 0.81; **moche after 1.02 / 1.11 / 1.01-1.02 / 0.76-0.89**. Key-to-datagram median 2-4 ms, same as stock (before: 11-39 ms). Echo-ack latency equal to stock. |

## Method

New tools (all in `tools/perf/`, none needs a remote host):

- `delay_relay.py` got a trace option: every datagram is written as `<monotonic time> C|S <hex>` **at the client edge** of the added
  delay (C when the client sent it, S when it receives it). It also polls at 1 ms: on this Mac a `select()` timeout of tens of ms
  was seen to wake up 0.5 to 1.6 s late, stalling every datagram.
- `wire_trace.py`: starts a local stock `mosh-server` (fake `SSH_CONNECTION`, `bash --norc`), the relay, the client on a pty owned by
  the script (a tmux control pipe stalled too), and types random letters with 80-120 ms gaps (seed 1, about 296 keys in 30 s), or
  an idle probe (one key after 2.5 s of silence), or trains of keys.
- `crates/moche-core/examples/trace_decode.rs`: decrypts and decodes a trace with the session key (reassembles fragments).
- `wire_analyze.py`, `wire_table.py`, `wire_idle.py`: classification and tables below.

Classes of client datagram: **NEWKEY** (fresh `new_num`, content longer than before), **RESEND** (same `new_num` as an earlier
datagram), **RESEND\*** (fresh number, same content: never seen from either client), **ACK** (empty diff, `ack_num` new),
**EMPTY** (empty diff, nothing new: heartbeat), **BURST** (see next paragraph). Server: **DATA** (host bytes), **ECHO** (echo-ack
only), **EMPTY**.

**Host stalls.** Both clients, and the server, stall on this Mac for 0.05 to 2 s at random (observed with the stock client too: a
resend burst of up to 20 copies of one state within 1 ms right after a stall). A RESEND within 3 ms of the previous client datagram
is classified BURST and left out of every count; medians are reported, not means; the headline numbers were repeated 3 to 4
times. A stall also poisons SRTT for a while (one run had a key median of 51 ms for that reason). Stock's own run-to-run spread
is 0.71 to 0.83 per key at RTT 200 and 0.97 to 1.03 at RTT 100.

## Before: counts per class (first key to 1.5 s after the last, BURST excluded, one 30 s run each)

| RTT ms | client | keys | NEWKEY | RESEND | ACK | EMPTY | total | per key | server DATA | ECHO | EMPTY |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 10 | stock | 296 | 295 | 7 | 1 | 0 | 303 | 1.02 | 620 | 3 | 2 |
| 10 | moche | 295 | 291 | 237 | 2 | 0 | 530 | **1.80** | 667 | 2 | 2 |
| 10 | moche adaptive | 296 | 294 | 214 | 1 | 0 | 509 | 1.72 | 623 | 0 | 2 |
| 50 | stock | 296 | 295 | 4 | 16 | 0 | 315 | 1.06 | 566 | 1 | 2 |
| 50 | moche | 296 | 294 | 469 | 1 | 0 | 764 | **2.58** | 582 | 1 | 3 |
| 50 | moche adaptive | 296 | 291 | 398 | 1 | 0 | 690 | 2.33 | 578 | 1 | 3 |
| 100 | stock | 296 | 286 | 11 | 1 | 0 | 298 | 1.01 | 442 | 0 | 3 |
| 100 | moche | 296 | 280 | 152 | 1 | 0 | 433 | **1.46** | 432 | 0 | 2 |
| 100 | moche adaptive | 296 | 287 | 154 | 0 | 0 | 441 | 1.49 | 457 | 0 | 3 |
| 200 | stock | 296 | 249 | 4 | 1 | 0 | 254 | 0.86 | 247 | 0 | 3 |
| 200 | moche | 296 | 231 | 7 | 1 | 0 | 239 | 0.81 | 230 | 0 | 2 |
| 200 | moche adaptive | 296 | 265 | 7 | 0 | 0 | 272 | 0.92 | 270 | 0 | 3 |

(The lab figure in `predict.md` was 1.67x at RTT 100 on two VMs; here 1.46x at RTT 100, and the ratio is larger at lower RTT.) The
whole difference is the RESEND column; NEWKEY, ACK, EMPTY and the server's columns agree. At RTT 200 the frame slot (106 ms) is
longer than the key gap, so there is nothing to re-send early and both clients agree: the effect disappears there.

Typical cycle at RTT 100, stock (keys 100 ms apart): `KEY, +3 ms C(new key frame), +28 S, +83 S, KEY ...`: one datagram per key,
nothing in between, the acks ride the key frames. moche before: `KEY, +12 C, +57 C(resend), +28.. S ..., KEY +100 C, +57 C(resend)`:
two datagrams per key, and a key landing within a frame slot after a resend waits (`KEY 20305 -> C 20368`).

## Stock's observed rules (OBSERVED unless marked)

| Rule | Evidence |
|---|---|
| First key after idle goes 2-5 ms after the key, no collection delay of its own. | idle probe, 12 keys per RTT: key-to-datagram min 2, median 3, max 5 ms at RTT 10, 50, 100 (the pty wake-up is part of it). moche before: 10-12 ms (8 ms collect). |
| Keys faster than the frame interval are batched by the interval alone: one datagram, sent at last send + interval. Interval = SRTT/2 + about 6 ms (55 ms at RTT 100, 106 ms at RTT 200, 20-23 ms at RTT 10). | trains of 8 keys, 8 ms apart: `c3 (1 key), c57 (7 keys), c117`; RTT 200 typing: new frames 106 ms apart. |
| An unacked key state is **not** re-sent at the frame interval while typing. Re-sends seen: 100-112 ms after the last send (10 cases at RTT 100, 4 at RTT 200), once +145 ms (RTT 100, after the last key), 3 % of keys. | 296-key runs, RESEND class: stock 4-15 per run at RTT 10-100 against moche's 152-469. |
| Exception: **after a long silence** the first key's state is re-sent at the frame interval (57-62 ms at RTT 100, 20-23 ms at RTT 10) until the ack arrives. | idle probes: 2 to 3 datagrams per probe key at RTT 50-100. moche does not copy this (OUR CHOICE: one fewer datagram per first key after idle). |
| Delayed ack: an empty ack goes out 103-119 ms (median 111) after the first unacked server frame when nothing else was sent; anything sent earlier carries the ack. | idle probes, stock: 103-116 ms; typing: 0 to 5 % standalone acks. moche: 103-111 ms (median 105-106), 6 ms earlier than stock, harmless. |
| Server frames come in pairs per key about 55 ms apart (data+ack, then a second frame with the echo ack); the client does not ack each, it acks the first within the delay. | typing runs: 1.9 server DATA frames per key; server ECHO-only frames are rare (0-4 per run). |
| An echo-ack-only frame gets no special treatment: next client datagram 84-105 ms later (RTT 10, n=3). | too few samples at other RTTs. |
| Heartbeat: about 3 s, unchanged. | `S_EMPTY`/`EMPTY` 2-3 per run both clients. |

Not found, not claimed: what makes the stock client re-send in 10 of the cases where the key gap exceeded 110 ms and not in the
others (about 70 expected for a plain 110 ms timer at RTT 100). A timer of `max(110, SRTT + 50)` matches the one idle re-send
observed (+145 at RTT 100) and removes most of moche's remaining re-sends; at RTT 200 it re-sends later than stock did (250 ms
against 100-112), which is the OUR CHOICE part (an ack cannot be back before RTT).

## Root cause, with the evidence

1. `Transport::next_deadline_ms` paced every unacked small state at `frame_interval()` (SRTT/2, at least 20 ms), and a pure
   re-send is an ordinary send that resets `last_send`. At 10 keys/s with an ack that comes RTT + up to 40 ms after the key (the server
   piggybacks the ack on its next frame), each state is re-sent about once at RTT 100 (1.46x), more often at lower RTT where the
   frame slot (20 ms) is shorter than the server's reply spacing (about 55 ms): up to 2.6x. Evidence: RESEND 152/469/237 against 11/4/7.
2. The re-send occupies the frame slot, so a key typed just after it waits up to a slot: key-to-first-datagram median 39 ms (RTT 100)
   against 3 ms (stock), 13 ms (RTT 50) against 3 ms. With COLLECT 8 ms the floor was 10-12 ms against 3.
3. Acks are not involved: moche ACK 1-2 per run against stock 1-16; the moche ack path (delay timer anchored on the first unacked
   server frame, min with any send) matches the observed stock one.

## The change (`crates/moche-core/src/ssp.rs`, working tree, not committed)

- `COLLECT_MS` 8 to 0 (OBSERVED: no collection delay; the frame interval batches). Wire effect: the first frame after idle goes at
  once; faster typing is paced by the frame interval exactly as before.
- New `SMALL_RESEND_MS = 110` (OBSERVED) and `SMALL_RESEND_MARGIN_MS = 50` (OUR CHOICE): `small_resend_ms() = max(frame interval,
  110, min(SRTT + 50, MAX_FRAME_MS = 250))`. Used only for a **pure re-send of a small state**. New input is still paced by
  `last_send + frame interval`; big states keep their RTO-based interval; an ack that falls due still pulls the next send forward
  (it goes out as a re-send carrying the ack, as before).
- `frame_interval()` = `SRTT/2 + 6 ms` (new constant `FRAME_EXTRA_MS`, OBSERVED, and what `protocol.md` item 2 specified).
- Unchanged: B1 (the `ack_num` is the highest held state), B2 (nothing shrinks, nothing is re-cut; the sim server model still panics
  on a violation), chaff, fresh `seq` and `frag_id` per datagram, heartbeat, delayed-ack time (100 ms), the cap/RTO logic of big states.

Rule labels: collection delay 0 OBSERVED; frame interval OBSERVED; resend floor 110 OBSERVED; `SRTT + 50` OUR CHOICE; ack delay
100 ms OBSERVED (stock 103-119, ours 100); no copy of the post-silence early re-send OUR CHOICE.

## Tests added (all in `ssp.rs`, all pass; existing 90 unchanged apart from three timing constants)

- `typing_datagram_count_matches_the_stock_pattern`: typing at 10 keys/s (80-120 ms gaps, 300 keys, 4 seeds) at one-way 5/25/50/100 ms
  against a model of the stock server (two frames per key, 30 ms hold), asserts the per-key count within +20 % / -25 % of the stock
  counts above. Sim values: 1.08, 1.00, 1.07, 0.95; the old policy gives 1.9, 3.7, 2.0, 1.0 in the same sim.
- `no_standalone_ack_when_a_keystroke_is_within_the_ack_window`, `typing_sends_few_standalone_acks` (at most 12 % of keys).
- `resends_only_after_the_small_resend_timeout` (a state is never re-sent less than 100 ms after its previous send: 110, or the ack
  delay when the re-send carries a due ack).
- `idle_ack_within_the_bound_and_heartbeat_unchanged`.
- `property_an_owed_ack_is_always_sent_in_bounded_time`: 60 random mixes of keys, 20 KB pastes and server frames; an owed ack
  leaves within 100 ms (small frames) or one maximum big-state interval plus 100 ms (pastes): no ack-owing situation starves the server.
- The existing lossy tests (10 % loss, B2 shrink/re-cut, 1 MiB paste cost: clean 1.04x, lossy within 8x) pass unchanged.

## Golden scenario, before and after (`tools/perf/golden-diff.sh --report`)

| phase | datagrams before | after | bytes before | after |
|---|---|---|---|---|
| typing 10 keys/s, one-way 2 ms (0-12.1 s) | 121 | 121 | 9094 | 9094 |
| burst + idle (12.1-16 s) | 3 | 3 | 232 | 232 |
| lossy 15 ms, 150 kB and 40 kB pastes (16-40 s) | 1423 | 1423 | 1676558 | 1676558 |
| one-way 30 ms: resize, `ls`, 110 kB repaint, idle (40-60 s) | 14 | **10** | 1124 | 770 |
| total | 1561 | 1557 | 1687008 | 1686654 |

Every difference: (a) all 1547 datagrams before 40 s are exactly 8 ms earlier (the collection delay is gone; the sealed bytes differ
because the datagram sequence timing feeds the timestamp); key-to-first-datagram median 8 ms to 0 ms; (b) after 40 s, the resize frame
(t = 41000) and the `ls -la` frame (t = 43000) were re-sent at +20 and +40 ms by the old frame-slot rule, 2 re-sends each, before the
ack could possibly arrive (one-way 30 ms): now they are sent once and re-sent at +160 ms, 4 datagrams fewer. No change in the lossy
paste phase except the 8 ms shift (counts identical, so the cost figures stand). The golden example is unchanged;
`golden-diff.sh` gained `--report` (this table), `--pin` and `--pinned` (hash of the new behaviour in `tools/perf/golden.sha256`).

## After: local relay, same harness

Per key (BURST excluded), 30 s, 296 keys; RTT 100 and 200 repeated (stock 4 runs, moche 3 runs).

| RTT ms | stock | moche before | moche after (never) | moche after (adaptive) |
|---|---|---|---|---|
| 10 | 1.06 (1.02 earlier run) | 1.80 | **1.02** | 1.04 |
| 50 | 1.07 (1.06) | 2.58 | **1.11** | 1.08 |
| 100 | 0.97 1.02 0.98 1.03 (mean 1.00) | 1.46 | **1.007 1.02 1.01 (mean 1.012)** | 1.04 |
| 200 | 0.71 0.83 0.76 0.72 (mean 0.76) | 0.81 | **0.86 0.76 0.89 (mean 0.84)** | 0.81 |

Class counts, after (RTT 100, never): NEWKEY 287, RESEND 9, ACK 2, EMPTY 0 (stock 277 / 8 / 2 / 0). RESEND at RTT 10/50/200: 4/4/5
(stock 18/9/2). Latency, median of key to first datagram carrying it: 2-4 ms at RTT 10-100 (stock 3-4; before 11-39), RTT 200 73-92 ms
(stock 76-99, frame-slot bound). Key to echo-ack at RTT 100: 208-209 ms (stock 200-211; before 225); RTT 10 77 ms (stock 81; before 87).
Idle probe, after: key to datagram 2 ms (stock 3 ms), 2.0/1.9/2.2/3.1 datagrams per probe key at RTT 10/50/100/200 where stock sent
2.8/3.1/3.4/2.7 (stock's extra is its post-silence re-send), ack-only frame 105-106 ms after the server frame (stock 111).
RTT 200 stays about 10 % above stock's mean because stock's own spread there is 0.71-0.83; the difference is in NEWKEY (stock merges
more keys into one frame) and was not chased further.

Other local checks (working tree): `moche --direct` e2e passes; `predict_e2e` passes (it fails intermittently on this Mac, also at
HEAD, when a host stall hits its latency window); 1 MiB random base64 paste: 0.7 s, sha256 match; `yes | head -c 5000000` 3.1 s and
`seq 1 1000000` 1.3 s complete.

## Risks (every way this could regress against a stock server)

1. **Lost last key frame recovers later**: re-send after at least 110 ms (RTT 100: 150 ms, RTT 200: 250 ms) instead of one frame slot. At
   10 % loss the last keystroke of a burst appears about 0.1-0.2 s later; the next keystroke carries it anyway (cumulative diff). Stock
   shows the same 100-145 ms re-send while typing. Not measured under loss here.
2. **B1** (never ack a state we do not hold): untouched; `ack_num` is still the highest held state, and the B1 loop (10 % loss freeze test)
   must be re-run in the lab, because acks now ride fewer re-sends and more often a standalone empty frame.
3. **B2** (client states only grow): untouched code path, the model-server tests still panic on a violation; only the timing of re-sends
   moved. A re-send is still the same `new_num` and content.
4. **Ack starvation**: an owed ack is sent at the delay deadline by `min(t, ack_due)` for small frames, as before (property test). With a big
   state in flight it is still deferred to the big resend interval (up to 1 s + 100 ms), unchanged.
5. **Server retransmit storms**: the server re-sends until it gets an ack. If the 100 ms ack delay (stock waits 103-119) ever lost against a
   server timer in some environment, nothing changed here: the ack path is not touched.
6. **No collection delay**: a paste split by the tty into several reads sends the first chunk at once and the rest one frame slot later:
   at most one extra small datagram per paste (golden: identical counts).
7. **Frame interval +6 ms**: new input waits 6 ms longer between frames; 5-10 % fewer datagrams at RTT 200, slightly later echo for keys
   that land inside the slot.
8. **First key after silence**: stock re-sends it at the frame interval, moche once at 110 ms+; if the first datagram is lost the echo
   comes about 50 ms later than with stock at RTT 100.
9. **Very high RTT** (SRTT over 300 ms): the cap (250 ms) means a re-send can precede the ack, as it did before the change.
10. **Roaming/heartbeat**: unchanged (3 s empty frame), but a re-send no longer happens at the frame slot, so after a network change the
    first re-send arrives 110 ms+ later than before.

## Lab validation plan (real server, VM lab, after this lands)

Use the round 7 drivers (`r4.sh`, `rp5.sh`/`rpb.sh`, `pb.sh`, `fl.sh`) and `predict_lab_run.sh wire`.

1. **Wire count A/B**, RTT 1/50/100/200 (netem on both sides), 3 runs each of stock, `--predict never` and adaptive, the 30 s typing script of
   `wire_trace.py` adapted to tcpdump on the data path (`predict_lab_run.sh wire` already counts datagrams): expect moche within 10 % of stock
   at every RTT, RESEND class under 5 % of keys.
2. **Echo latency** (`predict_lab_run.sh lat`, rtt0/30/60/100/200, never and adaptive): key-to-visible equal to stock within a few ms (was 5-18 ms slower).
3. **Loss**: 10 % loss each way at RTT 50 and 100, typing 30 s: every key echoed, last-key recovery time distribution (key typed, then silence, time
   to the echo) against stock; no freeze.
4. **B1 loop**: 32+ fresh sessions at 10 % loss each way, `echo RDY<i>`, 0 freezes expected (rounds 4-7).
5. **B2 loop**: 1 MiB random base64 paste at 10 % loss (6 runs) and 10 % loss + 100 ms one-way (3 runs), server under
   `strace -f -e trace=process,signal`: sha256 match, 0 SIGABRT, cost about 4x (round 7 figures).
6. **Pastes without loss**: 1 MiB and 4 MiB, cost 1.04x, sha256 match.
7. **Floods**: `yes | head -c 5000000`, `seq 1 1000000`, 256-colour dump at 250x80: text identical to stock.
8. **Ack starvation probe**: type one key, then nothing, at RTT 200 and 400 with 10 % loss; the server must stop re-sending within 2 s (tcpdump:
   no server frame repeated more than 3 times).
9. **Roaming**: 20 s NIC down and back (round 7 e), typing resumes within 1 s of link-up.

## Not verified

- Behaviour under loss on the wire (only the sim and the unchanged B1/B2 model tests cover it).
- Why stock re-sends in 3 % of keys and not more (the unexplained part of the timer).
- Whether the post-silence early re-send of stock matters for the server (probably not: it ignores duplicates).
- Server versions other than 1.4.0; RTT above 200 ms; a real network (jitter, reordering).
