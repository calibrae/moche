//! Simulation of predictive local echo against modelled servers.
//!
//! A virtual-clock world: keystrokes travel to a modelled server after a one-way
//! delay, the server (a line-editor-like shell, or a process that does not echo,
//! or one that echoes differently) writes into its own framebuffer, and each
//! state travels back as a numbered diff with an echo ack (about 50 ms after the
//! server processed the input, as observed on a stock server). The client side is
//! moche's own `Screen` plus the `Predictor`. The display is the latest server
//! state plus the overlay. Everything is seeded and deterministic.
//!
//! Run with `-- --nocapture` to see the latency numbers.

use moche_core::predict::{PredictMode, Predictor};
use moche_core::screen::{Framebuffer, Screen};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 24
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Beh {
    /// Echoes what is typed, like a line editor.
    Echo,
    /// Reads without echo (password prompt): consumes keys, shows nothing.
    Password,
    /// Ignores printable keys (vim normal mode).
    Ignore,
    /// Echoes a different character (upper case).
    Upper,
    /// Echoes with ANSI colour around each character.
    Color,
    /// Echoes `xyz` when `x` is typed (completion).
    Complete,
}

struct App {
    beh: Beh,
    line: Vec<char>,
    pos: usize,
}

const PROMPT: &str = "$ ";

impl App {
    fn new(beh: Beh) -> Self {
        App {
            beh,
            line: Vec::new(),
            pos: 0,
        }
    }

    fn tail_redraw(&self, out: &mut Vec<u8>) {
        let tail: String = self.line[self.pos..].iter().collect();
        if !tail.is_empty() {
            out.extend(tail.as_bytes());
            out.extend(format!("\x1b[{}D", tail.chars().count()).as_bytes());
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == 0x1b {
                if bytes.get(i + 1) == Some(&b'[') && matches!(bytes.get(i + 2), Some(b'D' | b'C'))
                {
                    let left = bytes[i + 2] == b'D';
                    if self.beh != Beh::Password && self.beh != Beh::Ignore {
                        if left && self.pos > 0 {
                            self.pos -= 1;
                            out.extend(b"\x1b[D");
                        } else if !left && self.pos < self.line.len() {
                            self.pos += 1;
                            out.extend(b"\x1b[C");
                        }
                    }
                    i += 3;
                } else {
                    i += 2;
                }
                continue;
            }
            match b {
                b'\r' => {
                    out.extend(b"\r\n");
                    if self.beh == Beh::Password {
                        out.extend(b"ok\r\n");
                    } else if self.beh != Beh::Ignore {
                        let l: String = self.line.iter().collect();
                        out.extend(format!("out: {l}\r\n").as_bytes());
                    }
                    out.extend(PROMPT.as_bytes());
                    self.line.clear();
                    self.pos = 0;
                    i += 1;
                }
                0x7f | 0x08 => {
                    if matches!(self.beh, Beh::Password | Beh::Ignore) {
                    } else if self.pos > 0 {
                        self.pos -= 1;
                        self.line.remove(self.pos);
                        if self.pos == self.line.len() {
                            out.extend(b"\x08 \x08");
                        } else {
                            out.push(0x08);
                            self.tail_redraw(&mut out);
                            out.extend(b" \x1b[D");
                            // put the cursor back on the edit point
                            let n = self.line.len() - self.pos + 1;
                            out.extend(format!("\x1b[{n}D").as_bytes());
                        }
                    }
                    i += 1;
                }
                0x00..=0x1f => i += 1,
                _ => {
                    let len = match b {
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf7 => 4,
                        _ => 1,
                    };
                    let end = (i + len).min(bytes.len());
                    let ch = std::str::from_utf8(&bytes[i..end])
                        .ok()
                        .and_then(|s| s.chars().next());
                    i = end;
                    let Some(ch) = ch else { continue };
                    self.typed(ch, &mut out);
                }
            }
        }
        out
    }

