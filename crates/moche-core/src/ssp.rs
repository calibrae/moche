//! Client side of the State Synchronization Protocol, sans-IO.
//!
//! The host feeds datagrams and user input with a monotonic millisecond clock
//! and drains datagrams to send. Nothing here sleeps or touches a socket.
//!
//! Sending: user input is a numbered stream of elements. Each frame carries
//! the cumulative diff from the last state the server acked to the latest one
//! (so any lost frame is repaired by the next). Unacked data is re-sent every
//! frame slot (about SRTT/2, at least 20 ms) with a fresh datagram sequence
//! number and fragment id, reusing the same `new_num`. Idle, an empty frame
//! (with a fresh `new_num`) goes out as a delayed ack after 100 ms, or as a
//! heartbeat after 3 s.
//!
//! Receiving: the transport does not keep screen states. It reports each
//! accepted instruction as `(old_num, new_num, bytes)` in arrival order and
//! drops only exact duplicates and stale `new_num <= latest delivered`. The
//! caller decides whether it still holds the `old_num` base.

use std::collections::VecDeque;

use crate::crypto::Session;
use crate::network::{fragment, Cipher, Direction, Inner, Reassembler, Rtt};
use crate::proto::{decode_diff, encode_diff, Element, Instruction, MAX_CHAFF, PROTOCOL_VERSION};

/// `new_num` marking the end of the session.
pub const SHUTDOWN_NUM: u64 = u64::MAX;

/// Lower bound of the frame interval in ms (50 Hz cap).
const MIN_FRAME_MS: u64 = 20;
/// Upper bound of the frame interval in ms (moche's choice, untested above 105).
const MAX_FRAME_MS: u64 = 250;
/// Wait after the first unsent input before sending, to batch keystrokes.
const COLLECT_MS: u64 = 8;
/// Delayed ack.
const ACK_DELAY_MS: u64 = 100;
/// Idle heartbeat.
const HEARTBEAT_MS: u64 = 3000;

/// Source of random bytes supplied by the host (used for chaff only).
pub type Rng = Box<dyn FnMut(&mut [u8]) + Send>;

/// Something the host should act on, from [`Transport::handle_datagram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The server resized the terminal. Emitted before the same
    /// instruction's [`Event::HostDiff`].
    HostResize {
        /// State the resize belongs to.
        new_num: u64,
        /// Columns.
        width: u16,
        /// Rows.
        height: u16,
    },
    /// The server confirmed it processed our keystrokes up to this state.
    EchoAck(u64),
    /// One accepted server state: host VT bytes (all data elements of the
    /// instruction concatenated; may be empty) taking state `old_num` to
    /// `new_num`. Emitted once per accepted instruction.
    HostDiff {
        /// Base state the diff applies to.
        old_num: u64,
        /// Resulting state.
        new_num: u64,
        /// Terminal bytes.
        bytes: Vec<u8>,
    },
    /// The server ended the session (already acked); stop using the transport.
    Shutdown,
}

/// Client-side SSP endpoint.
pub struct Transport {
    cipher: Cipher,
    rng: Rng,
    created: u64,
    // sending
    seq: u64,
    frag_id: u64,
    /// Unacked user elements; element `i` has global index `acked_count + i`.
    elements: VecDeque<Element>,
    total: u64,
    acked_count: u64,
    /// Sent states as `(num, element count)`; the front is the acked state.
    sent: VecDeque<(u64, u64)>,
    unsent_since: Option<u64>,
    last_send: Option<u64>,
    outbox: VecDeque<Vec<u8>>,
    // receiving
    reasm: Reassembler,
    latest: u64,
    ack_due: Option<u64>,
    peer_throwaway: u64,
    highest_seq: Option<u64>,
    reply: Option<(u16, u64)>,
    rtt: Rtt,
    shutdown: bool,
}

impl Transport {
    /// Creates a transport for `session`. `now_ms` starts the heartbeat clock;
    /// `rng` fills chaff (any source works, it needs no secrecy).
    pub fn new(session: &Session, now_ms: u64, rng: Rng) -> Self {
        Transport {
            cipher: Cipher::new(session),
            rng,
            created: now_ms,
            seq: 0,
            frag_id: 1,
            elements: VecDeque::new(),
            total: 0,
            acked_count: 0,
            sent: VecDeque::from([(0, 0)]),
            unsent_since: None,
            last_send: None,
            outbox: VecDeque::new(),
            reasm: Reassembler::new(),
            latest: 0,
            ack_due: None,
            peer_throwaway: 0,
            highest_seq: None,
            reply: None,
            rtt: Rtt::default(),
            shutdown: false,
        }
    }

