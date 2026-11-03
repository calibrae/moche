# moche observations, round 2 (stock mosh-server 1.4.0, lab container)

Date: 2026-11-03. Raw findings only; conclusions belong in `docs/protocol.md` (edited elsewhere).
Clean room: only captured bytes and our own docs were used.

## Method and caveats

- One container (`moche-mosh-lab`, Debian stable-slim, mosh 1.4.0, tmux, tcpdump, iproute2) on the
  colima docker host. Client and server both inside it, UDP over `lo`, server on 60000
  (`mosh-server new -s -c 256 -l LANG=en_US.UTF-8 -p 60000`, fake `SSH_CONNECTION`), client
  `mosh-client 127.0.0.1 60000` driven by `tmux send-keys`, key in `MOSH_KEY`. Fresh server and key per capture.
  Everything ran as root inside the container (no sshd this time; `tools/mosh-lab/capture.sh` is the driver,
  `colors.sh` / `misc.sh` the in-container helpers).
- `tcpdump -i lo -U -w`. **The tap sits after the netem qdisc.** Consequences: (a) packets that netem
  dropped never appear in the pcap (seen as holes in the wire counters, loss capture: 17 of 140 client
  datagrams and 21 of 160 server datagrams, 12%/13% for a 15% setting); (b) timestamps of netem'd
  captures are send time + one-way delay, for both directions. Gaps between consecutive packets of the
  same sender are unaffected by (b).
- netem applied on `lo` only inside the container netns, removed after every run (`qdisc_after` shows `noqueue`).
- Decoder: `tools/mosh-lab/analyze_pcap.py` (python + `cryptography` AESOCB3) was used on the docker host for
  all seven captures, because pulling 180 KB of binary through the tool channel was too costly. The Rust
  example `decrypt_pcap` was run locally on the two pulled captures (`misc`, `misc_kill`): 53/53 and 16/16
  tags verify, same results. Only `misc` and `misc_kill` pcaps are in
  `tools/mosh-lab/.state/captures/`; the other five pcaps (sha256 below) stay on the docker host in
  `~/moche-lab-tmp/out/<name>/` (cap.pcap, session.json, timeline.tsv, decoded.txt, inflated_all.bin).
  All seven `session.json` are local.

## Captures

| name | terminal | netem on lo | packets (C>S / S>C) | pcap bytes | sha256 (pcap) |
|---|---|---|---|---|---|
| frag | 250x80 | none | 73 (25 / 48) | 36860 | 97ad79ca...a699 |
| rtt0 | 100x30 | none | 253 (112 / 141) | 34499 | 40094f2a...fe15 |
| rtt50 | 100x30 | delay 25ms | 255 (113 / 142) | 35222 | 641f3d17...41b6 |
| rtt200 | 100x30 | delay 100ms | 201 (110 / 91) | 27937 | a9675d4f...b442 |
| loss | 100x30 | delay 25ms loss 15% | 262 (123 / 139) | 36658 | c562c79f...2c4 |
| misc | 100x30 | none | 53 (26 / 27) | 7984 | 2aadcac3...d822 |
| misc_kill | 100x30 | none | 16 (6 / 10) | 2381 | 8ae52911...ea07 |

Wire sizes (min/max/mean): frag C>S 62/92/73, S>C 68/1252/641; rtt0 S>C 62/259/82; loss S>C 63/294/87;
misc S>C 70/167/109. tcpdump reported 0 packets dropped by kernel in every run.
Session scripts: `frag`: `seq 1 20000`, 256-colour dump (random fg, then random fg+bg, 62 cells x 76 rows),
`htop` 3 s, `q`, `clear`, `exit`. `rtt*`: 10 s idle, `echo hello`, three bursts of 20 chars at ~115 ms spacing (measured
113-125 ms) each followed by C-u, `ls /usr/bin | head -40`, `exit`. `loss`: same typing, `ls --color`, `seq 1 300`, `clear`, 20 s idle, `exit`.

## A. Fragmentation (capture frag)

