# Observations, round 13: pipelined pastes (simulation only, lab run NOT done)

Date: 2026-11-05. Change: `ssp.rs` pipelines big states (up to 4 in flight), see `protocol.md` section 4.

## Status: lab measurements were not made

The x86 lab host (the x86 lab host) was unreachable for this work: the first an internal tool call (`host_status the x86 lab host`) was
refused with "this project has no an internal tool agent yet: approve its registration" and was not approved within
the 3-minute wait, twice. Not worked around. So NOT done: the 4 MiB paste at RTT 2/50/200 ms, lossless and
2 % / 10 % loss, against the pre-change binary and stock mosh-client; the B2 loop (>= 6 sessions, server abort
check); the B1 loop; and every question marked UNVERIFIED in `protocol.md` (does stock mosh-server 1.4.0 accept a
diff based on a received-but-unacked state, and tolerate out-of-order arrival).

## Simulation (virtual link, server model panics on B2 violations; not a substitute)

4 MiB lossless, client ack hold 100 ms, time to full delivery:

| one-way delay | before (1 state in flight) | after (<= 4) |
|---|---|---|
| 1 ms | 6834 ms | 1896 ms |
| 50 ms | 13400 ms | 4152 ms |
| 100 ms | 20100 ms | 7546 ms (max 3 in flight) |
| 0 ms, no hold | 1320 ms | 1320 ms |

Same bytes (1.03x). 1 MiB at 10 % loss, one-way 1/50/100 ms: cost 5.3-7.1x payload after against 5.7-7.9x
before (noisy: 1 ms seed 2 went from 5.65x to 7.05x, seed 1 6.08x to 6.11x); times within noise. Typing wire
schedule unchanged to the millisecond (golden fingerprint of 16 runs recorded on `a9b71c1`).

Regression risks to check on the lab: a lost state wastes the pipelined states behind it (mitigated: pipelining
stops after the first re-send and after a cap shrink); a key typed during a paste now goes out as a small state
in the pipeline instead of waiting for the ack.

## Lab results (2026-11-05, lima VMs on the Mac lab host)

Setup: stock `mosh-server` 1.4.0 (Debian VM) and clients in an Ubuntu VM, both aarch64, over the VM-to-VM
user-mode network. netem on eth0 of both guests (one-way D each, loss each way; RTT 2 ms = D 1 ms). Clients: the
pre-change binary (`base`, `a9b71c1`, one state in flight) and the pipelined one (`pipe`, `0b34093`), static
aarch64 musl, `--predict=never`. Paste = 4 MiB of base64 text via `tmux paste-buffer` into `cat > file` in the
remote shell; time = paste start to the echoed marker seen on the client screen; the file hash matched on every
run counted below. 3 runs per cell, fresh server each run, base and pipe alternating. Timeout 300 s.

### 4 MiB paste, median of 3 (all runs listed in seconds)

| loss | RTT | base | pipe | verdict |
|---|---|---|---|---|
| 0 | 2 ms | 2.4 (2.43 2.43 2.44) | 2.4 (2.38 2.42 2.47) | same (limited by the tmux paste rate) |
| 0 | 50 ms | 7.4 | 4.0 | pipe 1.9x faster |
| 0 | 200 ms | 19.7 (19.7 20.0 18.2) | 9.1 (9.1 9.1 8.8) | pipe 2.2x faster |
| 2 % | 2 ms | 16.4 | 16.4 (16.5 16.2 18.7) | same |
| 2 % | 50 ms | 25.5 (25.5 23.3 26.5) | 19.2 (18.0 28.0 19.2) | pipe faster in median, one slow run |
| 2 % | 200 ms | 56.5 (56.5 62.1 52.7) | 54.4 (56.1 49.2 54.4) | same within noise |
| 10 % | 2 ms | 103.8 (111 99 104) | 106.0 (103 106 113) | same |
| 10 % | 50 ms | 137.5 (137.5 140.3 134.2) | **190.8 (146.7 190.8 199.2)** | **REGRESSION: pipe 1.4x slower, 2.0x the bytes sent (28.3 MB vs 13.9 MB)** |
| 10 % | 200 ms | 276 (276 271 280) | 289 (264 300 289) | pipe 5 % slower, inside noise; both near the 300 s timeout |

