# moche protocol notes (the contract)

Status: draft 2, 2026-11-03. Built from public descriptions plus one black-box capture of
a stock `mosh-server` 1.4.0 (source [9]). 9 of the original 16 open questions are now
resolved by that capture; 7 remain in section 8 (Unverified).

Every claim below is one of three kinds:

- **SOURCED**: stated by a public document, cited as `[n]` (table below).
- **OBSERVED**: confirmed on the capture [9], cited as `obs Un` (the item id in
  `docs/observations.md`). Caveat for all of them: the capture is ONE session of 45
  datagrams over loopback (client and server on one box, RTT about 1 ms, no loss, no
  fragmentation, no roaming). "Observed" means true for that session; it is not
  proof of behaviour under loss, high RTT, large screens or other server versions.
- **UNVERIFIED**: section 8. Put behind one named constant or function so changing it
  is a one-line fix.

## Sources

| Ref | Source | Licence / status |
|-----|--------|------------------|
| [1] | Winstein & Balakrishnan, "Mosh: An Interactive Remote Shell for Mobile Clients", USENIX ATC 2012. https://mosh.org/mosh-paper.pdf | paper |
| [2] | mosh.org home page and FAQ. https://mosh.org/ | public site |
| [3] | mosh-devel, "thoughts on mosh", May 2012. https://mailman.mit.edu/pipermail/mosh-devel/2012-May/000169.html | mailing list |
| [4] | mosh-devel, "multipath mosh", Nov 2014. https://mailman.mit.edu/pipermail/mosh-devel/2014-November/000935.html | list post about a fork's own design, NOT upstream wire format; hint only |
| [5] | mosh-devel, "Mosh and multi-path", Jul 2015. https://mailman.mit.edu/pipermail/mosh-devel/2015-July/001095.html | mailing list |
| [6] | `dart_mosh` 0.0.4, API reference pages only (no source read). https://pub.dev/packages/dart_mosh , https://pub.dev/documentation/dart_mosh/0.0.4/dart_mosh/ | Apache-2.0 (per pub.dev). Independent-implementation status is not stated by the page; we use it only for names and sizes that it documents, and treat each as "documented by [6]" |
| [7] | RFC 6298, Computing TCP's Retransmission Timer (cited by [1] as the RTT algorithm) | IETF |
| [8] | ECMA-48 / ISO 6429 (cited by [1] as the terminal language) | standard |
| [9] | Our own capture: stock `mosh-server` 1.4.0 from the Debian package, treated as a black box, run in a container on loopback; 45 UDP datagrams, one session, 2026-11-02. Findings in `docs/observations.md`. Lab tooling (container, capture, session script) lives in `tools/mosh-lab`; the decoder is `crates/moche-core/examples/decrypt_pcap.rs` | our own work |

Not consulted, by rule: any file of mosh's repository (src, .proto), any mirror or
fork of it, any other GPL implementation. The man pages were not fetched. The capture
was decoded with our own code and generic `protoc --decode_raw`.

## 1. Scope, clean-room statement, bootstrap

- moche is a client only. Hosts run the stock `mosh-server`. moche never runs a server.
- Clean room: the wire format is reconstructed from [1]-[6] and from observation of a
  real server [9]. Nothing is copied, translated or paraphrased from mosh's source.
  Every byte layout below is sourced, observed, or in section 8.
- Bootstrap (done by the host app, outside the core). The user logs in with ordinary
  means (SSH), an unprivileged server is started, it listens on a high UDP port and
  prints a random shared key; the SSH connection is then dropped and the client talks
  UDP only [1 s2.1, 2].
- The server prints a line of the form `MOSH CONNECT <port> <key>`, for example
  `MOSH CONNECT 60001 <22-char-key>`. The key is 22 printable characters [2]. A manual
  client takes it via the `MOSH_KEY` environment variable, plus server IP and port [2].