    fn typed(&mut self, ch: char, out: &mut Vec<u8>) {
        let mut chars = vec![ch];
        match self.beh {
            Beh::Password | Beh::Ignore => return,
            Beh::Upper => chars = vec![ch.to_ascii_uppercase()],
            Beh::Complete if ch == 'x' => chars = vec!['x', 'y', 'z'],
            _ => {}
        }
        for c in chars {
            self.line.insert(self.pos, c);
            self.pos += 1;
            let s = c.to_string();
            if self.beh == Beh::Color {
                out.extend(format!("\x1b[31m{s}\x1b[0m").as_bytes());
            } else {
                out.extend(s.as_bytes());
            }
            self.tail_redraw(out);
        }
    }
}

enum Ev {
    /// Keystrokes reach the server; `total` input bytes so far.
    Keys(Vec<u8>, u64),
    /// The server sends an echo-ack-only instruction.
    EchoEmit,
    /// An instruction reaches the client.
    Deliver {
        num: u64,
        bytes: Vec<u8>,
        echo: u64,
        resize: Option<(u16, u16)>,
    },
    /// The app behaviour changes (a new program takes the terminal).
    Switch(Beh),
}

struct Net {
    up_ms: u64,
    down_ms: u64,
    /// Max extra random delay per datagram (reordering).
    jitter_ms: u64,
    /// Percent of server instructions dropped.
    loss_pct: u64,
    echo_ms: u64,
}

struct Sim {
    now: u64,
    net: Net,
    rng: Lcg,
    // server
    server_fb: Framebuffer,
    app: App,
    hist: Vec<u8>,
    next_num: u64,
    proc_log: Vec<(u64, u64)>,
    // client
    screen: Screen,
    echoed: u64,
    input_total: u64,
    last_heard: u64,
    srtt: f64,
    pred: Predictor,
    events: Vec<(u64, u64, Ev)>,
    seq: u64,
    /// Steps at which the composite differed from the server state.
    visible_steps: u64,
}

impl Sim {
    fn new(mode: PredictMode, beh: Beh, net: Net, seed: u64, cols: u16) -> Self {
        let mut app = App::new(beh);
        let mut server_fb = Framebuffer::new(cols, 6);
        server_fb.feed(PROMPT.as_bytes());
        app.line.clear();
        let mut s = Sim {
            now: 0,
            srtt: (net.up_ms + net.down_ms) as f64,
            net,
            rng: Lcg(seed),
            server_fb,
            app,
            hist: PROMPT.as_bytes().to_vec(),
            next_num: 1,
            proc_log: Vec::new(),
            screen: Screen::new(cols, 6),
            echoed: 0,
            input_total: 0,
            last_heard: 0,
            pred: Predictor::new(mode),
            events: Vec::new(),
            seq: 0,
            visible_steps: 0,
        };
        // the initial prompt reaches the client before the scenario starts
        s.screen.apply(0, 1, None, PROMPT.as_bytes()).unwrap();
        s.next_num = 2;
        s
    }

    fn push(&mut self, at: u64, ev: Ev) {
        self.seq += 1;
        self.events.push((at, self.seq, ev));
    }

    fn echo_at(&self, t: u64) -> u64 {
        self.proc_log
            .iter()
            .rev()
            .find(|(pt, _)| pt + self.net.echo_ms <= t)
            .map_or(0, |(_, c)| *c)
    }

    fn send_instruction(&mut self, resize: Option<(u16, u16)>, reliable: bool) {
        let num = self.next_num;
        self.next_num += 1;
        if !reliable && self.rng.below(100) < self.net.loss_pct {
            return;
        }
        let jitter = self.rng.below(self.net.jitter_ms + 1);
        let at = self.now + self.net.down_ms + jitter;
        let echo = self.echo_at(self.now);
        let bytes = self.hist.clone();
        self.push(
            at,
            Ev::Deliver {
                num,
                bytes,
                echo,
                resize,
            },
        );
    }

