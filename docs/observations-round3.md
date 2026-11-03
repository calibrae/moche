# Observations, round 3: real-network gate tests against a stock mosh-server

Date: 2026-11-03. Repo HEAD under test: `b2e9adb`. Lab: two lima VMs (see `tools/vm-lab/README.md`).
Server side `moche-a` (Debian 13, mosh 1.4.0). Client side `moche-b` (Ubuntu 24.04, mosh 1.4.0, moche
built from HEAD, release). Addresses below are placeholders: `<a-u2>`, `<b-u2>` (user-v2 `eth0`),
`<a-vz>`, `<b-vz>` (vzNAT `lima0`). Control channel for the whole run was plain ssh over the vzNAT
addresses with the lima key (never `limactl shell` while `eth0` was touched). Clean-room: stock
client and server were used as black boxes.

Every scenario ran in tmux inside `moche-b` (100x30), screens read back with `tmux capture-pane`.
Server started on `moche-a` with `SSH_CONNECTION='<b-u2> 1 <a-u2> 22' mosh-server new -s -c 256 -l LANG=C.UTF-8`;
client `moche --direct <a-u2> <port>` with `MOSH_KEY` in the environment, or stock `mosh-client <a-u2> <port>`.
Test scripts and raw outputs stay on the lab host (not in this repo); see "Scripts".

## Summary

| # | Scenario | Verdict |
|---|---|---|
| 1 | Basic (prompt, echo, colour ls, htop, vim, resize) | PASS |
| 2 | Differential vs stock `mosh-client` | PASS on text (13/13 stages identical); one cosmetic attribute difference (D1) |
| 3 | Bootstrap (Linux and macOS build) | PASS |
| 4 | Impairment (delay 50/200, loss 10, loss 30 + delay 100) | PASS, comparable to stock (I1: ~20% more packets) |
| 5 | Blackout 20 s and 90 s | PASS |
| 6 | Roaming: source port, source IP, replayed datagram | PASS (all three) |
| 6b | NIC down/up, new address, flap | PASS, same resume times as stock |
| 7 | Idle 330 s | PASS on liveness/CPU; W1 (about 10 wakeups per second idle) |

No functional bug found. Items to decide: D1 (SGR for colours 8-15), W1 (idle wakeups, relevant for iOS battery),
I1 (packet volume), E1 (locale string sent by the macOS build).

## 1. BASIC: pass

Evidence: scenario 2 covers prompt, `ls --color=always`, `cat` of a colour file, `seq 1 40`, unicode, vim
open/edit/quit, resize 100x30 to 80x24 to 120x40 with `tput cols/lines` correct afterwards. htop drew its first
full screen 127-134 ms after launch on LAN (also measured in 4). On macOS (3) htop and vim were also fine.
Not verified: htop being *quit* cleanly was not asserted by a script (my pattern was wrong, see "Test bugs"),
but the following prompt/echo checks in every later scenario worked.

## 2. DIFFERENTIAL vs stock client: text identical, one attribute difference

Method: two separate servers on `moche-a`, the identical scripted session (`tmux send-keys`, fixed files
in `~/fx`: listing, a colour file with 8/16/256/truecolor/bold/underline/reverse/italic, unicode with
wide chars, combining marks, box drawing, emoji, control chars `\r \t \b`, `seq 1 40`, vim edit of a 60-line
file, resize x2). The two sessions ran one after the other (a first parallel attempt collided on vim's
swap file: my fault, not a client difference). 13 snapshots each, compared with `capture-pane -p` (text)
and `-p -e` (text with attributes).

Result: **text identical in all 13 stages**, including the vim screens and both resizes.
Attributes identical in 11 of 13 snapshots. Differences, complete list:

* **D1 (cosmetic)**: palette colours 8-15. The stock client re-emits them as `38;5;8`..`38;5;15` (fg) and
  `48;5;8`..`48;5;15` (bg); moche emits `90-97` / `100-107` (aixterm bright). Seen in `ls --color` on
  `fixed.orig` (`38;5;8` vs `90`), and in an extra test of indexes 0-15 plus SGR 90-97/100-107 sent by the
  server (`/tmp/c16.sh`, in the tests directory). Indexes 0-7 are identical (`30-37`, `40-47`); indexes 16-255 and
  truecolor are identical. The input forms 90-97 and `38;5;8` are *normalised the same way* by moche
  (both become 90): moche stores one value for the colour, the stock client keeps the original form. Visually
  equivalent on any xterm-like terminal (and in SwiftTerm, where both map to the same bright slot). Decision:
  leave it, or keep the encoding in the cell if exactness is wanted.