    /// Queues keystrokes (or pasted bytes) for the server.
    pub fn send_keys(&mut self, now_ms: u64, keys: &[u8]) {
        if !keys.is_empty() {
            self.push(now_ms, Element::Data(keys.to_vec()));
        }
    }

    /// Queues a terminal size change. Also send one right after connecting.
    pub fn send_resize(&mut self, now_ms: u64, cols: u16, rows: u16) {
        self.push(
            now_ms,
            Element::Resize {
                width: cols,
                height: rows,
            },
        );
    }

    fn push(&mut self, now_ms: u64, e: Element) {
        if self.shutdown {
            return;
        }
        if self.total == self.last_count() {
            self.unsent_since = Some(now_ms);
        }
        self.elements.push_back(e);
        self.total += 1;
    }

    /// Highest `throwaway_num` the server announced: it will never again use
    /// a base older than this, so the caller may drop screen states below it.
    pub fn peer_throwaway(&self) -> u64 {
        self.peer_throwaway
    }

    /// Smoothed round-trip time estimate in ms (1000 before any sample).
    pub fn srtt_ms(&self) -> f64 {
        self.rtt.srtt_ms()
    }

    /// Processes one received UDP datagram. Datagrams that fail
    /// authentication, are malformed, or are stale yield no events.
    pub fn handle_datagram(&mut self, now_ms: u64, datagram: &[u8]) -> Vec<Event> {
        let Ok(opened) = self.cipher.open(datagram) else {
            return Vec::new();
        };
        if opened.direction != Direction::ToClient {
            return Vec::new();
        }
        let Ok(inner) = Inner::decode(&opened.plaintext) else {
            return Vec::new();
        };
        if self.highest_seq.is_none_or(|h| opened.seq > h) {
            self.highest_seq = Some(opened.seq);
            self.reply = Some((inner.ts, now_ms));
            if let Some(r) = inner.ts_reply {
                self.rtt.sample_from_reply(now_ms, r);
            }
        }
        let Ok(Some(message)) = self.reasm.push(inner.frag) else {
            return Vec::new();
        };
        match Instruction::decode(&message) {
            Ok(i) if i.version == PROTOCOL_VERSION => self.accept(now_ms, i),
            _ => Vec::new(),
        }
    }

    fn accept(&mut self, now_ms: u64, i: Instruction) -> Vec<Event> {
        self.peer_throwaway = self.peer_throwaway.max(i.throwaway_num);
        self.process_ack(i.ack_num);
        if i.new_num <= self.latest {
            // Duplicate or stale: the server may have missed our ack.
            let at = if self.shutdown {
                now_ms
            } else {
                now_ms + ACK_DELAY_MS
            };
            self.ack_due = Some(self.ack_due.map_or(at, |d| d.min(at)));
            return Vec::new();
        }
        let Ok(elements) = decode_diff(&i.diff) else {
            return Vec::new();
        };
        self.latest = i.new_num;
        let mut events = Vec::new();
        let mut bytes = Vec::new();
        for e in elements {
            match e {
                Element::Data(b) => bytes.extend_from_slice(&b),
                Element::Resize { width, height } => events.push(Event::HostResize {
                    new_num: i.new_num,
                    width,
                    height,
                }),
                Element::EchoAck(n) => events.push(Event::EchoAck(n)),
            }
        }
        events.push(Event::HostDiff {
            old_num: i.old_num,
            new_num: i.new_num,
            bytes,
        });
        let at = if i.new_num == SHUTDOWN_NUM {
            self.shutdown = true;
            events.push(Event::Shutdown);
            now_ms
        } else {
            now_ms + ACK_DELAY_MS
        };
        self.ack_due = Some(self.ack_due.map_or(at, |d| d.min(at)));
        events
    }

    fn process_ack(&mut self, n: u64) {
        let front = self.sent.front().map_or(0, |s| s.0);
        if n <= front {
            return;
        }
        let Some(p) = self.sent.iter().position(|s| s.0 == n) else {
            return;
        };
        self.sent.drain(..p);
        let count = self.sent[0].1;
        let drop = (count - self.acked_count) as usize;
        self.elements.drain(..drop.min(self.elements.len()));
        self.acked_count = count;
    }

