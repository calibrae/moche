//! Predictive local echo: a display-only overlay on the latest server state.
//!
//! Design and every constant: `docs/predict.md` (labelled SOURCED / OBSERVED /
//! OUR CHOICE). In short: when the user types, the host may show the typed
//! characters at once, then reconcile with what the server really displays.
//!
//! **Display-only.** A [`Predictor`] never touches a `Screen`, a `Transport` or
//! anything that reaches the wire. It reads the latest server [`Framebuffer`]
//! and the input byte counts the transport reports, and it writes only into a
//! *copy* of the framebuffer handed to [`Predictor::apply_overlay`]. The host
//! keeps two notions apart: the server's latest state (the base of every diff)
//! and what the terminal currently shows (server state plus overlay).
//!
//! **Never lies for long.** Every prediction is resolved by the server's echo
//! (the cell and the cursor match), by an echo ack that covers it without a
//! match (cancelled), or by a timeout ([`Predictor::tick`]).
//!
//! **No ghost secrets.** Predictions are grouped in epochs. A prediction is shown
//! only after a prediction of the same epoch was confirmed by the server's real
//! echo (unless the mode is [`PredictMode::Always`]). Keys that cannot be
//! predicted (Enter, Tab, control keys, escape sequences, pastes) end the epoch
//! and block prediction until the server has acknowledged them.
//!
//! Sans-IO like the rest of the core: the host passes `now_ms`, the smoothed
//! RTT and the byte counts; nothing here sleeps or allocates per frame.

use unicode_width::UnicodeWidthChar;

use crate::vt::{flag, Attrs, Cell, Framebuffer};

/// Smoothed RTT (ms) above which Adaptive mode starts predicting.
pub const ADAPTIVE_ON_MS: f64 = 30.0;
/// Smoothed RTT (ms) below which Adaptive mode stops (hysteresis).
pub const ADAPTIVE_OFF_MS: f64 = 20.0;
/// Smoothed RTT (ms) above which unconfirmed predictions are underlined.
pub const FLAG_ON_MS: f64 = 100.0;
/// Smoothed RTT (ms) below which the underline is dropped (hysteresis).
pub const FLAG_OFF_MS: f64 = 80.0;
/// Confirmed predictions needed to trust the connection again after a
/// contradiction.
pub const CONFIRM_STREAK: u32 = 2;
/// Most predictions kept pending; the next key then ends the epoch.
pub const MAX_PENDING: usize = 64;
/// A single input call longer than this is a paste: never predicted.
pub const PASTE_BYTES: usize = 1024;
/// Floor of the expiry of an unconfirmed prediction, in ms.
pub const MIN_TIMEOUT_MS: u64 = 3000;
/// The expiry is at least this many smoothed RTTs.
pub const TIMEOUT_RTT_FACTOR: f64 = 4.0;
/// Server silence (ms) after which every prediction is dropped.
pub const SILENT_MS: u64 = 3000;

/// When to show predictions (the `--predict` option).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PredictMode {
    /// Never predict.
    Never,
    /// Predict while the smoothed RTT is high enough to notice.
    #[default]
    Adaptive,
    /// Always predict. The first character of an epoch is shown before any
    /// confirmation: a secret typed where nothing echoes is visible until the
    /// echo ack names it (about one RTT), then hidden again.
    Always,
}

impl std::str::FromStr for PredictMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "never" => Ok(PredictMode::Never),
            "adaptive" => Ok(PredictMode::Adaptive),
            "always" => Ok(PredictMode::Always),
            _ => Err(format!(
                "bad --predict value {s:?}, expected never, adaptive or always"
            )),
        }
    }
}

/// A read-only snapshot of the internal state, for debug traces (the CLI's
/// `MOCHE_PREDICT_TRACE`). Reading it changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PredictDebug {
    /// Adaptive hysteresis: predictions are being made.
    pub adaptive_on: bool,
    /// The underline threshold is exceeded.
    pub flag_on: bool,
    /// The epoch has a confirmed prediction.
    pub gate_open: bool,
    /// A contradiction happened and the streak is still short.
    pub tentative: bool,
    /// Confirmations since the last contradiction.
    pub streak: u32,
    /// Pending (unresolved) predictions.
    pub pending: usize,
    /// A frozen overlay (shown at Enter) is still displayed.
    pub frozen: bool,
    /// Input bytes the echo ack has covered.
    pub echoed: u64,
    /// No prediction until `echoed` reaches this.
    pub barrier: u64,
    /// The last smoothed RTT given to the predictor, in ms.
    pub srtt_ms: f64,
}

/// Counters, for tests and `--stats`-style reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredictStats {
    /// Keys turned into predictions.
    pub predicted: u64,
    /// Predictions the server's echo confirmed.
    pub confirmed: u64,
    /// Chains cancelled because the server contradicted them.
    pub mismatches: u64,
    /// Chains or frozen overlays dropped by timeout or silence.
    pub expired: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Put(char),
    Erase,
    Move,
}

#[derive(Clone, Debug)]
struct Pred {
    kind: Kind,
    /// The cell written (unused for `Move`).
    row: u16,
    col: u16,
    /// Cursor after this key.
    after: (u16, u16),
    /// Cumulative input bytes through this key.
    end_bytes: u64,
    at_ms: u64,
    /// The server cell at `(row, col)` when first touched in the chain.
    orig: Option<Cell>,
}

