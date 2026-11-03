# Observations, round 4: large input and large server diffs against a stock mosh-server

Date: 2026-11-03. Repo HEAD under test: `acab026` (chunked input states, `MAX_STATE_BYTES` 64 KiB,
`CHUNK_BYTES` 16 KiB, `MAX_FRAGMENTS` 2048, `MAX_INFLATED` 16 MiB). Lab: the two lima VMs of
`tools/vm-lab/README.md` (server `moche-a`, Debian 13, mosh 1.4.0; client `moche-b`, Ubuntu 24.04, moche
built from `acab026` release, stock `mosh-client` 1.4.0). Addresses are placeholders (`<a-u2>`, `<b-u2>` user-v2
`eth0`, `<a-vz>`, `<b-vz>` vzNAT `lima0`). The control channel was plain ssh over the vzNAT addresses, so netem
on `eth0` never touched it. Stock mosh was a black box. No file under `crates/` was edited (one scratch
variant was built in a VM copy, see 3b; the VM copy was deleted).

## Method

* Server: `SSH_CONNECTION='<b-u2> 1 <a-u2> 22' mosh-server new -s -c 256 -l LANG=C.UTF-8` (fresh per run).
  Client in tmux (200x50) in `moche-b`: `MOSH_KEY=... moche --direct <a-u2> <port>` or `mosh-client <a-u2> <port>`.
* In the session: `stty -echo`, a 5 Hz background ticker (`date +T%s%N`) to watch downstream freshness, then
  `cat > /tmp/paste.out`. Source: `head -c N /dev/urandom | base64 -w 76` (lines of 76 chars, printable). Paste =
  `tmux load-buffer` + `tmux paste-buffer -r` as ONE block ("paste"), or the same file cut in 64 KiB pieces, one
  `paste-buffer` every 0.3 s ("chunks"). Then Ctrl-D and `echo FIN<n>`; completion = `FIN<n>` visible in the pane
  (so it includes the return path). sha256 of `/tmp/paste.out` on `moche-a` compared with the source.
* Bytes: iptables counters (OUTPUT dport / INPUT sport) on `moche-b`, zeroed at paste start; they count IP bytes
  (payload + 28 B header) of datagrams the client *sent*, before netem. tcpdump on `moche-a` (`-w`, then `-r`)
  agreed on the unimpaired runs.
* Important for "overhead": base64 text deflates to about 76% (gzip of the same data: 796,801 B for the
  1,048,509 B source). The unavoidable minimum on the wire is therefore about 0.8 MB for the 1 MiB paste,
  3.2 MB for 4 MiB, 76 KB for 100 KB. Ratios below are against that compressed size.
* Impairment: `tc qdisc replace dev eth0 root netem ...` on both VMs (each direction gets the figure, so
  `delay 150ms` is 300 ms RTT).

## Summary

| # | Scenario | Verdict |
|---|---|---|
| 1 | 100 KB, 1 MiB, 4 MiB paste, 100 KB / 1 MiB in 64 KiB pieces, no impairment | PASS (sha256 identical every time, no stall) |
| 2 | 1 MiB paste, 10% loss / 150 ms delay / 10% loss + 100 ms | PASS on integrity; FAIL on traffic cost under loss (63-99x) and stalls of 12-19 s |
| 3 | Server floods (5 MB `yes`, `seq 1 1000000`, full-screen colour dump 250x80 and 400x120) | PASS; text identical to stock; attributes identical except the known D1 |
| 4 | Stock `mosh-client`, same 1 MiB paste | 0% loss: faster and 2.3x cheaper than moche. 10% loss: stock never completes (0 byte arrived in 220 s, 1.9 GB sent) |
| 5 | BUG found: "out of sync" at session start under 10% loss, 4 of 28 sessions, never recovers | FAIL (B1) |

## 1. No impairment (moche)

