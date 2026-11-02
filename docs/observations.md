# moche observations from a stock mosh-server 1.4.0 capture

Date: 2026-11-02. Source: `tools/mosh-lab/.state/captures/cap.pcap` (classic pcap, little
endian, link type 1 Ethernet, 45 UDP datagrams, loopback, server port 60000) plus
`session.json` (throwaway key). Reproduce with:

    cargo run -q --example decrypt_pcap -p moche-core -- \
        tools/mosh-lab/.state/captures/cap.pcap tools/mosh-lab/.state/captures/session.json
    # MOCHE_FULL=1 for raw protobuf dumps; MOCHE_DUMP_DIR=<dir> writes inflated instructions

Packet numbers (#n) are 1-based capture order. Clean room: only the captured bytes and
docs/protocol.md were used; `protoc --decode_raw` (generic) confirmed the hand decoder.
Session script: idle 5 s, `echo hi`, `ls --color=always /usr/bin | head -200`, `vim`,
`ihello moche`, Esc, `:q!`, `ls ... /usr/lib | head -100`, resize 100x30 to 80x24, `exit`.

## Summary of the datagram format (all of it verified on 45/45 packets)

    wire:      nonce8 || OCB3-ciphertext || tag16
    nonce12:   00 00 00 00 || nonce8          (direction bit kept)
    plaintext: ts u16 BE || ts_reply u16 BE || frag_id u64 BE || flags u16 BE || zlib(instruction)
    flags:     bit 15 = final fragment, bits 0-14 = fragment number
    instruction (protobuf): 1 version, 2 old_num, 3 new_num, 4 ack_num, 5 throwaway_num, 6 diff, 7 chaff

## Tables

### U1 key encoding
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Unpadded standard base64 of 16 bytes? | Yes | `Session::from_base64` on the 22-char key decodes to 16 bytes and 45/45 tags verify | High |

### U2 direction bit
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Where, which direction | Top bit (0x80 of byte 0) of the 8-byte wire nonce; set for server to client | S->C counters read 0x8000000000000000, 0x80..01 ...; C->S 0, 1, 2 ... Counter +1 per packet per direction (C->S 0..24, S->C 0..19). | High |
| Kept in the AEAD nonce? | Yes, kept | Layout "0000+wire8 with bit cleared" verifies only 25/45 (exactly the client packets); keeping it verifies 45/45 | High |

### U3 nonce construction
| Candidate (tag 16, no AD) | Packets verifying |
|---|---|
| 4 zero bytes + 8 wire bytes (WINNER) | 45/45 |
| 4 zero bytes + 8 wire bytes, direction bit cleared | 25/45 (client only) |
| 8 wire bytes + 4 zero bytes | 1/45 (chance: counter 0 client packet) |
| same, bit cleared | 1/45 |
| 4 zero + wire bytes byte-reversed | 1/45 |
| any of the above with a 12-byte tag | 0/45 |

Implemented as `moche_core::crypto::build_nonce`. Confidence: high.

### U4 tag length and associated data
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Tag length | 16 bytes, at the end of the datagram | verifies at 16, never at 12. 8 not tried (ocb3 crate does not expose a 8-byte tag type), but a 16-byte verify on every packet makes another length impossible | High |
| Associated data | None (empty) | verifies with `b""` | High |

### U5 fragment header
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Layout | 10 bytes at plaintext offset 4: u64 BE id, then u16 BE: top bit final flag, low 15 bits fragment number | all 45: flags = `8000` (final, number 0); ids 1,2,3... e.g. #1 `0000000000000001 8000 789c...` | Medium: only single-fragment messages seen; a flag/number bit split is inferred, not exercised |
| Id value | Equals wire counter + 1 on every packet, both directions (#1 ctr 0 id 1 ... #44 ctr 19 id 20) | table in session | Medium: since every datagram was one new instruction, "per-instruction counter" and "wire counter + 1" cannot be told apart. Retransmits (#26-28) got new wire counters AND new ids 14,15,16, so the id is not a stable per-instruction id on resend |

### U6 (partial) order of operations, compression, MTU
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Order | zlib then (fragment header) then encrypt; the header sits in the encrypted plaintext, not in clear | header visible only after decrypt; zlib stream starts right after it | Medium for fragmenting order (never fragmented) |
| Compression framing | Standard zlib (RFC 1950): starts `78 9c` at plaintext offset 14; flate2 ZlibDecoder (miniz backend) inflates all 45 and validates adler32 | all packets | High |
| Max datagram | 443 bytes on the wire (#40, S->C, instruction inflated 919 bytes, 902 of VT bytes; plaintext 419) | | |
| MTU / chunk size | NOT settled: nothing near an MTU | see "Not settled" | |

### U7 protobuf field numbers (all verified with `protoc --decode_raw`)
Transport instruction (both directions):

| Field | Wire type | Meaning | Evidence |
|---|---|---|---|
| 1 | varint | protocol version = 2 | every packet |
| 2 | varint | old_num | |
| 3 | varint | new_num | |
| 4 | varint | ack_num | |
| 5 | varint | throwaway_num | |
| 6 | bytes | diff (nested message, below) | absent-or-empty on heartbeats: `32 00` (#3) |
| 7 | bytes | chaff, random 0..16 bytes (observed 0 to 16, both directions, e.g. #12 0, #19 16) | |

Diff (both directions) is a message holding repeated field 1, each an element holding exactly one of:

| Element field | Meaning | Inner fields | Seen in |
|---|---|---|---|
| 2 | bytes: keystrokes (C->S) or host bytes (S->C) | 4 = bytes | #8 `0a0c 120a 2208 "echo hi\r"`; #2 server |
| 3 | resize | 5 = width (varint), 6 = height (varint) | C->S #1 (100x30), #39 (80x24); S->C #2, #40 |
| 7 | echo ack (S->C only) | 8 = varint echo-ack number | #4 (1), #10 (5), #14 (7), #44 ... |

Wire example, #8 C->S inflated: `0802 1003 1805 2003 2803 320e 0a0c 120a 2208 6563686f2068 690d 3a0c <12 chaff>`.
Confidence: high for numbers; the inner names above are labels, only the numbers are data.
Several elements per diff appear in order (#2: resize then bytes; #10: echo ack then bytes).

### U8 protocol version
| Value | Evidence | Confidence |
|---|---|---|
| 2 (field 1 of every instruction, both directions) | all 45 | High for value; reaction to a mismatch NOT tested |

### U10 shutdown state
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Reserved number | new_num = 2^64 - 1 (0xffffffffffffffff, a 10-byte varint), a 64-bit field | #44 S->C right after `exit`: old=19 new=18446744073709551615 ack=22 thr=19, diff carries the last screen repaint ending `exit\r\nlogout\r\n\e[K\e[?25h\e[?2004l` | High |
| Who answers | Client acks it: #45 C->S old=22 new=23 ack=18446744073709551615 | | High. Then nothing; the server was gone, so what a client does about missing replies is not observed |
| Sender | The server (its shell exited). Client-initiated shutdown not observed | | |

### U11 (partial) throwaway rule
| Observation | Evidence |
|---|---|
| throwaway_num = highest number of the sender's OWN stream that the peer has acked | server #14 old=6 thr=5 (client had acked 5, #12 ack=5); #20 old=9 thr=8 (client acked 8 via #18); client #30 thr=14 after server ack 14 (#29) |
| old_num >= thr always, with old == thr common | #3 old=1 thr=1; #5; #30 |
| Therefore the receiver must keep state `thr` itself (it is the base of in-flight diffs), i.e. discard strictly below throwaway_num | Medium: inferred, no loss case seen |
| No cases where old_num < thr | 45/45 |

### U14 server payload
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Host bytes | Raw VT escape bytes, not a mosh-specific diff syntax. A repaint/delta generated by the server's own terminal: `\e[r \e[0m \e[H \e[2J \e[?25l ... \e[K \n`, OSC title `\e]0;moche@host: ~\x07`, SGR `\e[0;1;32m`, scroll region `\e[1;29r`, `\e[?25h`, `\e[?2004l`, `\r\n`, BEL `\x07` (#32) | #2, #4, #9, #13, #19, #32, #36, #40, #44 | High |
| Server resize | Yes, an element 3 {5=w, 6=h}, sent as the first element of the first message (#2: 100x30) and of the first message after the client's resize (#40: 80x24, followed by `\e[r\e[0m\e[H\e[2J` full clear) | | High |
| Echo ack | Element 7 {8=n}, n = the client's new_num of the keystroke state being echoed (#10 echo_ack 5 for the client's state 5 `echo hi\r`) | | High |
| Diff base | Each S->C instruction diffs from `old_num` which is the last state the client acked (#9 old=3 new=4; #10 old=3 new=5; #13 old=5 new=6) | | High |
| Initial screen | State 0 is blank; first screen arrives as old=0 new=1 (#2) | | |
| After a client resize | The next server instruction clears and repaints the whole screen (#40, 902 VT bytes) | | |

### U16 timestamps
| Question | Answer | Evidence | Confidence |
|---|---|---|---|
| Width/order | Two u16 big-endian at plaintext offset 0 and 2, outside the instruction | all 45 | High |
| Unit | Milliseconds, wraps at 65536 | ts advances by capture time (#1 2376 at t=0; #8 7588 at t=5.210; #45 21876 at t=19.499); ts - 1000*t is 2374..2378 throughout | High |
| Epoch | Not per-process: client and server stamps agree within 1-3 ms from packet 1 (2376 vs 2377) although the server started about 2.2 s earlier. Likely a shared clock (system-wide monotonic) mod 65536; cross-host epoch is not observable here, and it does not matter as only differences are used | | Medium |
| Reply value | ts_reply = peer's latest timestamp + time it was held. #3: server ts 2377 (#2), client replies 2481 = 2377 + 104 ms hold. #4: server replies 2484 for client 2484 (hold 0) | #3, #4, #9 (7588 + 8 = 7596) | High |
| "No reply" | 0xffff. Sent when no unreplied timestamp is pending: each received timestamp is echoed once (#8 `ffff`, #10 `ffff`, #12 ...) | roughly half the packets | High; a real value of 65535 collision handling unknown |
| Client wraps | none in 19 s | | A wrap case was not captured |

### U12 / U13 / U9 (partial, timing seen on loopback only)
| Observation | Evidence |
|---|---|
| New state per frame: an empty-diff frame still takes a fresh new_num (#7 old=3 new=4), and the next frame with data diffs from the last ACKED state to a later number (#8 old=3 new=5) | #5-#12 |
| Resend identical (old, new): client sent old=12 new=14 (Esc) three times at t=13.732, 13.753, 13.781, intervals 21 and 28 ms, before the server's first reply at 13.793 (#26-28), each with different chaff length | RTT here is ~1 ms and still resends came at ~20-30 ms, not >= 50 ms: the RTO floor/frame pacing of 1.4.0 may be lower than the paper's 50 ms. Needs netem to separate |
| Client delayed ack ~100 ms after the first unacked server frame (#2 0.003 to #3 0.107; #9 5.219 to #11 5.322; #13 7.345 to #15 7.455) | consistent with protocol.md section 4 |
| Server replies to a keystroke within ~8-9 ms (#8 5.210 to #9 5.219), then an extra echo-ack-only frame ~55-70 ms later (#10, #14, #20, #29, #33, #37, #41) | consistent with the 50 ms echo ack rule |
| Heartbeat: idle gap 0.211 to 3.117 (#5 to #6): server sent an empty frame ~2.9 s after the last traffic; #16 at 10.404 after #15 at 7.455 (2.95 s); client side empty frames are always acks | heartbeat ~3 s, as sourced |
| Client sends an immediate frame on a keystroke (#8, #12, #18, #22: first data frame about 2 s after the prior send; the script timing, not a send delay) | cannot judge collection interval |
| Chaff 0-16 bytes on empty frames too | |

### U15 (partial) VT sequences seen from the server
`\e[r`, `\e[0m`, `\e[H`, `\e[2J`, `\e[K`, `\e[?25l`, `\e[?25h`, `\e[?2004l`, `\e[1;29r`,
`\e[29;1H`, `\e[22;1H`, `\e[1;1H`, SGR `\e[0;1;32m` / `\e[0;1;36m` / `\e[0;1;34m`, OSC 0 with BEL,
BEL (0x07), `\r\n`. No `38;5;n` (256-colour) and no `?1049` (alt screen) anywhere in the inflated data
(grep of all 45 dumps). Also seen in #40: `?2004h`, `?1015l`, `?1006l`, `?1005l`, `?1004l` (mode resets/sets
sent on a full repaint). vim was not drawn via the alternate screen: the server repaints
with scroll regions on the primary screen (#19, #32), so the diff is a screen-state diff,
not a pass-through of the application's own escapes.

## Not settled by this capture

- Fragmentation: every instruction fit one datagram (max 443 bytes), so fragment numbering, the final bit split, the id's relation to retransmits, the MTU/chunk size, reassembly order, and whether each fragment is compressed separately are all open. Needs a large update (e.g. `cat` of a big file with a wide screen, or `htop` at 250x80) or a smaller path MTU.
- Frame timing: frame interval vs SRTT, RTO clamps, the 8 ms collection delay, ack and echo-ack delays at non-trivial RTT (needs `tc netem` at 0/50/200 ms).
- Loss and skipped states: no gaps in either direction (ctr contiguous), so retransmit rules (U13), old_num after loss, and throwaway semantics under loss are not observed beyond the Esc resend.
- Roaming: no source address/port change.
- Version mismatch behaviour (U8), bad tag handling, client heartbeat tolerance (U12).
- Timestamp wrap, and the 0xffff-versus-real-65535 case.
- Client-initiated shutdown, and server behaviour when the client goes silent.
- Wide characters, combining marks, mouse/bracketed-paste toggles beyond `?2004l`, bell, title changes in long sessions: not exercised.
- Whether the server clips/ignores a client resize with 0 dimensions, or sends resize outside a repaint.