* Not differences (both clients drop them identically): SGR 2 (dim), 9 (strike), 53 (overline), 4:3 (curly underline)
  are not rendered by the stock client either; blink (5) and hidden (8) are preserved by both.

Not verified: cursor position and cursor visibility were only compared indirectly (tmux `-p` does not show them),
the bell, window title, mouse modes, bracketed paste and `less` were not exercised.

## 3. BOOTSTRAP: pass

Linux (`moche-b` to `moche-a`): throwaway ed25519 key created in the VM, authorised on `moche-a` only,
`moche --ssh-arg -i --ssh-arg ~/.ssh/moche_lab --ssh-arg -o --ssh-arg IdentitiesOnly=yes
--ssh-arg -o --ssh-arg StrictHostKeyChecking=no --ssh-arg -o --ssh-arg UserKnownHostsFile=/dev/null <user>@<a-u2>`.
It parsed `MOSH CONNECT`, connected, prompt appeared within 5 s, echo/ls/vim worked. While the session ran there
was no `ssh` client process (only moche). After typing `exit` in the remote shell: exit code 0, no ssh
child left, server gone. With `Ctrl-^ .`: exit code 0, no ssh child, the `mosh-server` is left running
on the server (the stock client behaves identically: stock `Ctrl-^ .` also leaves the server running, still there 20 s later).

macOS-built moche on the lab host (arm64 Mach-O) against `moche-a` over `<a-vz>`, lima key via
`--ssh-arg -i ~/.lima/_config/user --ssh-arg -o --ssh-arg IdentitiesOnly=yes` (host-key options relaxed the same way):
first real macOS ssh bootstrap works: connect, echo, `ls --color`, vim, htop, `exit` (code 0, no ssh child) and
`Ctrl-^ .` (code 0, no ssh child).

* **E1 (note)**: the macOS run started the server with `LANG=en_XX.UTF-8` (taken from the Mac's environment: the server
  process list showed `-l LANG=en_XX.UTF-8`), the Linux run with `C.UTF-8`. The server accepted it and the session
  displayed fine, but a locale absent on the server may be a risk on other hosts. sigillo should pass its own
  `-l LANG=...` (the project plans `en_US.UTF-8`).

## 4. IMPAIRMENT (netem on `eth0` of both VMs, symmetric)

netem was applied on **both** VMs' `eth0` egress (so each direction gets the figure: delay 50 ms means 100 ms RTT;
loss 10% each way). One session per client kept for the whole series (the same session went through all profiles).
`echo` = time from `tmux send-keys -l tok` until `tok` appears in the pane (includes ~34 ms of tmux/poll overhead
seen in the unimpaired baseline). 8 samples each. `seq 1 2000` = time until the final marker is on screen.

| profile | client | echo latency (ms) | seq 1 2000 done (ms) | htop first screen (ms) | packets out / in |
|---|---|---|---|---|---|
| none | moche | 34-37 | 36 | 132 | 47 / 47 |
| none | stock | 34-37 | 33 | 134 | 48 / 50 |
| delay 50 | moche | 125-153 | 158 | 260 | 102 / 56 |
| delay 50 | stock | 120-137 | 132 | 260 | 83 / 58 |
| delay 200 | moche | 458-589 (med ~507) | 498 | 632 | 91 / 55 |
| delay 200 | stock | 416-511 (med ~450) | 465 | 653 | 73 / 58 |
| loss 10 | moche | 33-41 | 34 | 127 | 56 / 53 |
| loss 10 | stock | 34-129 (two hits) | 227 | 124 | 50 / 54 |
| loss 30 + delay 100 | moche | 273-937 (median ~370) | 270 | 451 | 133 / 67 |
| loss 30 + delay 100 | stock | 231-1482 (median ~600) | 599 | 1182 | 109 / 90 |

Verdict: the session stayed fully usable in every profile for both clients; no "connection lost" status appeared
(`status-line-lines: 0` in all), `seq 1 2000` completed with the last line on screen, htop drew. Latency tracks
RTT (no local prediction in either: moche v1 by design; stock with prediction not engaged on a LAN at these values).
Loss samples are small (8 per cell): do not read the 10% / 30% rows as a ranking.

* **I1 (observation)**: packets sent by moche (client to server) were consistently higher than stock under delay: 102 vs 83
  (+23%) at 50 ms, 91 vs 73 (+25%) at 200 ms, 133 vs 109 at loss 30. Counts at the client's OUTPUT chain over about
  60 s each. Same order of magnitude, no failure; possibly a slightly more eager resend or ack policy.
  Worth a look at the ack/resend timing in `moche-core` before iOS (cellular data), not urgent.

