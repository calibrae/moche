# Observations, round 5: validating the B1 fix and the new large-input policy against a stock mosh-server

Date: 2026-11-03. Build under test: `0e265a6` (HEAD: held-set acks, adaptive state size 64 KiB -> 8 KiB floor, RTO-paced
resends of states > 4 KiB, `CHUNK_BYTES` 4 KiB, `moche --stats`). Same lab as round 4 (`tools/vm-lab/README.md`: server
`moche-a`, Debian 13, mosh-server 1.4.0; client `moche-b`, Ubuntu 24.04, moche release build, stock `mosh-client` 1.4.0).
Addresses are placeholders: `<a-u2>` / `<b-u2>` are the user-v2 `eth0` addresses (the data path), the control channel is
ssh over the vzNAT `lima0` addresses, so netem on `eth0` never touched it. Stock mosh is a black box (no source read;
the server's behaviour was observed with `strace -e trace=process,signal` and packet captures only). No file under
`crates/` was edited.

## Summary

| # | Item | Verdict |
|---|---|---|
| 1 | Build + `cargo test -p moche-core` in the VM | build OK; **test run is OOM-killed in the 2 GiB VM** (`ssp::tests::big_incompressible_send_keys_is_fully_delivered`, 1.7 GB RSS in a debug build; passes on a Mac) |
| 2 | B1 regression (out of sync, 10% loss, fresh sessions) | **PASS: 0 freezes in 65 moche sessions** (round 4: 4 of 28), 0 of 20 for stock. Re-ack path fired in 5 of 65 (`rx_gaps=1`) |
| 3 | Paste matrix | **FAIL under loss: NEW BUG B2, the stock mosh-server ABORTS (SIGABRT) during a 1 MiB paste at 10% loss, 5 of 5 runs at 10% loss (3 of them confirmed with strace) and 1 of 1 at 10% loss + 100 ms.** Clean and delay-only runs complete, cheaper than round 4 on a clean path and at 150 ms; slower at 150 ms |
| 4 | Interactivity while pasting | measurable only at 150 ms delay (loss 10% kills the server first): Ctrl-C took effect 5.3 s after it was typed; small frames after the drain are normal |
| 5 | Floods (yes 5 MB, seq 1..1e6, 256-colour dump), 0% and 10% loss | PASS: screens identical to stock (D1 aside), no out-of-sync |
| 6 | Re-ack behaviour | CONFIRMED on a real capture: after our re-ack the server re-diffed from the re-acked base |
| 7 | Cleanup | done (netem, iptables, tmux, mosh-servers, captures removed; VMs running) |

## 2. B1 regression

Method (round 4's `rp.sh`, wrapped to keep stats and a short rotated capture): netem `loss 10%` on `eth0` of both VMs;
per session a fresh `mosh-server`, client in tmux, wait 3 s, type `echo RDY"" 1`, wait up to 20 s for `RDY1` on screen;
freeze = `RDY1` not seen in 20 s or `out of sync`/`Last contact` on the status line. moche run as
`moche --stats --direct <a-u2> <port> 2>/tmp/rp.stats`; tcpdump on the client VM, `-c 4000`, kept only for a
freeze or a re-ack run, deleted afterwards.

| client | sessions | freezes | `RDY1` seen after | `rx_gaps > 0` runs |
|---|---|---|---|---|
| moche `0e265a6` | 65 (45 + 20) | **0** | <= 0.4 s in all (second 0.2 s poll) | 5 (all with `rx_gaps=1`) |
| stock `mosh-client` | 20 | 0 | <= 0.6 s | n/a |
| moche `acab026` (round 4) | 28 | 4 | never, 20 s | n/a |

Stats of a clean run: `queued_bytes=0 send_errors=0 rx_errors=0 rx_gaps=0 datagrams_sent=5 datagrams_received=4`. `send_errors`
and `rx_errors` were 0 in every session. The old failure rate (14%) makes 65 clean sessions (0.86^65 < 0.01%) good evidence.

## 6. Re-ack on the wire (client-side capture of a run with `rx_gaps=1`, decrypted with the throwaway session key)

```
t=0.000 C>S old=0 new=1 ack=0   resize                    hello (the first server datagram, ctr 0, was lost)
t=0.261 S>C old=1 new=2 ack=1   (server state 2 built on state 1, which we never received)  -> rx_gaps=1
t=0.365 C>S old=1 new=2 ack=0 thr=1   re-ack of state 0, the only one we hold
t=0.388 S>C old=0 new=2 ack=2   server re-diffs from state 0 (full 324-byte diff)       <- the fix working
t=0.606 C>S ack=2               our ack of state 2
t=3.012 C>S 'echo RDY""1\r' ; t=3.025 S>C old=2 new=3 ... RDY1 appears
```

So the stock server does re-diff from the re-acked base instead of stalling. Two minor observations: (a) the re-diffed
frame was repeated 10 times (every ~22 ms) before our ack of state 2 arrived 218 ms after the first copy; (b) the
delayed ack was about 160-220 ms after receipt here, not the documented 100 ms (SRTT is inflated at the start of a lossy
session; harmless, but worth knowing). The old build would have acked state 2 and frozen.

## 3. BUG B2: the stock mosh-server aborts during a paste under loss

### Repro

netem `loss 10%` on both VMs' `eth0` (also reproduced with `loss 10% delay 100ms`); fresh server; client
`moche --direct`; `stty -echo; cat > /tmp/paste.out`; paste 1 MiB of base64 (`tmux paste-buffer -r`, round 4's `pb.sh`/`r4.sh`).
Result: the destination file stops growing after 0.1-0.45 MB and never recovers; the client keeps sending for ever
(6.2 MB in 80 s) and receives nothing.

* 5 of 5 loss-10% runs died this way (stopped at 443,366 / 404,404 / 922,460 / 110,033 / 408,023 bytes); 1 of 1 at loss 10% +
  100 ms (167,860 bytes). Not seen at 0% loss (1 MiB and 4 MiB) nor at 150 ms delay alone (2 of 2 completed) though the
  same abandon logic is exercised there.
* Server side, with `strace -f -e trace=process,signal` around `mosh-server` (3 runs): `tgkill(<pid>, <pid>, SIGABRT)`,
  `+++ killed by SIGABRT +++`, then the shell gets SIGHUP and the session is gone (`pgrep mosh-server` empty). The server
  process aborts itself (assert / uncaught exception), no kernel message.

### Trigger (from a full client-side capture of one run, decrypted)

Server death at t = 23.37 s of the capture; client frames (instruction `old -> new`):

```
t=22.889 old=132 new=133
t=22.974 old=133 new=134        <- server received it and acked (S>C echo_ack(134) at t=23.269)
t=23.377 old=133 new=135        <- same base, SMALLER diff (8 KiB instead of 16 KiB): "abandoned" state 134 replaced by 135
t=23.379 last datagram ever sent by the server; the abort is 3 ms after the first arrival of 135
```

The other runs show the same signature at the moment of death: `old=66 new=67 (16,390 B)` followed by `old=66 new=68 (8,199 B)`,
resent for ever with `ack=86`. Cause in the code: `Transport::maybe_shrink` (`ssp.rs`, ~line 447) halves `state_limit` when the newest
state has been sent twice unacked and then does `self.sent.pop_back()` on the states that no longer fit, i.e. it abandons a
state number that the server may already have received (the ack is merely late or lost: 10% loss, or RTT > RTO), and re-cuts
a state with a *higher number but fewer input bytes* than one the server already accepted. Hypothesis (observation, not
source): the stock server requires every newer user-input state to extend all older ones it has accepted, and aborts
otherwise. The invariant that fits all runs: **a `new_num` greater than one already sent must never carry less input than that one.**

### Fix directions (not applied)

1. Never shrink an in-flight state. Apply the smaller `state_limit` only to the NEXT state cut, after the current one is
   acked (the resend interval of the big state already bounds the cost); or
2. if a state must be replaced, the new one must cover at least as many input bytes as the highest state number already
   sent (that is the only safe "abandon"): it can only grow.
3. A test for it: transport test with a lossy/late-ack simulator asserting `bytes(new_num)` is non-decreasing in
   `new_num` across all frames ever sent (the round-4 test helper already asserts `new_num` monotonic and "state N changed";
   add "state N+k never shorter than state N"). The existing test only passed because its server model accepts shorter states.

Side effect worth fixing too: the client has no sign of a dead server except the "last contact" status line; with B2 the
paste is silently truncated.

## 3b. Paste matrix (1 MiB random base64 -> `cat > /tmp/paste.out`, sha256 check)

Compressed payload about 797 KB. Bytes = client UDP payload (tcpdump on the server VM, equals `--stats bytes_sent` exactly);
IP bytes from iptables are about 2% more. Round 4 numbers are the `acab026` ones.

| profile | time | client bytes (payload) | ratio vs 797 KB | round 4 (`acab026`) | sha256 |
|---|---|---|---|---|---|
| none | **0.85 s** | 828,381 (689 datagrams) | **1.04x** (sim 1.03x) | 0.91 s, 2.44x | match |
| delay 150 ms (300 ms RTT) | 22.7 s (a second run: about 8 s) | 1,617,091 (1378) | 2.03x | 7.95 s, 3.2x | match |
| loss 10% | **never: server abort** (5 runs) | 6.2 MB after 80 s of resending into a dead server | n/a (sim predicted 4.1x) | 33.3 s, 98.7x | no |
| loss 10% + delay 100 ms | **never: server abort** | n/a | n/a | 106.9 s, 63x | no |
| none, 4 MiB | 6.7 s | 4,834,822 (3932) | 1.51x of 3.2 MB | 3.35 s, 2.3x | match |
| stock, none (round 4) | 0.27 s | 842 KB | 1.06x | | |

Clean path: the cost prediction (1.03x) is confirmed (1.04x), and the 2.4x duplicate factor of round 4 is gone. The simulation's
4.1x at 10% loss cannot be confirmed or refuted: no run survived. Two other clean-path regressions: 4 MiB took twice as long
as in round 4 and cost 1.5x, not 1.0x, even with no loss and no delay (the state limit evidently halves during the
transfer without any real loss, probably the "sent twice without ack" test firing while the ack is still in flight, since
`MIN_RTO_MS` 100 ms is long against a LAN RTT but the ack waits on the server's 100 ms delayed-ack timer and frame slot);
at 150 ms delay one run took 22.7 s, three times round 4. The adaptive size reacts to latency as if it were loss.

## 4. Interactivity during a paste (150 ms delay only, see B2)

1 MiB paste, Ctrl-C at +3.0 s, then (after the prompt returns) `echo M""K<n>` and six more small commands.

| measure | value |
|---|---|
| Ctrl-C typed at | 3.0 s after paste start |
| prompt back (cat interrupted) | **+5.26 s after Ctrl-C** (8.3 s after paste start; the paste drained at about the same time) |
| marker echo after the prompt | 328 ms (about 1 RTT) |
| six small commands after the drain | 326-348 ms each (1 RTT = 300 ms): normal latency, no queueing behind old data |
| `--stats` | queued_bytes 0, send_errors 0, rx_errors 0, rx_gaps 0, 1247 datagrams / 1.44 MB sent |

Interrupt latency is the remaining paste time by construction (ordered stream): it is the price of a bounded, ordered
queue. Loss 10% could not be measured: the server aborted before any prompt returned (B2). Note the unread part of the paste
is flushed by the tty on ^C (dst 1,046,969 of 1,048,509 bytes), as on any terminal.

Typing echo latency, no paste (time from `send-keys -l tok` to `tok` on screen, 5 ms poll; 20 samples at 0%, 40 at 10%):

| profile | moche min / median / p90 / max (ms) | stock |
|---|---|---|
| none | 19 / 27 / 29 / 37 | 11 / 17 / 20 / 22 |
| loss 10% | 17 / 27 / 48 / 98 | 12 / 19 / 64 / 74 |

moche's median is about 10 ms above stock (the 8 ms `COLLECT_MS` batching wait), the tails are comparable, round 3's
"34-37 ms both" included about 30 ms of polling overhead. The throttle did not slow normal typing; stats in these runs:
`rx_gaps=0 send_errors=0`.

## 5. Floods (250x80, one session per client, no impairment and 10% loss)

`yes | head -c 5000000`, `seq 1 1000000`, 256-colour full-screen dump, then `echo AFTER`. Pane text identical to stock in all
12 comparisons; attributes identical after normalising the known D1 (palette 8-15). No "out of sync", `rx_errors=0`,
`send_errors=0` in every run.

| loss | step | moche done | stock done |
|---|---|---|---|
| 0% | yes / seq / colour | 1319 / 723 / 112 ms | (round 4: 1354 / 927 / 124 ms) |
| 10% | yes / seq / colour | 1328 / 726 / 17,137 ms (34.5 MB downstream, `rx_gaps=1`) | 1404 / 1057 / 6521 ms (12.9 MB) |

At 10% loss the 90 KB colour screen is about 70 fragments; both clients wait for the stock server to resend it until one copy
gets through whole (a lottery: 17 s and 6.5 s here, and one earlier moche run showed "connection lost, last contact 72 s
ago" on that step before it recovered). That is the server's policy, not a moche fault; both clients behave alike.
After each flood the session was still interactive (`AFTER` echo 55-59 ms).

## Verdicts on the points asked

* Freeze count: moche new 0 of 65, stock 0 of 20, moche old 4 of 28 (round 4). B1 is fixed.
* Cost ratios vs the simulation: clean 1.04x (sim 1.03x) confirmed; 10% loss unverifiable because of B2.
* Do the RTO floor / state limits need to change? Yes, after B2 is fixed:
  - state_limit must never shrink the in-flight state (see B2), only the next cut;
  - the shrink trigger must be loss, not latency: count "sent twice with no ack" only after one full RTO has elapsed
    on each send (use `max(MIN_RTO, 1.5 x SRTT + 4 x RTTVAR)`, and at least 250 ms on this lab's 300 ms RTT);
    otherwise 150 ms delay and a no-loss 4 MiB paste shrink to 8 KiB for no reason (22.7 s and 6.7 s above);
  - `MIN_STATE_BYTES` 8 KiB is fine at 10% loss in round 4's data (16 KiB already gave 5.9x / 8 s); raise the floor to 16 KiB
    unless a lossier link shows the need, and grow back faster (x2 per acked state, already the case) or use slow start;
  - `MIN_RTO_MS` 100 is OK on a LAN; keep `MAX_RTO_MS` 1000.

## Not verified

* Anything at 10% loss for the paste matrix, Ctrl-C under loss, 100 ms + loss (B2). The 4.1x loss figure.
* Whether the 150 ms run's abandon path can crash the server: 2 of 2 runs completed, but the same code path ran.
* Stock `mosh-server` cause of the abort (inferred from the trace and the capture timing, source deliberately not read).
* `cargo test -p moche-core` in the VM (OOM on one test), so the VM run proves only the release build; the suite passes on a Mac (53 tests).
* Asymmetric loss, reorder, duplication, loss below 10%.

## Scripts

Not in the repo; on the lab host in `~/moche-vmlab/r5` with raw outputs: `rp5.sh` + `rpb.sh`/`rpb.keep.sh` (B1 loop, stats and
capture), `r4.sh` + `pb.sh` + `nm.sh` + `chain*.sh` (paste matrix, `SRVWRAP=/tmp/msw.sh` runs the server under strace),
`fld.sh` + `fl.sh` + `cmp.py` (floods), `lat.sh`/`latb.sh` (echo latency), `ip.sh`/`ipb.sh` (Ctrl-C while pasting), `ana.py`
(client-side capture decoder: prints `old/new/ack/thr` per reassembled instruction; needs the session key).