/// Predictions shown when an unpredictable key arrived: they stay on screen
/// until the server has acknowledged that key, so a typed line does not vanish
/// at Enter and reappear one RTT later. Display only, never confirmed.
#[derive(Clone, Debug)]
struct Frozen {
    preds: Vec<Pred>,
    until_bytes: u64,
    at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Char(char),
    Backspace,
    Left,
    Right,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Ks {
    #[default]
    Ground,
    Esc,
    Ss3,
    Csi,
}

/// Incremental splitter of the host's input bytes into keys.
#[derive(Debug, Default)]
struct KeyParser {
    state: Ks,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
    csi: [u8; 8],
    csi_len: usize,
    csi_overflow: bool,
    in_paste: bool,
}

impl KeyParser {
    /// One byte: the key it completes (if any) and whether the byte must be
    /// fed again (it interrupted an unfinished sequence).
    fn step(&mut self, b: u8) -> (Option<Key>, bool) {
        match self.state {
            Ks::Ground => {
                if self.utf8_need > 0 {
                    if b & 0xc0 == 0x80 {
                        self.utf8[self.utf8_len] = b;
                        self.utf8_len += 1;
                        self.utf8_need -= 1;
                        if self.utf8_need == 0 {
                            let k = std::str::from_utf8(&self.utf8[..self.utf8_len])
                                .ok()
                                .and_then(|s| s.chars().next())
                                .map_or(Key::Other, Key::Char);
                            return (Some(k), false);
                        }
                        return (None, false);
                    }
                    self.utf8_need = 0;
                    return (Some(Key::Other), true);
                }
                match b {
                    0x1b => {
                        self.state = Ks::Esc;
                        (None, false)
                    }
                    0x08 | 0x7f => (Some(Key::Backspace), false),
                    0x00..=0x1f => (Some(Key::Other), false),
                    0x20..=0x7e => (Some(Key::Char(char::from(b))), false),
                    0xc2..=0xdf => self.utf8_start(b, 2),
                    0xe0..=0xef => self.utf8_start(b, 3),
                    0xf0..=0xf4 => self.utf8_start(b, 4),
                    _ => (Some(Key::Other), false),
                }
            }
            Ks::Esc => match b {
                b'[' => {
                    self.state = Ks::Csi;
                    self.csi_len = 0;
                    self.csi_overflow = false;
                    (None, false)
                }
                b'O' => {
                    self.state = Ks::Ss3;
                    (None, false)
                }
                0x1b => (Some(Key::Other), false),
                _ => {
                    self.state = Ks::Ground;
                    (Some(Key::Other), false)
                }
            },
            Ks::Ss3 => {
                self.state = Ks::Ground;
                match b {
                    b'D' => (Some(Key::Left), false),
                    b'C' => (Some(Key::Right), false),
                    _ => (Some(Key::Other), false),
                }
            }
            Ks::Csi => match b {
                0x20..=0x3f => {
                    if self.csi_len < self.csi.len() {
                        self.csi[self.csi_len] = b;
                        self.csi_len += 1;
                    } else {
                        self.csi_overflow = true;
                    }
                    (None, false)
                }
                0x40..=0x7e => {
                    self.state = Ks::Ground;
                    let params = &self.csi[..self.csi_len];
                    if self.csi_overflow {
                        return (Some(Key::Other), false);
                    }
                    match (params, b) {
                        ([], b'D') => (Some(Key::Left), false),
                        ([], b'C') => (Some(Key::Right), false),
                        (b"200", b'~') => {
                            self.in_paste = true;
                            (Some(Key::Other), false)
                        }
                        (b"201", b'~') => {
                            self.in_paste = false;
                            (Some(Key::Other), false)
                        }
                        _ => (Some(Key::Other), false),
                    }
                }
                _ => {
                    self.state = Ks::Ground;
                    (Some(Key::Other), true)
                }
            },
        }
    }

    fn utf8_start(&mut self, b: u8, len: usize) -> (Option<Key>, bool) {
        self.utf8[0] = b;
        self.utf8_len = 1;
        self.utf8_need = len - 1;
        (None, false)
    }
}

/// How a pending chain stands against a server state.
#[derive(Debug)]
struct NetCell {
    row: u16,
    col: u16,
    /// Value after the prefix: a character, `' '` for an erase.
    val: char,
    orig: char,
}

/// The predictive-echo state machine. See the module docs.
#[derive(Debug)]
pub struct Predictor {
    mode: PredictMode,
    adaptive_on: bool,
    flag_on: bool,
    last_srtt: f64,
    parser: KeyParser,
    pending: Vec<Pred>,
    /// Scratch for `resolve` (kept to avoid an allocation per server state).
    net: Vec<NetCell>,
    frozen: Option<Frozen>,
    /// Cursor of the server state when the chain started (or was rebased).
    base_cursor: (u16, u16),
    /// Grid size the chain was built for.
    dims: (u16, u16),
    /// A prediction of the current epoch was confirmed.
    gate_open: bool,
    /// A contradiction happened and fewer than `CONFIRM_STREAK` confirmations
    /// followed.
    tentative: bool,
    streak: u32,
    /// Input bytes the server echo ack has covered.
    echoed: u64,
    /// No prediction until the echo ack covers this many bytes.
    barrier: u64,
    version: u64,
    stats: PredictStats,
}

impl Predictor {
    /// A predictor in `mode`, with nothing pending.
    pub fn new(mode: PredictMode) -> Self {
        Predictor {
            mode,
            adaptive_on: mode == PredictMode::Always,
            flag_on: false,
            last_srtt: 1000.0,
            parser: KeyParser::default(),
            pending: Vec::new(),
            net: Vec::new(),
            frozen: None,
            base_cursor: (0, 0),
            dims: (0, 0),
            gate_open: false,
            tentative: false,
            streak: 0,
            echoed: 0,
            barrier: 0,
            version: 0,
            stats: PredictStats::default(),
        }
    }