- Port: first free UDP port in a range around 60000-61000 (the usage text and FAQ
  disagree on the exact bounds: 60000-61000 vs 60001-60999) [2]. moche takes the port
  from the `MOSH CONNECT` line and never assumes a range.
- Key decoding (OBSERVED, obs U1): the 22 characters are unpadded standard base64 of
  the 128-bit key (16 bytes). Implemented as `Session::from_base64` in
  `crates/moche-core/src/crypto.rs`; it rejects other lengths and non-alphabet input.
  The 128-bit size also follows from AES-128 [1, 2].
- Server command the host runs (from moche's brief, not a protocol claim):
  `mosh-server new -s -c 256 -l LANG=en_US.UTF-8`.

## 2. Crypto

SOURCED:

| Claim | Source |
|-------|--------|
| Datagram layer prepends an incrementing sequence number, encrypts, sends ciphertext in a UDP datagram | [1 s2.2] |
| AES-128 in OCB mode, confidentiality + authenticity with one secret key | [1 s2.2] |
| It is OCB3 (not OCB2); the session key is 128-bit AES | [2 FAQ] |
| The first 64 bits of the UDP payload are an incrementing nonce, "the only thing you see in the clear" | [3] |
| Wire nonce is 8 bytes and is prepended to each packet; the AEAD (OCB) nonce is 12 bytes | [6] |
| The sequence number has a direction bit "masked out" of it | [6] |
| No replay cache or message history is needed: each datagram is an idempotent diff between numbered states | [1 s2.2] |

OBSERVED (obs U2, U3, U4; 45/45 datagrams verify, both directions):

    wire datagram = nonce8 (cleartext) || OCB3 ciphertext || tag16
    nonce12       = 00 00 00 00 || nonce8         (the 8 wire bytes unchanged)
    AEAD          = AES-128-OCB3, key = the 16 decoded bytes, tag 16 bytes, NO associated data

- Direction bit: the top bit (0x80 of byte 0) of the 8-byte big-endian wire nonce.
  Clear for client to server, set for server to client. The remaining 63 bits are the
  sequence number: +1 per datagram per direction, starting at 0 (obs U2: client 0..24,
  server 0..19).
- The direction bit is KEPT in the 12-byte AEAD nonce. Clearing it before building the
  nonce verifies only the client packets (25/45). So "masked out" [6] describes how the
  sequence number is read, not what goes into the nonce.
- Rejected layouts (obs U3): wire bytes first then 4 zero bytes, byte-reversed wire
  bytes, and any layout with a 12-byte tag: all fail (about chance, 1/45 or 0/45).
- Tag is the last 16 bytes of the datagram. An 8-byte tag was not tried (the `ocb3`
  crate does not expose that type); a 16-byte verification on every packet makes
  another length impossible (obs U4).
- Code: `build_nonce`, `split_wire_nonce`, `DIRECTION_BIT`, `TAG_LEN` in
  `crates/moche-core/src/crypto.rs`, with unit tests on two real captured packets
  (one per direction) and a wrong-direction-bit negative test.

Crates: RustCrypto `ocb3` + `aes`.

Receive rule (SOURCED [1 s2.2]): decrypt and authenticate; drop silently on failure.
Only authentic datagrams may influence state (roaming, acks, RTT). Bad-tag handling by
the server was not tested (section 8, U12).

## 3. Network (datagram) layer

Decrypted plaintext layout (OBSERVED, obs summary, U5, U6, U16; 45/45):

    ts        u16 big-endian   sender's millisecond clock, mod 65536
    ts_reply  u16 big-endian   0xffff = "no reply"
    frag_id   u64 big-endian   \  10-byte fragment header
    flags     u16 big-endian   /  bit 15 = final fragment, bits 0-14 = fragment number
    payload   zlib stream (RFC 1950, starts 78 9c) of one protobuf transport instruction

The timestamps are outside the instruction (matches [1 s2.2]). The fragment header sits
inside the encrypted plaintext, not in clear. Order of operations on send:
compress, then add the fragment header, then encrypt (obs U6; the fragment-then-encrypt
part is only inferred, see U6 in section 8).

Timestamps (SOURCED [1 s2.2]: every datagram carries a millisecond timestamp and an
optional "timestamp reply", the peer's latest timestamp, adjusted by the time held):

- OBSERVED (obs U16): width is 16 bits, unit milliseconds, wraps at 65536; `ts` tracks
  capture time (ts minus 1000*t stayed within 2374..2378 over 19 s). Client and server
  stamps agreed within 1-3 ms from packet 1, so the clock looks shared on one host;
  the epoch is irrelevant to moche because only differences are used.
- OBSERVED: `ts_reply` = the peer's latest `ts` plus the time it was held locally
  (e.g. server ts 2377, client reply 2481 after a 104 ms hold; hold 0 gives the same
  value back). Each received timestamp is echoed once; with none pending the field is
  `0xffff`, which happened in roughly half the packets.
- moche rule (derived): send `ts = now_ms mod 65536`; send `ts_reply = peer_ts + held_ms`
  (mod 65536) once per received timestamp, otherwise `0xffff`; on receipt,
  `rtt = (now16 - ts_reply) mod 65536` when `ts_reply != 0xffff`.
- Single-capture gaps: no timestamp wrap was captured, and what a peer does with a
  genuine value of 65535 is unknown.

RTT (SOURCED [1 s2.2]): TCP's algorithm [7] with three changes:
1. each datagram has a unique sequence number, so retransmissions are unambiguous;
2. the timestamp reply is adjusted by the time elapsed since the corresponding
   timestamp was received, so delayed acks do not bias the estimate;
3. the minimum retransmission timeout is 50 ms (not 1 s).
moche keeps SRTT and RTTVAR per [7] (alpha 1/8, beta 1/4, RTO = SRTT + 4*RTTVAR,
clamped below at 50 ms). The constants are RFC 6298's; the upper clamp is U9. See the
contradiction note under section 4 (Retransmit): the capture saw resends sooner than
50 ms apart.

Roaming (SOURCED [1 s2.2]): when the server receives an authentic datagram with a
sequence number greater than any before, it adopts that packet's source IP and UDP port
as its new target. For moche (client) this means: just keep sending from whatever socket
the host app has; no signalling is needed. Heartbeats (section 4) let the server learn
the new address even when the client has nothing to say [1 s2.3]. Host-app note: after a
network change, send promptly (a heartbeat or any frame) so the server re-targets. The
client must not send with a repeated or lower sequence number than before (follows from
the rule). Roaming was NOT exercised in the capture [9].

Fragmentation:

- SOURCED [6]: the sender splits a compressed transport instruction into fragments, each
  at most an MTU's worth of content; fragments carry a group id, a zero-based fragment
  number and a final-fragment flag; the header is 10 bytes before the content. Both a
  default send MTU and a receive MTU exist. Compression is zlib.
- OBSERVED, single-fragment only (obs U5): the 10-byte header layout above, with
  `flags = 0x8000` (final, number 0) on all 45 datagrams. The bit split of `flags` is
  inferred from that and from [6]; a non-final fragment was never seen. Still open: U5, U6.
- `frag_id` (obs U5): equals the wire sequence number + 1 on every datagram in both
  directions. The three resends of one instruction (obs, pkts #26-28) got new wire
  numbers AND new ids, so the id is not stable per instruction across resends. moche
  rule: use a counter that increases by one for each datagram built (this satisfies both
  readings consistent with the capture, and is safe on resend).
- Largest datagram seen: 443 bytes on the wire (an instruction inflating to 919 bytes).
  Nothing came near an MTU, so MTU and chunk size are open (U6).

Reassembly (moche behaviour, derived from the fields above): collect fragments by group
id; complete when the final fragment has arrived and fragment numbers 0..=final are all
present; concatenate, zlib-inflate, parse the instruction. Drop incomplete groups when a
newer group id completes or when a bounded buffer is exceeded (the limit is a moche
choice; [6] mentions a fragment assembly limit exists). Whether each fragment is
compressed on its own or the whole instruction is compressed once and then cut is
untested (U6); the rule above assumes the latter.

## 4. SSP: transport layer

SSP runs once per direction, over two object types: client to server, the history of
the user's input; server to client, the contents of the terminal window [1 s2].

Instruction: a self-contained message listing the source and target states and the
binary diff between them [1 s2.3]. Protobuf, same message both directions. Field names
from [6]; field numbers and wire types OBSERVED (obs U7, confirmed with
`protoc --decode_raw`; 45/45):

| Field no. | Wire type | Name | Meaning | Source of name |
|-----|-----------|------|---------|--------|
| 1 | varint | protocol version | always 2 (obs U8) | [6] |
| 2 | varint | old_num | state number the diff is computed against | [1, 6] |
| 3 | varint | new_num | state number the diff produces | [1, 6] |
| 4 | varint | ack_num | latest state of the peer's stream the sender has received | [6] |
| 5 | varint | throwaway_num | state number below which the receiver may discard states (see Throwaway) | [6] |
| 6 | bytes | diff | opaque payload, a user-message or host-message encoding (section 5); present with length 0 on empty frames (`32 00`) | [1, 6] |
| 7 | bytes | chaff | random padding, length 0 to 16 bytes, seen in both directions, on empty frames too | [6] |

Receiver rule: skip unknown fields, do not reject (section 5).

Semantics:

- A diff is idempotent: applying it to state `old_num` yields `new_num`. SSP can diff
  any two states, so the sender need not send every intermediate one [1 s2.2-2.3].
- Numbering (OBSERVED, obs U12/13/9 table): every frame takes a fresh `new_num`, even
  an empty-diff frame (empty frame old=3 new=4, next data frame old=3 new=5, i.e. the
  base stays the last ACKED state and `new_num` keeps rising). Server instructions diff
  from the last state the client acked (obs U14).
- Shutdown (SOURCED [6]: a reserved state number signals shutdown; OBSERVED, obs U10):
  `new_num = 2^64 - 1` (`0xffffffffffffffff`, a 10-byte varint, so `new_num` is a full
  64-bit field). Seen once, server to client, right after the shell exited; its diff
  carried the final screen repaint. The client acked it with `ack_num = 2^64 - 1`.
  moche must therefore treat `new_num == u64::MAX` as "peer closing", send that ack,
  and not try to store it as a normal state. Client-initiated shutdown and what a client
  should do when the server then goes silent were not observed.
- Protocol version (OBSERVED, obs U8): 2 in every instruction, both directions. moche
  sends 2. Server reaction to another value is untested (U12).
- Acks: the transport sender uses delayed acks, like TCP; a delay of 100 ms was enough
  to piggyback on host data in over 99.9% of cases [1 s2.3]. An ack travels in the
  `ack_num` of whatever instruction goes out next. OBSERVED (single session, loopback):
  the client sent a bare ack about 100 ms after the first unacked server frame, three
  times (obs U12 table).
- Throwaway (OBSERVED, obs U11 partial): `throwaway_num` = the highest number of the
  sender's OWN stream that the peer has acked (server `thr=5` after the client acked 5;
  client `thr=14` after the server acked 14). `old_num >= throwaway_num` in 45/45, with
  equality common. So the receiver must keep state `throwaway_num` itself (it is the
  base of in-flight diffs) and may drop states strictly below it. Inferred from no-loss
  data only: U11.
- Frame interval (SOURCED [1 s2.3, footnote 1]): minimum interval between frames is
  half the smoothed RTT, so about one instruction is in flight; capped at 50 Hz (at
  least 20 ms). A bound at high RTT is not given (U9).
- Collection interval (SOURCED [1 s2.3, s4]): after the application's first write, the
  server waits 8 ms to collect more output, and always waits at least the frame
  interval after the previous frame. OBSERVED loosely: the server answered a keystroke
  in about 8-9 ms (obs U12 table). Moche's analogue for user input is U12.
- Heartbeat (SOURCED: SSP sends an occasional heartbeat so the server learns of
  roaming, the client can warn the user when silent, and NAT mappings stay open; the
  interval is 3 seconds [1 s2.3; [2] "at least every three seconds"]). OBSERVED: server
  empty frames arrived about 2.9-2.95 s after the last traffic (obs U12 table).
- Echo ack (SOURCED [1 s3.2]): the server-side terminal object carries an "echo ack"
  for the latest keystroke presented to the app for at least 50 ms. OBSERVED (obs
  U14): it is the client's `new_num` of the keystroke state being echoed, and arrives
  in the host message (section 5), often in a separate small frame 55-70 ms after the
  data frame.
- Retransmit: the paper gives the RTO floor and the idempotent-diff design, which
  implies "resend the diff from the last state the peer acked" rather than resend a
  specific packet. OBSERVED (obs U12/13, one case): the client sent the same
  (old=12, new=14) instruction three times, with new wire sequence numbers, new
  `frag_id`s and different chaff length each time. The exact trigger and base: U13.

**Contradiction flagged (not overwritten): RTO floor.** [1] gives a 50 ms minimum
retransmission timeout, but the three resends above were 21 and 28 ms apart (loopback,
RTT about 1 ms). Possible explanations, none tested: 1.4.0's floor or its frame pacing
is lower than the paper's (the paper's own 20 ms frame-interval cap would fit 21-28
ms), or resends are paced by the frame interval rather than by the RTO. moche keeps 50
ms as its own floor (the conservative choice) and U9 asks for a `tc netem` run.

moche sender (v1), as the minimum that keeps a stock server happy:
1. Keep `sent_states`: the pending user-input states, numbered, with the last one the
   peer acked as `old_num`. Every frame, including an empty one, takes a new `new_num`.
2. When input exists and the frame interval has elapsed: send diff(old=acked, new=latest)
   with the current `ack_num`, `throwaway_num` and a random chaff length 0-16.
3. If nothing was sent for the heartbeat interval: send an empty-diff instruction
   (with a new `new_num`).
4. If an ack is owed and 100 ms passed with nothing to piggyback on: send an empty
   instruction carrying the ack.
5. If an instruction is unacked after RTO: resend (diff from the last acked state).
Steps 3-5 timings are from [1] and match the single capture; whether a server tolerates
deviations is untested (U12).

## 5. Payload messages (protobuf)

The `diff` bytes are protobuf messages [6: "encoded user or host message diff";
"decodes from protobuf bytes"]. The transport instruction itself is protobuf as well
[5, 6]. The protobuf subset moche needs is small (varints, length-delimited bytes), so
a hand-rolled codec is fine (no `prost` required), and unknown fields must be skipped,
not rejected.

Structure (OBSERVED, obs U7, U14; all numbers verified with `protoc --decode_raw`;
names are our labels, only the numbers are data). The `diff` field is a message holding
repeated field 1; each repetition is an element holding exactly one of:

| Element field | Meaning | Inner fields | Direction |
|---|---|---|---|
| 2 | bytes of input or output | 4 = bytes | C->S keystrokes; S->C host bytes |
| 3 | resize | 5 = width (varint), 6 = height (varint) | both directions |
| 7 | echo ack | 8 = varint, the client `new_num` echoed | S->C only |

Several elements may occur in one diff, in order (server: resize then bytes; echo ack
then bytes). Worked example, a client instruction inflated:
`0802 1003 1805 2003 2803 320e 0a0c 120a 2208 6563686f2068 690d 3a0c <12 chaff>`
= version 2, old 3, new 5, ack 3, throwaway 3, diff { element 2 { 4 = "echo hi\r" } },
chaff 12 bytes.

Client to server, "user message" [6]: keystrokes and resize, in order. The paper
states that for user input the diff contains every intervening keystroke [1 s2.3]; so a
user-message diff for old=A, new=B is the concatenation of every keystroke and resize
event between A and B, in order. OBSERVED: the first client frame carried a resize
(100x30) and a later one an 80x24 resize.

Server to client, "host message" [6]:

- Host bytes (OBSERVED, obs U14): RAW VT escape bytes, not a mosh-specific diff syntax.
  They are what the server's own terminal emulates as a repaint or delta: `ESC[r`,
  `ESC[0m`, `ESC[H`, `ESC[2J`, `ESC[K`, OSC title, SGR, scroll region, `\r\n`, BEL, etc.
  So the client feeds them to a VT interpreter that mirrors the server's, and the
  screen diff is "what the server's emulator would write", never the application's own
  escapes (see section 6).
- Resize (OBSERVED, obs U14): the server DOES send a resize element. It is the first
  element of the first message (100x30) and of the first message after a client
  resize (80x24, followed by `ESC[r ESC[0m ESC[H ESC[2J` and a full repaint). This is
  how the client learns the server's screen size. [6] had documented resize only for
  the client message; the server direction is a capture finding, not sourced.
- Echo ack (S->C): element 7, see section 4.

## 6. Screen model

Sourced facts:

- Both ends keep the screen; the server contains a terminal emulator and synchronizes
  screen state, not an octet stream [1 s1, s3].
- For screen states, the diff "is only the minimal message that transforms the
  client's frame to the current one" [1 s2.3]. The receiver therefore applies the
  diff to the stored state named `old_num` and obtains `new_num`.
- The emulator implements the subset of ECMA-48 used by typical terminal emulators
  (xterm, gnome-terminal, Terminal.app, PuTTY): cursor motion, bold and colour
  rendering, erase [1 s3.1; 8]. Mosh itself has no scrollback [1 s2].

Observed (obs U14, U15; single session):

- State 0 is a blank screen. The first screen arrives as old=0, new=1.
- The server diffs from the last state the client acked, so `old_num` is usually an
  earlier state than the previous `new_num`.
- A client resize makes the server answer with a resize element then a full clear
  (`ESC[r ESC[0m ESC[H ESC[2J`) and a complete repaint (902 VT bytes for 80x24). The
  framebuffer must be resized on the resize element, then repainted from the bytes.
- Full-screen applications are NOT passed through. `vim` was drawn on the PRIMARY
  screen with scroll-region repaints (e.g. `ESC[1;29r`), with no `?1049` (alternate
  screen) anywhere in the 45 inflated instructions. This contradicts the earlier
  expectation that alternate screen would appear; moche needs the sequences seen in
  the data, not the ones a normal terminal would see. Alternate-screen support stays
  a "maybe", to be driven by recordings (U15).
- Mode toggles in a repaint were seen: `?25l/h`, `?2004h/l`, `?1015l`, `?1006l`,
  `?1005l`, `?1004l`.

moche model (design, to be validated against recordings, section 7):

- Keep `states: BTreeMap<u64, Framebuffer>`, starting with state 0 = blank screen of
  the initial size.
- On an authentic instruction with `new_num > latest`: look up `old_num`. If missing,
  drop the instruction (the server will retry against an acked base). Else clone it,
  apply `diff` through the VT interpreter, store as `new_num`, update `ack_num` to
  send, and mark the screen dirty. `new_num == u64::MAX` is shutdown, see section 4.
- Discard: when an instruction arrives with `throwaway_num = t`, drop all stored
  states with number < t, always keeping the latest. Strictly-below is observed-consistent
  but loss-untested (U11).
- Screen handed to the host: full repaint (clear, draw all cells, set cursor and
  modes) generated from the current framebuffer. Chosen for v1 because the host
  (SwiftTerm) only needs bytes, and it is stateless and robust against skipped
  states. A minimal diff against the last painted frame is a later optimisation.
- Predictive local echo: out of v1; the framebuffer is kept separate from any
  overlay so it can be added.

VT subset: U15. Seen so far, in this capture only: `ESC[r`, `ESC[0m`, `ESC[H`, `ESC[2J`,
`ESC[K`, `ESC[?25l/h`, `ESC[?2004l`, `ESC[1;29r`, cursor positions `ESC[29;1H` style,
SGR `ESC[0;1;32m` / `;36m` / `;34m`, OSC 0 title with BEL, BEL, `\r\n`. No `38;5;n`
(256-colour), no wide characters, no combining marks. Do not assume it equals xterm;
build the interpreter from recordings.

## 7. Test plan

All vectors are captured from a real `mosh-server` run in a container (lab tooling:
`tools/mosh-lab`), and checked into `tests/vectors/` with no real hostnames, IPs or
user data.

Status after [9]: items 1, 2 (primary) and 4 (field numbers, version, chaff, shutdown)
are done for one loopback session; the rest are open. Two real packets are already unit
tests in `crypto.rs`.

1. Handshake log: run `mosh-server new -s -c 256 -l LANG=en_US.UTF-8` via ssh, store
   the `MOSH CONNECT` line with a throwaway key (fresh key per recording, so the
   key in the repo is worthless).
2. Crypto known-answer: capture datagrams with `tcpdump -w`. With the known key,
   decrypt and check the tag verifies, for 20+ packets in both directions. Done once
   (45 packets). Also add an independent OCB3 vector test of the primitive (RFC 7253
   appendix vectors) to separate "my OCB is wrong" from "my nonce is wrong".
3. Fragment vectors: force a large screen update (`cat` of a big file, `htop` at a
   big size, or a colour dump on a 250x80 window) so the server fragments. Record header
   bytes, final flag, fragment numbers, MTU, the plaintext length before and after zlib.
   Settles U5, U6.
4. Instruction vectors: decrypt, inflate, hex-dump the protobuf, and decode with
   `protoc --decode_raw`. Record protocol version, state numbers over a session, ack
   and throwaway progression, chaff lengths, shutdown numbers (send `exit` in the
   shell). Done once; repeat with loss.
5. Timing: from the capture, tabulate server send intervals vs measured RTT (use
   `tc netem` for 0/50/200 ms), heartbeat spacing on an idle session, ack delay, and
   the echo-ack lag. Compare with sections 3-4 numbers. Settles U9, U12, U13.
6. Roaming: change the client source port/address mid-session; confirm the server
   follows after one authentic higher-sequence datagram. Test also a replayed
   old-sequence packet from the new address (must not re-target).
7. VT recordings: for each scenario, store the sequence `(old_num, new_num, diff
   bytes)` plus the final screen as a text dump of cells/attributes/cursor. Scenarios:
   shell prompt; `ls --color`; `vim` (scroll region); `htop`; `less`; wide characters,
   emoji, combining marks; line wrap at the right edge; resize during output; `reset`;
   bell; title; mouse mode toggles; bracketed paste toggles. Run them against the
   interpreter and compare to a reference: the same byte stream through a standard
   terminal emulator (any permissive-licensed headless one) is only a sanity check,
   since the server's diff output is what moche must accept. Settles U15.
8. Skipped states: record a session with `tc netem` loss, so `old_num` is not the
   previous `new_num`; unit-test the state map and throwaway behaviour. Settles U11, U13.
9. Negative tests: bad tag, truncated packet, duplicated fragment, missing final
   fragment, unknown protobuf fields, instruction with unknown `old_num`, version
   mismatch.

## 8. Unverified: confirm by observing a real mosh-server

Resolved by capture [9] and moved into sections 1-6: U1, U2, U3, U4, U7, U8, U10, U14,
U16 (marked RESOLVED (observed); the evidence is in `docs/observations.md` under the
same ids). Ids below are stable and not renumbered. Each open item blocks something
specific; keep it behind one named constant or function.

| # | Status and what is still open | Test still needed |
|---|-------|-------------|
| U5 | PARTIAL. Layout seen: 10 bytes = u64 BE id + u16 BE flags, top bit final, low 15 bits number (sec. 3). Only single-fragment messages exist in the capture (flags always `0x8000`), so the bit split, maximum fragment number, reassembly order and whether ids relate across fragments of one instruction are inferred, not exercised. | Capture a fragmented update (test 3: large screen, big `cat`, or a smaller path MTU) and check non-final flags, numbering from 0, shared id across fragments. |
| U6 | PARTIAL. Known: zlib framing (RFC 1950, `78 9c`), header inside the encrypted plaintext, zlib immediately after the header. Order compress-then-fragment-then-encrypt is INFERRED (never fragmented). Open: whether each fragment is compressed separately, default send MTU, receive MTU, chunk size (largest datagram was 443 bytes). | Same fragmented capture: inflate a reassembled group vs inflate each fragment; read datagram sizes at the split. |
| U9 | OPEN. RTO upper clamp; clamps of the frame interval at very low or very high RTT; the client's actual ack delay at non-trivial RTT. Also see the 50 ms floor contradiction in sec. 4 (resends 21-28 ms apart on loopback). | Timing capture under `tc netem` at 0/50/200 ms (test 5); tabulate resend gaps vs SRTT. |
| U11 | PARTIAL. Observed: throwaway = highest own-stream number the peer acked, `old_num >= throwaway` always, so receiver keeps state `throwaway_num` and drops strictly below (sec. 4). No loss case seen; "must the receiver retain the base of any in-flight diff beyond that" is unproven. | Loss run (test 8): confirm `old_num >= throwaway` still holds and a discard-below rule never loses a needed base. |
| U12 | OPEN. Minimum a stock server tolerates from a client: ack and heartbeat timings, whether chaff is required, what the server does with a client that sends no heartbeat, with a wrong protocol version, or with a bad tag. Single-session timings (ack ~100 ms, heartbeat ~3 s) are consistent with [1] but prove nothing about tolerance. | Run the CLI with timings off by 2x and with chaff omitted; watch for server timeouts or drops. Send a modified version field. |
| U13 | PARTIAL. One resend case: same (old=12, new=14) three times, new wire numbers and ids, different chaff. Trigger rule and diff base (last acked vs last sent) under real loss are open. | Capture with loss; inspect `old_num` on resends and the gap vs RTO. |
| U15 | PARTIAL. Escape sequences seen are listed in sec. 6 (one session, basic SGR, no 256-colour, no alternate screen, no wide characters or combining marks, no mouse modes beyond reset). The full set the server emits is unknown. | Collect all distinct sequences across the test 7 scenarios and generate the interpreter's coverage list from the data. |

Residual gaps on RESOLVED items (all single-capture; none blocks the first screen):

- U10: client-initiated shutdown, and client behaviour after the server's final
  frame, not observed.
- U16: no timestamp wrap captured; a real timestamp value of 65535 vs the `0xffff`
  "none" marker is not disambiguated; cross-host clock epoch is not observable.
- U8: server reaction to a version mismatch is untested (covered by U12's test).
- U2-U4: only server 1.4.0 on loopback; a different server version could in principle
  differ. 8-byte tag length was not tried.

Count: 7 items open or partial (U5, U6, U9, U11, U12, U13, U15); 9 resolved by
observation (U1, U2, U3, U4, U7, U8, U10, U14, U16). The byte layouts that block the
first handshake (U1-U5, U7, U16) are now all settled except the multi-fragment part of
U5; the rest affect robustness and tuning, not the first screen.
