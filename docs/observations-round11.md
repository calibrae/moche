# Observations, round 11: predictive-echo ghost-secret forensics

Date: 2026-11-05. Builds: `d244c68` plus the working-tree changes of this round (debug trace, harness, six new simulation
tests). Question: can predictive echo ever show a typed secret (a "ghost") at a prompt that does not echo, and are the
timing failures of `tests/predict_e2e.py` a moche defect? Answer: no ghost secret in about 650 password checks against a stock
`mosh-server` and in 400 random simulated seeds; the two simulation tests that first failed were test-model errors (verdict A);
the timing failures are datagrams delayed by the OS loopback, not by moche or by the relay. `predict.rs` is unchanged.

Labels as in `docs/predict.md`: **OBSERVED** (black-box, stock `mosh-server`, local), **OUR CHOICE**.

## 1. Tools

| tool | what it does |
|---|---|
| `MOCHE_PREDICT_TRACE=<file>` (`crates/moche-cli/src/trace.rs`) | one text line per event: `on_input` (typed keys in HEX: debugging only, file mode 0600), predictor state after every hook (`epoch gate tentative streak pending frozen adaptive_on flag_on srtt echoed barrier`), `confirmed`, `contradicted`, `expired`, `hysteresis`, `server_diff`, `paint overlay_cells=... [r,c shown= server=]` (the overlay cells), `tty_write` (size, duration), `tx` (every sendto), `poll_late` (a poll that returned >20 ms after its timeout with nothing ready). Unset: one `OnceLock` load per hook, no allocation. |
| `Predictor::debug_state()` (`predict.rs`) | read-only snapshot of the internal state for the trace and the tests. |
| `tests/predict_e2e.py` | stock server, delaying relay, tmux; samples the pane every ~10 ms. Parts: timing (adaptive and never), password `marker` (type once the prompt is established) and `early` (type before the server echoed the switch). Password assertions are strict and never retried. Timing may retry (`MOCHE_E2E_RETRY`, default 2) only when a stall was seen; since this round a stall includes `loopback_stall_ms` > 100 (below). |
| `tools/perf/predict_e2e_campaign.py` | N sequential runs with progress log; `RETRY=0`; artefacts of failing runs kept. |
| `tools/perf/predict_e2e_classify.py` | classes (a) stall, (b) real echo, (c) ghost, (?) unknown. |
| `tools/perf/stall_ticker.py` | machine-wide scheduler stall logger (separate process). It cannot see a loopback delay (it never waits on a socket). |
| `tools/perf/delay_relay.py` | `MOCHE_RELAY_WALL=1` wall-clock stamps; `L` lines when its own loop came back >20 ms late. |
| `tests/predict_sim.rs` | `Sim::watch`/`seen`/`ghosts()`: per visible step the first row and the number of pending predictions; `SIMTRACE=1` prints each step with `debug_state()`. |

Run a campaign (about 30 s per full run, keep the machine quiet):

```
cargo build --release -p moche-cli
python3 -I tools/perf/predict_e2e_campaign.py OUTDIR target/release/moche 40 LABEL full       # timing + password
python3 -I tools/perf/predict_e2e_campaign.py OUTDIR target/release/moche 0 LABEL password marker:200,early:100
python3 -I tools/perf/stall_ticker.py OUTDIR/stalls.log &                                      # optional
python3 -I tools/perf/predict_e2e_classify.py OUTDIR LABEL
```

## 2. Campaign numbers (stock `mosh-server` 1.4.0, 100 ms one way, assertions as in `predict_e2e.py`: chars 4-10 within 40 ms)

