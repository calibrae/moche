//! Client side of the State Synchronization Protocol, sans-IO.
//!
//! The host feeds datagrams and user input with a monotonic millisecond clock
//! and drains datagrams to send. Nothing here sleeps or touches a socket.
//!
//! Sending: user input is a numbered stream of elements. Each frame carries
//! the cumulative diff from the last state the server acked to the latest one
//! (so any lost frame is repaired by the next), bounded to `state_limit` bytes
//! of raw input: a state is any prefix of the input stream, so a larger backlog
//! goes out as a chain of states, each acked before the next is cut.
//!
//! **Invariant (B2, observed against a stock mosh-server 1.4.0, cause
//! inferred; `docs/observations-round5.md`, `docs/protocol.md` section 4):**
//! for any two states ever sent, `new_num_a < new_num_b` implies the content
//! of `b` extends the content of `a` (the input length is non-decreasing in
//! `new_num`), and a resend of a number carries the same cumulative content as
//! its first send. A state that is in flight is therefore NEVER abandoned,
//! shrunk or re-cut: it is resent unchanged (same `new_num`, same content)
//! until acked. Everything here follows from sending only prefixes of one
//! stream, cut at a non-decreasing point.
//!
//! The cap (`state_limit`) is adaptive and applies ONLY to the next cut, never
//! to a state already out. It starts at the floor `MIN_STATE_BYTES` (slow
//! start: a state is resent whole until every fragment of one copy gets
//! through, which costs about `1 / 0.9^fragments` copies at 10% loss, so a
//! 64 KiB first state would be hopeless and, unlike before, cannot be re-cut
//! once out) and doubles up to `MAX_STATE_BYTES`. A big in-flight
//! state counts as lost-suspected when it has been sent at least three times
//! and a full loss RTO (`max(MIN_LOSS_RTO_MS, 1.5 * srtt + 4 * rttvar + ACK_DELAY_MS)`)
//! has passed since its first send without an ack: the cap then halves (floor
//! `MIN_STATE_BYTES`), once per state, so latency alone (acks arrive within
//! that RTO) never shrinks it. The halved cap persists: it doubles again only
//! after `GROW_AFTER_CLEAN` consecutive acked-at-first-send states that
//! filled at least half the cap. While a big state is in flight no new state
//! is cut, so the cap chosen before the state went out is what bounds its cost.
//!
//! Unacked data is re-sent with a fresh datagram sequence number and fragment
//! id, reusing the same `new_num`. Small frames (at most `BIG_STATE_BYTES`
//! of already-sent raw input outstanding) are re-sent after `max(frame slot,
//! SMALL_RESEND_MS, min(SRTT + SMALL_RESEND_MARGIN_MS, MAX_FRAME_MS))` (round 8:
//! the stock client does not resend at the frame slot while typing); new input
//! waits only for the frame slot (about SRTT/2, at least 20 ms) after the last
//! send; a big outstanding state is re-sent at most once
//! per RTO plus the delayed ack (100 ms to 1 s). Idle, an empty frame (with a fresh `new_num`) goes
//! out as a delayed ack after 100 ms, or as a heartbeat after 3 s.
//!
//! Receiving: the transport tracks which server state numbers it can
//! reconstruct (`held`, the same numbers `screen::Screen` holds, pruned by the
//! same throwaway rule). An instruction is delivered only if its `old_num` is
//! held and its `new_num` is newer than the latest held state; the `ack_num` we
//! send is the highest held state, so the server never believes we hold a
//! state we could not apply. An instruction whose base is missing is counted
//! (`rx_gaps`), not delivered, and triggers a re-ack of what we do hold so the
//! server re-diffs from it. Duplicates and stale states are dropped (and
//! re-acked). A shutdown (`new_num == SHUTDOWN_NUM`) is delivered whatever its
//! base: the session is ending and the host only needs the end marker.

use std::collections::{BTreeSet, VecDeque};

use crate::crypto::Session;
#[cfg(test)]
use crate::network::fragment;
use crate::network::{Cipher, Deflater, Direction, Inner, NetError, Opened, Reassembler, Rtt};
use crate::proto::{decode_diff, encode_diff, Element, Instruction, MAX_CHAFF, PROTOCOL_VERSION};

/// `new_num` marking the end of the session.
pub const SHUTDOWN_NUM: u64 = u64::MAX;

/// Lower bound of the frame interval in ms (50 Hz cap).
const MIN_FRAME_MS: u64 = 20;
/// Observed constant added to SRTT/2 in the frame interval, in ms.
const FRAME_EXTRA_MS: f64 = 6.0;
/// Upper bound of the frame interval in ms (moche's choice, untested above 105).
const MAX_FRAME_MS: u64 = 250;
/// Wait after the first unsent input before sending, on top of the frame
/// interval. OBSERVED (round 8, `docs/observations-round8.md`): the stock client
/// sends the first key after idle 2-5 ms after the key (the pty wake-up), with
/// no collection delay of its own; keys that arrive faster than the frame
/// interval are batched by the interval alone. 0 here: the interval does the
/// batching, so fast typing costs no extra datagrams.
const COLLECT_MS: u64 = 0;
/// A small unacked state is re-sent no sooner than this after the last send,
/// whatever the frame interval. OBSERVED (round 8): while typing, the stock
/// client re-sent a key state only 100-112 ms (median 108) after its last send
/// at RTT 100 and 200, and after about 145 ms in the one idle case seen at RTT
/// 100 (and almost never at all: a new key usually supersedes it first), not
/// at the frame interval (about 56 ms at RTT 100, 20 ms at RTT 10).
const SMALL_RESEND_MS: u64 = 110;
/// OUR CHOICE: nor before an ack can have come back, `srtt` plus this (the
/// stock server's ack rode its next frame, up to about 40 ms after the key's
/// arrival in steady typing), capped at `MAX_FRAME_MS`. At RTT 100 this is
/// 150 ms, the observed idle resend; it keeps a re-send from landing just
/// before the next key and delaying it by a frame slot.
const SMALL_RESEND_MARGIN_MS: f64 = 50.0;
/// Delayed ack.
const ACK_DELAY_MS: u64 = 100;
/// Idle heartbeat.
const HEARTBEAT_MS: u64 = 3000;

/// Most raw input bytes one instruction carries (the cumulative diff from the
/// acked state to the state being sent). Incompressible, this is about 54
/// fragments, far below the fragment cap, so a frame is always buildable.
pub const MAX_STATE_BYTES: usize = 64 * 1024;
/// Floor of the adaptive state size.
pub const MIN_STATE_BYTES: usize = 16 * 1024;
/// Largest single `Data` element. Big `send_keys` calls are cut into pieces of
/// this size: it is the cut granularity, so a state at the floor holds four
/// pieces and fills at least 75% of the cap (a coarser piece would waste the
/// cap, a finer one only adds elements).
const CHUNK_BYTES: usize = 4 * 1024;
/// A sent-and-unacked state with more raw data than this is re-sent at most
/// once per RTO instead of every frame slot.
const BIG_STATE_BYTES: usize = 4 * 1024;
/// Bounds of the resend interval for a big state, in ms.
const MIN_RTO_MS: u64 = 100;
const MAX_RTO_MS: u64 = 1000;
/// Loss-suspicion timeout: floor and ceiling in ms. The floor sits above the
/// server's delayed ack (about 100 ms, up to about 220 ms seen at the start of
/// lossy sessions), so a LAN ack that merely waits for it is not a loss.
const MIN_LOSS_RTO_MS: u64 = 250;
const MAX_LOSS_RTO_MS: u64 = 3000;
/// Consecutive clean (acked at first send, cap-filling) states before the cap
/// doubles back after a shrink.
const GROW_AFTER_CLEAN: u32 = 2;

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
    deflater: Deflater,
    rng: Rng,
    created: u64,
    // sending
    seq: u64,
    frag_id: u64,
    /// Unacked user elements; element `i` has global index `acked_count + i`.
    elements: VecDeque<Element>,
    total: u64,
    acked_count: u64,
    /// Data bytes in `elements` (unacked, sent or not).
    queued: u64,
    /// Frames that could not be built or sealed (should stay 0).
    send_errors: u64,
    /// Received datagrams that authenticated but whose message was rejected
    /// (bad fragment, over a size limit, bad zlib, bad instruction or diff).
    rx_errors: u64,
    /// Sent states as `(num, element count)`; the front is the acked state.
    sent: VecDeque<(u64, u64)>,
    /// Next state number to hand out.
    next_num: u64,
    /// Current cap of raw input bytes per state.
    state_limit: usize,
    /// Times loss suspicion halved (or tried to halve) the cap.
    cap_shrinks: u64,
    /// Times the newest unacked state has been sent (0 when none is unacked).
    sends: u32,
    /// When the newest state was first sent.
    first_sent: u64,
    /// The newest state already caused a shrink (at most one per state).
    shrunk: bool,
    /// Consecutive clean cap-filling acks since the last shrink.
    clean_acks: u32,
    unsent_since: Option<u64>,
    last_send: Option<u64>,
    outbox: VecDeque<Vec<u8>>,
    // receiving
    reasm: Reassembler,
    /// Highest held server state; the `ack_num` we send.
    latest: u64,
    /// Server state numbers we can reconstruct (the `Screen` holds the same).
    held: BTreeSet<u64>,
    /// Instructions not delivered because their base state was not held.
    rx_gaps: u64,
    ack_due: Option<u64>,
    peer_throwaway: u64,
    highest_seq: Option<u64>,
    reply: Option<(u16, u64)>,
    rtt: Rtt,
    shutdown: bool,
    // input accounting for display-side prediction (read-only for the protocol)
    /// Cumulative data bytes ever queued with `send_keys`.
    input_bytes: u64,
    /// Data bytes in the elements already acked (dropped from `elements`).
    acked_bytes: u64,
    /// `(state num, cumulative data bytes it carries)` for sent states not yet
    /// passed by an echo ack; bounded by `MAX_ECHO_MARKS`.
    echo_marks: VecDeque<(u64, u64)>,
    /// Cumulative input bytes the server's echo ack has covered.
    echoed_bytes: u64,
}