Bytes sent by the client (median, from `--stats`): lossless 3.31 MB for both; 2 % loss 6.2-6.8 MB base against
7.1-7.3 MB pipe (+4 to +18 %); 10 % loss base 12.6-13.9 MB, pipe 12.8-13.7 MB except 28.3 MB at 50 ms.

So: pipelining pays only on a lossless (or 2 %) link with RTT >= 50 ms, where it is about 2x faster, and it does
not help once loss is 10 %. At 10 % loss with 50 ms RTT it is a measured slowdown with a doubled byte cost. Cause not
investigated (a lost state wastes the states pipelined behind it; the 50 ms cell is where several states are in
flight and the RTO is short enough to re-send them). This is a fail for the "no regression" criterion at 10 % / 50 ms.

Stock `mosh-client` reference: NOT obtained. In the same tmux harness it never finished a 4 MiB paste (9 of 9
lossless attempts at RTT 2/50/200 ms timed out; one early smoke run at RTT 2 ms did finish in 7.9 s) and a 1 MiB paste
at RTT 50 ms had delivered only 14 KB after 100 s (a 1 MiB paste at 0 ms completed). The stock client/server stalls on
a large paste in this setup; cause not investigated. Typing with the stock client works (below).

### What the stock server does (pcap on the server VM, ssp decoded with a throwaway script)

- Lossless, one-way 100 ms: the client sends state 10 with `old_num` 9 about 117 ms after state 9 arrived, far
  earlier than the server's ack of 9 could reach the client (the ack left the server at +138 ms and needs 100 ms).
  The server applied it: its ack_num moved 9, 10, 11, 12, ... one state per ~120 ms, with up to 4 big states
  (64 KiB, 40 fragments) in flight. So a stock 1.4.0 server DOES accept a diff based on a received-but-unacked
  state. Its replies kept `throwaway_num` behind the acked state, no abort.
- A state whose base was lost: 1 MiB paste at 10 % loss, one case captured (state 12 arrived with `old_num` 11 while 11
  never had). The server's ack_num stayed at 10 (it did not move to 12), kept sending normally, and acked 11 about 1.9 s
  later (the re-send), then 12 0.7 s after that: it silently drops the state, no abort (SIGABRT count 0). The wire
  capture only shows packets that survived netem, so lost states are visible only by absence.
- Out-of-order arrival (a lower-numbered state after a higher one): NOT verified. netem with a fixed delay never reorders;
  no reorder was injected.

### B2 loop (1 MiB paste, 10 % loss each way, pipe binary, server under `strace -f -e trace=process,signal`)

8 sessions: 8/8 hashes matched, 0 SIGABRT delivered or fatal to any server process, server process alive after each.
Times 29.5, 135.0, 27.0, 35.3, 26.5, 31.5, 26.6, 28.3 s. Session 2 (135 s) is a 4x outlier: it finished and hashed
correctly, no abort; I did not run the base binary in the same loop, so I cannot say whether the outlier is new.
(Two server-side child processes of session 2 logged `exited with 1`; not a signal, cause not looked into.)

### B1 loop (10 % loss each way, 15 sessions, `echo` echoed within 20 s)

15/15 ok, 0 freezes; echo back in 0.05-0.08 s except two at 0.27-0.28 s.

### Typing sanity (RTT 100 ms, about 300 keys at 80-120 ms, client datagrams per key, 3 runs each)

base 1.088 1.101 1.050, pipe 1.078 1.094 1.090, stock mosh-client 1.068 1.057 1.063. No difference between base and
pipe beyond run noise (mean 1.080 vs 1.087); stock is about 1.06. Small states keep their old schedule.

### Not verified / caveats

- No stock-client reference for pastes (stalls, above). Out-of-order arrival untested. Only aarch64 VMs, user-mode
  network (gvisor), tmux paste rate caps the lossless 2 ms cell at about 2.4 s for both binaries.
- 10 %/200 ms cells hit the 300 s timeout window (one pipe run finished at 300.3 s and was counted as done).
- 3 runs per cell is thin: the pipe 2 %/50 ms spread (18-28 s) and the B2 outlier show the noise.
- The pcap decoder reassembles by fragment id; a re-send reusing an id could merge fragments of two sends, so the
  count of complete states is an upper bound.

