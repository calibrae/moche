# moche protocol notes (the contract)

Status: draft 1, 2026-11-02. Built only from public descriptions. Where a public
source does not exist, the claim is in section 8 (Unverified) and must be settled by
observing a stock `mosh-server` before the code relies on it.

## Sources

| Ref | Source | Licence / status |
|-----|--------|------------------|
| [1] | Winstein & Balakrishnan, "Mosh: An Interactive Remote Shell for Mobile Clients", USENIX ATC 2012. https://mosh.org/mosh-paper.pdf | paper |
| [2] | mosh.org home page and FAQ. https://mosh.org/ | public site |
| [3] | mosh-devel, "thoughts on mosh", May 2012. https://mailman.mit.edu/pipermail/mosh-devel/2012-May/000169.html | mailing list |
| [4] | mosh-devel, "multipath mosh", Nov 2014. https://mailman.mit.edu/pipermail/mosh-devel/2014-November/000935.html | list post about a fork's own design, NOT upstream wire format; hint only |
| [5] | mosh-devel, "Mosh and multi-path", Jul 2015. https://mailman.mit.edu/pipermail/mosh-devel/2015-July/001095.html | mailing list |
| [6] | `dart_mosh` 0.0.4, API reference pages only (no source read). https://pub.dev/packages/dart_mosh , https://pub.dev/documentation/dart_mosh/0.0.4/dart_mosh/ | Apache-2.0 (per pub.dev). Independent-implementation status is not stated by the page; we use it only for names and sizes that it documents, and treat each as "documented by [6]", to be confirmed by capture |
| [7] | RFC 6298, Computing TCP's Retransmission Timer (cited by [1] as the RTT algorithm) | IETF |
| [8] | ECMA-48 / ISO 6429 (cited by [1] as the terminal language) | standard |

Not consulted, by rule: any file of mosh's repository (src, .proto), any mirror or
fork of it, any other GPL implementation. The man pages were not fetched in this pass.

## 1. Scope, clean-room statement, bootstrap

- moche is a client only. Hosts run the stock `mosh-server`. moche never runs a server.
- Clean room: the wire format is reconstructed from [1]-[6] and from observation of a
  real server (section 7). Nothing is copied, translated or paraphrased from mosh's
  source. Every byte layout below is either sourced or in section 8.
- Bootstrap (done by the host app, outside the core). The user logs in with ordinary
  means (SSH), an unprivileged server is started, it listens on a high UDP port and
  prints a random shared key; the SSH connection is then dropped and the client talks
  UDP only [1 s2.1, 2].
- The server prints a line of the form `MOSH CONNECT <port> <key>`; the example on [2]
  is `MOSH CONNECT 60004 4NeCCgvZFe2RnPgrcU1PQw`. The key is 22 printable characters
  [2]. A manual client takes it via the `MOSH_KEY` environment variable, plus server
  IP and port [2].
- Port: first free UDP port in a range around 60000-61000 (the usage text and FAQ
  disagree on the exact bounds: 60000-61000 vs 60001-60999) [2]. moche takes the port
  from the `MOSH CONNECT` line and never assumes a range.
- Key decoding: 22 characters = unpadded base64 of a 128-bit key (16 bytes encode to
  22 base64 characters plus 2 pad characters). The 128-bit size follows from AES-128
  [1, 2]; the unpadded-base64 reading is an inference, confirm in section 8 (U1).
