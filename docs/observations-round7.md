# Observations, round 7: protocol regression of the CPU optimisations (`bf5951d`) against a stock mosh-server

Date: 2026-11-04. Build under test: `bf5951d` ("CPU: reusable zlib state, shared Arc screen states, single decrypt in
FFI, fewer syscalls in the CLI loop"), the `aarch64-unknown-linux-musl` release artefact built with
`tools/release/build.sh` from a fresh clone (`moche 0.1.0 (bf5951d, aarch64-unknown-linux-musl)`). Same lab as rounds 4-6
(`tools/vm-lab/README.md`): server `moche-a` (Debian 13, stock mosh-server 1.4.0), client `moche-b` (Ubuntu 24.04, stock
`mosh-client` 1.4.0 for comparison). Drivers are the round 6 scripts (`r4.sh`, `rp5.sh`/`rpb.sh`, `pb.sh`, `fl.sh`/`fld.sh`,
`nm.sh`, `cmp.py`) copied unchanged except for the binary path; control channel over the vzNAT `lima0` addresses, netem on
`eth0` (the data path). Stock mosh is a black box; `crates/**` untouched. The CPU/mmap numbers of the same session are in
`docs/PERF.md` ("Linux A/B, same VM, same day").

## Summary

| # | Item | Result |
|---|---|---|
| a | B1 loop: fresh sessions at 10 % loss each way, `echo RDY<i>` | **PASS: 0 freezes in 32 sessions** (`RDY` within 0.4 s in all 32; `rx_gaps>0` in 5; `send_errors=rx_errors=0`) |
| b1 | B2 loop: 1 MiB random base64 paste, 10 % loss, sha256, server under `strace -f -e trace=process,signal` | **PASS: 6 of 6 completed, sha256 match, 0 server deaths, 0 SIGABRT** |
| b2 | Same, 10 % loss + 100 ms one-way delay | **PASS: 3 of 3 completed, sha256 match, 0 deaths** (one slow outlier, see below) |
| c | Pastes without loss, `--stats` | PASS: 1 MiB 1.04 s / 1.04x, 4 MiB 2.67 s / 1.04x, sha256 match |
| d | Floods at 250x80: `yes \| head -c 5000000`, `seq 1 1000000`, 256-colour dump | PASS: text identical to stock in all three; attributes identical (colour dump identical after normalising the known D1, palette 8-15) |
| e | 5-minute session, resize, NIC down 20 s, back with a new IP, resize | **PASS: session continued, typing and both resizes worked** |
| - | Cleanup | netem, iptables, tmux, mosh-servers, strace/tcpdump, captures and the temporary ssh key removed; both VMs running |

No bug found. Nothing in the receive path, the shared `Arc` screen states or the single-decrypt FFI path changed the wire
behaviour that rounds 4-6 established.

## a. B1 loop

`rp5.sh`/`rpb.sh` as in round 6: fresh server, `moche --stats --direct`, wait 3 s, type `echo RDY""1`, wait up to 20 s for `RDY1`.
Netem `loss 10%` on both VMs. 32 sessions: 32 OK, 0 FREEZE, every `RDY1` seen on the second poll (about 0.4 s). `rx_gaps>0` in 5
runs, `send_errors=0 rx_errors=0` in all. (Round 6: 0 of 32, round 5: 0 of 65, round 4: 4 of 28.)

## b. B2 loop

`r4.sh`-style driver, mosh-server under `/tmp/msw.sh` (strace `-e trace=process,signal`), client in tmux 200x50, 5 Hz ticker
on the server, 1,048,509 B of random base64 pasted with `tmux paste-buffer -r` into `cat > /tmp/paste.out`, `Ctrl-D`, `echo FIN<n>`.
Cost = `--stats bytes_sent` over the 797 KB compressed payload.

| profile | run | time (s) | bytes_sent (MB) | cost | sha256 | server alive / SIGABRT |
|---|---|---|---|---|---|---|
| loss 10 % | 1 | 28.8 | 3.18 | 4.0x | match | yes / 0 |
| loss 10 % | 2 | 26.0 | 2.84 | 3.6x | match | yes / 0 |
| loss 10 % | 3 | 33.2 | 4.04 | 5.1x | match | yes / 0 |
| loss 10 % | 4 | 31.4 | 3.58 | 4.5x | match | yes / 0 |
| loss 10 % | 5 | 24.5 | 3.08 | 3.9x | match | yes / 0 |
| loss 10 % | 6 | 33.8 | 3.83 | 4.8x | match | yes / 0 |
| loss 10 % + 100 ms | 1 | 73.4 | 3.07 | 3.9x | match | yes / 0 |
| loss 10 % + 100 ms | 2 | 63.7 | 3.24 | 4.1x | match | yes / 0 |
| loss 10 % + 100 ms | 3 | 84.1 | 6.02 | 7.6x | match | yes / 0 |

Round 6 (`6dff485`) for comparison: 10 % loss 23-31 s, 3.3-4.9x (10 runs); 10 % + 100 ms 59-76 s, 3.6-4.6x (6 runs). This round
sits at the upper edge for 10 % loss (mean 29.6 s, 4.4x) and the third delay run (84 s, 7.6x) is above the earlier range; all
completed with a matching hash and no server death, which is the property under test. Nine runs are too few to tell whether the
tail is wider or just sampled differently (the cost is a whole-copy retry process with a heavy tail; round 6 recorded one 8.1x
outlier as well), and the golden-diff scenario shows the sender behaviour byte-identical, so I attribute it to the lab's noise.
Not investigated further.

## c. Paste without loss

| size | time | client bytes | cost | sha256 |
|---|---|---|---|---|
| 1 MiB | 1.04 s | 828,705 | 1.04x | match |
| 4 MiB | 2.67 s | 3,310,203 | 1.04x | match |

Round 6: 0.82 s and 2.83 s, 1.04x. Cost per byte is the same; the 1 MiB wall time (measured by polling for `FIN` on the pane,
50 ms steps, so about 0.1 s of it is the harness) is 0.2 s above round 6 in a single sample, the 4 MiB one 0.16 s below; the
CPU measurement in `docs/PERF.md` (paste 1 MiB 0.45 s wall in both baseline and new, `tools/perf`) shows no change.

## d. Floods, 0 % loss, 250x80, one session per client

| step | moche done | stock done | text identical | attributes |
|---|---|---|---|---|
| `yes \| head -c 5000000` | 1428 ms | 1468 ms | yes | identical |
| `seq 1 1000000` | 757 ms | 989 ms | yes | identical |
| 256-colour dump | 119 ms | 121 ms | yes | identical after normalising the known D1 |

`rx_errors=0 send_errors=0 rx_gaps=0`, echo after the flood 61 ms (moche) / 65 ms (stock).

## e. 5-minute session with a NIC outage and a new IP

Client `moche --stats --direct` in a 100x30 tmux pane against a fresh stock server; the control channel is `lima0`, the data
path `eth0`. Timeline (seconds after start):

| t | event | observed |
|---|---|---|
| 4 | `echo RDY1` | seen |
| 4-94 | idle | |
| 94 | resize 100x30 to 90x26 | `tput` in the session printed `90 26` |
| 94-178 | idle | |
| 178 | start a 1 Hz `tick` loop | |
| 181 | `ip link set eth0 down` | |
| 201 | after 20 s | moche status line: "connection lost, last contact 20 s ago"; screen at tick4 |
| 201 | old address removed, new address `.60` added (the old one was already gone with the link), `eth0` up | |
| 202 | +1 s | screen at tick24, which is the current tick: the server's 20 s of output arrived in the first second, and ticks continued 1 per second |
| 216 | `Ctrl-C`, `echo POST1` | seen |
| 216 | resize 90x26 to 110x32 | `tput` printed `110 32` |
| 300 | `echo END1` | seen, server process alive |

Session continued across the outage and the address change with no reconnection step on the client; stock mosh-server needed no
intervention. (The client's own packets to the server left from the new address; the old address no longer existed on the
interface. I did not capture the peer address on the server side: `ss` shows an unconnected UDP socket.) My script's
"expected tick" column in the log used a wrong offset and is ignored; the tick numbers above are compared with the loop start.
One harmless script error: the default route's `src` pointed to the old address, so `ip route replace` was refused
("Invalid prefsrc address") for the moment between the two states; the on-link route to the server was unaffected, and the
original address and routes were restored at the end.