    fn process(&mut self, ev: Ev) {
        match ev {
            Ev::Keys(bytes, total) => {
                let out = self.app.feed(&bytes);
                self.server_fb.feed(&out);
                self.hist.extend(&out);
                self.proc_log.push((self.now, total));
                self.send_instruction(None, false);
                self.push(self.now + self.net.echo_ms, Ev::EchoEmit);
            }
            Ev::EchoEmit => self.send_instruction(None, false),
            Ev::Switch(b) => {
                self.app.beh = b;
            }
            Ev::Deliver {
                num,
                bytes,
                echo,
                resize,
            } => {
                if num <= self.screen.latest_num() {
                    return;
                }
                self.last_heard = self.now;
                // each instruction carries the whole history: base state 1 is
                // the initial prompt, so the diff is the rest
                let applied = if let Some((c, r)) = resize {
                    self.screen
                        .apply(1, num, Some((c, r)), &bytes[PROMPT.len()..])
                } else {
                    self.screen.apply(1, num, None, &bytes[PROMPT.len()..])
                };
                assert!(applied.is_ok());
                self.echoed = self.echoed.max(echo);
                self.pred
                    .on_server_state(self.screen.latest(), self.echoed, self.now);
            }
        }
    }

    /// Advances the virtual clock to `t`, 1 ms at a time.
    fn run_until(&mut self, t: u64) {
        while self.now < t {
            self.now += 1;
            loop {
                let due: Vec<usize> = (0..self.events.len())
                    .filter(|&i| self.events[i].0 <= self.now)
                    .collect();
                if due.is_empty() {
                    break;
                }
                let i = *due
                    .iter()
                    .min_by_key(|&&i| (self.events[i].0, self.events[i].1))
                    .unwrap();
                let (_, _, ev) = self.events.remove(i);
                self.process(ev);
            }
            if self.now.is_multiple_of(10) {
                self.pred.tick(
                    self.now,
                    self.srtt,
                    self.now.saturating_sub(self.last_heard),
                );
            }
            if let Some(d) = self.pred.next_deadline_ms() {
                // never shown past its expiry (ticks every 10 ms)
                assert!(d + 10 > self.now, "overdue expiry {d} at {}", self.now);
            }
            if !self.composite().display_eq(self.screen.latest()) {
                self.visible_steps += 1;
            }
        }
    }

    fn type_at(&mut self, t: u64, bytes: &[u8]) {
        self.run_until(t);
        self.input_total += bytes.len() as u64;
        let total = self.input_total;
        self.push(self.now + self.net.up_ms, Ev::Keys(bytes.to_vec(), total));
        self.pred
            .on_input(self.now, bytes, self.screen.latest(), self.srtt, total);
    }

    fn composite(&self) -> Framebuffer {
        let mut c = self.screen.latest().clone();
        self.pred.apply_overlay(&mut c);
        c
    }

    /// Runs until every event has been delivered and the timeouts have passed.
    fn settle(&mut self) {
        // a final reliable instruction carries the final truth and ack
        let t = self.now + 3 * (self.net.up_ms + self.net.down_ms + self.net.jitter_ms) + 200;
        self.run_until(t);
        self.proc_log.push((self.now, self.input_total));
        self.send_instruction(None, true);
        let t2 = self.now + self.net.down_ms + self.net.jitter_ms + 1;
        self.run_until(t2);
    }

    fn assert_converged(&self, what: &str) {
        let c = self.composite();
        assert!(
            c.display_eq(&self.server_fb),
            "{what}: display differs from the server\nshown:\n{}\nserver:\n{}",
            c.text(),
            self.server_fb.text()
        );
        assert!(!self.pred.is_displaying(), "{what}: overlay still active");
    }
}

fn net(one_way: u64) -> Net {
    Net {
        up_ms: one_way,
        down_ms: one_way,
        jitter_ms: 0,
        loss_pct: 0,
        echo_ms: 50,
    }
}

fn type_str(s: &mut Sim, text: &str, start: u64, gap: u64) -> Vec<(u64, String)> {
    let mut log = Vec::new();
    let mut t = start;
    let mut typed = String::new();
    for ch in text.chars() {
        typed.push(ch);
        let mut b = [0u8; 4];
        s.type_at(t, ch.encode_utf8(&mut b).as_bytes());
        log.push((s.now, typed.clone()));
        t += gap;
    }
    log
}

