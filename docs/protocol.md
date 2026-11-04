# moche protocol notes (the contract)

Status: draft 3, 2026-11-03. Built from public descriptions plus two rounds of black-box
capture of a stock `mosh-server` 1.4.0 (sources [9], [10]). 11 of the original 16 open
questions are resolved; 5 of them remain open or partial and 3 new ids (U17-U19) were
opened by round 2. 8 items remain in section 8 (Unverified).

Every claim below is one of three kinds:

- **SOURCED**: stated by a public document, cited as `[n]` (table below).
- **OBSERVED**: confirmed on our own captures, cited as `obs Un` (item id in
  `docs/observations.md`, round 1) or `r2 §X` (section letter in
  `docs/observations-round2.md`, round 2). Capture conditions, for all of them:
  - server `mosh-server` 1.4.0 (Debian package) only, no other version;
  - client and server on one box, UDP over loopback; round 2 adds `tc netem` on the
    loopback interface (delay 25 ms and 100 ms each way, i.e. RTT about 50 and 200 ms;
    delay 25 ms plus 15% loss);
  - round 1: one session, 45 datagrams; round 2: seven captures, one session each,
    about 1100 datagrams in total;
  - no real network, no roaming, no other server version, no other client implementation.

  "Observed" means true for those sessions. It is not proof of behaviour on a real
  network, under other loss patterns, or on other versions.
- **UNVERIFIED**: section 8. Put behind one named constant or function so changing it is
  a one-line fix.

A fourth label is used for design: **moche choice** marks behaviour moche picks for its
own sender or receiver. It is derived from the observations but is not a claim about the
server.

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
| [9] | Our own capture, round 1: stock `mosh-server` 1.4.0 from the Debian package, treated as a black box, run in a container on loopback; 45 UDP datagrams, one session, 2026-11-02. Findings in `docs/observations.md`. Lab tooling (container, capture, session script) lives in `tools/mosh-lab`; the decoder is `crates/moche-core/examples/decrypt_pcap.rs` | our own work |
| [10] | Our own captures, round 2: same server build and container, seven captures (fragmentation, three RTT settings, 15% loss, miscellaneous sequences, client killed), 2026-11-03. Findings in `docs/observations-round2.md`. Driver `tools/mosh-lab/capture.sh`; decoder for most captures `tools/mosh-lab/analyze_pcap.py`, cross-checked by `decrypt_pcap.rs` on two of them (53/53 and 16/16 tags verify) | our own work |

Not consulted, by rule: any file of mosh's repository (src, .proto), any mirror or
fork of it, any other GPL implementation. The man pages were not fetched. The captures
were decoded with our own code and generic `protoc --decode_raw`.

## 1. Scope, clean-room statement, bootstrap

- moche is a client only. Hosts run the stock `mosh-server`. moche never runs a server.
- Clean room: the wire format is reconstructed from [1]-[6] and from observation of a
  real server [9, 10]. Nothing is copied, translated or paraphrased from mosh's source.
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

OBSERVED (obs U2, U3, U4; 45/45 datagrams verify, both directions; round 2 re-verified
on 53/53 and 16/16 datagrams of two further captures with the Rust decoder, and on all
seven with the Python decoder, r2 Method):

    wire datagram = nonce8 (cleartext) || OCB3 ciphertext || tag16
    nonce12       = 00 00 00 00 || nonce8         (the 8 wire bytes unchanged)
    AEAD          = AES-128-OCB3, key = the 16 decoded bytes, tag 16 bytes, NO associated data

- Direction bit: the top bit (0x80 of byte 0) of the 8-byte big-endian wire nonce.
  Clear for client to server, set for server to client. The remaining 63 bits are the
  sequence number: +1 per datagram per direction, starting at 0 (obs U2: client 0..24,
  server 0..19). Under netem loss the counters show holes where datagrams were dropped
  but never repeat or go backwards (r2 §C).
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

Decrypted plaintext layout (OBSERVED, obs summary, U5, U6, U16; round 1 45/45; round 2
confirmed on all captures including multi-fragment messages, r2 §A):

    ts        u16 big-endian   sender's millisecond clock, mod 65536
    ts_reply  u16 big-endian   0xffff = "no reply"
    frag_id   u64 big-endian   \  10-byte fragment header
    flags     u16 big-endian   /  bit 15 = final fragment, bits 0-14 = fragment number
    payload   a slice of one zlib stream (RFC 1950, whole stream starts 78 9c) of one
              protobuf transport instruction

The timestamps are outside the instruction (matches [1 s2.2]). The fragment header sits
inside the encrypted plaintext, not in clear.

Order of operations on send (OBSERVED, r2 §A, 5 fragmented messages): serialise the
instruction, compress it ONCE into a single zlib stream, cut the compressed blob into
pieces, give each piece a fragment header, encrypt each datagram on its own. Evidence:
the concatenated payloads of fragments 0..n inflate as one valid zlib stream (adler32
ok) for all five messages; only fragment 0 starts with `78 9c`; fragments 1..n do not
inflate alone. This resolves U6.