Not verified: reorder and duplication (netem `reorder`/`duplicate`), larger loss, asymmetric impairment, MTU (large screen
diffs with fragmentation under loss were exercised only through `seq 2000` and htop).

## 5. BLACKOUT: pass

Method: counter loop on the server (`tickN` each second), `iptables -I OUTPUT/INPUT -p udp ... -j DROP` for the
server's address and port on the client VM, then removed. (Note: `-j DROP` on OUTPUT makes the client's `sendto` fail with
EPERM; a drop on the server side would be silent. Both clients handled it.)

| | moche | stock |
|---|---|---|
| 20 s blackout (rules held 25 s): status appears | `moche: connection lost, last contact 7 s ago` first seen at t+6 s (threshold 5 s), counts up | `mosh: sendto: Operation not permitted (9 s without contact.)` / `Last contact N seconds ago` first seen at t+11 s |
| status cleared + screen caught up after link back | 1.05 s | 1.05 s |
| typing afterwards | works | works |
| 90 s blackout (rules held 95 s) | status all along, `last contact 95 s ago`; resumed 1.57 s after restore | status all along; resumed 0.52 s after restore |
| typing afterwards | works | works |

The stock client also opened a new UDP source port during the blackout (it rebinds after send errors); moche kept one socket
and one source port, which is fine (U17 passes without it, see 6). The server kept resending for the whole blackout
(~4 packets/s toward the dead path) in both cases.

Not verified: a 5+ minute blackout, blackout started in the middle of a large redraw.

## 6. ROAMING (U17): pass

Method: `iptables -t nat -I POSTROUTING -p udp -d <a-u2> --dport <port> -j SNAT --to-source ...` on the client VM,
tcpdump on `moche-a` (`tcpdump -ni eth0 -tt udp port <port>`), counter loop running, typing checked after each change.
Important technique note: the SNAT only applies to a *new* conntrack entry, and the server's continuous replies
recreate the old entry first, so a plain `conntrack -D` does not work (my first run showed no change on the wire and
was discarded). What worked: block the server's packets for 1.5 s, `conntrack -D`, type one key so the client creates
the new flow, then unblock.

Results, moche (stock identical in every row):

| step | wire seen on `moche-a` | session |
|---|---|---|
| source port only: `<b-u2>:50000-50100` | server replies moved to `<b-u2>:50084` | continued, no reconnect, typing ok |
| second port range `51000-51100` | server followed to `:51006` | continued |
| back to native source | server followed back to the original port | continued |
| source IP change: alias `<b-u2>+47` on `eth0`, SNAT to it | server replies moved to the new IP (same port) | continued, typing ok |
| replayed OLD datagram (captured earlier, 5 copies) sent from a third address | server did **not** retarget: no packet to the third address in the capture, replies kept going to the real client | continued; the 5 datagrams were ignored |

So the server follows after one authentic higher-sequence datagram and ignores a replayed one: U17 and the
negative case are confirmed for mosh-server 1.4.0. moche behaved exactly like the stock client in each step.

Not verified: replay of a datagram with a forged higher sequence (not craftable without the key), simultaneous
paths, IPv6.

## 6b. ROAMING by taking the NIC down (requested by the maintainer): pass

Method (on `moche-b`, run by a script inside the VM, control over `<b-vz>` ssh, watchdog `sleep 330; ip link set eth0 up; ...` armed
in the VM, lima0 never touched, an iptables DROP on lima0 for the user-v2 subnet so nothing could leak out of lima0
while `eth0` was down). Counter loop running; "resume" = time after the trigger until the screen shows the live tick again
and the status line is gone (0.5 s polling).

| step | moche | stock |
|---|---|---|
| (i) `ip link set eth0 down`, 20 s, `up`, same address | status `connection lost` from t+5 s; resumed 2.1 s after up (first run); 0.52 s in 3 repeats | `Last contact...` shown; resumed 1.06 s (first run), 0.53 s in repeats |
| (ii) down 20 s, `addr flush`, up with a NEW address `<b-u2>+74` + default route restored | resumed 1.05 s after up; wire: client packets from the new IP, server replies moved to it (19 packets) | resumed 1.05 s; wire: server followed (stock opened two new source ports) |
| (ii-b) roam back to the original address (networkd restored) | resumed 1.02 s; typing ok | 1.02 s; typing ok |
| (iii) flap: down 2 s, up | resumed 1.02 s | 1.03 s |