| build | runs | password checks | failures | class of the failures |
|---|---|---|---|---|
| current (password only: 200 `marker` + 100 `early`) | 300 | 300 | 0 | - |
| `f4ff63c`-era (full) | 150 | 150 | 4 | 4 x (a): first 5-6 chars shown together after 0.65-1.3 s |
| `d063983` (full) | 100 | 100 | 2 | 1 x (a) with a harness sample gap, 1 marginal (53 ms) |
| `d07618b` (full) | 100 | 100 | 4 | 3 x ramp (1.1-1.4 s), 1 marginal (54 ms) |
| relay with zero-timeout busy poll, first attempt (stopped after 3 runs) | 3 | 3 | 2 | ramp (1.1-1.4 s), the machine was not yet quiet |
| relay with zero-timeout busy poll (full) | 40 | 40 | 1 | ramp, 892 ms |
| final relay + `loopback_stall_ms` (full, `RETRY=0`) | 40 | 40 | 1 | (a): `loopback_stall_ms` 592 (ramp 807 553 305 54), classified automatically |

Every failure is a timing assertion ("chars shown late"), never the password assertion. Total: about 650 password checks with
0 overlay paints, 0 differences from the bare prompt (`marker`), and in `early` only the server's own echo rows (class (b)
signature, expected: the keys typed before the server switched echo off were really echoed).

## 3. Verdict on the two failing simulation tests: both (A), bug in the TEST

`predict.rs` was not changed. Evidence by instrumenting the Sim (`SIMTRACE=1`).

### 3.1 `echo_off_mid_secret_leaks_one_window_at_most`: 500 ms was `visible_steps`, which is not the leak

`visible_steps` counts every ms at which the display differs from the server, including the legitimate lead of the display over
the REAL echo of keys that do reach the server. Timeline (100 ms one way, keys every 150 ms, echo off at 875):

```
t=500..600 keys 1,2 arrive and are echoed by the server for real; key 1 confirms the epoch (gate_open)
t=600  shown "$ se"   srv "$ s"      display leads the real echo: legitimate (the server did echo "e")
t=701  shown "$ sec"  srv "$ s"      same, key 3 arrives at 800 and is echoed (echo is off only from 875)
t=851  shown "$ secr" srv "$ se"     key 4 'r' typed at 850: PREDICTED. It reaches the server at 950: not echoed (echo off)
t=1001 shown "$ secre"srv "$ sec"    key 5 'e' typed at 1000: predicted too (epoch still confirmed, no ack yet)
t=1100 the echo ack that covers key 4 arrives: no cell appeared -> contradiction, tentative, both hidden
t=1150 key 6: not shown (tentative)
```