| payload (text) | mode | time to complete | client datagrams / IP bytes | bytes / compressed payload | sha256 |
|---|---|---|---|---|---|
| 99,946 B | paste | 58 ms | 69 / 81,178 | 1.07x | match |
| 99,946 B | 64 KiB pieces | 0.63 s sending + 67 ms | 201 / 242,832 | 3.2x | match |
| 1,048,509 B | paste | 912 ms | 1548 / 1,947,883 | 2.44x | match |
| 1,048,509 B | 64 KiB pieces (0.3 s apart) | 5.07 s sending + 71 ms | 1915 / 2,381,363 | 3.0x | match |
| 4,194,267 B | paste | 3354 ms (1.25 MB/s of text) | 5817 / 7,357,634 | 2.3x | match |

Server to client was tiny in all runs (26-106 datagrams, under 10 KB). Longest "no progress" seen by a 2 s poll of
the destination file size was 2-3 s (pre-paste and tail included): no stall. Downstream freshness while pasting:
ticker lag median 89-125 ms, max 228 ms (the ticker period is 200 ms, so about 100 ms is the floor).

Even without loss the client sends 2.3-3x the compressed payload: an unacked state is re-sent every 20 ms frame slot
until the ack comes back (the server acks about 50 ms later at this size), so each 64 KiB state goes out 2-5 times.

## 2. Impairment (moche, 1 MiB paste, `acab026`)

| profile | time to complete | client datagrams / IP bytes | bytes / compressed payload | average rate on the wire | longest no-progress (2 s poll) | ticker lag median / max | sha256 |
|---|---|---|---|---|---|---|---|
| none | 0.91 s | 1548 / 1.95 MB | 2.4x | | 2 s | 89 / 198 ms | match |
| delay 150 ms (300 ms RTT) | 7.95 s | 2017 / 2.54 MB | 3.2x | | 5 s | 290 / 476 ms | match |
| loss 10% | 33.3 s | 62,096 / 78.7 MB | 98.7x | 2.36 MB/s | 12 s | 125 / 237 ms | match |
| loss 10% + delay 100 ms (200 ms RTT) | 106.9 s | 40,018 / 50.5 MB | 63x | 0.47 MB/s | 19 s | 275 / 594 ms | match |

Verdict on the previous agent's estimate ("up to about 3 MB/s of duplicate traffic on a bad link because the whole
unacked 64 KiB state is resent every frame slot"): **confirmed**. 64 KiB / 20 ms is 3.2 MB/s of raw input, about
2.4 MB/s after deflate, and 10% loss on a LAN measured 2.36 MB/s for 33 s. The reason it is so expensive: one
compressed state is about 35-50 fragments, a state only arrives if all of them do (0.9^40 is about 1.5%), so each state
needs roughly 100 whole re-sends. Stalls above 10 s did occur (12 s and 19 s without a single byte reaching the
destination), although the session never died.

Does it hurt interactivity? Downstream, no: the ticker kept arriving (medians above) in every profile, so the
server-to-client path and the screen stay live while a paste is in flight. Upstream, anything typed during a paste is
in the same ordered stream behind it, so a Ctrl-C typed mid-paste reaches the server only after the paste: up to 33 s
at 10% loss and 107 s at 10% loss + 100 ms. I did not time a typed marker mid-paste (it cannot be echoed while
`cat` holds the tty); the figure is the remaining paste time by construction. On a real phone link (cellular, shared
uplink) the 2.4 MB/s would also compete with the user's other traffic and cost data: 79 MB for a 1 MiB paste.

## 3. Policy experiment (scratch variant, not in the repo)

3b. In a scratch copy inside `moche-b` I rebuilt with `MAX_STATE_BYTES = 16 KiB`, `CHUNK_BYTES = 4 KiB` (two
constants, nothing else) and ran the same 1 MiB paste (copy deleted afterwards):