Repeated plain down/up, 7 rounds each (down for 20, 5, 20, 5, 3, 1, 60 s, same address), moche vs stock resume time after up:
0.52 / 0.52 / 0.54 / 1.05 / 0.53 / 0.01 / 2.61 s vs 0.53 / 0.53 / 0.53 / 0.54 / 0.52 / 0.02 / 3.15 s. No difference of substance (the
quantum is my 0.5 s poll; the single 1.05 s moche value is one poll step later). After 60 s of down time the server's
resend back-off dominates (2.6-3.1 s).

Socket question asked in the brief ("does moche keep sending from a stale address?"): **no problem found**. moche binds
`0.0.0.0:0`, an unconnected socket, so after the address changed the source IP simply changed (client packets seen from the new IP
with the same source port) and the server followed. After the move back, the server kept sending to the abandoned
address for about 50 packets until the first client packet from the original address arrived (same with stock); that
is the server waiting for the next authentic datagram, not a client defect. Caveat for the Mac app: the bind to the wildcard
address is what makes this work; do not switch to a `connect()`ed socket or to a socket bound to a specific interface address.

Not verified: interface going down while *moche is mid-send* with the socket error path (the EPERM/ENETUNREACH
handling was exercised, no crash, no busy loop seen), dual interfaces with a changing default route on iOS (that needs
the real device and the Network.framework wrapper).

## 7. Idle: pass

moche and stock side by side, 330 s idle, same server type, both stayed alive and answered a typed `echo` afterwards.

| | moche | stock |
|---|---|---|
| CPU time over 330 s | 0.47 s (0.14%) | 0.08 s (0.02%) |
| RSS | 2.5 MB | 14 MB |
| threads | 1 | not measured |
| voluntary context switches (whole lifetime, about 340 s) | 3345 (~10/s) | 209 (~0.6/s) |
| datagrams out / in over 330 s | 113 / 109 | 110 / 110 |

Heartbeats are working: about one datagram each way every 3 s for both clients (the stock numbers are the same).

* **W1**: moche wakes up about 10 times per second while idle (a poll with a ~100 ms timeout, probably the 100 ms `recv`/tick loop),
  against about 0.6 per second for the stock client. CPU is still negligible (0.14%), but on iOS the wake rate is
  what costs battery and it prevents deep idle. The sans-IO design already returns the next deadline: use it as the
  poll timeout instead of a fixed period (host-app side or CLI side).

## Bugs and findings, ranked

1. W1, idle wakeups (about 10/s vs 0.6/s). Not a bug of correctness; matters for iOS. Repro: run moche idle, read
   `voluntary_ctxt_switches` from `/proc/<pid>/status` twice 60 s apart.
2. I1, ~20-25% more client packets under delay than stock. Repro: scenario 4 `delay 50ms` on both ends, count packets with an
   iptables counter on OUTPUT. No functional impact seen.
3. D1, colours 8-15 re-encoded as aixterm 90-97/100-107 instead of 38;5;8-15. Cosmetic. Repro: server prints
   `printf '\033[38;5;8mx\033[0m'` and compare `tmux capture-pane -p -e`.
4. E1, the macOS build sends the Mac's locale (`en_XX.UTF-8` here) as `-l LANG=`. Works, but a server without that locale can fail.

No crash, no hang, no desync ("out of sync") and no connection failure in any scenario.

## Remains unverified

Predictive echo (none in v1), cursor position/visibility comparison, bell/title/mouse/paste, `less`, reorder/duplication
under netem, blackouts longer than 90 s, 24 h sessions, IPv6, long-running memory growth, behaviour on the real iOS stack
(Network.framework, app suspension), a server-side silent blackout (drop on the server instead of the client's OUTPUT),
mosh-server versions other than 1.4.0 (U12).

## Test bugs (mine, not moche's)

* The vim stage of the first differential run collided on vim's swap file (two servers, same file): rerun sequentially.
* "htop quit" timing in scenario 4 always read TIMEOUT: my pattern expected a trailing space after the prompt, which
  `capture-pane` trims. Ignore that line.
* The first roaming run never changed the source: SNAT needs a conntrack entry created by the client side (see 6).
  Phase markers in the roaming output are one step early for the same reason (the 1.5 s remap happens before the marker).

## Scripts

Not copied into the repo (they stay in `~/moche-vmlab` on the lab host, with the raw outputs `*.out`, tcpdump logs
`td.*.txt`; copy on request): `session.sh` (differential session, runs in `moche-b`), `fixtures.sh` (files on `moche-a`),
`meas.sh` + `imp.sh` (impairment), `blk.sh`, `roam.sh`, `rd.sh`, `rd2.sh`, `common.sh`, `idle.sh`, `boot3.sh`, `mac3.sh`,
`extra.sh`, `sum.py`, `drv.sh`, `lib.sh`. They use the lab's addresses via variables at the top of `lib.sh`.