    /// The current mode.
    pub fn mode(&self) -> PredictMode {
        self.mode
    }

    /// Changes the mode; pending predictions are dropped.
    pub fn set_mode(&mut self, mode: PredictMode) {
        if mode != self.mode {
            self.mode = mode;
            self.adaptive_on = mode == PredictMode::Always;
            self.drop_all();
        }
    }

    /// Counters since creation.
    pub fn stats(&self) -> PredictStats {
        self.stats
    }

    /// Snapshot of the internal state, for debug traces. No side effect.
    pub fn debug_state(&self) -> PredictDebug {
        PredictDebug {
            adaptive_on: self.adaptive_on,
            flag_on: self.flag_on,
            gate_open: self.gate_open,
            tentative: self.tentative,
            streak: self.streak,
            pending: self.pending.len(),
            frozen: self.frozen.is_some(),
            echoed: self.echoed,
            barrier: self.barrier,
            srtt_ms: self.last_srtt,
        }
    }

    /// True if predictions may currently be made.
    fn active(&self) -> bool {
        match self.mode {
            PredictMode::Never => false,
            PredictMode::Adaptive => self.adaptive_on,
            PredictMode::Always => true,
        }
    }

    fn show(&self) -> bool {
        !self.tentative && (self.gate_open || self.mode == PredictMode::Always)
    }

    /// True if [`Predictor::apply_overlay`] would change the framebuffer.
    pub fn is_displaying(&self) -> bool {
        if !self.active() {
            return false;
        }
        self.frozen.is_some() || (!self.pending.is_empty() && self.show())
    }

    /// Number of pending (unresolved) predictions, shown or hidden.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn bump(&mut self) {
        self.version += 1;
    }

    fn drop_all(&mut self) {
        if !self.pending.is_empty() || self.frozen.is_some() {
            self.bump();
        }
        self.pending.clear();
        self.frozen = None;
        self.gate_open = false;
    }

    fn update_thresholds(&mut self, srtt_ms: f64) {
        self.last_srtt = srtt_ms;
        if self.mode == PredictMode::Adaptive {
            let was = self.adaptive_on;
            if !was && srtt_ms >= ADAPTIVE_ON_MS {
                self.adaptive_on = true;
            } else if was && srtt_ms < ADAPTIVE_OFF_MS {
                self.adaptive_on = false;
                self.drop_all();
            }
        }
        let f = self.flag_on;
        if !f && srtt_ms >= FLAG_ON_MS {
            self.flag_on = true;
        } else if f && srtt_ms < FLAG_OFF_MS {
            self.flag_on = false;
        }
        if f != self.flag_on {
            self.bump();
        }
    }

    fn timeout_ms(&self) -> u64 {
        MIN_TIMEOUT_MS.max((TIMEOUT_RTT_FACTOR * self.last_srtt.min(1e6)) as u64)
    }

    // ---- input ----

    /// The host fed `bytes` to the transport; `input_total_after` is
    /// `Transport::input_bytes_total()` right after. `server` is the latest
    /// server state. Returns true if the displayed overlay may have changed.
    pub fn on_input(
        &mut self,
        now_ms: u64,
        bytes: &[u8],
        server: &Framebuffer,
        srtt_ms: f64,
        input_total_after: u64,
    ) -> bool {
        if self.mode == PredictMode::Never {
            return false;
        }
        let v0 = self.version;
        self.update_thresholds(srtt_ms);
        if !self.active() {
            return self.version != v0;
        }
        let start = input_total_after.saturating_sub(bytes.len() as u64);
        if bytes.len() > PASTE_BYTES {
            self.parser = KeyParser::default();
            self.seal(input_total_after, now_ms);
            return self.version != v0;
        }
        for (i, &b) in bytes.iter().enumerate() {
            let end = start + i as u64 + 1;
            loop {
                let (key, again) = self.parser.step(b);
                if let Some(k) = key {
                    let k = if self.parser.in_paste { Key::Other } else { k };
                    self.key(k, if again { end - 1 } else { end }, now_ms, server);
                }
                if !again {
                    break;
                }
            }
        }
        self.version != v0
    }

    fn key(&mut self, k: Key, end: u64, now_ms: u64, fb: &Framebuffer) {
        let kind = match k {
            Key::Char(c) => Kind::Put(c),
            Key::Backspace => Kind::Erase,
            Key::Left | Key::Right => Kind::Move,
            Key::Other => {
                self.seal(end, now_ms);
                return;
            }
        };
        if !self.try_predict(kind, k == Key::Right, end, now_ms, fb) {
            self.seal(end, now_ms);
        }
    }

    /// Effective `(is_blank, width)` of a cell: the last pending write wins.
    fn eff(&self, fb: &Framebuffer, row: u16, col: u16) -> (bool, u8) {
        for p in self.pending.iter().rev() {
            if p.row == row && p.col == col {
                match p.kind {
                    Kind::Put(c) => return (c == ' ', 1),
                    Kind::Erase => return (true, 1),
                    Kind::Move => {}
                }
            }
        }
        let c = fb.cell(row, col);
        (c.ch == ' ' && c.comb.is_none(), c.width)
    }

