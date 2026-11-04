//! The state behind a `MocheSession*`: transport, screen model and paint
//! bookkeeping. Safe Rust; the FFI layer only adds pointers and error codes.

use std::sync::Arc;

use moche_core::crypto::Session;
use moche_core::predict::{PredictMode, PredictStats, Predictor};
use moche_core::screen::{repaint, Framebuffer, Screen, ScreenEvent, MAX_DIM};
use moche_core::ssp::{Event, Rng, Transport};

/// Event bits, see `moche.h`.
pub const EV_BELL: u32 = 1;
pub const EV_TITLE: u32 = 2;
pub const EV_RESIZED: u32 = 4;
pub const EV_OUT_OF_SYNC: u32 = 8;
pub const EV_SHUTDOWN: u32 = 16;

/// Timestamps above this are clamped so deadline arithmetic cannot overflow.
const MAX_NOW: u64 = 1 << 62;

/// Largest key-stroke / paste batch accepted in one call.
pub const MAX_KEYS: usize = 16 << 20;

/// Result of feeding one datagram.
#[derive(Debug, PartialEq, Eq)]
pub enum Fed {
    /// Authentic datagram from the server.
    Accepted,
    /// Failed authentication, wrong direction or too short: dropped.
    Dropped,
}

/// Plain copy of the framebuffer modes the host key row cares about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeSnapshot {
    pub mouse_mode: u16,
    pub mouse_encoding: u16,
    pub app_cursor_keys: bool,
    pub bracketed_paste: bool,
    pub cursor_visible: bool,
    pub alt_screen: bool,
}

/// Backlog and counters, counters saturated to `u32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub queued_bytes: u64,
    pub send_errors: u32,
    pub rx_errors: u32,
    pub rx_gaps: u32,
}

/// What the host terminal currently shows.
enum Painted {
    /// Nothing known: the next paint is a full repaint.
    None,
    /// A server state, shared with the screen (no copy).
    Server(Arc<Framebuffer>),
    /// A server state plus the prediction overlay (one owned copy).
    Overlay(Box<Framebuffer>),
}

impl Painted {
    fn get(&self) -> Option<&Framebuffer> {
        match self {
            Painted::None => None,
            Painted::Server(a) => Some(a),
            Painted::Overlay(f) => Some(f),
        }
    }
}

/// Plain copy of the predictor counters and state, see `moche.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PredictSnapshot {
    pub predictions: u32,
    pub confirmed: u32,
    pub contradicted: u32,
    pub expired: u32,
    pub displaying: bool,
    pub mode: u8,
}

pub struct Core {
    transport: Transport,
    screen: Screen,
    /// Display-only predictive echo; `Never` unless the host opts in.
    predictor: Predictor,
    /// What the host terminal currently shows (server state, or server state
    /// plus overlay); kept apart from `screen`, the base of every diff.
    painted: Painted,
    /// A recycled overlay grid, so an active overlay costs one memcpy per paint.
    spare: Option<Box<Framebuffer>>,
    dirty: bool,
    pending_resize: Option<(u16, u16)>,
    edge: u32,
    desync: bool,
    shutdown: bool,
    title_changed: bool,
    highest_seq: Option<u64>,
    last_contact: Option<u64>,
    created: u64,
    last_now: u64,
    /// Set after a panic: the code every later call returns.
    pub dead: Option<i32>,
}

fn os_rng() -> Rng {
    Box::new(|buf: &mut [u8]| {
        // Chaff needs no secrecy: degrade to zeros rather than fail.
        if getrandom::fill(buf).is_err() {
            buf.fill(0);
        }
    })
}

/// True if the OS random source answers.
pub fn rng_works() -> bool {
    getrandom::fill(&mut [0u8; 1]).is_ok()
}

pub fn dim_ok(cols: u16, rows: u16) -> bool {
    (1..=MAX_DIM).contains(&cols) && (1..=MAX_DIM).contains(&rows)
}