    fn last_count(&self) -> u64 {
        self.sent.back().map_or(0, |s| s.1)
    }

    fn frame_interval(&self) -> u64 {
        ((self.rtt.srtt_ms() / 2.0).ceil() as u64).clamp(MIN_FRAME_MS, MAX_FRAME_MS)
    }

    fn data_outstanding(&self) -> bool {
        !self.shutdown && self.total > self.acked_count
    }

    /// Earliest `now_ms` at which [`Transport::poll_transmit`] will produce a
    /// datagram, or `None` if nothing is scheduled. A value at or below the
    /// current time means "call it now".
    pub fn next_deadline_ms(&self) -> Option<u64> {
        if !self.outbox.is_empty() {
            return Some(0);
        }
        if self.shutdown {
            return self.ack_due;
        }
        if self.data_outstanding() {
            let by_interval = self.last_send.map(|t| t + self.frame_interval());
            let mut t = if self.total > self.last_count() {
                let collect = self.unsent_since.unwrap_or(0) + COLLECT_MS;
                by_interval.map_or(collect, |b| b.max(collect))
            } else {
                by_interval.unwrap_or(0)
            };
            if let Some(a) = self.ack_due {
                t = t.min(a);
            }
            return Some(t);
        }
        let heartbeat = self.last_send.unwrap_or(self.created) + HEARTBEAT_MS;
        Some(self.ack_due.map_or(heartbeat, |a| a.min(heartbeat)))
    }

    /// Returns the next ready datagram, if one is due at `now_ms`. Call
    /// repeatedly until it returns `None` (a message may span fragments).
    pub fn poll_transmit(&mut self, now_ms: u64) -> Option<Vec<u8>> {
        if self.outbox.is_empty() {
            if self.next_deadline_ms()? > now_ms {
                return None;
            }
            self.build_frame(now_ms);
        }
        self.outbox.pop_front()
    }

    fn build_frame(&mut self, now_ms: u64) {
        let old_num = self.sent[0].0;
        let last_num = self.sent.back().map_or(0, |s| s.0);
        let (new_num, diff) = if self.data_outstanding() {
            let diff = encode_diff(&coalesce(&self.elements));
            if self.total > self.last_count() {
                let n = last_num + 1;
                self.sent.push_back((n, self.total));
                self.unsent_since = None;
                (n, diff)
            } else {
                (last_num, diff)
            }
        } else {
            let n = last_num + 1;
            self.sent.push_back((n, self.total));
            (n, Vec::new())
        };
        let mut b = [0u8; 1];
        (self.rng)(&mut b);
        let mut chaff = vec![0u8; usize::from(b[0]) % (MAX_CHAFF + 1)];
        (self.rng)(&mut chaff);
        let message = Instruction {
            version: PROTOCOL_VERSION,
            old_num,
            new_num,
            ack_num: self.latest,
            throwaway_num: old_num,
            diff,
            chaff,
        }
        .encode();
        let id = self.frag_id;
        self.frag_id += 1;
        let Ok(frags) = fragment(id, &message) else {
            return;
        };
        for (k, frag) in frags.into_iter().enumerate() {
            let ts_reply = if k == 0 {
                self.take_reply(now_ms)
            } else {
                None
            };
            let inner = Inner {
                ts: now_ms as u16,
                ts_reply,
                frag,
            };
            let seq = self.seq;
            self.seq += 1;
            if let Ok(d) = self.cipher.seal(Direction::ToServer, seq, &inner.encode()) {
                self.outbox.push_back(d);
            }
        }
        self.last_send = Some(now_ms);
        self.ack_due = None;
    }

    /// Echo of the peer's latest timestamp plus the time we held it.
    fn take_reply(&mut self, now_ms: u64) -> Option<u16> {
        let (ts, at) = self.reply.take()?;
        let r = ts.wrapping_add(now_ms.saturating_sub(at) as u16);
        Some(if r == 0xffff { 0xfffe } else { r })
    }
}