/// Most `(state, bytes)` marks kept for echo-ack lookup.
const MAX_ECHO_MARKS: usize = 256;

impl Transport {
    /// Creates a transport for `session`. `now_ms` starts the heartbeat clock;
    /// `rng` fills chaff (any source works, it needs no secrecy).
    pub fn new(session: &Session, now_ms: u64, rng: Rng) -> Self {
        Transport {
            cipher: Cipher::new(session),
            deflater: Deflater::new(),
            rng,
            created: now_ms,
            seq: 0,
            frag_id: 1,
            elements: VecDeque::new(),
            total: 0,
            acked_count: 0,
            queued: 0,
            send_errors: 0,
            rx_errors: 0,
            sent: VecDeque::from([(0, 0)]),
            next_num: 1,
            state_limit: MIN_STATE_BYTES,
            cap_shrinks: 0,
            sends: 0,
            first_sent: 0,
            shrunk: false,
            clean_acks: 0,
            unsent_since: None,
            last_send: None,
            outbox: VecDeque::new(),
            reasm: Reassembler::new(),
            latest: 0,
            held: BTreeSet::from([0]),
            rx_gaps: 0,
            ack_due: None,
            peer_throwaway: 0,
            highest_seq: None,
            reply: None,
            rtt: Rtt::default(),
            shutdown: false,
            input_bytes: 0,
            acked_bytes: 0,
            echo_marks: VecDeque::from([(0, 0)]),
            echoed_bytes: 0,
        }
    }

    /// Cumulative input bytes ever queued with [`Transport::send_keys`]. Together
    /// with [`Transport::echoed_input_bytes`] it lets a host tell which of its
    /// keystrokes the server has had time to echo. Read-only: it changes no
    /// behaviour of the transport.
    pub fn input_bytes_total(&self) -> u64 {
        self.input_bytes
    }

    /// Cumulative input bytes covered by the server's latest echo ack: the
    /// server presented them to the application at least about 50 ms ago, so
    /// any echo should already be in the received state. Monotone; 0 until the
    /// first echo ack.
    pub fn echoed_input_bytes(&self) -> u64 {
        self.echoed_bytes
    }

    /// Queues keystrokes (or pasted bytes) for the server.
    pub fn send_keys(&mut self, now_ms: u64, keys: &[u8]) {
        for piece in keys.chunks(CHUNK_BYTES) {
            self.push(now_ms, Element::Data(piece.to_vec()));
        }
    }

    /// Input bytes accepted but not yet acked by the server (backpressure:
    /// a host may pause a big paste while this is large).
    pub fn queued_bytes(&self) -> u64 {
        self.queued
    }

    /// Frames that could not be built or sealed. Expected to stay 0; nonzero
    /// means input is stuck and the host should treat the session as failed.
    pub fn send_errors(&self) -> u64 {
        self.send_errors
    }

    /// Received messages dropped as invalid or over a limit (see
    /// `network::fragment` limits). Nonzero means the server sent something
    /// moche could not accept.
    pub fn rx_errors(&self) -> u64 {
        self.rx_errors
    }

    /// Server instructions dropped because their base state was not held
    /// (a lost earlier state). Each one is answered by a re-ack of the highest
    /// state we do hold, so the server re-diffs from it.
    pub fn rx_gaps(&self) -> u64 {
        self.rx_gaps
    }

    /// Current cap of raw input bytes per sent state (adaptive).
    pub fn state_limit(&self) -> usize {
        self.state_limit
    }

    /// Number of states suspected lost (each halves the cap, down to its floor).
    pub fn cap_shrinks(&self) -> u64 {
        self.cap_shrinks
    }