| profile | 64 KiB (HEAD) | 16 KiB variant |
|---|---|---|
| none | 0.91 s, 1.95 MB (2.4x) | 2.88 s, 1.66 MB (2.1x) |
| loss 10% | 33.3 s, 78.7 MB (98.7x) | 8.0 s, 4.68 MB (5.9x) |
| loss 10% + 100 ms | 106.9 s, 50.5 MB (63x) | 35.4 s, 4.30 MB (5.4x) |

sha256 matched in all. A smaller state (about 10 fragments) is 13-17x cheaper and 3x faster on a lossy path, but 3x
slower on a clean LAN (one state per ack round). So a fixed size is a bad compromise.

Suggestion (untested beyond the numbers above): make the state size adaptive, TCP-like. Start at 64 KiB; when the
oldest state has been re-sent twice without an ack, halve the cap (floor 4-8 KiB, about 4-8 fragments); double it
(ceiling 64 KiB) after each state acked at the first send. Independently, stop re-sending a large state every frame
slot: after the first send, re-send a state of more than about 4 KiB at most once per RTO (about SRTT + 4 x RTTVAR,
floor 100-200 ms) instead of once per 20 ms. On a clean path that takes the duplicate factor from 2.4x to about 1x
(the ack arrives within one RTT); on a lossy path it caps the stream at about 0.5 MB/s instead of 2.4 MB/s. Keep the
20 ms slot for small (keystroke-sized) frames, where latency matters and the cost is nothing. Backpressure:
`queued_bytes()` exists, so the host can also pause the feed of a paste above a few hundred KiB.

## 4. Stock `mosh-client`, same 1 MiB paste

| profile | time | client datagrams / IP bytes | bytes / compressed payload | result |
|---|---|---|---|---|
| none | 271 ms | 665 / 842,019 | 1.06x | sha256 match |
| loss 10% | never | 1,483,983 / 1,896,976,825 in about 220 s (8.6 MB/s) | n/a | 0 bytes arrived, "Last reply 2:51 ago", aborted by me |

Compared with stock at 0% loss, moche was 3.4x slower and sent 2.3x the bytes. Stock sends the whole backlog as one
instruction (about 800 fragments), re-sent forever, and 0.9^800 is zero: at 10% loss a 1 MiB paste is impossible for
stock, and it burns 1.9 GB trying. moche completes (slowly and expensively); this is the one place the bounded state
design is clearly ahead. (The tcpdump file on `moche-a` for this run filled the 12 GiB disk of the VM; the capture was
truncated and a run of the floods had to be redone, nothing else was affected; the capture files were then removed.)

## 5. Server floods (moche vs stock, 250x80 and 400x120, no impairment)

Each client ran the same script in its own session: `yes | head -c 5000000`, `seq 1 1000000`, then a deterministic full-screen
dump (every cell with a 256-colour background and foreground, `tools/vm-lab/tests`-style script `cd.sh`, awk), then
`echo AFTER`. `done` = time until the `FIN` marker is on screen. `in` = datagrams from the server during the step.

| size | step | moche done | stock done | moche in (pkts / B) | stock in | pane text vs stock | pane attributes vs stock |
|---|---|---|---|---|---|---|---|
| 250x80 | `yes` 5 MB | 1435 ms | 1354 ms | 11 / 1211 | 17 / 1893 | identical | identical |
| 250x80 | `seq` 1..1,000,000 | 768 ms | 927 ms | 38 / 12,059 | 46 / 15,793 | identical | identical |
| 250x80 | colour dump | 119 ms | 124 ms | 74 / 90,157 | 74 / 90,422 | identical | differs, D1 only |
| 400x120 | `yes` 5 MB | 1920 ms | 1918 ms | 17 / 1883 | 5 / 638 | identical | identical |
| 400x120 | `seq` | 970 ms | 1023 ms | 50 / 22,074 | 52 / 24,221 | identical | identical |
| 400x120 | colour dump | 165 ms | 177 ms | 173 / 215,720 | 175 / 215,883 | identical | differs, D1 only |