Legitimate worst case by design: the first un-echoed key typed at T is displayed from T until the echo ack that covers it returns:
up (100) + echo window (50) + down (100) = 250 ms. Measured: 249 ms (851..1099), 2 characters at once (`r`, `e`), 0 after the
contradiction (tentative needs 2 confirmations). The test now measures characters that never existed on the server (`ghosts()`:
the first row beyond the longest common prefix with the server's FINAL row) and asserts `<= up + 50 + down + 11` ms and
`<= window / key_gap + 1` characters. Leaked secret characters ever displayed: **2** (`r`, `e`), for 249 ms. The 500 ms total
overlay time is 249 ms ghost + 251 ms of display leading real echoes.

### 3.2 `property_no_ghost_when_echo_is_off_before_typing`, seed 55: 7 steps were the frozen line of the Enter

The 7 visible steps (t = 1137..1143) happen BEFORE the first secret key is typed (t = 1195): the line `$ aaaaa` typed before the Enter
was still frozen on screen (the Enter's ack had not arrived: 30 % loss in that seed), the baseline was taken at `t0` while the
frozen overlay was still displayed. No overlay of any kind (character, blank cell, cursor move) appears at or after a key of the
secret, including the backspaces (0x7f) and left arrows (ESC [ D) of the random keys: a prediction is never made or shown before
a confirmation in its epoch, and the barrier after the Enter forbids one before the ack. So there is nothing to avoid
(cursor-only predictions at a non-echo prompt do not leak, and no `predict.rs` change was needed).
The assertion is split: (i) strict, no character the server never had is ever displayed (`ghosts()`), (ii) no overlay of any kind
while a prediction of a key typed after the prompt is pending (`Seen::pending`, from `debug_state()`); both hold for 400 seeds
(was 120). The frozen line is exempt: it shows characters the server did echo.

## 4. The timing failures: class (a), and what it is not

Signature of every failing run: `latency ms [1106 860 609 349 97 5 5 5 6 6]`: the first 5-6 characters appear TOGETHER about 1.1 s
after the first key (each latency is 250 ms lower than the previous one: typed every 250 ms), then everything is normal. The `never`
part (no lateness assertion) shows a stall above 0.7 s in 16 of 146, 12 of 98 and 41 of 96 full runs of the three builds, so stalls are common and the
adaptive part is only the one with a bound that catches them. It appears on the oldest build too (`d07618b`), so it is not a
regression of the resend policy.

Where the second goes, from the traces of failing runs (`moche.trace` `tx` lines, `relay.log` `C` lines, both wall clock):

```
462.252 on_input 'a' ; tx            client sends at once (the same 1 ms as in good runs)
462.504, 462.642, 462.778, 463.012   tx for each following key: the client is running normally
463.1316 five C lines within 0.4 ms   the relay READS all five datagrams only now
```

The datagrams sat about 0.88 s between the client's `sendto` and the relay's `recv`. The relay was not late (no `L` line: its loop
never came back more than 20 ms late), the client's polls were not late (no `poll_late`), the machine-wide ticker saw no stall.
A first hypothesis, "the relay's `select()` timer is throttled by macOS after ~1 s idle", was tested: a zero-timeout busy poll while
datagrams are in flight and for 2 s after any datagram. It changed nothing (1 failure in 40 against 4 in 150, same signature).

A stand-alone check without moche and without the relay (a sender and a receiver, plain Python sockets on 127.0.0.1, the
receiver busy-polling a non-blocking socket, 80 datagrams at idle gaps of 0.2 and 1.1 s; run from the scratchpad, not
committed): 10 of 80 datagrams arrived 32 to 1130 ms late (examples 1130, 713, 513, 752 ms), the receiver's own loop never
stalled more than 4 ms and the sender's `sendto` returned at once. So the OS delays a loopback datagram between send and receive
for up to about a second, regardless of what the application does. That explains class (a), the ramp, why the same signature is on
every build, and why `stall_ticker.py` (which never waits on a socket) sees nothing.

The harness now measures it directly: `Session.loopback_stall_ms()` matches each client `tx` to the relay's `C` line, and a failing
timing part with a stall above 100 ms is marked as disturbed (retried when `MOCHE_E2E_RETRY` > 0, which is the default; the
campaign sets 0 to count raw failures). The password parts are not retried and have no such exemption.

Marginal failures (42-60 ms for one character, 3 in 350 runs) are the 40 ms bound of the assertion against ordinary scheduling
noise of a loaded desktop; the bound is the assertion's, not a property of moche (typing latency is 5-8 ms in the other 340).

## 5. Cost of the trace when unset

Library (`predict_bench`, `bench`, `allocsites`, release, same machine, `d244c68` in a worktree against the working tree): identical
within noise. Inactive Adaptive key+compose+echo 7 ns/op, 0 allocs (both); Never 5 ns; idle tick 5 vs 4 ns; active 2195 vs 2185 ns
(1 alloc, 8 B, both); `allocsites`: 0 allocations >= 4 KiB on every path, both. The CLI hooks themselves (`trace::on()`, a `OnceLock`
load per hook; one `Instant`-free branch per poll) are outside those benches: one relaxed atomic load each, no allocation.
`tools/perf/golden-diff.sh --pinned`: MATCHES (the predictor only changed by the read-only `debug_state`).

## 6. Recommendation

Ship predictive echo (Adaptive, default) in the iOS artifact. No ghost secret in any measurement; the only way to show a secret
character is a host that stops echoing in the middle of the secret, bounded by one round trip plus the 50 ms echo window and one or
two characters (250 ms and 2 characters at 100 ms one way). `always` stays opt-in and documented as leaking one window.
Open points: wide/combining characters and insertion inside a line are still not predicted (stock does the latter); thresholds were
validated on loopback and a VM lab, not on a real cellular path.