| Question | Finding | Evidence | Confidence |
|---|---|---|---|
| Fragment header layout | Unchanged from round 1: plaintext = ts u16, ts_reply u16, then 10 bytes (u64 BE id, u16 BE: bit 15 final, bits 0-14 fragment number), then payload | 19 packets with number != 0, 19 packets with final bit clear, i.e. exactly the non-last fragments of 5 messages; numbers run 0,1,2...; final bit set only on the last | High |
| Chunk size | 1214 bytes of payload per full fragment; datagram 1252 bytes = 8 nonce + 4 ts + 10 frag header + 1214 + 16 tag. 1252 + 28 (IP+UDP) = 1280 | sizes of all 19 non-final fragments `blen=1214`; last fragments 162, 470, 454, 454, 554 | High for 1214 on this build; the 1280 link is an inference (could also be a coincidence of an MTU constant minus overheads) |
| Fragment id | One id per sent instruction, shared by all its fragments; increments by 1 per instruction (ids 10..14 for five consecutive messages while wire counters go 9..32). In round 1 and here id advances on a resend too | #19-#42 | High (shared id), Medium (resend case, from rtt captures: resent (old,new) carry fresh ids) |
| Compress / fragment / encrypt order | Compress the whole serialized instruction once (one zlib stream), cut the compressed blob into 1214-byte pieces, wrap each piece in a header, encrypt each datagram on its own | the concatenated payloads of fragments 0..n inflate as one valid zlib stream (adler ok) for all 5 messages. Only fragment 0 starts with `78 9c`; fragments 1..n do not inflate alone | High |
| Reassembly | Fragments of one message are sent back to back with identical ts (same millisecond) and consecutive wire counters; no pacing between fragments | #22-#25 all `ts=56260`, ctr 12-15 | High (sender side only; receiver reordering handling not observed) |
| Message sizes | compressed 2590 / 4112 / 6524 / 10166 / 1768 bytes (3, 4, 6, 9, 2 fragments); inflated diff 9793 / 16040 / 25177 / 40377 / 6156 bytes | #19-#42 | High |
| Frame spacing under load | Five large messages sent 20 ms apart (t = 6.098, 6.118, 6.138, 6.159, 6.179) | | High: send interval floor ~20 ms at RTT ~0 |
| Max datagram length | **1252 bytes** (S>C). Largest C>S is 92 | | High |
| `seq 1 20000` | Never fragmented: mosh sends screen diffs, not output; datagrams stayed under 300 bytes | #9-#11 | High |