    fn blank_from(&self, fb: &Framebuffer, row: u16, col: u16) -> bool {
        (col..fb.cols()).all(|c| self.eff(fb, row, c) == (true, 1))
    }

    fn try_predict(
        &mut self,
        kind: Kind,
        right: bool,
        end: u64,
        now_ms: u64,
        fb: &Framebuffer,
    ) -> bool {
        let m = fb.modes();
        let dims = (fb.cols(), fb.rows());
        if self.echoed < self.barrier
            || m.alt_screen
            || m.mouse_mode != 0
            || !fb.cursor_visible()
            || self.pending.len() >= MAX_PENDING
            || (!self.pending.is_empty() && self.dims != dims)
        {
            return false;
        }
        let (row, col) = self
            .pending
            .last()
            .map_or((fb.cursor_row(), fb.cursor_col()), |p| p.after);
        if self.pending.is_empty() && fb.pending_wrap() {
            return false;
        }
        let cols = fb.cols();
        let (cell_pos, after, orig) = match kind {
            Kind::Put(ch) => {
                if ch.is_control()
                    || UnicodeWidthChar::width(ch) != Some(1)
                    || col + 1 >= cols
                    || self.eff(fb, row, col).1 != 1
                    || !self.blank_from(fb, row, col)
                {
                    return false;
                }
                ((row, col), (row, col + 1), Some(fb.cell(row, col).clone()))
            }
            Kind::Erase => {
                if col == 0
                    || self.eff(fb, row, col - 1) == (true, 1)
                    || self.eff(fb, row, col - 1).1 != 1
                    || !self.blank_from(fb, row, col)
                {
                    return false;
                }
                (
                    (row, col - 1),
                    (row, col - 1),
                    Some(fb.cell(row, col - 1).clone()),
                )
            }
            Kind::Move => {
                if right {
                    if col + 1 >= cols
                        || self.eff(fb, row, col) != (false, 1)
                        || self.eff(fb, row, col + 1).1 == 0
                    {
                        return false;
                    }
                    ((0, 0), (row, col + 1), None)
                } else {
                    if col == 0 || self.eff(fb, row, col - 1).1 != 1 {
                        return false;
                    }
                    ((0, 0), (row, col - 1), None)
                }
            }
        };
        if self.pending.is_empty() {
            self.base_cursor = (fb.cursor_row(), fb.cursor_col());
            self.dims = dims;
        }
        self.pending.push(Pred {
            kind,
            row: cell_pos.0,
            col: cell_pos.1,
            after,
            end_bytes: end,
            at_ms: now_ms,
            orig,
        });
        self.stats.predicted += 1;
        self.bump();
        true
    }

    /// An unpredictable key: end the epoch, keep what is shown on screen until
    /// the server acknowledges the key, and block prediction until then.
    fn seal(&mut self, end: u64, now_ms: u64) {
        if !self.pending.is_empty() {
            if self.show() {
                self.frozen = Some(Frozen {
                    preds: std::mem::take(&mut self.pending),
                    until_bytes: end,
                    at_ms: now_ms,
                });
            } else {
                self.pending.clear();
            }
            self.bump();
        } else if let Some(f) = &mut self.frozen {
            f.until_bytes = f.until_bytes.max(end);
        }
        self.gate_open = false;
        self.barrier = self.barrier.max(end);
    }

    /// The local terminal was resized: nothing predicted survives.
    pub fn on_resize(&mut self) {
        self.drop_all();
    }

    // ---- server side ----

    /// The server state changed (a diff was applied) or an echo ack arrived.
    /// `echoed_input_bytes` is `Transport::echoed_input_bytes()`. Returns true
    /// if the displayed overlay may have changed.
    pub fn on_server_state(
        &mut self,
        server: &Framebuffer,
        echoed_input_bytes: u64,
        _now_ms: u64,
    ) -> bool {
        if self.mode == PredictMode::Never {
            return false;
        }
        let v0 = self.version;
        self.echoed = self.echoed.max(echoed_input_bytes);
        if self.pending.is_empty() && self.frozen.is_none() {
            return false;
        }
        let dims = (server.cols(), server.rows());
        if server.modes().alt_screen || self.dims != dims {
            self.drop_all();
            return self.version != v0;
        }
        if let Some(f) = &self.frozen {
            let row = f.preds.last().map_or(0, |p| p.after.0);
            if self.echoed >= f.until_bytes || server.cursor_row() != row {
                self.frozen = None;
                self.bump();
            }
        }
        if !self.pending.is_empty() {
            self.resolve(server);
        }
        self.version != v0
    }

    fn cancel(&mut self, penalty: bool) {
        self.pending.clear();
        self.gate_open = false;
        if penalty {
            self.tentative = true;
            self.streak = 0;
            self.stats.mismatches += 1;
        }
        self.bump();
    }