## Round 13b: self-limiting pipelining (gate on clean streak), lab re-run

Cause of the doubled bytes at 10 % loss (from the code, confirmed by the simulation, not by a wire trace): the first
version resumed pipelining as soon as the oldest state was acked, so after every loss a fresh chain of states was cut on
a base the server might not hold, and each lost state wasted the states pipelined behind it.
Fix (`ssp.rs`): a paste is pipelined only after `PIPELINE_AFTER_CLEAN` = 4 big states in a row were acked with no
re-send (`clean_streak`), and any re-send of a big state resets the streak to 0. Unit test
`pipelining_is_earned_and_lost_on_any_resend`; the pipeline shape test starts with the gate open. In the simulation the
10 % loss runs are now byte-for-byte the pre-pipelining numbers (the gate never opens at 16 KiB states); lossless sim
unchanged in kind (4 MiB at 100 ms hold: 6834 -> 2080 ms at 1 ms one-way, 13400 -> 4472 ms at 50 ms, 20100 -> 8072 ms at 100 ms).
The typing golden test (schedule recorded on `a9b71c1`) still passes.

Lab: same VMs and harness as above; `base` = `a9b71c1`, `gate` = this change, aarch64 musl, 5 runs per cell
alternating, 4 MiB paste, paste time in s (all runs), sha256 verified on every run counted "ok".

| loss | RTT | base median (runs) | gate median (runs) | client MB sent, base / gate | verdict |
|---|---|---|---|---|---|
| 0 | 50 ms | 7.4 (7.36 7.37 7.37 7.43 7.32) | 3.9 (3.93 3.91 3.91 3.92 3.94) | 3.31 / 3.31 | gate 1.9x faster |
| 0 | 200 ms | 19.4 (19.65 19.39 19.43 19.39 19.72) | 10.2 (10.25 10.34 9.04 10.32 9.12) | 3.31 / 3.31 | gate 1.9x faster |
| 2 % | 50 ms | 25.3 (25.33 26.05 23.80 22.59 28.41) | 26.0 (22.57 26.03 21.43 27.38 27.29) | 6.35 / 7.59 | same time; **gate sends 20 % more bytes** |
| 10 % | 50 ms | 135.3 (135 147 134 121 141) | 140.0 (158 146 133 125 140) | 12.99 / 12.96 | same within noise (previous version: 190.8 s, 28 MB) |
| 10 % | 200 ms | 287 (266 301 302 287 284; 3/5 done) | 287 (287 284 272 302 296; 4/5 done) | 14.33 / 13.98 | same; both near the 300 s timeout (the "not done" runs hit it) |

Honest reading: the 10 %/50 ms regression is gone and no lossy cell is slower beyond noise, the lossless gain is kept.
Remaining cost: at 2 % loss the gate still opens now and then (4 clean 16-64 KiB states in a row is not rare) and a loss then
wastes the pipelined states, so bytes are +20 % there with no time gain or loss. Raising the streak would cut that but
delay the lossless gain (each streak state costs about one RTT plus the 100 ms ack delay); not tried.

B2 loop (1 MiB, 10 % loss each way, server under `strace -f -e trace=process,signal`), 8 sessions each, all sha256 ok,
0 SIGABRT, server alive every time:
base 29.1 24.0 24.3 22.1 23.8 31.9 27.2 44.2 s (median 25.8); gate 28.5 37.3 29.9 24.8 25.6 28.4 32.1 23.5 s (median 28.4).
The 135 s outlier of the first pipelined run did not reproduce in 8 gate or 8 base sessions (slowest 44.2 base, 37.3 gate).
B1 loop (10 % loss, 15 sessions, gate): 15/15 ok, 0 freezes, echo back in 0.05-0.38 s.
Typing wire counts: not re-measured on the lab this round; unchanged by construction (small states never take the pipeline
path) and pinned by `typing_wire_schedule_is_unchanged_by_pipelining`, which passes.
Not verified: out-of-order arrival on the real server (still no reorder injected); a wire trace of why the old version doubled bytes.