/// Like `type_str` but records, at each ms, which typed prefixes became visible.
fn type_and_watch(s: &mut Sim, text: &str, start: u64, gap: u64) -> Vec<u64> {
    let mut times = Vec::new();
    let mut seen: Vec<Option<u64>> = vec![None; text.chars().count()];
    let mut prefixes = Vec::new();
    let mut typed = String::new();
    for ch in text.chars() {
        typed.push(ch);
        prefixes.push(format!("{PROMPT}{typed}"));
    }
    let chars: Vec<char> = text.chars().collect();
    let mut next_key = 0usize;
    let mut t = start;
    let end = start + gap * chars.len() as u64 + 3000;
    while t <= end {
        s.run_until(t);
        if next_key < chars.len() && t == start + gap * next_key as u64 {
            let mut b = [0u8; 4];
            s.type_at(t, chars[next_key].encode_utf8(&mut b).as_bytes());
            times.push(t);
            next_key += 1;
        }
        let row = s.composite().row_text(0);
        for i in 0..next_key {
            if seen[i].is_none() && row.starts_with(prefixes[i].trim_end()) {
                seen[i] = Some(t);
            }
        }
        t += 1;
    }
    times
        .iter()
        .zip(&seen)
        .map(|(typed, seen)| seen.expect("every char shows up at last") - typed)
        .collect()
}

#[test]
fn latency_at_100ms_one_way() {
    let text = "echo hello world, predicted";
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(100), 1, 80);
    let lat = type_and_watch(&mut s, text, 10, 70);
    s.settle();
    s.assert_converged("latency run");
    eprintln!("adaptive, 100 ms one way, 70 ms between keys: {lat:?}");
    // the first characters wait for the first confirmation (one RTT of echo,
    // 200 ms plus the processing); afterwards displayed at once
    let confirm_by = 210 + 5;
    let first_zero = lat.iter().position(|&l| l == 0).expect("some char at once");
    assert!(
        first_zero <= 4,
        "first instant char is #{first_zero}: {lat:?}"
    );
    for (i, &l) in lat.iter().enumerate() {
        if i < first_zero {
            assert!(l <= confirm_by, "char {i}: {l} ms");
        } else {
            assert_eq!(l, 0, "char {i} after the first confirmed one: {lat:?}");
        }
    }
    let sum: u64 = lat.iter().sum();
    eprintln!(
        "mean displayed latency {:.1} ms (without prediction: about 200 ms)",
        sum as f64 / lat.len() as f64
    );

    let mut n = Sim::new(PredictMode::Never, Beh::Echo, net(100), 1, 80);
    let lat_never = type_and_watch(&mut n, text, 10, 70);
    n.settle();
    n.assert_converged("never");
    // (a typed space is invisible until the next char: skip those in the check)
    for (c, &l) in text.chars().zip(&lat_never) {
        assert!(c == ' ' || l >= 200, "{lat_never:?}");
    }
    eprintln!(
        "never: mean {:.1} ms",
        lat_never.iter().sum::<u64>() as f64 / lat_never.len() as f64
    );
}

#[test]
fn fast_link_adaptive_shows_nothing() {
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(5), 2, 80);
    let lat = type_and_watch(&mut s, "ls -la /tmp", 10, 60);
    s.settle();
    s.assert_converged("fast");
    assert_eq!(s.visible_steps, 0, "no overlay was ever displayed");
    for (c, &l) in "ls -la /tmp".chars().zip(&lat) {
        assert!(c == ' ' || l >= 10, "{lat:?}");
    }
}

#[test]
fn epoch_gating_after_enter() {
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(100), 3, 80);
    // a first epoch, then Enter
    let _ = type_str(&mut s, "ab", 10, 50);
    s.type_at(400, b"\r");
    s.run_until(900);
    assert_eq!(s.server_fb.row_text(1), "out: ab");
    // the first char after the prompt is back is hidden until confirmed
    s.type_at(1000, b"c");
    assert!(
        s.composite().display_eq(s.screen.latest()),
        "first char of the new epoch is hidden"
    );
    s.run_until(1000 + 215);
    assert_eq!(
        s.composite().row_text(2),
        "$ c",
        "confirmed by the real echo"
    );
    // the next chars are shown at once
    s.type_at(1300, b"d");
    assert_eq!(
        s.composite().row_text(2),
        "$ cd",
        "second char shown at once"
    );
    s.type_at(1320, b"e");
    assert_eq!(s.composite().row_text(2), "$ cde");
    s.settle();
    s.assert_converged("gating");
}

