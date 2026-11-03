//! Numbered framebuffer snapshots and the current screen handed to the host app.
//!
//! The server sends each diff against a numbered earlier state. [`Screen`] keeps
//! those states, applies diffs to the right base, discards states the server no
//! longer needs, and renders the latest one for the host terminal.
//!
//! Decision (v1): the host gets *bytes* for a standard xterm-like terminal.
//! [`Screen::repaint`] writes a full repaint, or, given the previously painted
//! framebuffer, only the changed rows, plus mode and title transitions.

use std::collections::BTreeMap;
use std::fmt::Write as _;

pub use crate::vt::{flag, Attrs, Cell, Color, Framebuffer, Modes, ScreenEvent, MAX_DIM};

/// Why a diff could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// The base state is not held (already discarded, or never received).
    BaseMissing(u64),
    /// A resize with a zero dimension or one above [`MAX_DIM`].
    BadSize(u16, u16),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::BaseMissing(n) => write!(f, "base state {n} not held"),
            ApplyError::BadSize(c, r) => write!(f, "unsupported size {c}x{r}"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// Numbered framebuffer states.
#[derive(Clone, Debug)]
pub struct Screen {
    states: BTreeMap<u64, Framebuffer>,
    events: Vec<ScreenEvent>,
}

impl Screen {
    /// State 0 is a blank `cols` x `rows` screen.
    pub fn new(cols: u16, rows: u16) -> Self {
        let mut states = BTreeMap::new();
        states.insert(0, Framebuffer::new(cols, rows));
        Screen {
            states,
            events: Vec::new(),
        }
    }

    /// Applies a host diff to held state `old_num`, producing `new_num`.
    ///
    /// `resize`, if any, is applied to the cloned base before `bytes` are
    /// interpreted. If `new_num` is already held the call is a no-op (the same
    /// state cannot differ), and the held state is kept.
    pub fn apply(
        &mut self,
        old_num: u64,
        new_num: u64,
        resize: Option<(u16, u16)>,
        bytes: &[u8],
    ) -> Result<(), ApplyError> {
        if let Some((c, r)) = resize {
            if c == 0 || r == 0 || c > MAX_DIM || r > MAX_DIM {
                return Err(ApplyError::BadSize(c, r));
            }
        }
        if self.states.contains_key(&new_num) {
            return Ok(());
        }
        let mut fb = self
            .states
            .get(&old_num)
            .ok_or(ApplyError::BaseMissing(old_num))?
            .clone();
        if let Some((c, r)) = resize {
            fb.resize(c, r);
        }
        fb.feed(bytes);
        self.events.extend(fb.take_events());
        self.states.insert(new_num, fb);
        Ok(())
    }

    /// Drops states numbered below `throwaway_num`; the latest state is always kept.
    pub fn discard_below(&mut self, throwaway_num: u64) {
        let latest = self.latest_num();
        self.states
            .retain(|&n, _| n >= throwaway_num || n == latest);
    }

    /// Highest state number held.
    pub fn latest_num(&self) -> u64 {
        self.states.keys().next_back().copied().unwrap_or(0)
    }

    /// The highest state.
    pub fn latest(&self) -> &Framebuffer {
        // never empty: state 0 at creation, and discard keeps the latest
        self.states
            .values()
            .next_back()
            .expect("screen always holds a state")
    }

    /// True if state `num` is held.
    pub fn has_state(&self, num: u64) -> bool {
        self.states.contains_key(&num)
    }

    /// A held state, if any.
    pub fn state(&self, num: u64) -> Option<&Framebuffer> {
        self.states.get(&num)
    }

    /// Numbers of held states, ascending.
    pub fn held(&self) -> Vec<u64> {
        self.states.keys().copied().collect()
    }

    /// Bytes for the host terminal that repaint [`Screen::latest`].
    /// See [`repaint`].
    pub fn repaint(&self, prev: Option<&Framebuffer>) -> Vec<u8> {
        repaint(self.latest(), prev)
    }

    /// Events (bell, title changes) since the last call.
    pub fn take_events(&mut self) -> Vec<ScreenEvent> {
        std::mem::take(&mut self.events)
    }
}

fn is_blank(c: &Cell) -> bool {
    c.is_plain_blank()
}

fn sgr(out: &mut String, a: &Attrs) {
    out.push_str("\x1b[0");
    let names = [
        (flag::BOLD, 1),
        (flag::FAINT, 2),
        (flag::ITALIC, 3),
        (flag::UNDERLINE, 4),
        (flag::BLINK, 5),
        (flag::REVERSE, 7),
        (flag::INVISIBLE, 8),
        (flag::STRIKE, 9),
    ];
    for (bit, code) in names {
        if a.flags & bit != 0 {
            let _ = write!(out, ";{code}");
        }
    }
    for (color, base, bright, ext) in [(a.fg, 30, 90, 38), (a.bg, 40, 100, 48)] {
        match color {
            Color::Default => {}
            Color::Indexed(n) if n < 8 => {
                let _ = write!(out, ";{}", base + u32::from(n));
            }
            Color::Indexed(n) if n < 16 => {
                let _ = write!(out, ";{}", bright + u32::from(n) - 8);
            }
            Color::Indexed(n) => {
                let _ = write!(out, ";{ext};5;{n}");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(out, ";{ext};2;{r};{g};{b}");
            }
        }
    }
    out.push('m');
}

fn mode(out: &mut String, n: u16, on: bool) {
    let _ = write!(out, "\x1b[?{n}{}", if on { 'h' } else { 'l' });
}

fn paint_modes(out: &mut String, fb: &Modes, prev: Option<&Modes>) {
    if prev.is_none_or(|p| p.app_cursor != fb.app_cursor) {
        mode(out, 1, fb.app_cursor);
    }
    if prev.is_none_or(|p| p.bracketed_paste != fb.bracketed_paste) {
        mode(out, 2004, fb.bracketed_paste);
    }
    for (new, old, all) in [
        (
            fb.mouse_mode,
            prev.map(|p| p.mouse_mode),
            [1003, 1002, 1000],
        ),
        (fb.mouse_enc, prev.map(|p| p.mouse_enc), [1015, 1006, 1005]),
    ] {
        if old == Some(new) {
            continue;
        }
        match old {
            Some(0) => {}
            Some(o) => mode(out, o, false),
            None => all.iter().for_each(|&m| mode(out, m, false)),
        }
        if new != 0 {
            mode(out, new, true);
        }
    }
}

fn paint_row(out: &mut String, cells: &[Cell], r: usize, full: bool) {
    let end = cells
        .iter()
        .rposition(|c| !is_blank(c))
        .map_or(0, |i| i + 1);
    if full && end == 0 {
        return;
    }
    let _ = write!(out, "\x1b[{};1H\x1b[0m", r + 1);
    let mut cur = Attrs::default();
    let mut resync = false;
    for (c, cell) in cells.iter().enumerate().take(end) {
        if cell.width == 0 {
            continue;
        }
        if resync {
            // a trailing ZWJ would otherwise glue this cell onto the previous one
            let _ = write!(out, "\x1b[{}G", c + 1);
        }
        if cell.attrs != cur {
            sgr(out, &cell.attrs);
            cur = cell.attrs;
        }
        cell.push_text(out);
        resync = cell
            .comb
            .as_deref()
            .is_some_and(|s| s.ends_with('\u{200d}'));
    }
    if !full && end < cells.len() {
        if cur != Attrs::default() {
            out.push_str("\x1b[0m");
        }
        out.push_str("\x1b[K");
    }
}

/// Bytes that make a standard xterm-like host terminal show `fb`.
///
/// `prev` is the framebuffer last painted on that host. With `None`, or when
/// the size differs, everything is redrawn (`ESC[2J` and all rows); otherwise
/// only rows that differ are rewritten (each ends with erase-to-end-of-line).
/// Mode changes (application cursor keys, mouse mode and encoding, bracketed
/// paste) and title changes are emitted as transitions against `prev`. The cursor
/// is hidden while painting, then restored. Rows start from a known `ESC[0m` state.
pub fn repaint(fb: &Framebuffer, prev: Option<&Framebuffer>) -> Vec<u8> {
    let mut o = String::new();
    o.push_str("\x1b[?25l");
    let full = prev.is_none_or(|p| p.cols() != fb.cols() || p.rows() != fb.rows());
    paint_modes(&mut o, fb.modes(), prev.map(|p| p.modes()));
    if prev.is_none_or(|p| p.title() != fb.title()) && (prev.is_some() || !fb.title().is_empty()) {
        let _ = write!(o, "\x1b]2;{}\x07", fb.title());
    }
    if prev.is_none_or(|p| p.icon_name() != fb.icon_name())
        && (prev.is_some() || !fb.icon_name().is_empty())
    {
        let _ = write!(o, "\x1b]1;{}\x07", fb.icon_name());
    }
    if full {
        o.push_str("\x1b[0m\x1b[?6l\x1b[?7h\x1b[r\x1b[2J");
    }
    for r in 0..fb.rows() {
        let cells = fb.row_cells(r);
        if !full && prev.is_some_and(|p| p.row_cells(r) == cells) {
            continue;
        }
        paint_row(&mut o, cells, usize::from(r), full);
    }
    o.push_str("\x1b[0m");
    let _ = write!(o, "\x1b[{};{}H", fb.cursor_row() + 1, fb.cursor_col() + 1);
    if fb.cursor_visible() {
        o.push_str("\x1b[?25h");
    }
    o.into_bytes()
}