- Server command the host runs (from moche's brief, not a protocol claim):
  `mosh-server new -s -c 256 -l LANG=en_US.UTF-8`.

## 2. Crypto

| Claim | Source |
|-------|--------|
| Datagram layer prepends an incrementing sequence number, encrypts, sends ciphertext in a UDP datagram | [1 s2.2] |
| AES-128 in OCB mode, confidentiality + authenticity with one secret key | [1 s2.2] |
| It is OCB3 (not OCB2); the session key is 128-bit AES | [2 FAQ] |
| The first 64 bits of the UDP payload are an incrementing nonce, "the only thing you see in the clear" | [3] |
| Wire nonce is 8 bytes and is prepended to each packet | [6] |
| The AEAD (OCB) nonce is 12 bytes | [6] |
| The sequence number has a direction bit "masked out" of it | [6] |
| A client-to-server nonce is built from a sequence value | [6] |
| No replay cache or message history is needed: each datagram is an idempotent diff between numbered states | [1 s2.2] |

Packet on the wire = `nonce(8 bytes, cleartext) || ciphertext || tag`. The 8-byte
nonce is what is sent; the 12-byte OCB nonce is derived (implied), not transmitted.
Exact derivation, the position of the direction bit and the tag length are in
section 8 (U2-U4). moche's core must treat them as parameters in one small module
until confirmed.

Crates: RustCrypto `ocb3` + `aes`. The AES key is the decoded 16 bytes.

Receive rule: decrypt and authenticate; drop silently on failure. Only authentic
datagrams may influence state (roaming, acks, RTT) [1 s2.2].

## 3. Network (datagram) layer

Sourced behaviour:

- Every outgoing datagram carries a millisecond timestamp and an optional "timestamp
  reply", the most recently received timestamp from the peer [1 s2.2].
- RTT: TCP's algorithm [7] with three changes [1 s2.2]:
  1. each datagram has a unique sequence number, so retransmissions are unambiguous;
  2. the timestamp reply is adjusted by the time elapsed since the corresponding
     timestamp was received, so delayed acks do not bias the estimate;
  3. the minimum retransmission timeout is 50 ms (not 1 s).
  moche keeps SRTT and RTTVAR per [7] (alpha 1/8, beta 1/4, RTO = SRTT + 4*RTTVAR,
  clamped below at 50 ms). The constants are RFC 6298's; the upper clamp is U9.
- Roaming: when the server receives an authentic datagram with a sequence number
  greater than any before, it adopts that packet's source IP and UDP port as its new
  target [1 s2.2]. For moche (client) this means: just keep sending from whatever
  socket the host app has; no signalling is needed. Heartbeats (section 4) let the
  server learn the new address even when the client has nothing to say [1 s2.3].
  Host-app note: after a network change, send promptly (a heartbeat or any frame) so
  the server re-targets.
- The client must not send with a repeated or lower sequence number than before,
  otherwise the server will not re-target (follows from the rule above).
- Fragmentation: the sender splits a compressed transport instruction into
  fragments, each at most an MTU's worth of content; fragments carry a group id, a
  zero-based fragment number and a final-fragment flag; the header is 10 bytes
  before the content [6]. Compression is zlib [6].
- MTU: both a default send MTU and a receive MTU exist [6]; values in U6.

Byte layout of the 10-byte fragment header, and the order
(compress -> fragment -> encrypt) are in section 8 (U5, U6). Working shape to test:
an 8-byte group id, then 2 bytes holding the final flag and the fragment number,
big-endian. This is a hypothesis, not a contract.

Reassembly (moche behaviour, derived from the fields above): collect fragments by
group id; complete when the final fragment has arrived and fragment numbers
0..=final are all present; concatenate, zlib-inflate, parse the instruction. Drop
incomplete groups when a newer group id completes or when a bounded buffer is
exceeded (the limit is a moche choice; [6] mentions a fragment assembly limit exists).

## 4. SSP: transport layer

SSP runs once per direction, over two object types: client to server, the history of
the user's input; server to client, the contents of the terminal window [1 s2].

Instruction: a self-contained message listing the source and target states and the
binary diff between them [1 s2.3]. Fields documented by [6] (names only; their
protobuf field numbers are in U7):

| Field | Meaning | Source |
|-------|---------|--------|
| protocol version | integer carried in every instruction; value in U8 | [6] |
| old_num | state number the diff is computed against (source state) | [1, 6] |
| new_num | state number the diff produces (target) | [1, 6] |
| ack_num | latest state of the peer's stream that the sender has received | [6] |
| throwaway_num | state number up to which the sender says the receiver may discard older states | [6] |
| diff | opaque payload, a user-message or host-message encoding | [1, 6] |
| chaff | random padding bytes | [6] |

Semantics, with sources:

- A diff is idempotent: applying it to state `old_num` yields `new_num`. SSP can
  diff any two states, so the sender need not send every intermediate one [1 s2.2-2.3].
- A reserved state number signals that the peer is shutting down [6]. Value in U10.
- Acks: the transport sender uses delayed acks, like TCP; a delay of 100 ms was enough
  to piggyback on host data in over 99.9% of cases [1 s2.3]. An ack travels in the
  `ack_num` of whatever instruction goes out next.
- throwaway_num: lets the peer free stored states [6, and 1 "numbered source and
  target state"]. Exact rule in U11.
- Frame interval: minimum interval between frames is half the smoothed RTT, so about
  one instruction is in flight; capped at 50 Hz (i.e. at least 20 ms) [1 s2.3 and
  footnote 1]. A lower bound on the interval at high RTT is not given (U9).
- Collection interval: after the application's first write, the server waits 8 ms to
  collect more output, and always waits at least the frame interval after the
  previous frame [1 s2.3, s4]. (Server rule; moche's analogue for user input is U12.)
- Heartbeat: SSP sends an occasional heartbeat so the server learns of roaming, the
  client can warn the user when silent, and NAT mappings stay open. The interval is 3
  seconds [1 s2.3; [2] says "at least every three seconds"].
- Echo ack: the server-side terminal object carries an "echo ack" for the latest
  keystroke presented to the app for at least 50 ms [1 s3.2]. Delivered in the host
  message (section 5).
- Retransmit: the paper gives the RTO floor (50 ms, section 3) and the idempotent-diff
  design, which implies "resend the diff from the last state the peer acked" rather
  than resend a specific packet. The exact retransmit trigger and diff base are U13.

moche sender (v1), as the minimum that keeps a stock server happy:
1. Keep `sent_states`: the pending user-input states, numbered, with the last one the
   peer acked as `old_num`.
2. When input exists and the frame interval has elapsed: send diff(old=acked, new=latest)
   with the current `ack_num`, `throwaway_num` and random chaff length.
3. If nothing was sent for the heartbeat interval: send an empty-diff instruction.
4. If an ack is owed and 100 ms passed with nothing to piggyback on: send an empty
   instruction carrying the ack.
5. If an instruction is unacked after RTO: resend.
Steps 3-5 timings are from [1]; whether a server tolerates deviations is untested (U12).

## 5. Payload messages (protobuf)

The `diff` bytes are protobuf messages [6: "encoded user or host message diff";
"decodes from protobuf bytes"]. The transport instruction itself is protobuf as well
[5, 6]. Public descriptions give names and meanings, not field numbers.

Client to server, "user message": a message holding one or more instructions, encoded
in order [6]. Instruction kinds documented by [6]: keystroke (bytes typed) and resize
(width, height). The paper states that for user input the diff contains every
intervening keystroke [1 s2.3]. So a user-message diff for old=A, new=B is the
concatenation of every keystroke and resize event between A and B, in order.

Server to client, "host message": terminal output bytes from the server ("host
bytes"), plus an optional echo-ack number [6]; the paper also mentions an "echo ack"
field in the synchronized terminal object [1 s3.2]. [6] documents resize only for the client message; whether the server sends one is U14.

Field numbers, wire types, the names of the instruction wrappers and how the
host-bytes payload relates to the screen diff: all in U7 and U14. The protobuf subset
moche needs is small (varints, length-delimited bytes), so a hand-rolled codec is
fine (no `prost` required), and unknown fields must be skipped, not rejected.

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
- Because states can be skipped, the server can ack/throw away states; the client
  keeps a numbered set of framebuffers.

moche model (design, to be validated against recordings, section 7):

- Keep `states: BTreeMap<u64, Framebuffer>`, starting with state 0 = blank screen of
  the initial size.
- On an authentic instruction with `new_num > latest`: look up `old_num`. If missing,
  drop the instruction (the server will retry against an acked base). Else clone it,
  apply `diff` through the VT interpreter, store as `new_num`, update `ack_num` to
  send, and mark the screen dirty.
- Discard: when an instruction arrives with `throwaway_num = t`, drop all stored
  states with number < t (U11 confirms the comparison), always keeping the latest.
- Screen handed to the host: full repaint (clear, draw all cells, set cursor and
  modes) generated from the current framebuffer. Chosen for v1 because the host
  (SwiftTerm) only needs bytes, and it is stateless and robust against skipped
  states. A minimal diff against the last painted frame is a later optimisation.
- Predictive local echo: out of v1; the framebuffer is kept separate from any
  overlay so it can be added.

VT subset: U15. Do not assume it equals xterm; build the interpreter from recordings.
Expect at least cursor movement, erase in line and display, SGR (including 256
colours, since the server is started with `-c 256`), scroll regions, alternate
screen, UTF-8 with wide characters and combining marks.

## 7. Test plan

All vectors are captured from a real `mosh-server` run in a container, and checked
into `tests/vectors/` with no real hostnames, IPs or user data.

1. Handshake log: run `mosh-server new -s -c 256 -l LANG=en_US.UTF-8` via ssh, store
   the `MOSH CONNECT` line with a throwaway key (fresh key per recording, so the
   key in the repo is worthless).
2. Crypto known-answer: capture datagrams with `tcpdump -w`. With the known key,
   decrypt with a candidate nonce layout and check the tag verifies. Record, for each
   of 20+ packets in both directions: the 8 nonce bytes, the 12-byte AEAD nonce that
   works, the plaintext. This settles U2-U4. Also an independent OCB3 vector test of
   the primitive (RFC 7253 appendix vectors) to separate "my OCB is wrong" from "my
   nonce is wrong".
3. Fragment vectors: force a large screen update (`cat` of a big file, `htop` at a
   big size, or `-c 256` colour dump) so the server fragments. Record header bytes,
   final flag, fragment numbers, MTU, the plaintext length before and after zlib.
4. Instruction vectors: decrypt, inflate, hex-dump the protobuf, and decode with
   `protoc --decode_raw` (a generic tool, no mosh .proto) to read the field numbers
   and wire types. Record protocol version, state numbers over a session, ack and
   throwaway progression, chaff lengths, shutdown numbers (send `exit` in the shell).
5. Timing: from the capture, tabulate server send intervals vs measured RTT (use
   `tc netem` for 0/50/200 ms), heartbeat spacing on an idle session, ack delay, and
   the echo-ack lag. Compare with sections 3-4 numbers.
6. Roaming: change the client source port/address mid-session; confirm the server
   follows after one authentic higher-sequence datagram. Test also a replayed
   old-sequence packet from the new address (must not re-target).
7. VT recordings: for each scenario, store the sequence `(old_num, new_num, diff
   bytes)` plus the final screen as a text dump of cells/attributes/cursor. Scenarios:
   shell prompt; `ls --color`; `vim` (alternate screen, scroll region); `htop`;
   `less`; wide characters, emoji, combining marks; line wrap at the right edge;
   resize during output; `reset`; bell; title; mouse mode toggles; bracketed paste
   toggles. Run them against the interpreter and compare to a reference: the same
   byte stream through a standard terminal emulator (e.g. a headless xterm-compatible
   one, any permissive-licensed) is only a sanity check, since the server's diff
   output is what moche must accept.
8. Skipped states: record a session with `tc netem` loss, so `old_num` is not the
   previous `new_num`; unit-test the state map and throwaway behaviour.
9. Negative tests: bad tag, truncated packet, duplicated fragment, missing final
   fragment, unknown protobuf fields, instruction with unknown `old_num`.

## 8. Unverified: confirm by observing a real mosh-server

Each item blocks something specific. Until settled, put it behind one named constant
or function so changing it is a one-line fix.

| # | Question | How to test |
|---|----------|-------------|
| U1 | Is the key unpadded standard base64 of 16 bytes (22 chars)? Any alphabet quirk? | Decode the printed key; check length 16 and that it decrypts a captured datagram (test 2). |
| U2 | Position of the direction bit in the 64-bit nonce, and which direction sets it. [4] describes a fork's layout (1 direction bit, 15-bit flow id, 48-bit seq); upstream is not confirmed, though [6] says the bit is "masked out" of the sequence. Working guess: top bit, set for server-to-client. | Capture both directions; the top bit of byte 0 should differ by direction. |
| U3 | How the 12-byte OCB nonce is built from the 8-byte wire nonce. Working guess: 4 zero bytes, then the 8 bytes. | Try candidates against captured packets until the tag verifies (test 2). |
| U4 | OCB tag length (working guess 16 bytes) and whether any associated data is used (guess: none). | Same capture: packet length minus 8 minus payload size; try with and without AD. |
| U5 | Exact 10-byte fragment header layout: field widths, byte order, which bit is the final flag, the maximum fragment number. | Capture a fragmented update (test 3). |
| U6 | Order of operations (compress, fragment, encrypt each fragment?), compression framing (zlib header or raw), default send MTU, receive MTU, and the sender's chunk size. | Capture; inflate a reassembled group; look at datagram sizes. |
| U7 | Protobuf field numbers and wire types of the transport instruction, user message (keystroke, resize), host message (host bytes, echo ack). | `protoc --decode_raw` on decrypted payloads (test 4); vary the input to tell fields apart (type a key, resize the terminal). |
| U8 | Value of the protocol version, and how a server reacts to a mismatch. | Read from capture; send a modified version and watch the response. |
| U9 | RTO upper clamp; lower/upper clamp of the frame interval for very low or very high RTT; ack delay value actually used by the client side. | Timing capture under `tc netem` (test 5). |
| U10 | Value of the reserved shutdown state number, and who sends it when. | Exit the shell; capture the last instructions. |
| U11 | Exact throwaway rule: is it "may discard states with number below N" or "up to and including N"? Does the receiver have to retain the base of any in-flight diff? | Observe throwaway vs the `old_num` of later instructions; fault-inject with loss (test 8). |
| U12 | Minimum behaviour a stock server tolerates from a client: timings of acks/heartbeats, the use and length of chaff, and what a server does with a client that sends no heartbeat. | Run the CLI with the timings off by 2x and watch for server timeouts. |
| U13 | Retransmit policy: what triggers a resend, and does it always diff from the last acked state or from the last sent state? | Capture with loss; inspect `old_num` on resends. |
| U14 | Is there a resize instruction from the server, and what is inside `host bytes`: raw VT escape bytes, or a mosh-specific diff stream? How does the client learn the server's screen size? | Decode a host message for a simple `echo hi`; resize the client and inspect. |
| U15 | The exact set of escape sequences emitted by the server in diffs (the paper only says "the subset of ECMA-48 used by typical terminals"). | Collect all distinct sequences across the section 7 scenarios and generate the interpreter's coverage list from the data. |
| U16 | Clock for the 16/32-bit millisecond timestamp (width, epoch, wrap) and where it sits in the datagram (outside the instruction, per [1]); how a timestamp reply of "none" is encoded. | Decrypt a datagram; compare timestamps with capture time; send no reply and observe. |

Count: 16 items. Items U1-U5, U7, U16 are the byte-layout ones that block the
first handshake; the rest affect robustness and tuning, not the first screen.