#[test]
fn password_prompt_shows_nothing_in_adaptive() {
    for beh in [Beh::Password, Beh::Ignore] {
        let mut s = Sim::new(PredictMode::Adaptive, beh, net(100), 4, 80);
        let _ = type_str(&mut s, "correct horse battery", 10, 90);
        assert_eq!(s.visible_steps, 0, "{beh:?}: a typed char was displayed");
        s.type_at(3000, b"\r");
        s.run_until(3700);
        let _ = type_str(&mut s, "again", 3800, 90);
        assert_eq!(s.visible_steps, 0, "{beh:?}: after Enter too");
        s.settle();
        s.assert_converged("password");
    }
}

#[test]
fn always_mode_leak_is_bounded_by_one_echo_window() {
    let mut s = Sim::new(PredictMode::Always, Beh::Password, net(100), 5, 80);
    let _ = type_str(&mut s, "secret-secret-secret", 10, 30);
    s.settle();
    s.assert_converged("always/password");
    // typed over 600 ms; shown only until the first echo ack contradicts (about
    // 100 + 50 + 100 ms after the first key), then tentative
    assert!(s.visible_steps <= 330, "visible for {} ms", s.visible_steps);
    assert!(s.visible_steps > 0);
    eprintln!(
        "always on a non-echo prompt: predictions visible {} ms of 600 ms typing",
        s.visible_steps
    );
}

#[test]
fn app_stops_echoing_mid_epoch() {
    // the gate is open (confirmed), then the process stops echoing
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(100), 6, 80);
    let _ = type_str(&mut s, "abcdefgh", 10, 60);
    s.run_until(600);
    s.push(s.now, Ev::Switch(Beh::Ignore));
    let before = s.visible_steps;
    let _ = type_str(&mut s, "ijklmnopqrstu", 700, 60);
    s.settle();
    s.assert_converged("stops echoing");
    let leaked = s.visible_steps - before;
    eprintln!("echo stops: overlay visible {leaked} ms in total");
    // never longer than the typing window plus one echo ack: well under the timeout
    assert!(leaked < 1500, "{leaked}");
}

#[test]
fn different_echoes_converge() {
    for beh in [Beh::Upper, Beh::Color, Beh::Complete] {
        let mut s = Sim::new(PredictMode::Adaptive, beh, net(100), 7, 80);
        let _ = type_str(&mut s, "xabxcdxx", 10, 50);
        s.type_at(600, b"\r");
        let _ = type_str(&mut s, "xyx", 1200, 50);
        s.settle();
        s.assert_converged(&format!("{beh:?}"));
    }
}

#[test]
fn line_wrap_converges() {
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(100), 8, 16);
    let _ = type_str(&mut s, "abcdefghijklmnopqrstuvwxyz0123456789", 10, 40);
    s.settle();
    s.assert_converged("wrap");
}

#[test]
fn editing_keys_converge() {
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(100), 9, 40);
    let _ = type_str(&mut s, "abcdef", 10, 40);
    for (i, k) in [
        &b"\x1b[D"[..],
        b"\x1b[D",
        b"X",
        b"\x7f",
        b"\x1b[C",
        b"\x7f",
        b"\x7f",
        b"\x1b[C",
        b"\x1bOC",
        b"q",
    ]
    .iter()
    .enumerate()
    {
        s.type_at(500 + 45 * i as u64, k);
    }
    s.settle();
    s.assert_converged("editing");
}

#[test]
fn utf8_and_unpredictable_chars_converge() {
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(100), 10, 40);
    let _ = type_str(&mut s, "aébç字d", 10, 60);
    s.type_at(800, "e\u{301}".as_bytes());
    s.type_at(850, b"\t");
    s.type_at(900, b"z");
    s.settle();
    s.assert_converged("utf8");
}