impl Core {
    /// `key` is the 22-character key; the decoded copy dies at the end of this call.
    pub fn new(
        key: &str,
        cols: u16,
        rows: u16,
        now: u64,
    ) -> Result<Core, moche_core::crypto::KeyError> {
        let now = now.min(MAX_NOW);
        let session = Session::from_base64(key)?;
        let mut transport = Transport::new(&session, now, os_rng());
        drop(session);
        transport.send_resize(now, cols, rows);
        Ok(Core {
            transport,
            screen: Screen::new(cols, rows),
            predictor: Predictor::new(PredictMode::Never),
            painted: Painted::None,
            spare: None,
            dirty: true,
            pending_resize: None,
            edge: 0,
            desync: false,
            shutdown: false,
            title_changed: false,
            highest_seq: None,
            last_contact: None,
            created: now,
            last_now: now,
            dead: None,
        })
    }

    /// Never lets time go backwards (and keeps it small enough to add to).
    fn clock(&mut self, now: u64) -> u64 {
        self.last_now = self.last_now.max(now.min(MAX_NOW));
        self.last_now
    }

    pub fn datagram(&mut self, now: u64, data: &[u8]) -> Fed {
        let now = self.clock(now);
        let before = {
            let f = self.screen.latest();
            (f.cols(), f.rows())
        };
        // One decryption: the transport authenticates and processes the datagram.
        let Some((seq, events)) = self.transport.receive(now, data) else {
            return Fed::Dropped;
        };
        // Only a newer sequence number counts as proof of life: a replayed
        // old datagram must not keep a dead connection looking alive.
        if self.highest_seq.is_none_or(|h| seq > h) {
            self.highest_seq = Some(seq);
            self.last_contact = Some(now);
        }
        for ev in events {
            match ev {
                Event::HostResize { width, height, .. } => {
                    self.pending_resize = Some((width, height))
                }
                Event::HostDiff {
                    old_num,
                    new_num,
                    bytes,
                } => {
                    let rs = self.pending_resize.take();
                    match self.screen.apply(old_num, new_num, rs, &bytes) {
                        Ok(()) => {
                            self.desync = false;
                            self.dirty = true;
                        }
                        // The transport still acks this state, so the server may
                        // keep diffing against a base we lack; flagged for the host.
                        Err(_) => self.desync = true,
                    }
                    for e in self.screen.take_events() {
                        match e {
                            ScreenEvent::Bell => self.edge |= EV_BELL,
                            ScreenEvent::TitleChanged(_) => {
                                self.edge |= EV_TITLE;
                                self.title_changed = true;
                            }
                            ScreenEvent::IconNameChanged(_) => {}
                        }
                    }
                }
                Event::EchoAck(_) => {}
                Event::Shutdown => self.shutdown = true,
            }
        }
        self.screen.discard_below(self.transport.peer_throwaway());
        if self.predictor.mode() != PredictMode::Never {
            let echoed = self.transport.echoed_input_bytes();
            self.dirty |= self
                .predictor
                .on_server_state(self.screen.latest(), echoed, now);
        }
        let f = self.screen.latest();
        if (f.cols(), f.rows()) != before {
            self.edge |= EV_RESIZED;
        }
        Fed::Accepted
    }

    pub fn send_keys(&mut self, now: u64, keys: &[u8]) {
        let now = self.clock(now);
        self.transport.send_keys(now, keys);
        if self.predictor.mode() != PredictMode::Never && !keys.is_empty() {
            let changed = self.predictor.on_input(
                now,
                keys,
                self.screen.latest(),
                self.transport.srtt_ms(),
                self.transport.input_bytes_total(),
            );
            self.dirty |= changed;
        }
    }

    pub fn send_resize(&mut self, now: u64, cols: u16, rows: u16) {
        let now = self.clock(now);
        self.transport.send_resize(now, cols, rows);
        if self.predictor.mode() != PredictMode::Never {
            // nothing predicted survives a resize; the next paint removes it
            self.dirty |= self.predictor.is_displaying();
            self.predictor.on_resize();
        }
    }

    pub fn poll_transmit(&mut self, now: u64) -> Option<Vec<u8>> {
        let now = self.clock(now);
        self.transport.poll_transmit(now)
    }