Side observation (diff base while a burst is in flight): in the colour dump the server diffed each new
state from the *previous state it had just sent* (old = 9, 10, 11, 12, 13 for new = 10..14), not from the
last state the client had acked (thr stayed 8, client's latest ack in C>S was 8). Whereas in the rtt50
capture (#16-#18, RTT 50 ms) the server kept old=5 (the acked state) for new = 7, 8, 9 sent 30-40 ms apart. Rule
for picking `old` is therefore not simply "last acked". Confidence: Medium (observation clear, rule unknown).

## B. Timing versus RTT (rtt0 / rtt50 / rtt200)

| Question | rtt0 | rtt50 | rtt200 | Confidence |
|---|---|---|---|---|
| Idle heartbeat gap, both directions | 3004-3010 ms | 2996-3017 ms | 3003-3015 ms | High: ~3 s regardless of RTT; both peers send an empty frame about every 3 s (the client's is the delayed ack of the server's) |
| Client ack delay after a server empty/heartbeat frame | | #6 S>C t=3.200, #7 C>S t=3.348 (148 ms; includes 25 ms netem on the return) | | Medium: ~100 ms delayed ack + netem |
| Server data frame -> next C>S packet (median / p90) | 89 / 108 ms | 87 / 135 ms | 102 / 106 ms | Medium: delayed ack ~100 ms, or earlier if the client has keystrokes to send |
| Re-send of unacked instruction (same old,new): gap | none occurred | median 32 ms (26-41), 11 gaps | median 105 ms (101-112), 11 gaps | High: gap tracks RTT/2 plus a few ms. Round 1 (RTT ~1 ms): 21-28 ms |
| Keystroke to first C>S datagram carrying it | median 10 ms (5-46) | median 40 ms (33-76) | median 147 ms (27-220) | Medium: includes netem delay in the capture. At RTT 200 a key waits for the next frame slot (up to ~105 ms) |
| C>S datagram with key -> next S>C with echo/data | median 10 ms | median 41 ms | median 26 ms (1-116) | Medium |
| Echo-ack only frame after a keystroke | see round 1 (~50-70 ms later) | present | present | Medium |

Reading: frame interval ~20 ms at RTT ~0, ~32 ms at RTT 50, ~105 ms at RTT 200, i.e. about RTT/2 plus 5-8 ms,
clamped at 20 ms below. An unacked instruction is simply re-sent at each frame slot until acked (up to 3
copies per key at RTT 50, each with a fresh fragment id and fresh wire counter). No exponential backoff seen in
any of these runs. The idle heartbeat does not scale with RTT.

## C. Loss (capture loss: delay 25 ms, loss 15%)

| Question | Finding | Evidence | Confidence |
|---|---|---|---|
| Loss visible as | holes in wire counters: C>S missing 7,22,25,29,30,33,43,51,59,64,65,80,83,90,99,113,116 (17/140); S>C missing 21 of 160 | decoded.txt | High |
| Re-send spacing | C>S same-(old,new) gaps: median 30 ms, min 25, p90 51, max 119; S>C median 36, min 29, p90 42, max 181 (one 5-resend run: 181, 36, 30, 36, 33) | | Medium: no separate RTO visible; the sender keeps re-sending each frame slot (~RTT/2+5 ms); rare larger gaps up to ~180 ms are probably lost sends or send-interval stretch |
| old_num after loss | Sender keeps using the last acked state as `old` and re-sends cumulative diffs: S>C #9 (old 2, new 4), #10 identical resend, #11 (old 2, new 5), #13 (old 2, new 5), then after the client's ack of 4 (#12) `old` jumps to 4 (#14 old 4 new 6) | #9-#15 | High |
| Skipped new_num | S>C #28 new=15 ctr 16, then #30 old=14 new=17 ctr 18; wire counter 17 missing: the lost datagram carried new=16 and the next send continues numbering, so a receiver sees `new` jump while `old` is still the acked state | #28-#30 | High |
| `thr` vs `old` | in every line of the first and last ~50 of the capture `thr == old` (no case old < thr) | | Medium (not exhaustively checked on all 262) |
| `old != previous new` | counts: loss C>S 41, S>C 73; no loss (rtt50) C>S 30, S>C 101. So that pattern is normal operation, not a loss marker | an3 output | High |
| Shutdown under loss | After `exit`: C>S exit (#256-#257, #259 resend), S>C new = 2^64-1 sent twice 32 ms apart (#260, #262) until client ack `ack = 2^64-1` (#261) | | High |
| Timestamp reply `0xffff` | present when no unreplied timestamp pending, as round 1 | | High |

## D. Misc

### Client SIGKILL (misc_kill)

Client killed at t = 2.04 s of the capture (after one echo). Server side for ~12 s afterwards:
empty frames at t = 5.117, 8.128, 11.134, 14.147 (gaps 3.011, 3.006, 3.013 s), `new` incrementing 7,8,9,10,
`old = ack_of_client = 6` frozen, `thr = 6`. No backoff, no extra traffic, no shutdown message. The
`mosh-server` process was still alive 12 s later (`pgrep`). Nothing is received from the dead client, so
the server's only reaction is the unchanged 3 s heartbeat. Confidence: High for 12 s; behaviour beyond 12 s
(server idle timeout, `MOSH_SERVER_NETWORK_TMOUT`) not tested.

### 256-colour / truecolor / attributes (frag, misc)

| Observation | Evidence | Confidence |
|---|---|---|
| 256-colour fg is emitted as `\e[0;38;5;Nm` with a leading `0;` reset in the same SGR for each run; fg+bg as `\e[0;38;5;F;48;5;Bm`; counts in frag: 4617 `38;5;N`, 2297 `48;5;N` | inflated_all.bin | High |
| Low colours are rewritten to classic codes when they fit: seen `\e[0;31;48;5;126m`, `\e[0;34...` (N < 8 becomes 3x) | frag | Medium |
| Truecolor passes through: `\e[0;38;2;10;200;30m` | misc #47 | High |
| Attributes: input `\e[1;4;7;9m` came out as `\e[0;1;4;7m`: bold/underline/reverse kept, **strikethrough (9) dropped**; italic `\e[3m` -> `\e[0;3m` | misc #47 | High (for 9) |
| Every SGR run starts with `0;` (full reset then set) | all | High |
| htop (alt screen app) used `\e[2;120H`, `\e[113X` (erase chars), `\e[K`, `\e[0;1;38;5;8m`, mouse enable `?1000h ?1006h` | frag #51-#56 | High |

### Wide chars, combining marks (misc)

| Observation | Evidence | Confidence |
|---|---|---|
| Input `日本語 😀 🎉 ＡＢＣ ñ` travels as raw UTF-8 in client keystroke bytes, and comes back as raw UTF-8 in host bytes (`\xe6\x97\xa5...`), no escape-encoded width | misc #6-#11 | High |
| Combining marks: `e` + U+0301 and `a` + U+0308 + U+0302 are emitted as base char followed by its combining bytes (not precomposed); Devanagari `न` + `ि` also as base + mark | misc #16 | High |
| No width hints on the wire; the receiver has to compute cell widths itself | | High |

### Mode toggles, bell, title, alt screen (misc)

| Observation | Evidence | Confidence |
|---|---|---|
| `?1000h` forwarded as `\e[?1000h` in its own frame (and echoed together with `\e[?2004l` of the command start); `?1006h` forwarded in its own frame | #23-#27 | High |
| Mouse reset: `?1000l` + `?1006l` from the app produced one frame with `\e[?1003l\e[?1002l\e[?1001l\e[?1000l\e[?1015l\e[?1006l` (all mouse modes, 1003 first) | #29 | High |
| Bracketed paste `\e[?2004h` / `\e[?2004l` each in its own frame | #31, #33 | High |
| BEL: one frame, host bytes `\x07` | #35 | High |
| Title: app sent OSC 0 `\e]0;moche title zero\a`; server forwards `\e]0;...\a`. After the app then sent OSC 2 the next frame carried `\e]1;moche title zero\a\e]2;moche title two\a` (icon name and title as separate sequences, re-sent) | #37, #39 | Medium |
| Alternate screen: `?1049h/l` **never appear** in any capture (0 hits in frag and misc). The server paints the alt-screen content on the primary screen with cursor addressing: `\e[?25l\e[2;1H\e[K\n\e[K\n\e[K\n\e[9X\e[5;10Halt screen` | misc #41, frag (htop) | High |
| Cursor visibility `\e[?25l` / `\e[?25h` each in its own frame | #43, #45 | High |

## Other observations

- Exit: server `new = 2^64-1` after `exit` in every capture; client acks it with `ack = 2^64-1` (frag #72-#73, misc #52).
- Chaff: 0-16 bytes everywhere, including fragmented messages (2-10) and heartbeats.
- ts_reply `0xffff`: roughly half of all packets, only when no unreplied timestamp.
- ts wrap seen once: frag, C>S ts 65384 -> 732 (65536 wrap) with normal operation; ts_reply handling unaffected. Confidence High (one occurrence).

## Still not settled

- Roaming (source address/port change): not possible on a single loopback pair; not attempted. Possible routes, untested: an nftables/iptables SNAT rule in the container netns rewriting the client's source port mid-session, or running the client from a second `lo` alias address and swapping NAT. Needs a container with nft/iptables.
- Receiver side of fragmentation (out-of-order or lost fragments, how long partial messages are kept).
- Exact RTO / send-interval constants (only the observed gaps: ~RTT/2+5-8 ms, floor ~20 ms, ceiling beyond 105 ms at RTT 200).
- Ack delay constants beyond "~100 ms delayed ack" and the 50 ms echo-ack pacing.
- Server behaviour beyond 12 s after client vanishes; version mismatch; bad tag handling.
- Rule used by the server to choose `old_num` (acked state vs previous in-flight state), see section A.
