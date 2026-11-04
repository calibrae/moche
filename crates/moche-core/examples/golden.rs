//! Golden/differential scenario for the sans-IO core.
//!
//! Runs a deterministic scripted session (typing, a lossy paste, heartbeats, a
//! resize, a 70-fragment server repaint) against a tiny simulated server and
//! prints, one line per event: every datagram the client emits (time, length,
//! hash of the sealed bytes: the Cipher nonce is the datagram sequence number so
//! sealing is reproducible), every `next_deadline_ms` change, every event the
//! client decoded, and the hash of the host paint. Compare the output of two
//! builds with `diff`: it must be byte-identical for a wire-neutral change.
//!
//! `GOLDEN_FULL=1` prints the full hex of each datagram instead of its hash.
//! Uses only the public API (so it also builds against older commits).

use std::collections::{BTreeMap, VecDeque};

use moche_core::crypto::Session;
use moche_core::network::{fragment, Cipher, Direction, Fragment, Inner, Reassembler};
use moche_core::proto::{decode_diff, encode_diff, Element, Instruction};
use moche_core::screen::Screen;
use moche_core::ssp::{Event, Transport};

const KEY: &str = "Ixu2un/jwqLpFzsnDHm5lw";

fn fnv(b: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &x in b {
        h ^= u64::from(x);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 24
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Server model: applies user input, echoes it, resends unacked frames.
struct Server {
    cipher: Cipher,
    seq: u64,
    fid: u64,
    reasm: Reassembler,
    user_applied: u64,
    /// host stream and, per state number, its length
    stream: Vec<u8>,
    states: BTreeMap<u64, usize>,
    next: u64,
    acked: u64,                     // highest host state the client acked
    last_frame: Option<(u64, u64)>, // (new, time sent)
    pending_resize: Option<(u16, u16)>,
    rng: Lcg,
}

impl Server {
    fn new() -> Self {
        let mut states = BTreeMap::new();
        states.insert(0, 0);
        Server {
            cipher: Cipher::new(&Session::from_base64(KEY).unwrap()),
            seq: 0,
            fid: 1,
            reasm: Reassembler::new(),
            user_applied: 0,
            stream: Vec::new(),
            states,
            next: 1,
            acked: 0,
            last_frame: None,
            pending_resize: None,
            rng: Lcg(99),
        }
    }

    fn send_frame(&mut self, t: u64, new: u64, with_resize: bool) -> Vec<Vec<u8>> {
        let old = self.acked;
        let from = self.states[&old];
        let to = self.states[&new];
        let mut els = Vec::new();
        if with_resize {
            if let Some((w, h)) = self.pending_resize {
                els.push(Element::Resize {
                    width: w,
                    height: h,
                });
            }
        }
        if to > from {
            els.push(Element::Data(self.stream[from..to].to_vec()));
        }
        els.push(Element::EchoAck(self.user_applied));
        let ins = Instruction {
            version: 2,
            old_num: old,
            new_num: new,
            ack_num: self.user_applied,
            throwaway_num: old,
            diff: encode_diff(&els),
            chaff: vec![],
        };
        let id = self.fid;
        self.fid += 1;
        let mut out = Vec::new();
        for frag in fragment(id, &ins.encode()).unwrap() {
            let inner = Inner {
                ts: t as u16,
                ts_reply: Some(t as u16),
                frag: Fragment { ..frag },
            };
            let seq = self.seq;
            self.seq += 1;
            out.push(
                self.cipher
                    .seal(Direction::ToClient, seq, &inner.encode())
                    .unwrap(),
            );
        }
        self.last_frame = Some((new, t));
        out
    }

    /// New host output: a state with these bytes appended.
    fn publish(&mut self, t: u64, bytes: &[u8], resize: bool) -> Vec<Vec<u8>> {
        self.stream.extend_from_slice(bytes);
        let n = self.next;
        self.next += 1;
        self.states.insert(n, self.stream.len());
        self.send_frame(t, n, resize)
    }

    fn receive(&mut self, t: u64, d: &[u8]) -> Vec<Vec<u8>> {
        let o = self.cipher.open(d).unwrap();
        let inner = Inner::decode(&o.plaintext).unwrap();
        let Some(msg) = self.reasm.push(inner.frag).unwrap() else {
            return vec![];
        };
        let i = Instruction::decode(&msg).unwrap();
        self.acked = self.acked.max(i.ack_num);
        let mut echo = Vec::new();
        let mut got = false;
        if i.new_num > self.user_applied && i.old_num <= self.user_applied {
            for e in decode_diff(&i.diff).unwrap() {
                match e {
                    Element::Data(b) => {
                        got = true;
                        for &c in &b {
                            if c == b'\r' {
                                echo.extend_from_slice(b"\r\n\x1b[1;32m$\x1b[0m ");
                            } else if c.is_ascii_graphic() || c == b' ' {
                                let col = 31 + (c % 7);
                                echo.extend_from_slice(format!("\x1b[{col}m").as_bytes());
                                echo.push(c);
                            }
                        }
                    }
                    Element::Resize { width, height } => {
                        self.pending_resize = Some((width, height));
                        echo.extend_from_slice(
                            b"\x1b[2J\x1b[Hresized \xe2\x82\xac\xe4\xb8\xad\r\n",
                        );
                        got = true;
                    }
                    Element::EchoAck(_) => {}
                }
            }
            self.user_applied = i.new_num;
        }
        let _ = (&mut self.rng, t);
        if got {
            let resize = self.pending_resize.take().is_some();
            if resize {
                self.pending_resize = Some((100, 30));
            }
            return self.publish(t, &echo, resize);
        }
        vec![]
    }
}

struct Link {
    q: VecDeque<(u64, Vec<u8>)>, // (deliver_at, datagram), kept sorted by time
}
impl Link {
    fn push(&mut self, at: u64, d: Vec<u8>) {
        let pos = self
            .q
            .iter()
            .position(|(a, _)| *a > at)
            .unwrap_or(self.q.len());
        self.q.insert(pos, (at, d));
    }
    fn due(&mut self, t: u64) -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        while self.q.front().is_some_and(|(a, _)| *a <= t) {
            v.push(self.q.pop_front().unwrap().1);
        }
        v
    }
}

/// Random byte soup of printable text, controls, CSI/OSC sequences, wide and
/// combining characters, ZWJ and invalid UTF-8, fed to framebuffers of several
/// sizes; prints hashes of the resulting text, repaint and cursor.
fn vt_corpus() {
    use moche_core::screen::{repaint, Framebuffer};
    for seed in 0..40u64 {
        let mut r = Lcg(1000 + seed);
        let mut b: Vec<u8> = Vec::new();
        while b.len() < 20_000 {
            match r.next() % 24 {
                0..=7 => {
                    for _ in 0..(1 + r.next() % 90) {
                        b.push(32 + (r.next() % 95) as u8);
                    }
                }
                8 => b.extend_from_slice(b"\r\n"),
                9 => b.push(b"\x08\t\n\x0b\x0c\r\x07"[(r.next() % 7) as usize]),
                10 => b.extend_from_slice(
                    format!("\x1b[{};{}H", r.next() % 70, r.next() % 210).as_bytes(),
                ),
                11 => b.extend_from_slice(
                    format!("\x1b[{};{}m", r.next() % 110, r.next() % 110).as_bytes(),
                ),
                12 => b.extend_from_slice(
                    format!(
                        "\x1b[38;5;{}m\x1b[48;2;{};{};{}m",
                        r.next() % 256,
                        r.next() % 256,
                        r.next() % 256,
                        r.next() % 256
                    )
                    .as_bytes(),
                ),
                13 => b.extend_from_slice(
                    format!(
                        "\x1b[{}{}",
                        r.next() % 9,
                        ["J", "K", "@", "P", "L", "M", "S", "T", "X", "A", "B", "C", "D"]
                            [(r.next() % 13) as usize]
                    )
                    .as_bytes(),
                ),
                14 => b.extend_from_slice(
                    format!("\x1b[{};{}r", 1 + r.next() % 5, 6 + r.next() % 20).as_bytes(),
                ),
                15 => b.extend_from_slice(
                    [
                        "\x1b[?1049h",
                        "\x1b[?1049l",
                        "\x1b[?7l",
                        "\x1b[?7h",
                        "\x1b[?25l",
                        "\x1b[?6h",
                        "\x1b[?6l",
                        "\x1b[r",
                        "\x1bM",
                        "\x1bD",
                        "\x1bE",
                        "\x1b7",
                        "\x1b8",
                    ][(r.next() % 13) as usize]
                        .as_bytes(),
                ),
                16 => b.extend_from_slice("\u{4e2d}\u{6587}\u{20ac}\u{1f600}x".as_bytes()),
                17 => b.extend_from_slice("e\u{301}a\u{200d}\u{1f468}b".as_bytes()),
                18 => b.push(0x80 + (r.next() % 128) as u8),
                19 => b.extend_from_slice(b"\x1b]2;title here\x07"),
                20 => b.extend_from_slice(format!("\x1b[{}b", r.next() % 5).as_bytes()),
                21 => b.extend_from_slice(b"\x1b[2J\x1b[H"),
                22 => b.extend_from_slice(
                    format!("\x1b[{}G\x1b[{}d", r.next() % 100, r.next() % 40).as_bytes(),
                ),
                _ => b.extend_from_slice("\u{4e2d}".as_bytes()),
            }
        }
        for &(c, rw) in &[(80u16, 24u16), (200, 60), (7, 5)] {
            let mut fb = Framebuffer::new(c, rw);
            for chunk in b.chunks(1 + (seed as usize * 37) % 700) {
                fb.feed(chunk);
            }
            let p = repaint(&fb, None);
            println!(
                "vt seed={seed} {c}x{rw} text={:016x} paint={:016x} cur={},{} vis={} wrap={}",
                fnv(fb.text().as_bytes()),
                fnv(&p),
                fb.cursor_row(),
                fb.cursor_col(),
                fb.cursor_visible(),
                fb.pending_wrap()
            );
        }
    }
}

fn main() {
    vt_corpus();
    let full = std::env::var("GOLDEN_FULL").is_ok();
    let session = Session::from_base64(KEY).unwrap();
    let mut rs = 7u64;
    let rng = Box::new(move |b: &mut [u8]| {
        for x in b {
            rs = rs.wrapping_mul(6364136223846793005).wrapping_add(1);
            *x = (rs >> 40) as u8;
        }
    });
    let mut client = Transport::new(&session, 0, rng);
    let mut screen = Screen::new(80, 24);
    let mut painted: Option<moche_core::Framebuffer> = None;
    let mut srv = Server::new();
    let mut c2s = Link { q: VecDeque::new() };
    let mut s2c = Link { q: VecDeque::new() };
    let mut loss = Lcg(12345);
    let mut last_dl: Option<Option<u64>> = None;
    let mut pending_resize: Option<(u16, u16)> = None;
    let mut paste = Lcg(4242);
    let mut burst = Lcg(777);
    let mut tx = 0u64;
    let mut rx = 0u64;

    client.send_resize(0, 80, 24);

    let end = 60_000u64;
    let mut t = 0u64;
    let mut typed = 0u32;
    let mut lossy = false;
    let mut one_way = 2u64;
    while t <= end {
        // loss/latency regime per phase
        match t {
            0 => {
                lossy = false;
                one_way = 2;
            }
            16_000 => {
                lossy = true;
                one_way = 15;
            }
            40_000 => {
                lossy = false;
                one_way = 30;
            }
            _ => {}
        }
        // scripted client input
        if (100..12_100).contains(&t) && t.is_multiple_of(100) {
            let ch = [b'a' + (typed % 26) as u8];
            client.send_keys(t, &ch);
            typed += 1;
            if typed.is_multiple_of(40) {
                client.send_keys(t, b"\r");
            }
        }
        if t == 13_000 {
            // many small keys in one tick, like a fast typist or key repeat
            client.send_keys(t, b"hello world, quick brown fox");
        }
        if t == 17_000 {
            let mut p = Vec::new();
            for _ in 0..150_000 {
                p.push(32 + (paste.next() % 95) as u8);
            }
            client.send_keys(t, &p);
        }
        if t == 30_000 {
            client.send_keys(t, &vec![b'z'; 40_000]);
        }
        if t == 41_000 {
            client.send_resize(t, 132, 43);
        }
        if t == 43_000 {
            client.send_keys(t, b"ls -la\r");
        }
        // server-side burst: a 70-fragment colourful repaint
        if t == 45_000 {
            let mut b = Vec::new();
            for _ in 0..110_000 {
                let c = 32 + (burst.next() % 95) as u8;
                if burst.next().is_multiple_of(7) {
                    b.extend_from_slice(format!("\x1b[38;5;{}m", burst.next() % 256).as_bytes());
                }
                b.push(c);
                if burst.next().is_multiple_of(90) {
                    b.extend_from_slice(b"\r\n");
                }
            }
            for d in srv.publish(t, &b, false) {
                if !(lossy && loss.next() % 100 < 8) {
                    s2c.push(t + one_way, d);
                }
            }
        }
        // server resend timer: re-send an unacked frame every 250 ms
        if let Some((n, at)) = srv.last_frame {
            if n > srv.acked && t >= at + 250 {
                for d in srv.send_frame(t, n, false) {
                    if !(lossy && loss.next() % 100 < 8) {
                        s2c.push(t + one_way, d);
                    }
                }
            }
        }

        // deliver to server
        for d in c2s.due(t) {
            for o in srv.receive(t, &d) {
                if !(lossy && loss.next() % 100 < 8) {
                    s2c.push(t + one_way, o);
                }
            }
        }
        // deliver to client
        for d in s2c.due(t) {
            rx += 1;
            let events = client.handle_datagram(t, &d);
            for ev in events {
                match ev {
                    Event::HostResize { width, height, .. } => {
                        println!("{t} ev resize {width}x{height}");
                        pending_resize = Some((width, height));
                    }
                    Event::HostDiff {
                        old_num,
                        new_num,
                        bytes,
                    } => {
                        let rz = pending_resize.take();
                        let r = screen.apply(old_num, new_num, rz, &bytes);
                        println!(
                            "{t} ev diff {old_num}->{new_num} len={} h={:016x} apply={r:?}",
                            bytes.len(),
                            fnv(&bytes)
                        );
                    }
                    Event::EchoAck(n) => println!("{t} ev echoack {n}"),
                    Event::Shutdown => println!("{t} ev shutdown"),
                }
            }
            screen.discard_below(client.peer_throwaway());
            let p = screen.repaint(painted.as_ref());
            painted = Some(screen.latest().clone());
            println!(
                "{t} paint len={} h={:016x} held={:?} rx_errors={} gaps={}",
                p.len(),
                fnv(&p),
                screen.held(),
                client.rx_errors(),
                client.rx_gaps()
            );
        }
        // client transmit
        while let Some(d) = client.poll_transmit(t) {
            tx += 1;
            if full {
                println!("{t} tx {}", hex(&d));
            } else {
                println!("{t} tx len={} h={:016x}", d.len(), fnv(&d));
            }
            let drop = lossy && loss.next() % 100 < 12;
            if !drop {
                c2s.push(t + one_way, d);
            }
        }
        let dl = client.next_deadline_ms();
        if last_dl != Some(dl) {
            println!("{t} deadline {dl:?}");
            last_dl = Some(dl);
        }
        t += 1;
    }
    println!(
        "end tx={tx} rx={rx} srtt={:.3} queued={} limit={} shrinks={} send_errors={}",
        client.srtt_ms(),
        client.queued_bytes(),
        client.state_limit(),
        client.cap_shrinks(),
        client.send_errors()
    );
}