    /// Server state numbers held (ascending); `Screen::held` has the same.
    pub fn held_states(&self) -> Vec<u64> {
        self.held.iter().copied().collect()
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
        if let Element::Data(b) = &e {
            self.queued += b.len() as u64;
            self.input_bytes += b.len() as u64;
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
        self.receive(now_ms, datagram)
            .map_or_else(Vec::new, |(_, events)| events)
    }

    /// Like [`Transport::handle_datagram`], but also tells the caller whether the
    /// datagram authenticated as a server datagram: `None` for a datagram that is
    /// too short, forged or wrongly directed (dropped), else its sequence number
    /// and the events (possibly none). Lets a host classify and process a
    /// datagram with a single decryption.
    pub fn receive(&mut self, now_ms: u64, datagram: &[u8]) -> Option<(u64, Vec<Event>)> {
        let opened = self.cipher.open(datagram).ok()?;
        if opened.direction != Direction::ToClient {
            return None;
        }
        let seq = opened.seq;
        Some((seq, self.handle_opened(now_ms, opened)))
    }

    fn handle_opened(&mut self, now_ms: u64, opened: Opened) -> Vec<Event> {
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
        let message = match self.reasm.push(inner.frag) {
            Ok(Some(m)) => m,
            Ok(None) => return Vec::new(),
            Err(_) => {
                self.rx_errors += 1;
                return Vec::new();
            }
        };
        match Instruction::decode(&message) {
            Ok(i) if i.version == PROTOCOL_VERSION => self.accept(now_ms, i),
            _ => {
                self.rx_errors += 1;
                Vec::new()
            }
        }
    }

    fn accept(&mut self, now_ms: u64, i: Instruction) -> Vec<Event> {
        self.peer_throwaway = self.peer_throwaway.max(i.throwaway_num);
        self.process_ack(i.ack_num);
        let is_shutdown = i.new_num == SHUTDOWN_NUM;
        if i.new_num <= self.latest || self.held.contains(&i.new_num) {
            // Duplicate or stale: the server may have missed our ack.
            self.schedule_ack(now_ms);
            self.prune_held();
            return Vec::new();
        }
        if !is_shutdown && !self.held.contains(&i.old_num) {
            // A state this one builds on was lost. Do not advance, do not
            // ack `new_num`; tell the server what we do hold.
            self.rx_gaps += 1;
            self.schedule_ack(now_ms);
            self.prune_held();
            return Vec::new();
        }
        let Ok(elements) = decode_diff(&i.diff) else {
            self.rx_errors += 1;
            return Vec::new();
        };
        self.latest = i.new_num;
        self.held.insert(i.new_num);
        self.prune_held();
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
                Element::EchoAck(n) => {
                    self.note_echo(n);
                    events.push(Event::EchoAck(n));
                }
            }
        }
        events.push(Event::HostDiff {
            old_num: i.old_num,
            new_num: i.new_num,
            bytes,
        });
        if is_shutdown {
            self.shutdown = true;
            events.push(Event::Shutdown);
        }
        self.schedule_ack(now_ms);
        events
    }

    /// Records that the echo ack named state `n`: every input byte up to the
    /// newest state numbered at most `n` has been presented to the application.
    fn note_echo(&mut self, n: u64) {
        let Some(p) = self.echo_marks.iter().rposition(|m| m.0 <= n) else {
            return;
        };
        self.echoed_bytes = self.echoed_bytes.max(self.echo_marks[p].1);
        // marks below the matched one can never be asked for again
        self.echo_marks.drain(..p);
    }

    /// Plans an ack frame: at once after shutdown, else after `ACK_DELAY_MS`.
    fn schedule_ack(&mut self, now_ms: u64) {
        let at = if self.shutdown {
            now_ms
        } else {
            now_ms + ACK_DELAY_MS
        };
        self.ack_due = Some(self.ack_due.map_or(at, |d| d.min(at)));
    }

    /// Drops held numbers below the peer's throwaway, always keeping the
    /// highest (the same rule as `Screen::discard_below`).
    fn prune_held(&mut self) {
        let keep = self.held.iter().next_back().copied();
        let below = self.peer_throwaway;
        self.held.retain(|&n| n >= below || Some(n) == keep);
    }

    fn process_ack(&mut self, n: u64) {
        let front = self.sent.front().map_or(0, |s| s.0);
        if n <= front {
            return;
        }
        let Some(p) = self.sent.iter().position(|s| s.0 == n) else {
            return;
        };
        let was_newest = p + 1 == self.sent.len();
        if was_newest {
            // Everything sent is acked. Grow only after clean first-send acks
            // of states that filled the cap (a keystroke ack proves nothing).
            let size = self.bytes_to(self.last_count());
            if self.sends == 1 && size * 2 >= self.state_limit {
                self.clean_acks += 1;
                if self.clean_acks >= GROW_AFTER_CLEAN && self.state_limit < MAX_STATE_BYTES {
                    self.state_limit = (self.state_limit * 2).min(MAX_STATE_BYTES);
                    self.clean_acks = 0;
                }
            } else if self.sends > 1 {
                self.clean_acks = 0;
            }
            self.sends = 0;
            self.shrunk = false;
        }
        self.sent.drain(..p);
        let count = self.sent[0].1;
        let drop = ((count - self.acked_count) as usize).min(self.elements.len());
        for e in self.elements.drain(..drop) {
            if let Element::Data(b) = e {
                self.queued -= b.len() as u64;
                self.acked_bytes += b.len() as u64;
            }
        }
        self.acked_count = count;
    }

    fn last_count(&self) -> u64 {
        self.sent.back().map_or(0, |s| s.1)
    }

    /// Highest element count (global index) the next frame may carry: the
    /// longest prefix of the unacked elements whose data is at most
    /// `state_limit`, but always at least one element.
    fn cut_count(&self) -> u64 {
        let mut bytes = 0usize;
        let mut n = 0u64;
        for e in &self.elements {
            if let Element::Data(b) = e {
                if n > 0 && bytes + b.len() > self.state_limit {
                    break;
                }
                bytes += b.len();
            }
            n += 1;
        }
        self.acked_count + n
    }

    /// Raw data bytes of the unacked elements up to global count `count`.
    fn bytes_to(&self, count: u64) -> usize {
        let n = count.saturating_sub(self.acked_count) as usize;
        self.elements
            .iter()
            .take(n)
            .map(|e| match e {
                Element::Data(b) => b.len(),
                _ => 0,
            })
            .sum()
    }

    /// True while a sent, unacked state carries more than `BIG_STATE_BYTES`.
    fn outstanding_big(&self) -> bool {
        self.last_count() > self.acked_count && self.bytes_to(self.last_count()) > BIG_STATE_BYTES
    }

    /// RTO plus the server's delayed ack: an ack for a big state cannot be
    /// expected sooner, and resending before it arrives only burns bandwidth.
    fn big_resend_interval(&self) -> u64 {
        ((self.rtt.rto_ms() + ACK_DELAY_MS as f64).ceil() as u64).clamp(MIN_RTO_MS, MAX_RTO_MS)
    }

    /// Loss-suspicion timeout in ms, from measured values: the time an ack
    /// of a state may legitimately take (RTT with margin, plus the server's
    /// delayed ack).
    fn loss_rto_ms(&self) -> u64 {
        let v = 1.5 * self.rtt.srtt_ms() + 4.0 * self.rtt.rttvar_ms() + ACK_DELAY_MS as f64;
        (v.ceil() as u64).clamp(MIN_LOSS_RTO_MS, MAX_LOSS_RTO_MS)
    }

    /// Called on each resend of the newest state. Halves the cap, for the
    /// NEXT cut only, when that (big) state has been sent three times and a
    /// full loss RTO has passed since its first send. The state in flight is
    /// never touched (see the module invariant).
    fn note_resend(&mut self, now_ms: u64) {
        if self.shrunk || !self.outstanding_big() || self.sends < 3 {
            return;
        }
        if now_ms.saturating_sub(self.first_sent) < self.loss_rto_ms() {
            return;
        }
        self.shrunk = true;
        self.cap_shrinks += 1;
        self.clean_acks = 0;
        self.state_limit = (self.state_limit / 2).max(MIN_STATE_BYTES);
    }

    /// Element count the next frame carries: the newest sent state again
    /// while a big state is in flight (never re-cut), else the longest prefix
    /// under the cap, never less than what was already sent.
    fn cut_target(&self) -> u64 {
        let last = self.last_count();
        if self.outstanding_big() {
            last
        } else {
            self.cut_count().max(last)
        }
    }

    /// Pause before an unacked small state is sent again.
    fn small_resend_ms(&self) -> u64 {
        let ack_back = (self.rtt.srtt_ms() + SMALL_RESEND_MARGIN_MS).ceil() as u64;
        SMALL_RESEND_MS
            .max(ack_back.min(MAX_FRAME_MS))
            .max(self.frame_interval())
    }

    /// OBSERVED (`docs/protocol.md` section 4, round 2 and round 8): about
    /// SRTT/2 plus 6 ms, at least 20 ms (55 ms at RTT 100, 106 ms at RTT 200).
    fn frame_interval(&self) -> u64 {
        ((self.rtt.srtt_ms() / 2.0 + FRAME_EXTRA_MS).ceil() as u64)
            .clamp(MIN_FRAME_MS, MAX_FRAME_MS)
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
            let big = self.outstanding_big();
            let pace = if big {
                self.big_resend_interval()
            } else {
                self.small_resend_ms()
            };
            let mut t = if self.cut_target() > self.last_count() {
                // new input: one frame interval after the last send of any kind
                let collect = self.unsent_since.unwrap_or(0) + COLLECT_MS;
                let by_interval = self.last_send.map(|t| t + self.frame_interval());
                by_interval.map_or(collect, |b| b.max(collect))
            } else {
                // pure resend of the unacked state
                self.last_send.map_or(0, |t| t + pace)
            };
            // A big state is not re-sent early just to carry a delayed ack.
            if let (Some(a), false) = (self.ack_due, big) {
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
        let (new_num, diff) = if self.data_outstanding() {
            let last_num = self.sent.back().map_or(0, |s| s.0);
            let cut = self.cut_target();
            let n_el = (cut - self.acked_count) as usize;
            let diff = encode_diff(&coalesce(self.elements.iter().take(n_el)));
            if cut > self.last_count() {
                let n = self.next_num;
                self.next_num += 1;
                self.sent.push_back((n, cut));
                let cum = self.acked_bytes + self.bytes_to(cut) as u64;
                if self.echo_marks.len() >= MAX_ECHO_MARKS {
                    // an echo ack older than the oldest kept mark is ignored
                    self.echo_marks.pop_front();
                }
                self.echo_marks.push_back((n, cum));
                self.sends = 1;
                self.first_sent = now_ms;
                self.shrunk = false;
                self.unsent_since = (self.total > cut).then_some(now_ms);
                (n, diff)
            } else {
                self.sends += 1;
                self.note_resend(now_ms);
                (last_num, diff)
            }
        } else {
            let n = self.next_num;
            self.next_num += 1;
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
        if self.emit(now_ms, &message).is_err() {
            self.send_errors += 1;
        }
        self.last_send = Some(now_ms);
        self.ack_due = None;
    }

    /// Fragments, seals and queues one serialized instruction.
    fn emit(&mut self, now_ms: u64, message: &[u8]) -> Result<(), NetError> {
        let id = self.frag_id;
        self.frag_id += 1;
        let frags = self.deflater.fragment(id, message)?;
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
            let d = self
                .cipher
                .seal(Direction::ToServer, seq, &inner.encode())?;
            self.outbox.push_back(d);
        }
        Ok(())
    }

    /// Echo of the peer's latest timestamp plus the time we held it.
    fn take_reply(&mut self, now_ms: u64) -> Option<u16> {
        let (ts, at) = self.reply.take()?;
        let r = ts.wrapping_add(now_ms.saturating_sub(at) as u16);
        Some(if r == 0xffff { 0xfffe } else { r })
    }
}