    pub fn next_deadline(&self) -> Option<u64> {
        let t = self.transport.next_deadline_ms();
        match (t, self.predictor.next_deadline_ms()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Chooses the predictive echo mode; pending predictions are dropped.
    pub fn set_predict_mode(&mut self, mode: PredictMode) {
        if self.predictor.mode() != mode {
            self.dirty |= self.predictor.is_displaying();
            self.predictor.set_mode(mode);
        }
    }

    pub fn predict_stats(&self) -> PredictSnapshot {
        let sat = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
        let s: PredictStats = self.predictor.stats();
        PredictSnapshot {
            predictions: sat(s.predicted),
            confirmed: sat(s.confirmed),
            contradicted: sat(s.mismatches),
            expired: sat(s.expired),
            displaying: self.predictor.is_displaying(),
            mode: match self.predictor.mode() {
                PredictMode::Never => 0,
                PredictMode::Adaptive => 1,
                PredictMode::Always => 2,
            },
        }
    }

    fn set_painted(&mut self, p: Painted) {
        if let Painted::Overlay(old) = std::mem::replace(&mut self.painted, p) {
            self.spare = Some(old);
        }
    }

    /// Bytes for the host terminal, advancing the painted state. `None` when
    /// the host already shows the current screen.
    pub fn take_paint(&mut self) -> Option<Vec<u8>> {
        if self.predictor.mode() != PredictMode::Never {
            // Expiry runs on the host's clock as last seen (every call that
            // takes `now` advances it; the pump polls before painting).
            let silent = self
                .last_now
                .saturating_sub(self.last_contact.unwrap_or(self.created));
            let srtt = self.transport.srtt_ms();
            self.dirty |= self.predictor.tick(self.last_now, srtt, silent);
        }
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let latest = self.screen.latest_shared();
        if self.predictor.is_displaying() {
            // server state + overlay, drawn into a recycled copy
            let mut buf = match self.spare.take() {
                Some(mut b) => {
                    (*b).clone_from(&latest);
                    b
                }
                None => Box::new(Framebuffer::clone(&latest)),
            };
            self.predictor.apply_overlay(&mut buf);
            let out = if self.painted.get().is_some_and(|p| p.display_eq(&buf)) {
                None
            } else {
                Some(repaint(&buf, self.painted.get()))
            };
            self.set_painted(Painted::Overlay(buf));
            return out;
        }
        let same = match &self.painted {
            Painted::Server(p) => Arc::ptr_eq(p, &latest) || p.display_eq(&latest),
            Painted::Overlay(p) => p.display_eq(&latest),
            Painted::None => false,
        };
        if same {
            // the terminal already shows it: no write, painted state kept
            return None;
        }
        let out = self.screen.repaint(self.painted.get());
        self.set_painted(Painted::Server(latest));
        Some(out)
    }

    pub fn force_full_paint(&mut self) {
        self.set_painted(Painted::None);
        self.dirty = true;
    }

    pub fn modes(&self) -> ModeSnapshot {
        let f = self.screen.latest();
        let m = f.modes();
        ModeSnapshot {
            mouse_mode: m.mouse_mode,
            mouse_encoding: m.mouse_enc,
            app_cursor_keys: m.app_cursor,
            bracketed_paste: m.bracketed_paste,
            cursor_visible: f.cursor_visible(),
            alt_screen: m.alt_screen,
        }
    }

    /// Edge bits (cleared by this call) plus the level bits.
    pub fn take_events(&mut self) -> u32 {
        let mut e = std::mem::take(&mut self.edge);
        if self.desync {
            e |= EV_OUT_OF_SYNC;
        }
        if self.shutdown {
            e |= EV_SHUTDOWN;
        }
        e
    }

    pub fn take_title(&mut self) -> Option<Vec<u8>> {
        std::mem::take(&mut self.title_changed)
            .then(|| self.screen.latest().title().as_bytes().to_vec())
    }

    pub fn size(&self) -> (u16, u16) {
        let f = self.screen.latest();
        (f.cols(), f.rows())
    }

    /// Time since the last authentic, newer datagram (or since creation).
    pub fn since_contact(&mut self, now: u64) -> u64 {
        let now = self.clock(now);
        now - self.last_contact.unwrap_or(self.created).min(now)
    }

    pub fn stats(&self) -> StatsSnapshot {
        let sat = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
        StatsSnapshot {
            queued_bytes: self.transport.queued_bytes(),
            send_errors: sat(self.transport.send_errors()),
            rx_errors: sat(self.transport.rx_errors()),
            rx_gaps: sat(self.transport.rx_gaps()),
        }
    }

    pub fn srtt_ms(&self) -> u32 {
        let v = self.transport.srtt_ms();
        if v.is_finite() && v > 0.0 {
            v.round().min(f64::from(u32::MAX)) as u32
        } else {
            0
        }
    }
}