#[test]
fn timeout_expires_a_prediction_the_server_never_answers() {
    // the link dies right after the keys: nothing comes back
    let mut s = Sim::new(PredictMode::Adaptive, Beh::Echo, net(100), 11, 80);
    let _ = type_str(&mut s, "abc", 10, 50);
    s.run_until(600);
    s.net.loss_pct = 100;
    s.type_at(700, b"d");
    assert_eq!(s.composite().row_text(0), "$ abcd");
    // silence (3 s since the last datagram, here about 3.4 s) or the 3 s expiry
    // (3.7 s), whichever comes first: gone by then, still shown before
    s.run_until(3300);
    assert_eq!(s.composite().row_text(0), "$ abcd", "still pending");
    s.run_until(3720);
    assert!(!s.pred.is_displaying(), "expired");
    assert_eq!(s.composite().row_text(0), "$ abc");
}

/// Random keystrokes, random programs, delay, jitter (reordering) and loss.
#[test]
fn property_converges_without_residue_or_panic() {
    let alphabet: &[&[u8]] = &[
        b"a",
        b"b",
        b"z",
        b" ",
        b"\x7f",
        b"\x1b[D",
        b"\x1b[C",
        b"\x1bOD",
        b"\r",
        b"\t",
        b"\x1b",
        b"\x03",
        "é".as_bytes(),
        "字".as_bytes(),
        "e\u{301}".as_bytes(),
        b"x",
    ];
    let behs = [
        Beh::Echo,
        Beh::Echo,
        Beh::Password,
        Beh::Ignore,
        Beh::Upper,
        Beh::Color,
        Beh::Complete,
    ];
    let modes = [PredictMode::Adaptive, PredictMode::Always];
    let mut agg = moche_core::predict::PredictStats::default();
    for seed in 0..150u64 {
        let mut r = Lcg(seed * 7919 + 1);
        let beh = behs[r.below(behs.len() as u64) as usize];
        let cols = [10u16, 20, 40][r.below(3) as usize];
        let net = Net {
            up_ms: 5 + r.below(150),
            down_ms: 5 + r.below(150),
            jitter_ms: r.below(120),
            loss_pct: [0, 10, 30][r.below(3) as usize],
            echo_ms: 50,
        };
        let mode = modes[r.below(2) as usize];
        let mut s = Sim::new(mode, beh, net, seed, cols);
        let mut t = 10;
        for _ in 0..(20 + r.below(80)) {
            t += 5 + r.below(200);
            let k = if r.below(40) == 0 {
                // a paste
                vec![b'p'; 1000 + r.below(300) as usize]
            } else if r.below(100) < 60 {
                // mostly plain typing, so chains and confirmations happen
                alphabet[r.below(4) as usize].to_vec()
            } else {
                alphabet[r.below(alphabet.len() as u64) as usize].to_vec()
            };
            if r.below(25) == 0 {
                let b = behs[r.below(behs.len() as u64) as usize];
                let at = s.now.max(t);
                s.push(at, Ev::Switch(b));
            }
            s.type_at(t, &k);
            // every step: the overlay touches one row at most
            let c = s.composite();
            let latest = s.screen.latest();
            let mut rows = std::collections::BTreeSet::new();
            for row in 0..latest.rows() {
                if c.row_cells(row) != latest.row_cells(row) {
                    rows.insert(row);
                }
            }
            assert!(rows.len() <= 1, "seed {seed}: overlay on rows {rows:?}");
        }
        s.settle();
        s.assert_converged(&format!("seed {seed} {beh:?} {mode:?}"));
        let st = s.pred.stats();
        agg.predicted += st.predicted;
        agg.confirmed += st.confirmed;
        agg.mismatches += st.mismatches;
        agg.expired += st.expired;
    }
    eprintln!("property run (150 seeds): {agg:?}");
    // the run must actually exercise every path
    assert!(agg.predicted > 500 && agg.confirmed > 200, "{agg:?}");
    assert!(agg.mismatches > 20 && agg.expired > 0, "{agg:?}");
}