/// Merges adjacent data elements, keeping order.
fn coalesce<'a>(els: impl Iterator<Item = &'a Element>) -> Vec<Element> {
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
        // 1 MB of incompressible keystrokes: goes out as a chain of bounded states
        // (nothing is acked here, so the first state is resent every slot).
        let mut seed = 99u64;
        let big: Vec<u8> = (0..1_000_000)
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
        assert!(total > 20, "expected many fragments, got {total}");
        assert_eq!(t.send_errors(), 0);
        // only the first bounded state is in flight, not the whole megabyte
        assert!(total < 6 * 70, "too many fragments: {total}");
    }

    #[test]
    fn keys_go_out_after_collection_delay_and_resend_until_acked() {
        let srv = Server::new();
        let mut t = transport();
        t.send_resize(100, 100, 30);
        t.send_keys(100, b"ls\r");
        // no collection delay of its own: the first frame after idle goes at once
        assert_eq!(t.next_deadline_ms(), Some(100));
        let out = drain(&mut t, 100);
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
        assert_eq!(t.next_deadline_ms(), Some(100 + 250)); // SRTT unknown: 250 ms cap
        let out = drain(&mut t, 350);
        let (seq, inner, i) = srv.open_client(&out[0]);
        assert_eq!((seq, inner.frag.id, i.new_num, i.old_num), (1, 2, 1, 0));
        // new keys while unacked: cumulative diff from the acked state, new num
        t.send_keys(352, b"x");
        assert_eq!(t.next_deadline_ms(), Some(350 + 250)); // one frame slot after the resend
        let out = drain(&mut t, 600);
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
                                                                 // skips ahead, but its base (1) is held, so it is applicable
        assert_eq!(diffs(t.handle_datagram(3, &d3[0])), vec![(1, 3)]);
        assert_eq!(diffs(t.handle_datagram(4, &d2[0])), vec![]); // stale
                                                                 // ack_num we send is the highest held
        let (_, _, i) = srv.open_client(&drain(&mut t, 200)[0]);
        assert_eq!(i.ack_num, 3);
    }

    #[test]
    fn resize_echo_ack_and_throwaway() {
        let mut srv = Server::new();
        let mut t = transport();
        let mut i = Server::instr(
            0,
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
        i.throwaway_num = 0;
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
                    old_num: 0,
                    new_num: 4,
                    bytes: b"hi".to_vec()
                },
            ]
        );
        assert_eq!(t.peer_throwaway(), 0);
    }

    #[test]
    fn echo_ack_maps_to_cumulative_input_bytes() {
        let mut srv = Server::new();
        let mut t = transport();
        assert_eq!((t.input_bytes_total(), t.echoed_input_bytes()), (0, 0));
        t.send_keys(10, b"abc");
        t.send_resize(11, 100, 30);
        t.send_keys(12, b"de");
        assert_eq!(t.input_bytes_total(), 5);
        let sent = |t: &mut Transport, srv: &Server, now: u64| {
            let d = drain(t, now);
            srv.open_client(&d[0]).2.new_num
        };
        let n1 = sent(&mut t, &srv, 40);
        // the server echoes the first state only: all five bytes were in it
        // (one state carries everything queued so far), then more keys come
        t.send_keys(50, b"fgh");
        let ack = |srv: &mut Server, t: &mut Transport, n: u64, seq: u64| {
            let i = Server::instr(0, seq, 0, &[Element::EchoAck(n)]);
            t.handle_datagram(60, &srv.send(0, None, i)[0]);
        };
        ack(&mut srv, &mut t, n1, 1);
        assert_eq!(t.echoed_input_bytes(), 5);
        // acked frame, then a new state with the new keys
        let mut now = 100;
        let n2 = loop {
            let d = drain(&mut t, now);
            if let Some(d) = d.first() {
                let n = srv.open_client(d).2.new_num;
                if n > n1 {
                    break n;
                }
            }
            now += 50;
        };
        // an echo ack never goes backwards and an unknown future number maps to
        // the newest state at or below it
        ack(&mut srv, &mut t, n1, 2);
        assert_eq!(t.echoed_input_bytes(), 5);
        ack(&mut srv, &mut t, n2, 3);
        assert_eq!(t.echoed_input_bytes(), 8);
        ack(&mut srv, &mut t, n2 + 40, 4);
        assert_eq!(t.echoed_input_bytes(), 8);
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
        let (_, inner, _) = srv.open_client(&drain(&mut t, 1000)[0]);
        // server echoes our ts 1000 after 100 ms of RTT, no hold
        let ev = t.handle_datagram(
            1100,
            &srv.send(7, Some(inner.ts), Server::instr(0, 1, 0, &[]))[0],
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(t.srtt_ms(), 100.0);
        t.send_keys(1200, b"z");
        let first = drain(&mut t, 1200);
        assert_eq!(first.len(), 1);
        // an unacked small state is re-sent once an ack can have come back (srtt 100 +
        // margin), not after the 50 ms frame slot
        assert_eq!(t.next_deadline_ms(), Some(1200 + 150));
        // new input is paced by the frame interval alone
        t.send_keys(1210, b"y");
        assert_eq!(t.next_deadline_ms(), Some(1200 + 56)); // SRTT/2 + 6
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

    /// Deterministic pseudo-random bytes (incompressible).
    fn noise(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (s >> 33) as u8
            })
            .collect()
    }

    /// Server model of the stock server's apparent receiver rule (B2,
    /// inferred, see `docs/observations-round5.md`): every newer state must
    /// extend every older one, and a number resent must carry the same
    /// content. It PANICS when the transport breaks that. Holds one master
    /// copy of the stream plus (length, resizes) per state, so memory is
    /// the payload, not payload times states.
    #[derive(Default)]
    struct Model {
        master: Vec<u8>,
        /// state num -> (data bytes, resize count) of its content
        states: std::collections::BTreeMap<u64, (usize, usize)>,
        accepted: usize,
    }

    impl Model {
        fn new() -> Self {
            Model {
                states: std::collections::BTreeMap::from([(0, (0, 0))]),
                ..Default::default()
            }
        }

        /// Applies one client instruction; returns its `new_num`.
        fn accept(&mut self, i: &Instruction) -> u64 {
            assert!(
                i.old_num < i.new_num,
                "old {} >= new {}",
                i.old_num,
                i.new_num
            );
            let (mut len, mut rs) = *self.states.get(&i.old_num).expect("unknown base");
            for e in decode_diff(&i.diff).unwrap() {
                match e {
                    Element::Data(b) => {
                        let end = len + b.len();
                        if end <= self.master.len() {
                            assert_eq!(
                                self.master[len..end],
                                b[..],
                                "content diverges from the stream"
                            );
                        } else {
                            let have = self.master.len().saturating_sub(len);
                            assert_eq!(self.master[len..len + have], b[..have], "content diverges");
                            self.master.extend_from_slice(&b[have..]);
                        }
                        len = end;
                    }
                    Element::Resize { .. } => rs += 1,
                    Element::EchoAck(_) => {}
                }
            }
            let content = (len, rs);
            if let Some(prev) = self.states.get(&i.new_num) {
                assert_eq!(
                    *prev, content,
                    "B2: state {} resent with different content",
                    i.new_num
                );
            } else {
                if let Some((m, c)) = self.states.range(..i.new_num).next_back() {
                    assert!(
                        c.0 <= len && c.1 <= rs,
                        "B2: state {} ({len} B) does not extend state {m} ({} B)",
                        i.new_num,
                        c.0
                    );
                }
                if let Some((m, c)) = self.states.range(i.new_num + 1..).next() {
                    assert!(
                        len <= c.0 && rs <= c.1,
                        "B2: state {} ({len} B) is longer than later state {m} ({} B)",
                        i.new_num,
                        c.0
                    );
                }
                self.states.insert(i.new_num, content);
            }
            self.accepted += 1;
            i.new_num
        }

        fn stream_at(&self, num: u64) -> &[u8] {
            &self.master[..self.states[&num].0]
        }
    }

    struct Sim {
        srv: Server,
        reasm: Reassembler,
        model: Model,
        acked: u64,
        reply_num: u64,
        datagrams: usize,
    }

    impl Sim {
        fn new() -> Self {
            Sim {
                srv: Server::new(),
                reasm: Reassembler::new(),
                model: Model::new(),
                acked: 0,
                reply_num: 0,
                datagrams: 0,
            }
        }

        fn receive(&mut self, d: &[u8]) {
            assert!(d.len() <= crate::network::MAX_DATAGRAM);
            self.datagrams += 1;
            let o = self.srv.cipher.open(d).unwrap();
            let inner = Inner::decode(&o.plaintext).unwrap();
            let Some(msg) = self.reasm.push(inner.frag).unwrap() else {
                return;
            };
            let n = self.model.accept(&Instruction::decode(&msg).unwrap());
            self.acked = self.acked.max(n);
        }

        fn ack_datagram(&mut self) -> Vec<u8> {
            self.reply_num += 1;
            let i = Server::instr(0, self.reply_num, self.acked, &[]);
            self.srv.send(0, None, i).remove(0)
        }

        fn stream(&self) -> &[u8] {
            self.model.stream_at(self.acked)
        }
    }

    /// Runs the transport until `want` bytes are delivered and acked.
    /// `lossy(step)` returns (drop ack, duplicate ack).
    fn run(
        t: &mut Transport,
        sim: &mut Sim,
        want: &[u8],
        max_slots: usize,
        mut lossy: impl FnMut(usize) -> (bool, bool),
    ) -> usize {
        let mut now = 0u64;
        for slot in 0..max_slots {
            if let Some(d) = t.next_deadline_ms() {
                now = now.max(d);
            }
            let out = drain(t, now);
            for d in &out {
                sim.receive(d);
            }
            if !out.is_empty() {
                let (drop, dup) = lossy(slot);
                if !drop {
                    let a = sim.ack_datagram();
                    t.handle_datagram(now, &a);
                    if dup {
                        t.handle_datagram(now, &a);
                    }
                }
            }
            now += 1;
            if sim.stream() == want && t.queued_bytes() == 0 {
                return slot + 1;
            }
        }
        panic!(
            "no convergence: delivered {} of {} bytes, queued {}",
            sim.stream().len(),
            want.len(),
            t.queued_bytes()
        );
    }

    #[test]
    fn big_incompressible_send_keys_is_fully_delivered() {
        // The server model keeps one copy of the stream, so 4 MiB costs about
        // 4 MiB (the old per-state copies needed 1.7 GB for 16 MiB).
        for (len, seed) in [(1 << 20, 5u64), (4 << 20, 6)] {
            let data = noise(seed, len);
            let mut t = transport();
            let mut sim = Sim::new();
            t.send_keys(0, &data);
            assert_eq!(t.queued_bytes(), len as u64);
            let slots = run(
                &mut t,
                &mut sim,
                &data,
                len / MAX_STATE_BYTES * 2 + 20,
                |_| (false, false),
            );
            assert!(slots >= len / MAX_STATE_BYTES);
            assert_eq!(t.send_errors(), 0);
            assert_eq!(t.queued_bytes(), 0);
        }
    }

    #[test]
    fn random_sizes_chunks_and_ack_loss_converge() {
        let mut r = 12345u64;
        let mut next = move |m: u64| {
            r = r
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (r >> 33) % m
        };
        for case in 0..12u64 {
            let len = if case == 0 { 0 } else { next(300_000) as usize };
            let data = noise(case + 100, len);
            let mut t = transport();
            let mut sim = Sim::new();
            // random chunking, with a resize and a few tiny sends mixed in
            let mut pos = 0;
            let mut now = 0;
            while pos < data.len() {
                let n = (next(90_000) as usize + 1).min(data.len() - pos);
                t.send_keys(now, &data[pos..pos + n]);
                pos += n;
                if next(4) == 0 {
                    t.send_resize(now, 80 + next(50) as u16, 24);
                }
                now += next(5);
            }
            let mut loss = move |_: usize| (next(3) == 0, next(3) == 0);
            let slots = run(&mut t, &mut sim, &data, 2000, &mut loss);
            assert!(slots > 0 || len == 0);
            assert_eq!(t.send_errors(), 0);
            assert_eq!(t.queued_bytes(), 0);
        }
    }

    #[test]
    fn unbuildable_frame_is_counted_not_dropped_silently() {
        let mut t = transport();
        // an instruction no frame of ours can reach: incompressible, over the cap
        let huge = noise(7, 3 * 1024 * 1024);
        assert_eq!(t.emit(0, &huge), Err(NetError::TooLarge));
        assert!(drain(&mut t, 0).is_empty());
        assert_eq!(t.send_errors(), 0); // emit itself only reports; build_frame counts
                                        // a rejected server message is counted on receive
        let mut srv = Server::new();
        let ev = t.handle_datagram(0, &srv.send(0, None, Server::instr(0, 1, 0, &[]))[0]);
        assert_eq!(ev.len(), 1);
        assert_eq!(t.rx_errors(), 0);
        let mut bad = Server::instr(0, 2, 0, &[]);
        bad.version = 9;
        t.handle_datagram(0, &srv.send(0, None, bad)[0]);
        assert_eq!(t.rx_errors(), 1);
    }

    #[test]
    fn oversized_server_message_is_counted() {
        use crate::network::{Fragment, MAX_FRAGMENTS};
        let mut t = transport();
        let srv = Server::new();
        let inner = Inner {
            ts: 0,
            ts_reply: None,
            frag: Fragment {
                id: 1,
                num: MAX_FRAGMENTS,
                last: false,
                data: vec![0; 10],
            },
        };
        let d = srv
            .cipher
            .seal(Direction::ToClient, 0, &inner.encode())
            .unwrap();
        assert!(t.handle_datagram(0, &d).is_empty());
        assert_eq!(t.rx_errors(), 1);
    }

    #[test]
    fn large_server_repaint_is_accepted() {
        let mut t = transport();
        let mut srv = Server::new();
        // 1.5 MB of noise is ~1250 fragments: over the old 256 cap, under 2048
        let big = noise(3, 1_500_000);
        let d = srv.send(
            0,
            None,
            Server::instr(0, 1, 0, &[Element::Data(big.clone())]),
        );
        assert!(d.len() > 1000);
        let mut ev = Vec::new();
        for x in &d {
            ev.extend(t.handle_datagram(0, x));
        }
        assert_eq!(ev.len(), 1);
        assert_eq!(t.rx_errors(), 0);
    }

    fn diffs_of(ev: &[Event]) -> Vec<(u64, u64)> {
        ev.iter()
            .filter_map(|e| match e {
                Event::HostDiff {
                    old_num, new_num, ..
                } => Some((*old_num, *new_num)),
                _ => None,
            })
            .collect()
    }

    fn client_ack(srv: &Server, t: &mut Transport, now: u64) -> Option<u64> {
        let out = drain(t, now);
        out.first().map(|d| srv.open_client(d).2.ack_num)
    }

    /// B1: a lost state followed by a diff based on it must not be acked.
    #[test]
    fn diff_with_missing_base_is_not_delivered_or_acked() {
        let mut srv = Server::new();
        let mut t = transport();
        let d1 = srv.send(
            0,
            None,
            Server::instr(0, 1, 0, &[Element::Data(b"A".to_vec())]),
        );
        let _lost2 = srv.send(
            0,
            None,
            Server::instr(1, 2, 0, &[Element::Data(b"B".to_vec())]),
        );
        let d3 = srv.send(
            0,
            None,
            Server::instr(2, 3, 0, &[Element::Data(b"C".to_vec())]),
        );
        let d4 = srv.send(
            0,
            None,
            Server::instr(1, 4, 0, &[Element::Data(b"BCD".to_vec())]),
        );
        assert_eq!(diffs_of(&t.handle_datagram(10, &d1[0])), vec![(0, 1)]);
        assert_eq!(t.handle_datagram(20, &d3[0]), vec![]);
        assert_eq!(t.rx_gaps(), 1);
        // a re-ack of what we hold is scheduled, nothing earlier
        // (the delayed ack of state 1, due at 110, is the earlier one)
        assert_eq!(t.next_deadline_ms(), Some(110));
        assert_eq!(client_ack(&srv, &mut t, 109), None);
        assert_eq!(client_ack(&srv, &mut t, 110), Some(1));
        // the server re-diffs from 1: delivered and acked
        let ev = t.handle_datagram(150, &d4[0]);
        assert_eq!(diffs_of(&ev), vec![(1, 4)]);
        assert_eq!(client_ack(&srv, &mut t, 250), Some(4));
        // the gap's throwaway (2) pruned 0 at once and 1 once 4 arrived
        assert_eq!(t.held_states(), vec![4]);
        // the missing state arriving late is stale
        assert_eq!(t.handle_datagram(260, &d3[0]), vec![]);
    }

    #[test]
    fn held_states_follow_throwaway_and_keep_the_highest() {
        let mut srv = Server::new();
        let mut t = transport();
        for (old, new, throw) in [(0, 1, 0), (1, 2, 0), (2, 3, 0)] {
            let mut i = Server::instr(old, new, 0, &[]);
            i.throwaway_num = throw;
            t.handle_datagram(0, &srv.send(0, None, i)[0]);
        }
        assert_eq!(t.held_states(), vec![0, 1, 2, 3]);
        let mut i = Server::instr(3, 4, 0, &[]);
        i.throwaway_num = 2;
        t.handle_datagram(0, &srv.send(0, None, i)[0]);
        assert_eq!(t.held_states(), vec![2, 3, 4]);
        // a throwaway beyond everything still keeps the highest
        let mut i = Server::instr(4, 5, 0, &[]);
        i.throwaway_num = 99;
        t.handle_datagram(0, &srv.send(0, None, i)[0]);
        assert_eq!(t.held_states(), vec![5]);
        // 2 was pruned: a diff based on it is a gap now
        let ev = t.handle_datagram(0, &srv.send(0, None, Server::instr(2, 6, 0, &[]))[0]);
        assert!(ev.is_empty());
        assert_eq!(t.rx_gaps(), 1);
    }

    #[test]
    fn shutdown_with_missing_base_is_still_delivered() {
        let mut srv = Server::new();
        let mut t = transport();
        let d = srv.send(0, None, Server::instr(7, SHUTDOWN_NUM, 0, &[]));
        let ev = t.handle_datagram(5, &d[0]);
        assert_eq!(ev.last(), Some(&Event::Shutdown));
        assert_eq!(t.rx_gaps(), 0);
        assert_eq!(client_ack(&srv, &mut t, 5), Some(SHUTDOWN_NUM));
    }

    /// Lossy server model: states are lost at random, the server optimistically
    /// diffs from the last state it sent (as a stock server does) unless the
    /// client's ack is far behind. We must never ack a state we could not
    /// apply, `Screen::apply` must never miss its base, and we must converge.
    #[test]
    fn lossy_server_never_gets_an_ack_for_an_unapplied_state() {
        use crate::screen::Screen;
        let mut total_gaps = 0;
        for seed in 1..=20u64 {
            let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut rnd = move |m: u64| {
                r = r
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (r >> 33) % m
            };
            let mut srv = Server::new();
            let mut t = transport();
            let mut screen = Screen::new(80, 100);
            let mut applied: std::collections::BTreeSet<u64> = [0].into();
            const LAST: u64 = 40;
            let mut sent_last = 0u64;
            let mut acked = 0u64;
            let mut now = 0u64;

            for _ in 0..4000 {
                now += 50;
                // server tick: new state while any remain, else re-send the last
                let behind = sent_last.saturating_sub(acked) > 2;
                let (old, new) = if sent_last < LAST {
                    sent_last += 1;
                    (if behind { acked } else { sent_last - 1 }, sent_last)
                } else {
                    (acked, LAST)
                };
                if new > acked {
                    let body: Vec<u8> = (old + 1..=new)
                        .flat_map(|n| format!("line {n}\r\n").into_bytes())
                        .collect();
                    let mut i = Server::instr(old, new, 0, &[Element::Data(body)]);
                    i.throwaway_num = acked.min(old);
                    for d in srv.send(0, None, i) {
                        if rnd(10) < 3 {
                            continue; // lost
                        }
                        for ev in t.handle_datagram(now, &d) {
                            if let Event::HostDiff {
                                old_num,
                                new_num,
                                bytes,
                            } = ev
                            {
                                screen
                                    .apply(old_num, new_num, None, &bytes)
                                    .expect("transport delivered a diff the screen cannot apply");
                                applied.insert(new_num);
                            }
                        }
                        screen.discard_below(t.peer_throwaway());
                    }
                    total_gaps += t.rx_gaps();
                }
                // the client answers; 20% of its frames are lost
                for d in drain(&mut t, now + 100) {
                    let i = srv.open_client(&d).2;
                    assert!(
                        applied.contains(&i.ack_num),
                        "seed {seed}: acked {} which was never applied",
                        i.ack_num
                    );
                    if rnd(10) >= 2 {
                        acked = acked.max(i.ack_num);
                    }
                }
                now += 100;
                assert_eq!(t.held_states(), screen.held(), "seed {seed}");
                if acked == LAST {
                    break;
                }
            }
            assert_eq!(acked, LAST, "seed {seed}: did not converge");
            let text = screen.latest().text();
            assert!(
                text.contains("line 1") && text.contains("line 40"),
                "{text}"
            );
        }
        assert!(total_gaps > 0, "the model never produced a gap");
    }

    struct LinkResult {
        ms: u64,
        bytes: usize,
        datagrams: usize,
        limit: usize,
        shrinks: u64,
    }

    /// Behaviour of the virtual link, percentages per datagram.
    #[derive(Clone, Copy)]
    struct LinkCfg {
        loss_up: u64,
        loss_down: u64,
        one_way: u64,
        /// Extra random delay 0..=jitter per datagram (reorders).
        jitter: u64,
        dup: u64,
        /// The server holds each ack this long (its delayed-ack timer).
        ack_hold: u64,
    }

    impl LinkCfg {
        fn new(loss: u64, one_way: u64) -> Self {
            LinkCfg {
                loss_up: loss,
                loss_down: loss,
                one_way,
                jitter: 0,
                dup: 0,
                ack_hold: 0,
            }
        }
    }

    /// Pushes `payload` through a virtual-clock link: `loss_pct` percent loss
    /// each way, `one_way` ms delay each way, a server that acks every
    /// complete instruction at once (echoing our timestamps so the transport
    /// learns the RTT). Returns what the client sent.
    fn link_sim(payload: &[u8], loss_pct: u64, one_way: u64, seed: u64) -> LinkResult {
        link_sim_cfg(&[(0, payload)], LinkCfg::new(loss_pct, one_way), seed)
    }

    /// Like `link_sim` with a typing script `(time, bytes)` and a full link
    /// config. Two server models run: one sees every datagram the transport
    /// EMITS in emission order (the invariant holds for everything ever sent),
    /// one sees what the link delivers (reordered, duplicated). Both panic on
    /// a B2 violation.
    fn link_sim_cfg(script: &[(u64, &[u8])], cfg: LinkCfg, seed: u64) -> LinkResult {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut rnd = move |m: u64| {
            r = r
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (r >> 33) % m
        };
        let want: Vec<u8> = script.iter().flat_map(|(_, b)| b.iter().copied()).collect();
        let mut srv = Server::new();
        let mut t = transport();
        let mut reasm = Reassembler::new();
        let mut emit_reasm = Reassembler::new();
        let mut model = Model::new();
        let mut emit_model = Model::new();
        let (mut acked, mut reply_num) = (0u64, 0u64);
        let mut to_srv: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
        let mut to_cli: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
        let mut ctr = 0u64;
        let (mut now, mut bytes, mut datagrams) = (0u64, 0usize, 0usize);
        let mut next_script = 0usize;
        for _ in 0..5_000_000 {
            let mut next = u64::MAX;
            for q in [&to_srv, &to_cli] {
                if let Some(Reverse((tm, ..))) = q.peek() {
                    next = next.min(*tm);
                }
            }
            if let Some((tm, _)) = script.get(next_script) {
                next = next.min(*tm);
            }
            if let Some(d) = t.next_deadline_ms() {
                next = next.min(d.max(now));
            }
            assert!(next < 3_600_000, "no convergence in an hour of link time");
            now = next.max(now);
            while script.get(next_script).is_some_and(|(tm, _)| *tm <= now) {
                t.send_keys(now, script[next_script].1);
                next_script += 1;
            }
            while to_srv.peek().is_some_and(|Reverse((tm, ..))| *tm <= now) {
                let Reverse((_, _, d)) = to_srv.pop().unwrap();
                let o = srv.cipher.open(&d).unwrap();
                let inner = Inner::decode(&o.plaintext).unwrap();
                let ts = inner.ts;
                let Some(msg) = reasm.push(inner.frag).unwrap() else {
                    continue;
                };
                acked = acked.max(model.accept(&Instruction::decode(&msg).unwrap()));
                reply_num += 1;
                let reply = Server::instr(0, reply_num, acked, &[]);
                let hold = cfg.ack_hold;
                for d in srv.send(0, Some(ts.wrapping_add(hold as u16)), reply) {
                    if rnd(100) >= cfg.loss_down {
                        ctr += 1;
                        let at = now + hold + cfg.one_way + rnd(cfg.jitter + 1);
                        to_cli.push(Reverse((at, ctr, d)));
                    }
                }
            }
            while to_cli.peek().is_some_and(|Reverse((tm, ..))| *tm <= now) {
                let Reverse((_, _, d)) = to_cli.pop().unwrap();
                t.handle_datagram(now, &d);
            }
            while let Some(d) = t.poll_transmit(now) {
                bytes += d.len();
                datagrams += 1;
                let o = srv.cipher.open(&d).unwrap();
                let inner = Inner::decode(&o.plaintext).unwrap();
                if let Some(msg) = emit_reasm.push(inner.frag).unwrap() {
                    emit_model.accept(&Instruction::decode(&msg).unwrap());
                }
                let copies = 1 + usize::from(rnd(100) < cfg.dup);
                for _ in 0..copies {
                    if rnd(100) >= cfg.loss_up {
                        ctr += 1;
                        let at = now + cfg.one_way + rnd(cfg.jitter + 1);
                        to_srv.push(Reverse((at, ctr, d.clone())));
                    }
                }
            }
            if next_script == script.len()
                && model.stream_at(acked) == want
                && t.queued_bytes() == 0
            {
                assert_eq!(t.send_errors(), 0);
                return LinkResult {
                    ms: now,
                    bytes,
                    datagrams,
                    limit: t.state_limit(),
                    shrinks: t.cap_shrinks(),
                };
            }
        }
        panic!("simulation did not finish");
    }

    /// 1 MiB of incompressible input over a lossy, delayed link: completes,
    /// and the client does not send many times the payload.
    #[test]
    fn one_mib_over_lossy_link_stays_cheap() {
        let payload = noise(77, 1 << 20);
        let n = payload.len() as f64;
        let run_set = |loss: u64, delay: u64, seeds: &[u64]| -> f64 {
            let mut worst = 0f64;
            for &sd in seeds {
                let x = link_sim(&payload, loss, delay, sd);
                let ratio = x.bytes as f64 / n;
                eprintln!(
                    "link loss {loss:>2}% one-way {delay:>3} ms seed {sd}: {:>6} ms, {:>5} datagrams, {:>8} B sent = {ratio:.2}x payload, final limit {} KiB",
                    x.ms, x.datagrams, x.bytes, x.limit / 1024
                );
                worst = worst.max(ratio);
            }
            worst
        };
        assert!(run_set(0, 50, &[1]) <= 1.5, "clean link too costly");
        assert!(
            run_set(10, 50, &[1, 2, 3, 4]) <= 8.0,
            "lossy link too costly"
        );
        assert!(run_set(10, 1, &[1, 2]) <= 8.0, "lossy LAN too costly");
        assert!(
            run_set(10, 100, &[1, 2]) <= 8.0,
            "lossy slow link too costly"
        );
    }

    fn open_all(srv: &Server, out: &[Vec<u8>]) -> Instruction {
        let mut r = Reassembler::new();
        let mut msg = None;
        for d in out {
            let o = srv.cipher.open(d).unwrap();
            let inner = Inner::decode(&o.plaintext).unwrap();
            msg = r.push(inner.frag).unwrap().or(msg);
        }
        Instruction::decode(&msg.unwrap()).unwrap()
    }

    /// The exact B2 repro: state 1 (big) is in flight, the loss trigger fires
    /// and the cap halves. The next frame must be state 1 again, unchanged
    /// (never a new number with less data); the smaller cap only shapes the
    /// cut after the ack, which must extend state 1.
    #[test]
    fn b2_shrink_never_recuts_the_state_in_flight() {
        let mut srv = Server::new();
        let mut model = Model::new();
        let mut t = transport();
        let data = noise(5, 400_000);
        t.send_keys(0, &data);
        assert_eq!(t.state_limit(), MIN_STATE_BYTES, "slow start");
        t.state_limit = MAX_STATE_BYTES; // as after a clean run of acks
        let mut now = 8;
        let first = open_all(&srv, &drain(&mut t, now));
        assert_eq!((first.old_num, first.new_num), (0, 1));
        model.accept(&first);
        let first_len = decode_diff(&first.diff)
            .unwrap()
            .iter()
            .map(|e| match e {
                Element::Data(b) => b.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert!(first_len > MAX_STATE_BYTES / 2);
        let mut shrunk = false;
        for _ in 0..20 {
            now = t.next_deadline_ms().unwrap();
            let i = open_all(&srv, &drain(&mut t, now));
            model.accept(&i); // panics if state 1 changed or a shorter newer state appears
            assert_eq!(
                (i.old_num, i.new_num),
                (0, 1),
                "the state in flight is resent as is"
            );
            assert_eq!(i.diff, first.diff);
            if t.state_limit() < MAX_STATE_BYTES {
                shrunk = true;
                break;
            }
        }
        assert!(shrunk, "sustained silence must be suspected as loss");
        assert!(now >= MIN_LOSS_RTO_MS, "not before a full loss RTO: {now}");
        assert_eq!(t.state_limit(), MAX_STATE_BYTES / 2);
        // one more resend after the shrink: still state 1
        now = t.next_deadline_ms().unwrap();
        let i = open_all(&srv, &drain(&mut t, now));
        assert_eq!((i.old_num, i.new_num), (0, 1));
        assert_eq!(i.diff, first.diff);
        // ack: the next cut uses the halved cap and extends state 1
        t.handle_datagram(now + 10, &srv.send(0, None, Server::instr(0, 1, 1, &[]))[0]);
        let next = t.next_deadline_ms().unwrap();
        let i = open_all(&srv, &drain(&mut t, next));
        model.accept(&i);
        assert_eq!((i.old_num, i.new_num), (1, 2));
        let len2: usize = decode_diff(&i.diff)
            .unwrap()
            .iter()
            .map(|e| match e {
                Element::Data(b) => b.len(),
                _ => 0,
            })
            .sum();
        assert!(
            len2 <= MAX_STATE_BYTES / 2 && len2 > MIN_STATE_BYTES / 2,
            "{len2}"
        );
        // acked at its first send, filling the cap: two such acks regrow it
        t.handle_datagram(
            next + 10,
            &srv.send(0, None, Server::instr(0, 2, 2, &[]))[0],
        );
        assert_eq!(
            t.state_limit(),
            MAX_STATE_BYTES / 2,
            "one clean ack is not enough"
        );
    }

    #[test]
    fn halved_cap_persists_and_regrows_after_clean_acks() {
        let mut srv = Server::new();
        let mut t = transport();
        t.send_keys(0, &noise(5, 2_000_000));
        t.state_limit = MAX_STATE_BYTES;
        let mut now = 8;
        let mut num = 0u64;
        // state 1 never acked in time: halved to 32 KiB
        for _ in 0..30 {
            drain(&mut t, now);
            if t.state_limit() < MAX_STATE_BYTES {
                break;
            }
            now = t.next_deadline_ms().unwrap();
        }
        assert_eq!(t.state_limit(), 32 * 1024);
        // ack it late (not clean); then three states acked at first send
        num += 1;
        t.handle_datagram(now, &srv.send(0, None, Server::instr(0, 1, num, &[]))[0]);
        assert_eq!(t.state_limit(), 32 * 1024, "a late ack does not regrow");
        let mut seen = Vec::new();
        for k in 0..4u64 {
            now = t.next_deadline_ms().unwrap();
            let i = open_all(&srv, &drain(&mut t, now));
            num = i.new_num;
            t.handle_datagram(
                now + 5,
                &srv.send(0, None, Server::instr(0, 10 + k, num, &[]))[0],
            );
            seen.push(t.state_limit());
        }
        // the first of these was cut under 32 KiB and acked clean (1 of 2),
        // the second regrows to 64 KiB
        assert_eq!(seen[0], 32 * 1024);
        assert_eq!(seen[1], MAX_STATE_BYTES);
    }

    /// Pure latency, even with a server that holds its acks, is not loss.
    #[test]
    fn latency_alone_never_shrinks_the_cap() {
        let payload = noise(11, 1 << 20);
        for (one_way, hold) in [(1, 100), (50, 100), (150, 0), (150, 100), (300, 100)] {
            let mut cfg = LinkCfg::new(0, one_way);
            cfg.ack_hold = hold;
            let x = link_sim_cfg(&[(0, &payload)], cfg, 3);
            eprintln!(
                "latency {one_way} ms hold {hold}: shrinks {}, final cap {} KiB, {:.2}x",
                x.shrinks,
                x.limit / 1024,
                x.bytes as f64 / payload.len() as f64
            );
            assert_eq!(x.shrinks, 0, "latency {one_way} hold {hold} suspected loss");
        }
    }

    #[test]
    fn lossless_four_mib_paste_never_shrinks_the_cap() {
        let payload = noise(12, 4 << 20);
        for (one_way, hold) in [(0, 0), (1, 100), (50, 100)] {
            let mut cfg = LinkCfg::new(0, one_way);
            cfg.ack_hold = hold;
            let x = link_sim_cfg(&[(0, &payload)], cfg, 4);
            let ratio = x.bytes as f64 / payload.len() as f64;
            eprintln!("4 MiB lossless one-way {one_way} hold {hold}: {ratio:.2}x, final cap {} KiB, {} ms", x.limit / 1024, x.ms);
            assert_eq!(x.shrinks, 0, "one-way {one_way} hold {hold}");
            assert_eq!(x.limit, MAX_STATE_BYTES, "slow start reached the full cap");
            assert!(ratio <= 1.5, "{ratio}");
        }
    }

    #[test]
    fn sustained_loss_shrinks_the_cap() {
        let payload = noise(13, 1 << 20);
        for one_way in [1, 50] {
            let x = link_sim(&payload, 10, one_way, 5);
            assert!(x.shrinks > 0, "one-way {one_way}: loss never suspected");
            assert!(x.limit >= MIN_STATE_BYTES);
        }
    }

    /// Property test: seeded random links (loss each way, jitter that
    /// reorders, duplication, delayed acks) with a typing script; the
    /// invariant holds on every datagram emitted and on every one delivered
    /// (both models panic otherwise), and the stream always converges.
    #[test]
    fn property_random_links_keep_the_state_invariant() {
        let mut r = 0xB2B2u64;
        let mut next = move |m: u64| {
            r = r
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (r >> 33) % m
        };
        for case in 0..40u64 {
            let cfg = LinkCfg {
                loss_up: next(25),
                loss_down: next(40),
                one_way: next(120),
                jitter: next(80),
                dup: next(20),
                ack_hold: next(3) * 50,
            };
            let mut chunks: Vec<(u64, Vec<u8>)> = Vec::new();
            let mut at = 0;
            for k in 0..(1 + next(6)) {
                let len = match next(3) {
                    0 => 1 + next(20),
                    1 => 1 + next(60_000),
                    _ => 1 + next(200_000),
                } as usize;
                chunks.push((at, noise(case * 10 + k, len)));
                at += next(3000);
            }
            let script: Vec<(u64, &[u8])> =
                chunks.iter().map(|(t, b)| (*t, b.as_slice())).collect();
            let x = link_sim_cfg(&script, cfg, case + 1);
            assert!(x.datagrams > 0);
        }
    }

    #[test]
    fn big_resends_wait_an_rto_but_keystrokes_keep_the_frame_slot() {
        let mut srv = Server::new();
        let mut t = transport();
        // learn a 100 ms RTT (rto = 300 ms, plus 100 ms delayed ack), 50 ms frame slot
        t.send_keys(1000, b"k");
        let (_, inner, _) = srv.open_client(&drain(&mut t, 1000)[0]);
        t.handle_datagram(
            1100,
            &srv.send(7, Some(inner.ts), Server::instr(0, 1, 1, &[]))[0],
        );
        assert_eq!(t.srtt_ms(), 100.0);
        // a big paste: first send at once, resend one RTO later
        t.state_limit = MAX_STATE_BYTES;
        t.send_keys(1200, &noise(9, 30_000));
        assert!(!drain(&mut t, 1200).is_empty());
        assert_eq!(t.next_deadline_ms(), Some(1200 + 400));
        assert!(drain(&mut t, 1599).is_empty());
        assert!(!drain(&mut t, 1600).is_empty());
        // acked: the stream is idle again and a keystroke goes within one frame slot
        t.handle_datagram(1700, &srv.send(7, None, Server::instr(0, 2, 2, &[]))[0]);
        t.send_keys(2000, b"x");
        let d = t.next_deadline_ms().unwrap();
        assert!(d <= 2000 + 50, "keystroke deadline {d}");
        let out = drain(&mut t, d);
        assert_eq!(out.len(), 1);
        // sent, unacked, small: re-sent after srtt + margin, not an RTO
        assert_eq!(t.next_deadline_ms(), Some(d + 150));
    }

    // ---- round 8: typing pattern and the ack/resend policy ------------------

    /// One datagram the client sent in a typing run.
    #[derive(Debug, Clone, Copy)]
    struct Dg {
        at: u64,
        new_num: u64,
        empty: bool,
        ack: u64,
    }

    struct Typing {
        keys: usize,
        first_key: u64,
        last_key: u64,
        /// Every datagram the client sent.
        sent: Vec<Dg>,
        /// Arrival times of server frames at the client.
        server_frames: Vec<u64>,
    }

    impl Typing {
        /// Datagrams from the first key to 1.5 s after the last (the window the
        /// wire analysis of round 8 uses).
        fn window(&self) -> Vec<Dg> {
            self.sent
                .iter()
                .copied()
                .filter(|d| d.at >= self.first_key && d.at <= self.last_key + 1500)
                .collect()
        }

        fn per_key(&self) -> f64 {
            self.window().len() as f64 / self.keys as f64
        }
    }

    /// `keys` keystrokes 80..=120 ms apart (seeded) against a model of the stock
    /// server's reply pattern: for every client instruction carrying a NEW state
    /// it sends a data frame `hold` ms later (acking that state) and a second
    /// frame 55 ms after it (OBSERVED, round 8: two server frames per key about
    /// 55 ms apart); it does not reply to empty or repeated frames. Nothing is
    /// lost. The state invariants are checked by `Model`.
    fn typing_run(one_way: u64, hold: u64, keys: usize, seed: u64) -> Typing {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut rnd = move |m: u64| {
            r = r
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (r >> 33) % m
        };
        let mut srv = Server::new();
        let mut model = Model::new();
        let mut reasm = Reassembler::new();
        let mut t = transport();
        let mut key_at = Vec::new();
        let mut x = 1000u64;
        for _ in 0..keys {
            key_at.push(x);
            x += 80 + rnd(41);
        }
        let mut to_srv: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
        let mut to_cli: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
        // server frames to send: (time, ctr)
        let mut srv_out: BinaryHeap<Reverse<(u64, u64)>> = BinaryHeap::new();
        let (mut ctr, mut acked, mut srv_num) = (0u64, 0u64, 0u64);
        let (mut last_ts, mut last_rx) = (0u16, 0u64);
        let mut run = Typing {
            keys,
            first_key: key_at[0],
            last_key: *key_at.last().unwrap(),
            sent: Vec::new(),
            server_frames: Vec::new(),
        };
        let end = run.last_key + 4000;
        let mut next_key = 0usize;
        let mut now = 0u64;
        loop {
            let mut next = u64::MAX;
            if let Some(k) = key_at.get(next_key) {
                next = next.min(*k);
            }
            if let Some(Reverse((tm, ..))) = to_srv.peek() {
                next = next.min(*tm);
            }
            if let Some(Reverse((tm, ..))) = to_cli.peek() {
                next = next.min(*tm);
            }
            if let Some(Reverse((tm, _))) = srv_out.peek() {
                next = next.min(*tm);
            }
            if let Some(d) = t.next_deadline_ms() {
                next = next.min(d.max(now));
            }
            now = next.max(now);
            if now > end {
                return run;
            }
            while key_at.get(next_key).is_some_and(|k| *k <= now) {
                t.send_keys(now, b"x");
                next_key += 1;
            }
            while to_srv.peek().is_some_and(|Reverse((tm, ..))| *tm <= now) {
                let Reverse((_, _, d)) = to_srv.pop().unwrap();
                let o = srv.cipher.open(&d).unwrap();
                let inner = Inner::decode(&o.plaintext).unwrap();
                last_ts = inner.ts;
                last_rx = now;
                let Some(msg) = reasm.push(inner.frag).unwrap() else {
                    continue;
                };
                let i = Instruction::decode(&msg).unwrap();
                let was = acked;
                acked = acked.max(model.accept(&i));
                if acked > was && !i.diff.is_empty() {
                    ctr += 1;
                    srv_out.push(Reverse((now + hold, ctr)));
                    ctr += 1;
                    srv_out.push(Reverse((now + hold + 55, ctr)));
                }
            }
            while srv_out.peek().is_some_and(|Reverse((tm, _))| *tm <= now) {
                srv_out.pop();
                let reply = last_ts.wrapping_add(now.saturating_sub(last_rx) as u16);
                let ins =
                    Server::instr(srv_num, srv_num + 1, acked, &[Element::Data(b"x".to_vec())]);
                srv_num += 1;
                for d in srv.send(now as u16, Some(reply), ins) {
                    ctr += 1;
                    to_cli.push(Reverse((now + one_way, ctr, d)));
                }
            }
            while to_cli.peek().is_some_and(|Reverse((tm, ..))| *tm <= now) {
                let Reverse((_, _, d)) = to_cli.pop().unwrap();
                run.server_frames.push(now);
                t.handle_datagram(now, &d);
            }
            while let Some(d) = t.poll_transmit(now) {
                let (_, _, i) = srv.open_client(&d);
                run.sent.push(Dg {
                    at: now,
                    new_num: i.new_num,
                    empty: i.diff.is_empty(),
                    ack: i.ack_num,
                });
                ctr += 1;
                to_srv.push(Reverse((now + one_way, ctr, d)));
            }
        }
    }

    /// Client datagrams per keystroke at 10 keys/s, against the per-key counts
    /// measured on the stock client through the same kind of delay (round 8,
    /// `docs/observations-round8.md`, one-way 5/25/50/100 ms). The sim server
    /// is a model with a fixed reply delay, the stock numbers are one 30 s run
    /// each (and at one-way 25 include standalone acks the model does not
    /// provoke), so this is an envelope: never more than 20 % above the stock
    /// count (the real-network runs are +9 % at one-way 50 and -2 % at 100: the
    /// stock client re-sent a key state less often than a plain 110 ms timer
    /// predicts, cause not found; the sim server answers at a fixed delay) (the old policy sent 1.5 to 2.6 times as many), never below
    /// 75 % of it (that would mean keys are being held back).
    #[test]
    fn typing_datagram_count_matches_the_stock_pattern() {
        // (one-way ms, stock datagrams per key)
        let mut bad = Vec::new();
        for (ow, stock) in [(5u64, 1.05f64), (25, 1.25), (50, 1.06), (100, 0.86)] {
            for seed in 1..=4 {
                let run = typing_run(ow, 30, 300, seed);
                let pk = run.per_key();
                eprintln!("one-way {ow:>3} ms seed {seed}: {pk:.3} datagrams/key (stock {stock})");
                if pk > stock * 1.20 || pk < stock * 0.75 {
                    bad.push((ow, seed, pk));
                }
            }
        }
        assert!(bad.is_empty(), "outside the stock envelope: {bad:?}");
    }

    /// A standalone ack never goes out when a keystroke is within the ack
    /// window: the key's own frame carries the ack. A key outside the window
    /// finds the ack already sent.
    #[test]
    fn no_standalone_ack_when_a_keystroke_is_within_the_ack_window() {
        let mut srv = Server::new();
        let mut t = transport();
        t.handle_datagram(1000, &srv.send(5, None, Server::instr(0, 1, 0, &[]))[0]);
        assert_eq!(t.next_deadline_ms(), Some(1000 + ACK_DELAY_MS));
        t.send_keys(1050, b"a");
        let out = drain(&mut t, 1050);
        assert_eq!(out.len(), 1);
        let (_, _, i) = srv.open_client(&out[0]);
        assert_eq!((i.ack_num, i.diff.is_empty()), (1, false));
        // the ack was owed once and is now settled: nothing at 1100
        assert!(drain(&mut t, 1100).is_empty());
        // a key outside the window: the empty ack went first, at the bound
        t.handle_datagram(5000, &srv.send(5, None, Server::instr(1, 2, 1, &[]))[0]);
        let out = drain(&mut t, 5000 + ACK_DELAY_MS);
        let (_, _, i) = srv.open_client(&out[0]);
        assert_eq!((i.ack_num, i.diff.is_empty()), (2, true));
    }

    /// While typing at 10 keys/s the standalone (empty) acks are a small
    /// fraction of the keys: the stock client sent 0 to 5 %.
    #[test]
    fn typing_sends_few_standalone_acks() {
        for ow in [5u64, 25, 50, 100] {
            let run = typing_run(ow, 30, 300, 7);
            let empties = run
                .window()
                .iter()
                .filter(|d| d.empty && d.at <= run.last_key)
                .count();
            assert!(
                empties * 100 <= 12 * run.keys,
                "one-way {ow}: {empties} standalone acks for {} keys",
                run.keys
            );
        }
    }

    /// A state is only sent again at least one ack delay (100 ms: the resend
    /// that carries a due ack, else SMALL_RESEND_MS) after its previous send,
    /// and the typing run never re-sends more than a third of its keys at 10
    /// keys/s.
    #[test]
    fn resends_only_after_the_small_resend_timeout() {
        for ow in [5u64, 50, 100] {
            let run = typing_run(ow, 30, 300, 3);
            let mut last: std::collections::HashMap<u64, u64> = Default::default();
            let mut resends = 0;
            for d in run.sent.iter().filter(|d| !d.empty) {
                if let Some(prev) = last.insert(d.new_num, d.at) {
                    resends += 1;
                    assert!(
                        d.at - prev >= ACK_DELAY_MS,
                        "one-way {ow}: state {} re-sent after {} ms",
                        d.new_num,
                        d.at - prev
                    );
                }
            }
            assert!(
                resends <= 100,
                "one-way {ow}: {resends} resends for 300 keys"
            );
        }
    }

    /// Idle after typing: the last server frame is acked within the delayed-ack
    /// bound, and the heartbeat keeps its 3 s spacing.
    #[test]
    fn idle_ack_within_the_bound_and_heartbeat_unchanged() {
        let run = typing_run(50, 30, 60, 5);
        let last_frame = *run.server_frames.last().unwrap();
        let ack = run
            .sent
            .iter()
            .find(|d| d.at >= last_frame && d.ack >= 1)
            .expect("ack sent");
        assert!(
            ack.at - last_frame <= ACK_DELAY_MS,
            "ack {} ms after the last server frame",
            ack.at - last_frame
        );
        let beats: Vec<u64> = run
            .sent
            .iter()
            .filter(|d| d.empty && d.at > last_frame + ACK_DELAY_MS)
            .map(|d| d.at)
            .collect();
        for w in beats.windows(2) {
            assert_eq!(w[1] - w[0], HEARTBEAT_MS);
        }
    }

    /// Whatever the mix of keystrokes, pastes and server frames, an ack the
    /// client owes always goes out within a bounded time (otherwise the server
    /// would retransmit forever): at most the delayed-ack time when only small
    /// frames are in flight, at most one big-state resend interval plus that
    /// when a paste is.
    #[test]
    fn property_an_owed_ack_is_always_sent_in_bounded_time() {
        for seed in 1..=60u64 {
            let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut rnd = move |m: u64| {
                r = r
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (r >> 33) % m
            };
            let pastes = seed % 3 == 0;
            let mut srv = Server::new();
            let mut reasm = Reassembler::new();
            let mut t = transport();
            let mut srv_num = 0u64;
            let mut now = 0u64;
            // (time, kind): kind 0 = server frame, 1 = keys, 2 = paste
            let mut events: Vec<(u64, u8)> = Vec::new();
            let mut at = 0u64;
            for _ in 0..80 {
                at += 1 + rnd(400);
                let kind = match rnd(10) {
                    0..=3 => 0,
                    4..=8 => 1,
                    _ if pastes => 2,
                    _ => 1,
                };
                events.push((at, kind));
            }
            let end = at + 5000;
            let bound = if pastes {
                MAX_RTO_MS + ACK_DELAY_MS + 1
            } else {
                ACK_DELAY_MS
            };
            let mut next_ev = 0usize;
            let mut owed: Option<u64> = None;
            loop {
                let mut next = events.get(next_ev).map_or(u64::MAX, |e| e.0);
                if let Some(d) = t.next_deadline_ms() {
                    next = next.min(d.max(now));
                }
                now = next.max(now);
                assert!(
                    owed.is_none_or(|o| now - o <= bound),
                    "seed {seed}: ack owed since {} not sent by {now}",
                    owed.unwrap()
                );
                if now > end {
                    break;
                }
                while events.get(next_ev).is_some_and(|e| e.0 <= now) {
                    match events[next_ev].1 {
                        0 => {
                            let ins = Server::instr(srv_num, srv_num + 1, 0, &[]);
                            srv_num += 1;
                            for d in srv.send(now as u16, None, ins) {
                                t.handle_datagram(now, &d);
                            }
                            owed.get_or_insert(now);
                        }
                        1 => t.send_keys(now, b"k"),
                        _ => t.send_keys(now, &noise(now, 20_000)),
                    }
                    next_ev += 1;
                }
                while let Some(d) = t.poll_transmit(now) {
                    let o = srv.cipher.open(&d).unwrap();
                    let inner = Inner::decode(&o.plaintext).unwrap();
                    if let Some(msg) = reasm.push(inner.frag).unwrap() {
                        if Instruction::decode(&msg).unwrap().ack_num >= srv_num {
                            owed = None;
                        }
                    }
                }
            }
        }
    }
}