    /// Compares the pending chain with the server state.
    fn resolve(&mut self, fb: &Framebuffer) {
        let cursor = (fb.cursor_row(), fb.cursor_col());
        let n = self.pending.len();
        let covered = self
            .pending
            .iter()
            .rposition(|p| p.end_bytes <= self.echoed)
            .map_or(0, |i| i + 1);
        let plain = |row: u16, col: u16, want: char| {
            let c = fb.cell(row, col);
            c.ch == want && c.comb.is_none() && c.width == 1
        };
        let mut net = std::mem::take(&mut self.net);
        net.clear();
        let mut best = 0usize;
        // some j >= covered matches without being distinguishable from "nothing
        // happened" (typed and erased again)
        let mut flat_match_beyond = false;
        for j in 1..=n {
            let p = &self.pending[j - 1];
            let (val, touched) = match p.kind {
                Kind::Put(c) => (c, true),
                Kind::Erase => (' ', true),
                Kind::Move => (' ', false),
            };
            if touched {
                let orig = self
                    .pending
                    .iter()
                    .take(j)
                    .find(|q| touched_same(q, p))
                    .and_then(|q| q.orig.as_ref())
                    .map_or(' ', |c| c.ch);
                match net.iter_mut().find(|c| c.row == p.row && c.col == p.col) {
                    Some(c) => c.val = val,
                    None => net.push(NetCell {
                        row: p.row,
                        col: p.col,
                        val,
                        orig,
                    }),
                }
            }
            if cursor == p.after && net.iter().all(|c| plain(c.row, c.col, c.val)) {
                let dist = p.after != self.base_cursor || net.iter().any(|c| c.val != c.orig);
                if dist {
                    best = j;
                } else if j >= covered {
                    flat_match_beyond = true;
                }
            }
        }
        self.net = net;
        if best > 0 {
            self.stats.confirmed += best as u64;
            self.streak = self.streak.saturating_add(best as u32);
            self.gate_open = true;
            if self.tentative && self.streak >= CONFIRM_STREAK {
                self.tentative = false;
            }
            self.pending.drain(..best);
            self.base_cursor = cursor;
            // rebase what is left: the server now shows the confirmed prefix
            for i in 0..self.pending.len() {
                if self.pending[i].orig.is_some() && !touched_before(&self.pending, i) {
                    let (r, c) = (self.pending[i].row, self.pending[i].col);
                    self.pending[i].orig = Some(fb.cell(r, c).clone());
                }
            }
            self.bump();
            if covered > best {
                self.cancel(true);
            }
            return;
        }
        let baseline = cursor == self.base_cursor
            && self.pending.iter().all(|p| match (&p.orig, p.kind) {
                (Some(o), Kind::Put(_) | Kind::Erase) => {
                    let c = fb.cell(p.row, p.col);
                    c.ch == o.ch && c.comb == o.comb
                }
                _ => true,
            });
        if covered > 0 {
            // the server has had time to echo these and showed nothing of it
            self.cancel(!flat_match_beyond);
        } else if !baseline {
            // neither the old state nor any predicted one: something else happened
            self.cancel(true);
        }
    }

    /// Time-based housekeeping; call with the loop's wake-ups. `silent_ms` is the
    /// time since the server was last heard. Returns true if the overlay changed.
    pub fn tick(&mut self, now_ms: u64, srtt_ms: f64, silent_ms: u64) -> bool {
        if self.mode == PredictMode::Never {
            return false;
        }
        let v0 = self.version;
        self.update_thresholds(srtt_ms);
        let to = self.timeout_ms();
        let silent = silent_ms >= SILENT_MS;
        if let Some(p) = self.pending.first() {
            if silent || now_ms.saturating_sub(p.at_ms) >= to {
                self.stats.expired += 1;
                self.cancel(!silent);
            }
        }
        if let Some(f) = &self.frozen {
            if silent || now_ms.saturating_sub(f.at_ms) >= to {
                self.stats.expired += 1;
                self.frozen = None;
                self.bump();
            }
        }
        self.version != v0
    }

    /// Earliest `now_ms` at which [`Predictor::tick`] will change something.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        let to = self.timeout_ms();
        let a = self.pending.first().map(|p| p.at_ms + to);
        let b = self.frozen.as_ref().map(|f| f.at_ms + to);
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    // ---- rendering ----

    /// Draws the displayed predictions into `fb`, which must be a copy of the
    /// latest server state: predicted characters (underlined once the RTT is
    /// above the flag threshold) and the predicted cursor. No-op when nothing is
    /// displayed. This is the only place a prediction becomes visible.
    pub fn apply_overlay(&self, fb: &mut Framebuffer) {
        if !self.is_displaying() {
            return;
        }
        let preds: &[Pred] = match &self.frozen {
            Some(f) => &f.preds,
            None => &self.pending,
        };
        let (cols, rows) = (fb.cols(), fb.rows());
        for p in preds {
            if p.after.0 >= rows || p.after.1 >= cols {
                continue;
            }
            match p.kind {
                Kind::Put(ch) => {
                    let mut attrs = p.orig.as_ref().map_or(Attrs::default(), |c| c.attrs);
                    if self.flag_on {
                        attrs.flags |= flag::UNDERLINE;
                    }
                    fb.set_cell(
                        p.row,
                        p.col,
                        Cell {
                            ch,
                            comb: None,
                            width: 1,
                            attrs,
                        },
                    );
                }
                Kind::Erase => {
                    let mut attrs = Attrs::default();
                    if self.flag_on {
                        attrs.flags |= flag::UNDERLINE;
                    }
                    fb.set_cell(p.row, p.col, Cell::blank(attrs));
                }
                Kind::Move => {}
            }
        }
        if let Some(last) = preds.last() {
            fb.set_cursor(last.after.0, last.after.1);
        }
    }
}