/// Merges adjacent data elements, keeping order.
fn coalesce(els: &VecDeque<Element>) -> Vec<Element> {
    let mut out: Vec<Element> = Vec::new();
    for e in els {
        match (out.last_mut(), e) {
            (Some(Element::Data(a)), Element::Data(b)) => a.extend_from_slice(b),
            _ => out.push(e.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "Ixu2un/jwqLpFzsnDHm5lw";

    struct Server {
        cipher: Cipher,
        seq: u64,
        id: u64,
    }

    impl Server {
        fn new() -> Self {
            Server {
                cipher: Cipher::new(&Session::from_base64(KEY).unwrap()),
                seq: 0,
                id: 1,
            }
        }

        fn send(&mut self, ts: u16, reply: Option<u16>, i: Instruction) -> Vec<Vec<u8>> {
            let id = self.id;
            self.id += 1;
            fragment(id, &i.encode())
                .unwrap()
                .into_iter()
                .map(|frag| {
                    let inner = Inner {
                        ts,
                        ts_reply: reply,
                        frag,
                    };
                    let seq = self.seq;
                    self.seq += 1;
                    self.cipher
                        .seal(Direction::ToClient, seq, &inner.encode())
                        .unwrap()
                })
                .collect()
        }

        fn instr(old: u64, new: u64, ack: u64, els: &[Element]) -> Instruction {
            Instruction {
                version: 2,
                old_num: old,
                new_num: new,
                ack_num: ack,
                throwaway_num: old,
                diff: encode_diff(els),
                chaff: vec![],
            }
        }

        fn open_client(&self, d: &[u8]) -> (u64, Inner, Instruction) {
            let o = self.cipher.open(d).unwrap();
            assert_eq!(o.direction, Direction::ToServer);
            let inner = Inner::decode(&o.plaintext).unwrap();
            let mut r = Reassembler::new();
            let msg = r.push(inner.frag.clone()).unwrap().unwrap();
            (o.seq, inner, Instruction::decode(&msg).unwrap())
        }
    }

    fn transport() -> Transport {
        let mut s = 1u64;
        Transport::new(
            &Session::from_base64(KEY).unwrap(),
            0,
            Box::new(move |b| {
                for x in b {
                    s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                    *x = (s >> 40) as u8;
                }
            }),
        )
    }

    fn drain(t: &mut Transport, now: u64) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| t.poll_transmit(now)).collect()
    }

    #[test]
    fn datagrams_never_exceed_max_datagram() {
        assert_eq!(crate::network::MAX_DATAGRAM, 1252);
        let mut t = transport();
        // ~200 KB of incompressible keystrokes (about 165 full fragments). Larger
        // incompressible input exceeds the 256-fragment limit and is not sent at all
        // (known gap: fragment() returns TooLarge and build_frame drops the frame).
        let mut seed = 99u64;
        let big: Vec<u8> = (0..200_000)
            .map(|_| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 33) as u8
            })
            .collect();
        t.send_keys(10, &big);
        let mut total = 0;
        let mut now = 10;
        // several rounds: first sends and unacked resends, plus a resize and small keys
        for round in 0..6 {
            if round == 2 {
                t.send_resize(now, 120, 40);
                t.send_keys(now, b"x");
            }
            now += 400;
            for d in drain(&mut t, now) {
                assert!(d.len() <= crate::network::MAX_DATAGRAM, "len {}", d.len());
                total += 1;
            }
        }
        assert!(total > 100, "expected many fragments, got {total}");
    }

    #[test]
    fn keys_go_out_after_collection_delay_and_resend_until_acked() {
        let srv = Server::new();
        let mut t = transport();
        t.send_resize(100, 100, 30);
        t.send_keys(101, b"ls\r");
        assert_eq!(t.next_deadline_ms(), Some(108));
        assert!(drain(&mut t, 107).is_empty());
        let out = drain(&mut t, 108);
        assert_eq!(out.len(), 1);
        let (seq, inner, i) = srv.open_client(&out[0]);
        assert_eq!((seq, inner.frag.id), (0, 1));
        assert_eq!(
            (i.old_num, i.new_num, i.ack_num, i.throwaway_num),
            (0, 1, 0, 0)
        );
        assert_eq!(
            decode_diff(&i.diff).unwrap(),
            vec![
                Element::Resize {
                    width: 100,
                    height: 30
                },
                Element::Data(b"ls\r".to_vec())
            ]
        );
        assert!(i.chaff.len() <= 16);
        // not acked: re-sent one frame slot later, same new_num, fresh seq/id
        assert_eq!(t.next_deadline_ms(), Some(108 + 250)); // SRTT unknown: 250 ms cap
        let out = drain(&mut t, 358);
        let (seq, inner, i) = srv.open_client(&out[0]);
        assert_eq!((seq, inner.frag.id, i.new_num, i.old_num), (1, 2, 1, 0));
        // new keys while unacked: cumulative diff from the acked state, new num
        t.send_keys(360, b"x");
        let out = drain(&mut t, 608);
        let (_, _, i) = srv.open_client(&out[0]);
        assert_eq!((i.old_num, i.new_num), (0, 2));
        let els = decode_diff(&i.diff).unwrap();
        assert_eq!(els.len(), 2);
        assert_eq!(els[1], Element::Data(b"ls\rx".to_vec()));
    }

    #[test]
    fn ack_stops_resend_and_advances_base() {
        let mut srv = Server::new();
        let mut t = transport();
        t.send_keys(0, b"a");
        let _ = drain(&mut t, 8);
        // server acks state 1 inside its first instruction
        let ev = t.handle_datagram(20, &srv.send(5, None, Server::instr(0, 1, 1, &[]))[0]);
        assert_eq!(
            ev,
            vec![Event::HostDiff {
                old_num: 0,
                new_num: 1,
                bytes: vec![]
            }]
        );
        // nothing outstanding: next deadline is the delayed ack (120), not a resend
        assert_eq!(t.next_deadline_ms(), Some(120));
        assert!(drain(&mut t, 119).is_empty());
        let out = drain(&mut t, 120);
        let (_, inner, i) = srv.open_client(&out[0]);
        // empty frame, fresh num, old = acked 1, ack = 1; reply = 5 + 100 held
        assert_eq!(
            (i.old_num, i.new_num, i.ack_num, i.throwaway_num),
            (1, 2, 1, 1)
        );
        assert!(i.diff.is_empty() || decode_diff(&i.diff).unwrap().is_empty());
        assert_eq!(inner.ts_reply, Some(105));
        // later keys diff from state 1
        t.send_keys(200, b"b");
        let (_, _, i) = srv.open_client(&drain(&mut t, 400)[0]);
        assert_eq!((i.old_num, i.new_num), (1, 3));
        assert_eq!(
            decode_diff(&i.diff).unwrap(),
            vec![Element::Data(b"b".to_vec())]
        );
    }

    #[test]
    fn heartbeat_after_three_seconds_idle() {
        let srv = Server::new();
        let mut t = transport();
        assert_eq!(t.next_deadline_ms(), Some(3000));
        assert!(drain(&mut t, 2999).is_empty());
        let (_, _, i) = srv.open_client(&drain(&mut t, 3000)[0]);
        assert_eq!((i.old_num, i.new_num), (0, 1));
        assert_eq!(t.next_deadline_ms(), Some(6000));
        let (_, _, i) = srv.open_client(&drain(&mut t, 6000)[0]);
        assert_eq!(i.new_num, 2);
    }

    #[test]
    fn delivers_in_order_and_drops_duplicates_and_stale() {
        let mut srv = Server::new();
        let mut t = transport();
        let d1 = srv.send(
            0,
            None,
            Server::instr(0, 1, 0, &[Element::Data(b"A".to_vec())]),
        );
        let d3 = srv.send(
            0,
            None,
            Server::instr(1, 3, 0, &[Element::Data(b"C".to_vec())]),
        );
        let d2 = srv.send(
            0,
            None,
            Server::instr(1, 2, 0, &[Element::Data(b"B".to_vec())]),
        );
        let diffs = |ev: Vec<Event>| -> Vec<(u64, u64)> {
            ev.into_iter()
                .filter_map(|e| match e {
                    Event::HostDiff {
                        old_num, new_num, ..
                    } => Some((old_num, new_num)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(diffs(t.handle_datagram(1, &d1[0])), vec![(0, 1)]);
        assert_eq!(diffs(t.handle_datagram(2, &d1[0])), vec![]); // exact duplicate
        assert_eq!(diffs(t.handle_datagram(3, &d3[0])), vec![(1, 3)]); // gap: old != last
        assert_eq!(diffs(t.handle_datagram(4, &d2[0])), vec![]); // stale
                                                                 // ack_num we send is the highest delivered
        let (_, _, i) = srv.open_client(&drain(&mut t, 200)[0]);
        assert_eq!(i.ack_num, 3);
    }

    #[test]
    fn resize_echo_ack_and_throwaway() {
        let mut srv = Server::new();
        let mut t = transport();
        let mut i = Server::instr(
            2,
            4,
            0,
            &[
                Element::Resize {
                    width: 80,
                    height: 24,
                },
                Element::EchoAck(9),
                Element::Data(b"hi".to_vec()),
            ],
        );
        i.throwaway_num = 2;
        let ev = t.handle_datagram(0, &srv.send(0, None, i)[0]);
        assert_eq!(
            ev,
            vec![
                Event::HostResize {
                    new_num: 4,
                    width: 80,
                    height: 24
                },
                Event::EchoAck(9),
                Event::HostDiff {
                    old_num: 2,
                    new_num: 4,
                    bytes: b"hi".to_vec()
                },
            ]
        );
        assert_eq!(t.peer_throwaway(), 2);
    }

    #[test]
    fn rejects_forged_wrong_direction_and_wrong_version() {
        let mut srv = Server::new();
        let mut t = transport();
        let mut d = srv.send(0, None, Server::instr(0, 1, 0, &[]))[0].clone();
        d[12] ^= 1;
        assert!(t.handle_datagram(0, &d).is_empty());
        assert!(t.handle_datagram(0, &[1, 2, 3]).is_empty());
        // a client-direction datagram echoed back must not be accepted
        t.send_keys(0, b"x");
        let own = drain(&mut t, 8);
        assert!(t.handle_datagram(9, &own[0]).is_empty());
        let mut bad = Server::instr(0, 1, 0, &[]);
        bad.version = 3;
        assert!(t.handle_datagram(0, &srv.send(0, None, bad)[0]).is_empty());
    }

    #[test]
    fn rtt_from_reply_sets_frame_interval() {
        let mut srv = Server::new();
        let mut t = transport();
        t.send_keys(1000, b"k");
        let (_, inner, _) = srv.open_client(&drain(&mut t, 1008)[0]);
        // server echoes our ts 1008 after 100 ms of RTT, no hold
        let ev = t.handle_datagram(
            1108,
            &srv.send(7, Some(inner.ts), Server::instr(0, 1, 0, &[]))[0],
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(t.srtt_ms(), 100.0);
        t.send_keys(1200, b"z");
        let first = drain(&mut t, 1208);
        assert_eq!(first.len(), 1);
        assert_eq!(t.next_deadline_ms(), Some(1208 + 50));
    }

    #[test]
    fn shutdown_is_acked_immediately_and_again_on_repeat() {
        let mut srv = Server::new();
        let mut t = transport();
        let d = srv.send(
            0,
            None,
            Server::instr(3, SHUTDOWN_NUM, 0, &[Element::Data(b"bye".to_vec())]),
        );
        let ev = t.handle_datagram(500, &d[0]);
        assert_eq!(ev.last(), Some(&Event::Shutdown));
        assert_eq!(t.next_deadline_ms(), Some(500));
        let (_, _, i) = srv.open_client(&drain(&mut t, 500)[0]);
        assert_eq!(i.ack_num, SHUTDOWN_NUM);
        assert_eq!(t.next_deadline_ms(), None);
        // server repeats it: ack again, no new events
        let d = srv.send(0, None, Server::instr(3, SHUTDOWN_NUM, 0, &[]));
        assert!(t.handle_datagram(532, &d[0]).is_empty());
        assert_eq!(drain(&mut t, 532).len(), 1);
        t.send_keys(600, b"x");
        assert_eq!(t.next_deadline_ms(), None);
    }

    #[test]
    fn fragmented_host_message_reassembles() {
        let mut srv = Server::new();
        let mut t = transport();
        let mut s = 99u64;
        let big: Vec<u8> = (0..6000)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (s >> 33) as u8
            })
            .collect();
        let mut d = srv.send(
            0,
            None,
            Server::instr(0, 1, 0, &[Element::Data(big.clone())]),
        );
        assert!(d.len() >= 5);
        d.reverse();
        let mut ev = Vec::new();
        for x in &d {
            ev.extend(t.handle_datagram(0, x));
        }
        assert_eq!(
            ev,
            vec![Event::HostDiff {
                old_num: 0,
                new_num: 1,
                bytes: big
            }]
        );
    }

    #[test]
    fn datagram_sequence_numbers_never_repeat() {
        let srv = Server::new();
        let mut t = transport();
        let mut seen = std::collections::HashSet::new();
        for k in 0..30u64 {
            t.send_keys(k * 1000, b"x");
            for d in drain(&mut t, k * 1000 + 999) {
                assert!(seen.insert(srv.open_client(&d).0));
            }
        }
        assert!(seen.len() >= 30);
    }
}