Timestamps (SOURCED [1 s2.2]: every datagram carries a millisecond timestamp and an
optional "timestamp reply", the peer's latest timestamp, adjusted by the time held):

- OBSERVED (obs U16): width is 16 bits, unit milliseconds, wraps at 65536; `ts` tracks
  capture time (ts minus 1000*t stayed within 2374..2378 over 19 s). Client and server
  stamps agreed within 1-3 ms from packet 1, so the clock looks shared on one host;
  the epoch is irrelevant to moche because only differences are used.
- OBSERVED: `ts_reply` = the peer's latest `ts` plus the time it was held locally
  (e.g. server ts 2377, client reply 2481 after a 104 ms hold; hold 0 gives the same
  value back). Each received timestamp is echoed once; with none pending the field is
  `0xffff`, which happened in roughly half the packets (both rounds).
- OBSERVED (r2 Other, one occurrence): a client `ts` wrapped 65384 to 732 during normal
  operation with no effect on the `ts_reply` handling.
- moche rule (derived): send `ts = now_ms mod 65536`; send `ts_reply = peer_ts + held_ms`
  (mod 65536) once per received timestamp, otherwise `0xffff`; on receipt,
  `rtt = (now16 - ts_reply) mod 65536` when `ts_reply != 0xffff`.
- Remaining gap: what a peer does with a genuine value of 65535 is unknown (U16 residual).
- Netem caveat for timing data (r2 Method): the capture tap sits after the netem qdisc,
  so netem'd captures have timestamps of send time plus one-way delay in BOTH directions.
  Gaps between consecutive packets of the same sender are unaffected; gaps measured
  across directions (for example a delayed ack) include the added one-way delay.

RTT (SOURCED [1 s2.2]): TCP's algorithm [7] with three changes:
1. each datagram has a unique sequence number, so retransmissions are unambiguous;
2. the timestamp reply is adjusted by the time elapsed since the corresponding
   timestamp was received, so delayed acks do not bias the estimate;
3. the minimum retransmission timeout is 50 ms (not 1 s).
moche keeps SRTT and RTTVAR per [7] (alpha 1/8, beta 1/4). Round 2 shows 1.4.0 pacing
resends by frame slot, not by an RTO (section 4, Retransmit), so moche v1 needs SRTT only
(for the frame interval). The RTO (SRTT + 4*RTTVAR, clamped below at 50 ms per [1]) is
computed but not used to pace sends; its upper clamp and any other use are U9. See the
contradiction note in section 4.

Roaming (SOURCED [1 s2.2]): when the server receives an authentic datagram with a
sequence number greater than any before, it adopts that packet's source IP and UDP port
as its new target. For moche (client) this means: just keep sending from whatever socket
the host app has; no signalling is needed. Heartbeats (section 4) let the server learn
the new address even when the client has nothing to say [1 s2.3]. Host-app note: after a
network change, send promptly (a heartbeat or any frame) so the server re-targets. The
client must not send with a repeated or lower sequence number than before (follows from
the rule). Roaming was NOT exercised in either capture round (U17).

Fragmentation:

- SOURCED [6]: the sender splits a compressed transport instruction into fragments, each
  at most an MTU's worth of content; fragments carry a group id, a zero-based fragment
  number and a final-fragment flag; the header is 10 bytes before the content. Both a
  default send MTU and a receive MTU exist. Compression is zlib.
- OBSERVED (r2 §A; capture `frag`, terminal 250x80, loopback, 5 multi-fragment messages
  of 2, 3, 4, 6 and 9 fragments, all server to client): the 10-byte header layout above
  is confirmed. 19 datagrams had fragment number != 0 and final bit clear, exactly the
  non-last fragments; numbers run 0, 1, 2, ...; the final bit is set only on the last
  fragment of a message. Resolves the bit split of `flags` (U5).
- OBSERVED (r2 §A): a full fragment carries 1214 payload bytes. The datagram is then
  1252 bytes = 8 nonce + 4 timestamps + 10 fragment header + 1214 payload + 16 tag.
  1252 bytes is the maximum datagram length seen (server to client); the largest client
  datagram seen is 92. The last fragments of the five messages carried 162, 470, 454,
  454 and 554 bytes. Compressed message sizes were 2590, 4112, 6524, 10166 and 1768
  bytes (inflated 6156 to 40377).
- INFERENCE, not a measurement: 1252 + 28 (IPv4 + UDP headers) = 1280, which looks like
  the IPv6 minimum MTU. It could equally be a coincidence of constants. Nothing in the
  captures confirms that 1.4.0 derives its chunk size from an MTU, or that it ever
  adapts it. moche treats 1214 as an observed value, not as a protocol constant: the
  receiver must accept any chunk size, and the sender's chunk size is one named constant.
- `frag_id` (OBSERVED, r2 §A): ONE id per sent instruction, shared by all of its
  fragments, increasing by 1 per instruction (ids 10..14 for five consecutive messages
  while the wire counters ran 9..32). A resend of an unacked instruction gets a fresh id
  (and fresh wire counters) on every copy (round 1 resends #26-28; r2 §A, §B). Round 1's
  observation "id = wire counter + 1" was a coincidence of one datagram per instruction
  (see the flagged corrections list at the end of section 8). moche rule: keep a counter
  that increases by one for each INSTRUCTION sent, resends included; all fragments of
  that send carry the same value.
- Fragments of one message are sent back to back, with identical `ts` (same
  millisecond) and consecutive wire counters, no pacing between them (r2 §A, #22-#25).
- `seq 1 20000` and similar bulk output never fragmented: the server sends screen
  diffs, not an output stream, and datagrams stayed under 300 bytes (r2 §A). Fragmentation
  happens on large screen changes (full-screen colour fills at 250x80, a full repaint
  after a resize on a big window).
- Client messages are small (max 92 bytes in all captures). moche still needs a
  fragmenter on the send side for large pastes (a pasted block is one keystroke diff);
  it uses the same split as above (compress once, cut, header per piece, encrypt each).

Reassembly (moche choice, derived from the fields above; the receiver side is NOT
exercised by any capture, U18): collect fragments by group id; complete when the final
fragment has arrived and fragment numbers 0..=final are all present; concatenate in
fragment-number order, zlib-inflate the whole, parse the instruction. Do not inflate a
fragment alone. Drop incomplete groups when a newer group id completes or when a bounded
buffer is exceeded (the limit is a moche choice; [6] mentions a fragment assembly limit
exists). A duplicate fragment (same id and number) is ignored. Because ids are per
instruction and fresh on resend, a resent instruction arrives as a new group; do not
try to merge fragments across ids.

## 4. SSP: transport layer

SSP runs once per direction, over two object types: client to server, the history of
the user's input; server to client, the contents of the terminal window [1 s2].

Instruction: a self-contained message listing the source and target states and the
binary diff between them [1 s2.3]. Protobuf, same message both directions. Field names
from [6]; field numbers and wire types OBSERVED (obs U7, confirmed with
`protoc --decode_raw`; 45/45, and all captures decode in round 2):

| Field no. | Wire type | Name | Meaning | Source of name |
|-----|-----------|------|---------|--------|
| 1 | varint | protocol version | always 2 (obs U8) | [6] |
| 2 | varint | old_num | state number the diff is computed against | [1, 6] |
| 3 | varint | new_num | state number the diff produces | [1, 6] |
| 4 | varint | ack_num | latest state of the peer's stream the sender has received | [6] |
| 5 | varint | throwaway_num | state number below which the receiver may discard states (see Throwaway) | [6] |
| 6 | bytes | diff | opaque payload, a user-message or host-message encoding (section 5); present with length 0 on empty frames (`32 00`) | [1, 6] |
| 7 | bytes | chaff | random padding, length 0 to 16 bytes, seen in both directions, on empty frames and on fragmented messages too (r2 Other) | [6] |

Receiver rule: skip unknown fields, do not reject (section 5).

Semantics:

- A diff is idempotent: applying it to state `old_num` yields `new_num`. SSP can diff
  any two states, so the sender need not send every intermediate one [1 s2.2-2.3].
- Numbering (OBSERVED, obs U12/13/9 table; r2 §C): every frame that is a new send takes
  a fresh `new_num`, even an empty-diff frame (empty frame old=3 new=4, next data frame
  old=3 new=5). Under loss, `new_num` may SKIP values the receiver never sees: r2 §C
  saw S>C new=15 followed by old=14 new=17 with the datagram carrying new=16 lost. A
  receiver must therefore never assume `new_num == previous new_num + 1`.
- Choice of `old_num`: see "Receiver rule for old_num" below. This is the part the
  server's rule is not fully known for (U13).
- Shutdown (SOURCED [6]: a reserved state number signals shutdown; OBSERVED, obs U10;
  r2 Other, §C): `new_num = 2^64 - 1` (`0xffffffffffffffff`, a 10-byte varint, so
  `new_num` is a full 64-bit field). Seen in every capture that ended with `exit`,
  server to client, right after the shell exited; its diff carried the final screen
  repaint. The client acks it with `ack_num = 2^64 - 1`. Under 15% loss the server sent
  the final instruction twice, 32 ms apart, until the client's ack arrived (r2 §C), and
  the client resent its own last keystrokes too. moche must therefore treat
  `new_num == u64::MAX` as "peer closing", send that ack, and not store it as a normal
  state. Client-initiated shutdown was not observed.
- Protocol version (OBSERVED, obs U8): 2 in every instruction, both directions. moche
  sends 2. Server reaction to another value is untested (U12).

Timing (OBSERVED, r2 §B, §C, §D; loopback with netem delay on `lo`, 1.4.0; see the
netem caveat in section 3). "RTT" below is the added round trip (0, 50, 200 ms) on top
of about 1 ms of loopback.

| Quantity | rtt0 | rtt50 | rtt200 | Note |
|---|---|---|---|---|
| Frame interval (spacing of resends and back-to-back sends) | about 20 ms | about 32 ms (median, 26-41) | about 105 ms (median, 101-112) | about RTT/2 plus 5-8 ms, floor about 20 ms at low RTT; round 1 (RTT about 1 ms): 21-28 ms |
| Idle heartbeat gap, both directions | 3004-3010 ms | 2996-3017 ms | 3003-3015 ms | about 3.0 s regardless of RTT |
| Server data frame to next client datagram (median / p90) | 89 / 108 ms | 87 / 135 ms | 102 / 106 ms | delayed ack about 100 ms, earlier if the client has keystrokes to send; rtt50/rtt200 include the netem delay |
| Keystroke to first client datagram carrying it (median) | 10 ms (5-46) | 40 ms (33-76) | 147 ms (27-220) | includes netem delay; a key waits for the next frame slot |

- Frame interval (SOURCED [1 s2.3, footnote 1]): minimum interval between frames is
  half the smoothed RTT, so about one instruction is in flight; capped at 50 Hz (at
  least 20 ms). The observed 20 ms floor at low RTT and about RTT/2 at high RTT fit
  this, with a consistent extra 5-8 ms that the paper does not mention. The ceiling at
  very high RTT is not bounded by anything observed beyond 105 ms (U9). Five large
  messages at rtt0 were sent 20 ms apart (r2 §A).
- Collection interval (SOURCED [1 s2.3, s4]): after the application's first write, the
  server waits 8 ms to collect more output, and always waits at least the frame
  interval after the previous frame. OBSERVED loosely: the server answered a keystroke
  in about 8-9 ms (obs U12 table). Not separately resolved from the frame interval.
- Delayed ack (SOURCED [1 s2.3]: a delay of 100 ms was enough to piggyback on host data
  in over 99.9% of cases; an ack travels in the `ack_num` of whatever instruction goes
  out next). OBSERVED (obs U12 table; r2 §B): the client sent a bare ack about 100 ms
  after the first unacked server frame (loopback: 104, 103, 110 ms) and the median
  data-frame-to-next-datagram times above sit at 87-102 ms at every RTT. The constant
  is not resolved beyond "about 100 ms".
- Heartbeat (SOURCED: SSP sends an occasional heartbeat so the server learns of
  roaming, the client can warn the user when silent, and NAT mappings stay open; the
  interval is 3 seconds [1 s2.3; [2] "at least every three seconds"]). OBSERVED (r2
  §B): about 3.0 s between empty frames in both directions at every RTT; round 1's
  2.9-2.95 s was measured from the last traffic, not from the last empty frame. The
  client's empty frames are mostly its delayed acks of the server's heartbeats.
- Throwaway (OBSERVED, obs U11 and r2 §C): `throwaway_num` = the highest number of the
  sender's OWN stream that the peer has acked (server `thr=5` after the client acked 5;
  client `thr=14` after the server acked 14). `old_num >= throwaway_num` in 45/45 of
  round 1; in round 2, `thr == old` on every line of the first and last ~50 packets of
  the loss capture, and in the burst case below `old > thr` (never `old < thr`); not
  exhaustively checked on all packets (r2 §C, Medium). So the receiver must keep state
  `throwaway_num` itself (it is the base of in-flight diffs) and may drop states
  strictly below it.
- Echo ack (SOURCED [1 s3.2]): the server-side terminal object carries an "echo ack"
  for the latest keystroke presented to the app for at least 50 ms. OBSERVED (obs
  U14): it is the client's `new_num` of the keystroke state being echoed, and arrives
  in the host message (section 5), often in a separate small frame 55-70 ms after the
  data frame. Round 2 saw the echo-ack-only frame at every RTT (r2 §B, Medium).
- Retransmit (OBSERVED, r2 §B, §C; obs U12/13): an unacked instruction is simply sent
  again at each frame slot until the peer acks it, up to 3 copies per key at RTT 50. A
  resend of an unchanged state repeats the same (old_num, new_num) and the same diff,
  with a fresh wire counter, a fresh `frag_id` and a freshly chosen chaff length.
  There is no exponential backoff in any run: resend gaps stay at the frame interval
  (loss capture, delay 25 ms, 15% loss: client median 30 ms, server median 36 ms, p90
  51 and 42 ms), with rare larger gaps up to about 180 ms (one 5-resend run: 181, 36,
  30, 36, 33 ms) which are probably lost sends or a stretched send interval, not a
  separate timer. No separate RTO is visible.

Client send timing while typing (OBSERVED, round 8, `observations-round8.md`; stock `mosh-client` 1.4.0
black box, 10 keys/s, one-way delay 5-100 ms on loopback; each "OBSERVED" below is what the stock client did,
each "moche choice" is ours):

- First key after idle (OBSERVED): the datagram leaves 2-5 ms after the key (the pty wake-up); there is no
  separate collection delay. Keys that arrive faster than the frame interval wait for it (one datagram for
  all of them, sent at last send + frame interval; at RTT 100: one datagram per 55 ms).
- Frame interval (OBSERVED, consistent with the table above): last send of any kind + about SRTT/2 + 6 ms, floor
  20 ms; it gates NEW data only.
- Retransmission of an unacked key state (OBSERVED): not at the frame interval. While typing, the stock client
  re-sent a state only about 100-112 ms after its last send (RTT 100 and 200), once about 145 ms (RTT 100,
  idle), and almost never otherwise (10 resends for 296 keys at RTT 100, 3 at RTT 200): the next key supersedes
  the state first. Only one case differs: right after a long silence (nothing received for seconds) the
  stock client re-sent at the frame interval (57-62 ms at RTT 100, 20-23 ms at RTT 10); moche does not copy that.
- Delayed ack (OBSERVED): a standalone empty ack goes out 100-119 ms after the FIRST unacked server frame when
  nothing else was sent; a key frame sent earlier carries the ack, so while typing at 10 keys/s the stock
  client sent 0 to 5 % standalone acks. The server sent two frames per key about 55 ms apart (data+ack, then
  an echo-ack frame); the client acks neither on its own.
- Moche choice: `COLLECT_MS = 0`; small-state resend after `max(frame interval, 110 ms, min(SRTT + 50 ms, 250 ms))`.
  Big states keep their RTO-based resend interval.

**Contradiction flagged (not overwritten): RTO floor.** [1] gives a 50 ms minimum
retransmission timeout. Round 1 saw resends 21 and 28 ms apart (RTT about 1 ms). Round 2
does not remove the contradiction: at RTT 50 the median resend gap is 32 ms (26-41),
again below 50 ms, and it tracks RTT/2 plus a few ms at RTT 200 (105 ms). So 1.4.0 as
observed paces resends by the frame interval, and no 50 ms floor shows up on resend
spacing. The paper's RTO statement may describe a timer that fires only in cases we did
not hit, or an older version; untested. moche does not depend on the floor (it resends
per frame slot, see the moche sender below) and keeps 50 ms as its own clamp where an
RTO is needed at all (U9).

Receiver rule for `old_num` (OBSERVED rule of the server's output; moche choice for
the receiver; the server-side cause is open as U13):

- In capture `frag` (r2 §A), during a burst of five large messages the server diffed
  each new state from the PREVIOUS STATE IT HAD JUST SENT (old 9, 10, 11, 12, 13 for
  new 10..14), while `thr` stayed at 8 and the client's latest ack in that direction
  was 8. In capture `rtt50` (r2 §A, #16-#18) and in the loss capture (r2 §C #9-#15), the
  server kept `old` at the last ACKED state for several successive new numbers
  (old=5 for new 7, 8, 9 at 30-40 ms apart; old=2 for new 4, 5, 5, then old jumps to 4
  after the client's ack). Round 1 (obs U14) saw only the second pattern.
- Counts (r2 §C): `old != previous new` is normal operation, not a loss marker (loss
  C>S 41 and S>C 73 occurrences; no-loss rtt50 C>S 30 and S>C 101).
- Therefore the receiver MUST NOT assume `old_num == last acked state`, nor
  `old_num == previous new_num`. Rule: when an authentic instruction arrives, look up
  `old_num` in the stored states. If the state is held, apply the diff to it and store the
  result as `new_num` (when `new_num` is not already held). Drop the instruction only
  when the base state is gone (never held, or discarded by throwaway). This is
  correct for both patterns because diffs are idempotent against a named state.
- Corollary for the receiver's retention: keep every state `>= throwaway_num`, because an
  in-flight diff may be based on any of them (U11).
- Corollary for acks: `ack_num` sent = the highest `new_num` applied (or held) so far;
  an instruction whose `new_num` is already held is a duplicate, ignore its diff but
  still send an ack (the earlier ack may have been lost).

**Open question (U13): the server's rule for choosing `old_num`.** Two patterns are
observed (previous in-flight state vs last acked state) and the trigger is unknown. Not
tested: message size or fragmentation (the in-flight pattern happened on 3-9-fragment
messages), send spacing relative to SRTT (20 ms apart at RTT about 0 vs 30-40 ms at RTT
50), or a per-state decision. The receiver rule above makes moche independent of the
answer. The answer matters for the client's own sender, where the choice is moche's
(next list).

moche sender (v1), **moche choice derived from observation, not a claim about the
server.** Constants are named and live in one place:

1. Keep `sent_states`: the pending user-input states, numbered, with the last state the
   server acked as the base. Remove states below the acked one when the server's
   `ack_num` advances. Every NEW send takes a fresh `new_num` (empty frames included).
2. Frame interval (r2 §B): `FRAME_INTERVAL = max(20 ms, SRTT/2 + 6 ms)`, with a
   placeholder upper cap of 250 ms (U9; observed only up to 105 ms) and 20 ms before the
   first RTT sample. Never send two frames closer than this.
3. Input: user bytes and resizes accumulate. At the next frame slot send one instruction
   (old = last acked state, new = a fresh number) whose diff is the CUMULATIVE
   concatenation of every input event after the acked state, in order, with the current
   `ack_num` and `throwaway_num`, and a random chaff length 0-16 (U12: whether chaff
   is required is untested).
4. Resend (r2 §B, §C; timing revised in round 8): while the last instruction is unacked, send it
   again no sooner than `max(frame slot, 110 ms, min(SRTT + 50 ms, 250 ms))` after the last send (round 1-7 moche
   used the frame slot, which sent 1.5 to 2.8 times the stock client's datagrams while typing, see
   `observations-round8.md`). If nothing new was typed, repeat the same (old, new) and diff; if new input
   arrived, use a fresh `new_num` and the cumulative diff from the acked state (this is
   what the server was seen doing under loss). Every copy gets a fresh wire counter and
   a fresh `frag_id` (per instruction sent, section 3). No exponential backoff.
5. Delayed ack (r2 §B): if an ack is owed (an authentic server instruction advanced the
   state we hold) and 100 ms passed with nothing to piggyback it on, send an empty
   instruction (fresh `new_num`, empty diff) carrying `ack_num`. Anything sent sooner
   carries the ack.
6. Heartbeat (r2 §B): if nothing was sent for 3.0 s, send an empty instruction with a fresh
   `new_num`. This also lets the server learn a new address after roaming.
7. Shutdown: on a server instruction with `new_num == u64::MAX`, send an empty
   instruction with `ack_num = u64::MAX` and stop; resend it if the server repeats the
   final instruction (it did so under loss).

**Sender invariant: states only grow (OBSERVED, cause INFERRED).** For any two states
the client has ever sent, `new_num_a < new_num_b` implies the content of `b` extends the
content of `a` (the input length is non-decreasing in `new_num`), and a resend of a
number carries the same cumulative content as its first send (the diff's base may move
with the acked state, the content does not). Evidence (round 5, `observations-round5.md`
B2, 6 of 6 runs at 10% loss, strace-confirmed `SIGABRT` of mosh-server 1.4.0): the
client sent `old=133 new=134` (16 KiB), then, before the ack, `old=133 new=135` with
only 8 KiB, and the server died 3 ms after the second frame; the same signature
(`old=66 new=67` 16,390 B then `old=66 new=68` 8,199 B) in the other runs. Inferred, not
from source: the server's receiver treats the user-input history as a growing sequence and
asserts when a newer state does not extend an older one it accepted. The earlier note
that state numbers are the client's to choose was wrong for states already sent. Rules
that follow, implemented in `ssp.rs` and tested against a server model that panics on a
violation: (1) never abandon, shrink or re-cut a state in flight, resend it unchanged
(same `new_num`, same content) until acked; (2) the adaptive size cap applies to the next
cut only; while a big state is in flight no new state is cut; (3) an empty or heartbeat
frame still takes a fresh number and has the content of the acked state, which is an
extension of every older one. Cost consequence: a state is resent whole and the server
needs every fragment of one copy, so a state of N fragments needs about `1/0.9^N` copies
at 10% loss; the cap is therefore chosen before a state goes out (slow start from 16 KiB,
halved once per state that stays unacked for a full loss RTO and three sends, regrown
after two clean cap-filling first-send acks). Simulated 1 MiB paste at 10% loss: about
5.7-7.9x payload (round-4 fixed 16 KiB: 5.9x); the earlier 4.1x relied on re-cutting.

Items 5-6 timings are from [1] and match both capture rounds; whether a server tolerates
deviations from them (a client that never heartbeats, acks late, omits chaff) is
untested (U12). The cumulative-from-acked choice in item 3 is safe for the stock server
because every stored base on the server side is a numbered state it still holds; it is
the same pattern the server itself used under loss.

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
(100x30) and a later one an 80x24 resize. Non-ASCII input travels as raw UTF-8 bytes
in the keystroke element, with no escaping (r2 §D, misc #6-#11).

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

Observed, round 1 (obs U14, U15):

- State 0 is a blank screen. The first screen arrives as old=0, new=1.
- A client resize makes the server answer with a resize element then a full clear
  (`ESC[r ESC[0m ESC[H ESC[2J`) and a complete repaint (902 VT bytes for 80x24). The
  framebuffer must be resized on the resize element, then repainted from the bytes.
- Mode toggles in a repaint were seen: `?25l/h`, `?2004h/l`, `?1015l`, `?1006l`,
  `?1005l`, `?1004l`.

Observed, round 2 (r2 §A, §D; the capture conditions stated at the top apply):

- Full-screen applications are NOT passed through. No `?1049` (alternate screen) appears
  in ANY capture of either round (0 hits in `frag` and `misc` as well as round 1). The
  server paints alt-screen content on the PRIMARY screen with cursor addressing and
  erase sequences: `htop` used `ESC[2;120H`, `ESC[113X` (erase characters), `ESC[K`;
  a test of the alternate screen produced `ESC[?25l ESC[2;1H ESC[K ... ESC[9X ESC[5;10H`
  plus the text (r2 §D, misc #41, `frag` #51-#56). The server's emulator is the filter:
  the client never sees the application's own mode switches for the alternate screen.
  This confirms and widens the round 1 finding; it is not a contradiction of it.
- SGR (r2 §D): every SGR run starts with `0;` (a full reset, then the set), e.g.
  `ESC[0;1;32m`. 256-colour foreground is `ESC[0;38;5;Nm`, foreground plus background
  `ESC[0;38;5;F;48;5;Bm` (counts in `frag`: 4617 `38;5;N`, 2297 `48;5;N`). Low colours
  appear to be rewritten to classic codes when they fit (N < 8 became `3x`, seen
  `ESC[0;31;48;5;126m`; Medium). Truecolor passes through: `ESC[0;38;2;10;200;30m`.
  Attributes: input `ESC[1;4;7;9m` came out as `ESC[0;1;4;7m`: bold, underline and
  reverse are kept, STRIKETHROUGH (9) is DROPPED, italic `ESC[3m` becomes `ESC[0;3m`.
  Consequence: the interpreter needs no strikethrough cell attribute; 256-colour and
  truecolor foreground and background must be modelled.
- Wide and combining characters (r2 §D): wide characters (CJK, emoji, fullwidth Latin)
  travel as raw UTF-8 in host bytes, with no escape-encoded width and no width hints on
  the wire. Combining marks are emitted as the base character followed by its combining
  bytes, not precomposed (`e` + U+0301, `a` + U+0308 + U+0302, Devanagari base + mark).
  The receiver must compute cell widths itself and attach combining marks to the
  previous cell. Which Unicode width table the server uses is not observable; a
  mismatch would shift columns (U15).
- Mode toggles and one-shot events (r2 §D): each of the following arrived in its own
  small frame, as raw sequences in the host bytes: `ESC[?1000h` and `ESC[?1006h` (mouse
  enable); a mouse reset, one frame with `ESC[?1003l ESC[?1002l ESC[?1001l ESC[?1000l
  ESC[?1015l ESC[?1006l` (all mouse modes, 1003 first); `ESC[?2004h` and `ESC[?2004l`
  (bracketed paste); `ESC[?25l` and `ESC[?25h` (cursor visibility); BEL (`0x07`, a
  single byte in its own frame).
- Title (r2 §D, Medium): an application OSC 0 (`ESC]0;...BEL`) is forwarded as `ESC]0;...BEL`. After
  the application then sent OSC 2, the next frame carried `ESC]1;<old>BEL ESC]2;<new>BEL`
  (icon name and title as separate sequences, re-sent).
- `seq 1 20000` never fragmented: the server sends screen diffs, not application output.

moche model (design, to be validated against recordings, section 7):

- Keep `states: BTreeMap<u64, Framebuffer>`, starting with state 0 = blank screen of
  the initial size.
- On an authentic instruction: apply the receiver rule for `old_num` in section 4
  (apply whenever the base state is held; drop only when it is gone). A diff with
  `new_num` not greater than the latest held is allowed (idempotent re-application to
  another base can only produce the same state); store it only if not already held.
  `new_num == u64::MAX` is shutdown, see section 4. Update the `ack_num` to send and
  mark the screen dirty.
- Discard: when an instruction arrives with `throwaway_num = t`, drop all stored
  states with number < t, always keeping the latest. Strictly-below is observed-consistent
  on loss data too (r2 §C, Medium), but the sender's choice of `old_num` (U13) is not
  fully known, so a state `>= t` is never dropped.
- The framebuffer holds, besides the cell grid and cursor: current title and icon name,
  cursor-visible, bracketed-paste, mouse-mode flags (1000-1006, 1015) and the other mode
  toggles listed above. BEL is an EVENT, not state: the interpreter reports it to the
  host and does not store it.
- Screen handed to the host: full repaint (clear, draw all cells, set cursor and
  modes, title) generated from the current framebuffer. Chosen for v1 because the host
  (SwiftTerm) only needs bytes, and it is stateless and robust against skipped
  states. A minimal diff against the last painted frame is a later optimisation.
- Predictive local echo: a display-only overlay on the latest state, see `docs/predict.md`
  (the framebuffer and the numbered states are never modified by it).

VT subset (U15). Seen so far across both rounds, on loopback with 1.4.0 only: `ESC[r`,
`ESC[0m`, `ESC[H`, `ESC[2J`, `ESC[K`, `ESC[nX` (erase characters), `ESC[?25l/h`,
`ESC[?2004h/l`, `ESC[1;29r` scroll regions, cursor positions `ESC[29;1H` style,
SGR forms in the bullet above (classic, 256-colour, truecolor, bold/underline/reverse/italic),
mouse mode sets and resets, mode resets `?1015l ?1006l ?1005l ?1004l`, OSC 0/1/2
titles with BEL, BEL, `\r\n`, raw UTF-8 with wide and combining characters. Not seen:
`?1049` (alternate screen), strikethrough. Do not assume it equals xterm; build the
interpreter from recordings.

## 7. Test plan

All vectors are captured from a real `mosh-server` run in a container (lab tooling:
`tools/mosh-lab`), and checked into `tests/vectors/` with no real hostnames, IPs or
user data. Of the round 2 captures, only `misc` and `misc_kill` are in
`tools/mosh-lab/.state/captures/`; the other five pcaps stay on the docker host, with
their sha256 listed in `docs/observations-round2.md`. Pull and trim them (the `frag`
capture especially, for the fragment vectors) into `tests/vectors/` before building
tests on them.

Status after [9] and [10]:

1. Handshake log: DONE for the key format (the container and the throwaway-key flow).
   Run `mosh-server new -s -c 256 -l LANG=en_US.UTF-8` via ssh, store the
   `MOSH CONNECT` line with a throwaway key (fresh key per recording, so the key in the
   repo is worthless). Round 2 ran without sshd (fake `SSH_CONNECTION`); an ssh-driven
   run is still to do for the end-to-end CI.
2. Crypto known-answer: DONE for 45 + 53 + 16 packets (both directions, one captured
   session per file). Remaining: add an independent OCB3 vector test of the primitive
   (RFC 7253 appendix vectors) to separate "my OCB is wrong" from "my nonce is wrong".
3. Fragment vectors: DONE (`frag`: 5 multi-fragment messages, 19 non-final fragments).
   Remaining: turn them into unit vectors (reassembly, one-zlib-stream inflate).
4. Instruction vectors: DONE for field numbers, version, chaff, shutdown, loss. Repeat
   when a second server version is available.
5. Timing: DONE on loopback with netem delay (0 / 50 / 200 ms), idle heartbeat, delayed
   ack, resend spacing, echo ack. Exact RTO and send-interval constants and a real
   network remain open (U9).
6. Roaming: NOT DONE (U17). Change the client source port/address mid-session; confirm
   the server follows after one authentic higher-sequence datagram. Test also a replayed
   old-sequence packet from the new address (must not re-target). Needs a container with
   nft/iptables for SNAT, or a second `lo` alias.
7. VT recordings: PARTIAL. Done in round 1 or 2: shell prompt, `ls --color`, `vim`,
   `htop`, a 256-colour dump, truecolor, attributes, wide characters and emoji,
   combining marks, resize at idle, bell, title, mouse mode toggles, bracketed paste
   toggles, cursor visibility, the alternate-screen test. No evidence in the notes for:
   `less`, line wrap at the right edge, resize during output, `reset`. For each scenario
   store `(old_num, new_num, diff bytes)` plus the final screen as a text dump of
   cells/attributes/cursor, run them against the interpreter, and use a standard
   permissive-licensed headless terminal emulator only as a sanity check, since the
   server's diff output is what moche must accept. Settles U15.
8. Skipped states and loss: DONE for the sender side (one capture, delay 25 ms plus 15%
   loss: skipped `new_num`, cumulative diffs, resends, shutdown under loss). Remaining:
   unit-test the state map, throwaway and the receiver rule for `old_num` in section 4
   with the loss capture as vectors, and the in-flight-base pattern from `frag`.
9. Negative tests: NOT DONE, against a real server or as unit tests: bad tag (server's
   reaction, U12), truncated packet, duplicated fragment, missing final fragment,
   fragments out of order (receiver-side reorder, U18), unknown protobuf fields,
   instruction with unknown `old_num`, version mismatch (U12), and a client that stops
   sending for more than 12 s (U19).

## 8. Unverified: confirm by observing a real mosh-server

Resolved by captures [9, 10] and moved into sections 1-6: U1, U2, U3, U4, U5, U6, U7,
U8, U10, U14, U16 (marked RESOLVED (observed); the evidence is in
`docs/observations.md` and `docs/observations-round2.md` under the same ids or sections).
Ids are stable and not renumbered; U17-U19 are new in draft 3. Each open item blocks
something specific; keep it behind one named constant or function.

| # | Status and what is still open | Test still needed |
|---|-------|-------------|
| U9 | PARTIAL. Observed (r2 §B): frame interval about RTT/2 + 5-8 ms with a floor of about 20 ms at low RTT (20 / 32 / 105 ms at RTT 0 / 50 / 200); resend per frame slot, no backoff; delayed ack about 100 ms; idle heartbeat about 3.0 s at every RTT. Still open: the exact RTO and send-interval constants (the 5-8 ms offset, a ceiling above 105 ms, any RTO upper clamp, whether the paper's 50 ms RTO floor exists in 1.4.0, see the flagged contradiction in sec. 4), ack-delay constants beyond "about 100 ms", the 50 ms echo-ack pacing constant, behaviour on a real (non-loopback) network, jitter. | A run over a real LAN or WAN path with jitter, plus netem at 400 ms and above; tabulate resend gaps vs SRTT. |
| U11 | PARTIAL. Observed: throwaway = highest own-stream number the peer acked; `old_num >= throwaway` always (round 1 exhaustively; loss capture on the first and last ~50 packets); so keep state `throwaway_num` and drop strictly below. Not exhaustively checked on all 262 loss-capture packets, and "never lose a needed base" depends on U13. | Check `old_num >= throwaway_num` over every instruction of the loss and `frag` captures; add as a unit test over the pulled vectors. |
| U12 | OPEN. Minimum a stock server tolerates from a client: ack and heartbeat timings, whether chaff is required, what the server does with a client that sends no heartbeat, with a wrong protocol version (U8), or with a bad tag. Timings (ack about 100 ms, heartbeat about 3 s) are consistent with [1] and both rounds but prove nothing about tolerance. | Run the CLI with timings off by 2x and with chaff omitted; watch for server timeouts or drops. Send a modified version field and a corrupted tag. |
| U13 | PARTIAL. Observed in detail: resends repeat (old, new) with fresh wire counter, `frag_id` and chaff; new input gets a fresh `new_num` and a cumulative diff from the last acked `old`; `new_num` can skip under loss; shutdown is repeated until acked. OPEN QUESTION: the server's rule for choosing `old_num`. In the `frag` burst it diffed each state from the previous in-flight state (old 9..13 for new 10..14, `thr` 8, latest client ack 8); in `rtt50` and the loss capture it kept the last acked state. The receiver rule that follows (apply any diff whose base is held; never assume old_num == last acked) is in sec. 4; the trigger is untested (size or fragmentation, spacing vs SRTT, per-state decision). | Targeted captures: a burst of large messages at RTT 50 and 0, and a burst of small ones, noting `old` vs the last ack received; compare with fragmented vs unfragmented diffs. |
| U15 | PARTIAL. Escape sequences seen across both rounds are listed in sec. 6 (SGR classic, 256-colour, truecolor, bold/underline/reverse/italic, strikethrough dropped, wide characters and combining marks as raw UTF-8, mouse, bracketed paste, cursor visibility, BEL, titles, no alternate screen, erase characters). The full set the server emits is still unknown (scenarios in test 7 not yet run: `less`, wrap at the right edge, resize during output, `reset`), and which Unicode width table the server's emulator uses is not observable. | Collect all distinct sequences across the remaining test 7 scenarios and generate the interpreter's coverage list from the data; compare cell widths on a set of ambiguous-width characters. |
| U17 | OPEN (new). Roaming: no source address or port change was exercised in either round; the server re-targeting to a new source, ignoring a replayed lower sequence number from another address, and the first-send latency after a change are all sourced [1 s2.2] only. | Test 6: SNAT or second `lo` alias mid-session. |
| U18 | OPEN (new). Receiver side of fragmentation: out-of-order, duplicated, lost or missing-final fragments, and how long a server keeps a partial message, are not observed (only the sender side). moche's reassembly in sec. 3 is its own choice. Also whether the server ever changes its chunk size (the 1280 MTU reading is an inference). | Inject reordered, duplicated and dropped fragments toward a real server from the CLI; read the server's reaction in later `ack_num`s. |
| U19 | OPEN (new). Server behaviour beyond 12 s after the client vanishes: in the one run (r2 §D) the server kept sending an empty frame about every 3.0 s with `new` incrementing and `old`, `thr` frozen, no backoff, no shutdown, and the process alive after 12 s. Its idle timeout (including any network-timeout environment variable) is untested. Also: whether a returning client (a different address, a long gap) is resumed with a diff against its old state, which moche's resume-without-reauth story depends on. | Kill the client for 30 s / 5 min / over the default timeout, then resume from a saved session state; record what the server sends. |

Residual gaps on RESOLVED items (none blocks the first screen):

- U5: maximum fragment number seen is 8 (9 fragments); the full 15-bit range is
  untested; receiver side is U18.
- U6: the 1214-byte chunk and the 1280 MTU reading are for this build only; the MTU
  link is an inference (sec. 3). Whether a sender ever splits differently is U18.
- U10: client-initiated shutdown, and client behaviour after the server's final frame,
  not observed.
- U16: one timestamp wrap seen (client, 65384 to 732) with normal handling; a real
  timestamp value of 65535 vs the `0xffff` "none" marker is still not disambiguated;
  cross-host clock epoch is not observable.
- U8: server reaction to a version mismatch is untested (covered by U12's test).
- U2-U4: only server 1.4.0 on loopback; a different server version could in principle
  differ. 8-byte tag length was not tried.
- U14: the host-bytes and echo-ack findings stand; the round 1 statement that the server
  diffs from the last acked state is refined, not removed, by U13.

Corrections made while folding in round 2 (flagged, not silent):

1. Round 1 / earlier protocol.md: "`frag_id` equals the wire counter + 1" and the moche
   rule "a counter that increases for each DATAGRAM built". Round 2 shows the id is per
   INSTRUCTION (shared by fragments, +1 per instruction, fresh on resend) while wire
   counters are per datagram. Round 1 itself noted the two readings were
   indistinguishable at one datagram per instruction; the moche rule was changed.
2. Round 1 / earlier protocol.md: "Server instructions diff from the last state the
   client acked" (obs U14). Round 2 shows a second pattern (previous in-flight state,
   capture `frag`); the first holds in the other captures. See U13 and the receiver rule.
3. Sourced [1 s2.2]: minimum RTO 50 ms. Observed resends at 21-41 ms at RTT about 1 and 50
   ms; resend pacing is the frame interval, not an RTO. Unresolved; see the flagged
   paragraph in sec. 4.
4. Round 1: heartbeat "about 2.9-2.95 s". Round 2 measured the gap between empty frames
   at about 3.0 s (2996-3017 ms). Not a contradiction, a refinement of what was
   measured against what.
5. Round 1: largest datagram 443 bytes and "MTU and chunk size open". Superseded: 1252.
6. Round 1 U15: "no 256-colour". Superseded (observed in round 2); round 1's absence was
   specific to its session script.
7. Round 1 "no alternate screen" stands and is extended; no contradiction.

Count: 8 items open or partial (U9, U11, U12, U13, U15, U17, U18, U19); 11 resolved by
observation (U1, U2, U3, U4, U5, U6, U7, U8, U10, U14, U16). Of the original 16 questions,
11 are resolved and 5 remain (U9, U11, U12, U13, U15); U17-U19 were opened by round 2.
Every byte layout that blocks the first handshake and the first screen (U1-U8, U10, U14,
U16) is now settled; what remains affects robustness, tuning and the roaming/resume
story, not the first screen.