fn touched_same(a: &Pred, b: &Pred) -> bool {
    a.row == b.row
        && a.col == b.col
        && !matches!(a.kind, Kind::Move)
        && !matches!(b.kind, Kind::Move)
}

/// True if an earlier pending prediction touched the same cell as `pending[i]`.
fn touched_before(pending: &[Pred], i: usize) -> bool {
    pending[..i].iter().any(|q| touched_same(q, &pending[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-line world: server framebuffer plus predictor and byte counters.
    struct H {
        fb: Framebuffer,
        p: Predictor,
        total: u64,
        echoed: u64,
        now: u64,
        srtt: f64,
    }

    impl H {
        fn new(mode: PredictMode) -> Self {
            let mut fb = Framebuffer::new(20, 5);
            fb.feed(b"$ ");
            H {
                fb,
                p: Predictor::new(mode),
                total: 0,
                echoed: 0,
                now: 1000,
                srtt: 200.0,
            }
        }
        fn key(&mut self, b: &[u8]) {
            self.total += b.len() as u64;
            self.p
                .on_input(self.now, b, &self.fb, self.srtt, self.total);
        }
        fn server(&mut self, b: &[u8]) {
            self.fb.feed(b);
            self.p.on_server_state(&self.fb, self.echoed, self.now);
        }
        fn ack(&mut self) {
            self.echoed = self.total;
            self.p.on_server_state(&self.fb, self.echoed, self.now);
        }
        fn shown_fb(&self) -> Framebuffer {
            let mut c = self.fb.clone();
            self.p.apply_overlay(&mut c);
            c
        }
        fn shown(&self) -> String {
            self.shown_fb().row_text(0)
        }
        /// Types `s` (one key per call), the server echoes and acks each.
        fn type_echoed(&mut self, s: &str) {
            for ch in s.chars() {
                let mut b = [0u8; 4];
                let b = ch.encode_utf8(&mut b).as_bytes().to_vec();
                self.key(&b);
                self.server(&b);
                self.ack();
            }
        }
        /// Opens the gate: one echoed character.
        fn open(&mut self) {
            self.type_echoed("a");
            assert_eq!(self.p.pending_len(), 0);
        }
    }

    #[test]
    fn first_char_hidden_then_shown() {
        let mut h = H::new(PredictMode::Adaptive);
        h.key(b"a");
        assert_eq!(h.shown(), "$", "first char of an epoch is not shown");
        h.server(b"a");
        assert_eq!(h.shown(), "$ a");
        h.ack();
        h.key(b"b");
        assert_eq!(
            h.shown(),
            "$ ab",
            "shown at once once the epoch is confirmed"
        );
        assert_eq!(h.shown_fb().cursor_col(), 4);
        h.server(b"b");
        h.ack();
        assert!(h.shown_fb().display_eq(&h.fb), "no residue");
        assert!(!h.p.is_displaying());
    }

    #[test]
    fn no_ghost_secret_in_adaptive() {
        let mut h = H::new(PredictMode::Adaptive);
        for ch in b"hunter2" {
            h.key(&[*ch]);
            assert!(h.shown_fb().display_eq(&h.fb), "nothing displayed");
        }
        h.ack();
        assert!(h.shown_fb().display_eq(&h.fb));
        assert_eq!(h.p.pending_len(), 0);
        assert_eq!(h.p.stats().mismatches, 1);
    }

    #[test]
    fn always_leaks_only_until_the_ack() {
        let mut h = H::new(PredictMode::Always);
        h.key(b"p");
        h.key(b"w");
        assert_eq!(h.shown(), "$ pw", "always shows at once (documented risk)");
        h.ack();
        assert_eq!(h.shown(), "$", "gone when the echo ack covers it");
        h.key(b"x");
        assert_eq!(h.shown(), "$", "tentative: hidden until confirmed again");
    }

    #[test]
    fn tentative_needs_a_streak() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"x");
        assert_eq!(h.shown(), "$ ax");
        h.ack();
        assert_eq!(h.shown(), "$ a", "contradicted: removed");
        // the app now echoes again, but we are tentative
        h.key(b"b");
        h.server(b"b");
        h.ack();
        h.key(b"c");
        assert_eq!(h.shown(), "$ ab", "one confirmation is not enough");
        h.server(b"c");
        h.ack();
        h.key(b"d");
        assert_eq!(h.shown(), "$ abcd", "streak reached: shown again");
    }

    #[test]
    fn expiry_removes_unconfirmed() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"z");
        assert_eq!(h.shown(), "$ az");
        let dl = h.p.next_deadline_ms().unwrap();
        assert_eq!(dl, h.now + MIN_TIMEOUT_MS.max(800));
        assert!(!h.p.tick(dl - 1, 200.0, 0));
        assert!(h.p.tick(dl, 200.0, 0));
        assert_eq!(h.shown(), "$ a");
        assert_eq!(h.p.next_deadline_ms(), None);
        assert_eq!(h.p.stats().expired, 1);
    }

    #[test]
    fn expiry_scales_with_rtt() {
        let mut h = H::new(PredictMode::Adaptive);
        h.srtt = 2000.0;
        h.open();
        h.key(b"z");
        h.p.tick(h.now + 1, 2000.0, 0);
        assert_eq!(h.p.next_deadline_ms(), Some(h.now + 8000));
    }

    #[test]
    fn silence_drops_everything() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"z");
        assert!(h.p.tick(h.now + 10, 200.0, SILENT_MS));
        assert_eq!(h.shown(), "$ a");
    }

    #[test]
    fn backspace_and_arrows() {
        let mut h = H::new(PredictMode::Adaptive);
        h.type_echoed("abc");
        h.key(&[0x7f]);
        assert_eq!(h.shown(), "$ ab");
        assert_eq!(h.shown_fb().cursor_col(), 4);
        h.server(b"\x08 \x08");
        h.ack();
        assert!(h.shown_fb().display_eq(&h.fb));
        // left moves the cursor only; right at the end of the text is not predicted
        h.key(b"\x1b[D");
        assert_eq!(h.shown_fb().cursor_col(), 3);
        h.server(b"\x1b[D");
        h.ack();
        h.key(b"\x1bOC");
        assert_eq!(h.shown_fb().cursor_col(), 4, "SS3 right over text");
        h.server(b"\x1b[C");
        h.ack();
        h.key(b"\x1b[C");
        assert_eq!(
            h.shown_fb().cursor_col(),
            4,
            "right at the end: not predicted"
        );
        assert_eq!(h.p.pending_len(), 0);
    }

    #[test]
    fn typing_inside_text_is_not_predicted() {
        let mut h = H::new(PredictMode::Adaptive);
        h.type_echoed("abc");
        h.key(b"\x1b[D");
        h.server(b"\x1b[D");
        h.ack();
        h.key(b"X");
        assert_eq!(h.p.pending_len(), 0, "insert in the middle shifts the tail");
        assert!(h.shown_fb().display_eq(&h.fb));
    }

    #[test]
    fn unpredictable_keys_end_the_epoch() {
        for key in [
            &b"\r"[..],
            b"\t",
            b"\x03",
            b"\x1b",
            b"\x1b[A",
            b"\x1b[1;5D",
            b"\x1bx",
        ] {
            let mut h = H::new(PredictMode::Adaptive);
            h.open();
            h.key(b"b");
            assert_eq!(h.shown(), "$ ab");
            h.key(key);
            h.key(b"c");
            assert_eq!(h.p.pending_len(), 0, "{key:?}: blocked until acked");
            h.server(b"b");
            h.ack();
            h.server(b"c");
            assert!(h.shown_fb().display_eq(&h.fb), "{key:?}");
            // after the ack the epoch is new: first char hidden again
            h.key(b"d");
            assert_eq!(h.p.pending_len(), 1, "{key:?}");
            assert_eq!(
                h.shown_fb().row_text(0),
                h.fb.row_text(0),
                "{key:?}: hidden"
            );
        }
    }

    #[test]
    fn typed_line_survives_enter_until_the_server_catches_up() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"b");
        h.key(b"\r");
        assert_eq!(h.shown(), "$ ab", "still shown after Enter");
        h.server(b"b\r\n");
        assert!(!h.p.is_displaying(), "server moved to the next line");
        assert!(h.shown_fb().display_eq(&h.fb));
        h.ack();
        // typing right after Enter, before the server's reply: not predicted
        h.key(b"x");
        assert_eq!(h.p.pending_len(), 1);
    }

    #[test]
    fn paste_is_not_predicted() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        let big = vec![b'x'; PASTE_BYTES + 1];
        h.key(&big);
        assert_eq!(h.p.pending_len(), 0);
        h.ack();
        h.key(b"\x1b[200~ab");
        h.key(b"cd\x1b[201~");
        assert_eq!(h.p.pending_len(), 0, "bracketed paste");
        h.ack();
        h.key(b"y");
        assert_eq!(h.p.pending_len(), 1, "typing resumes after the paste");
    }

    #[test]
    fn wide_and_combining_chars_are_not_predicted() {
        for s in ["字", "\u{301}", "\u{200d}", "😀"] {
            let mut h = H::new(PredictMode::Adaptive);
            h.open();
            h.key(s.as_bytes());
            assert_eq!(h.p.pending_len(), 0, "{s:?}");
        }
        // plain multi-byte, also split across two reads
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(&[0xc3]);
        h.key(&[0xa9]);
        assert_eq!(h.shown(), "$ aé");
        h.server("é".as_bytes());
        h.ack();
        assert!(h.shown_fb().display_eq(&h.fb));
        assert_eq!(h.p.stats().mismatches, 0);
    }

    #[test]
    fn last_column_is_not_predicted() {
        let mut h = H::new(PredictMode::Adaptive);
        h.fb.feed(b"\r");
        h.fb.feed(&[b'.'; 18]);
        h.server(b"");
        h.open();
        // cursor now at column 19 (the last one)
        assert_eq!(h.fb.cursor_col(), 19);
        h.key(b"z");
        assert_eq!(h.p.pending_len(), 0);
        assert_eq!(h.fb.cursor_col(), 19);
    }

    #[test]
    fn cursor_hidden_alt_screen_and_mouse_block_prediction() {
        for setup in [&b"\x1b[?25l"[..], b"\x1b[?1000h"] {
            let mut h = H::new(PredictMode::Adaptive);
            h.open();
            h.server(setup);
            h.key(b"q");
            assert_eq!(h.p.pending_len(), 0, "{setup:?}");
        }
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"q");
        assert_eq!(h.shown(), "$ aq");
        h.server(b"\x1b[?1049h");
        assert!(
            h.shown_fb().display_eq(&h.fb),
            "alt screen drops predictions"
        );
        h.key(b"w");
        assert_eq!(h.p.pending_len(), 0);
    }

    #[test]
    fn resize_and_scroll_invalidate() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"q");
        h.fb.resize(30, 6);
        h.p.on_server_state(&h.fb, h.echoed, h.now);
        assert!(h.shown_fb().display_eq(&h.fb));

        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"q");
        h.p.on_resize();
        assert!(h.shown_fb().display_eq(&h.fb));

        // the server scrolled the screen instead of echoing
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"q");
        h.server(b"\r\n\r\n\r\n\r\n\r\n\r\nout");
        assert!(h.shown_fb().display_eq(&h.fb));
        assert_eq!(h.p.pending_len(), 0);
    }

    #[test]
    fn never_does_nothing() {
        let mut h = H::new(PredictMode::Never);
        h.type_echoed("abc");
        h.key(b"d");
        assert_eq!(h.p.pending_len(), 0);
        assert!(!h.p.is_displaying());
        assert_eq!(h.p.stats(), PredictStats::default());
        assert_eq!(h.p.next_deadline_ms(), None);
    }

    #[test]
    fn adaptive_hysteresis_and_flag() {
        let mut h = H::new(PredictMode::Adaptive);
        h.srtt = 10.0;
        h.type_echoed("abc");
        h.key(b"d");
        assert_eq!(h.p.pending_len(), 0, "fast link: nothing predicted");
        h.ack();
        h.srtt = 25.0;
        h.key(b"e");
        assert_eq!(h.p.pending_len(), 0, "between the thresholds, still off");
        h.ack();
        h.srtt = 35.0;
        h.key(b"f");
        assert_eq!(h.p.pending_len(), 1, "on from 30 ms");
        h.srtt = 25.0;
        h.key(b"g");
        assert_eq!(h.p.pending_len(), 2, "hysteresis keeps it on");
        h.srtt = 10.0;
        h.key(b"h");
        assert_eq!(h.p.pending_len(), 0, "off below 20 ms, pending dropped");
    }

    #[test]
    fn unconfirmed_cells_are_underlined_on_slow_links() {
        let underlined_at =
            |h: &H, col: u16| h.shown_fb().cell(0, col).attrs.flags & flag::UNDERLINE != 0;
        let underlined = |h: &H| underlined_at(h, 3);
        let mut h = H::new(PredictMode::Adaptive);
        h.srtt = 120.0;
        h.open();
        h.key(b"b");
        assert!(underlined(&h));
        h.server(b"b");
        h.ack();
        h.p.tick(h.now, 70.0, 0);
        h.key(b"c");
        // 70 ms: between flag thresholds, stays on
        assert_eq!(h.shown(), "$ abc");
        assert!(underlined_at(&h, 4));
        h.p.tick(h.now, 60.0, 0);
        assert!(!underlined(&h));
    }

    #[test]
    fn key_parser_cases() {
        let feed = |chunks: &[&[u8]]| {
            let mut p = KeyParser::default();
            let mut out = Vec::new();
            for c in chunks {
                for &b in *c {
                    loop {
                        let (k, again) = p.step(b);
                        out.extend(k);
                        if !again {
                            break;
                        }
                    }
                }
            }
            out
        };
        use Key::*;
        assert_eq!(feed(&[b"a\x7fb"]), [Char('a'), Backspace, Char('b')]);
        assert_eq!(feed(&[b"\x1b[D\x1b[C\x1bOD"]), [Left, Right, Left]);
        assert_eq!(feed(&[b"\x1b", b"[", b"D"]), [Left], "split sequence");
        assert_eq!(feed(&[b"\x1b[1;5D"]), [Other]);
        assert_eq!(feed(&[b"\x1b[A", b"x"]), [Other, Char('x')]);
        assert_eq!(feed(&[b"\r\t\x03"]), [Other, Other, Other]);
        assert_eq!(feed(&["é".as_bytes()]), [Char('é')]);
        assert_eq!(
            feed(&[&[0xc3, b'a']]),
            [Other, Char('a')],
            "bad UTF-8 resyncs"
        );
        assert_eq!(feed(&[&[0xff]]), [Other]);
        assert_eq!(feed(&[b"\x1b[200~"]), [Other]);
    }

    #[test]
    fn overlay_never_changes_the_server_state() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"b");
        let before = h.fb.clone();
        let _ = h.shown_fb();
        assert!(h.fb.display_eq(&before));
    }

    #[test]
    fn partial_echo_keeps_the_rest_pending() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"b");
        h.key(b"c");
        h.key(b"d");
        assert_eq!(h.shown(), "$ abcd");
        h.server(b"b");
        assert_eq!(h.p.pending_len(), 2);
        assert_eq!(h.shown(), "$ abcd");
        h.server(b"c");
        h.server(b"d");
        h.ack();
        assert_eq!(h.p.pending_len(), 0);
        assert!(h.shown_fb().display_eq(&h.fb));
    }

    #[test]
    fn color_echo_still_confirms() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"b");
        h.server(b"\x1b[31mb\x1b[0m");
        h.ack();
        assert_eq!(h.p.stats().mismatches, 0);
        assert_eq!(h.p.pending_len(), 0);
    }

    #[test]
    fn typed_then_erased_is_not_a_contradiction() {
        let mut h = H::new(PredictMode::Adaptive);
        h.open();
        h.key(b"b");
        h.key(&[0x7f]);
        h.server(b"b\x08 \x08");
        h.ack();
        assert_eq!(h.p.stats().mismatches, 0);
        assert!(h.shown_fb().display_eq(&h.fb));
    }
}