The attribute difference is the already-known D1 (palette 8-15 as `90-97/100-107` in moche, `38;5;8-15` in stock): after
rewriting stock's `38;5;8..15` / `48;5;8..15` to the aixterm form, the two attribute dumps are byte-identical at both sizes.
The 400x120 colour screen is about 215 KB of server diff over 173 datagrams, no loss: it fits the new receive limits
(MAX_FRAGMENTS 2048, MAX_INFLATED 16 MiB) with a wide margin. The session stayed interactive afterwards (`AFTER` echo 60-117 ms),
the last screen of `seq` ends with `1000000`, no crash, no hang. moche RSS 5-8 MB, stock 21-31 MB, CPU about 0.1-0.4%.

The counters (`queued_bytes`, `send_errors`, `rx_errors`) are Rust getters on `Transport`; the CLI does not print
them and there is no env var or flag for it, so I could not read them without editing `crates/`. Inferred from behaviour:
everything arrived and every screen matched, so `rx_errors` was effectively 0 in the floods and `send_errors` 0 in the
pastes (a stuck frame would have blocked the sha256 match). Not measured, only inferred. A debug line (or an exit-time
stats print, `--stats`) in the CLI would make this checkable; I suggest it.

## 6. BUG B1: "moche: out of sync" at session start under loss, never recovers

Repro (no paste involved): netem `loss 10%` on both VMs' `eth0`; then repeatedly start a fresh `mosh-server`, start
`moche --direct`, wait 3 s, type `echo RDY1`, wait up to 20 s for `RDY1` on screen.

* moche `acab026`: 3 of 20 sessions never showed `RDY1` and showed the status line `moche: out of sync` for the whole 20 s
  (plus 1 of the 8 sessions of my first run, so 4 of 28, about 14%). The first run of the scenario with the 16 KiB variant
  hit the same state (its paste data still arrived: the upstream path was fine, the display was frozen).
* stock `mosh-client`, same procedure: 0 of 20.

Cause (read from the code, hypothesis): `crates/moche-cli/src/display.rs` `on_host_diff` flags `desync` when a diff's base state is
unknown and keeps the transport's ack of that state ("v1: ignore and flag it ... see U13"). A first diff based on a state lost in
transit then leaves the server believing we hold it. Verify with the repro above and the transport log of which `old_num` was missing.
Fix direction: do not ack (or ack the last applied state) when `apply` fails, so the server falls back to a base we do have.
This is a correctness problem on cellular links and not related to the large-input change.

## What remains unverified

* Typed-marker latency while a paste is in flight (inferred only).
* The counters `queued_bytes`, `send_errors`, `rx_errors` (not reachable from the CLI).
* Asymmetric loss, reorder and duplication, loss below 10% (where stock may still complete a 1 MiB paste), delay 150 ms
  for stock, 4 MiB under impairment, 16 KiB variant at 150 ms delay.
* The adaptive-size / once-per-RTO policy itself: only the fixed 16 KiB variant was run, one run per cell (small
  sample; the loss rows are lotteries, do not read them as a ranking).
* iOS: memory and battery cost of 2.4 MB/s of duplicate uplink; backgrounding mid-paste.
* Bracketed paste and a pty in canonical mode with lines over 4095 characters (all lines here were 76).

## Scripts

Not copied into the repo; they stay in `~/moche-vmlab/r4` on the lab host with raw outputs (`res.*.txt`, `prog.*.txt`, pane
captures): `pb.sh` (in the client VM: tmux, paste, counters), `r4.sh` (driver: server, tcpdump, polling, sha),
`nm.sh` (netem profiles on both VMs), `fl.sh` + `fld.sh` + `cd.sh` (floods), `rp.sh` (B1 repro), `mkvar.sh` (scratch
variant), `chain*.sh`.
