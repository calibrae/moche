//! VT interpreter and framebuffer.
//!
//! Built from ECMA-48 and the xterm control sequence reference, and from our
//! captures of a stock server (`docs/observations*.md`). The interpreter is a
//! byte-at-a-time state machine with incremental UTF-8 decoding. Unknown
//! sequences are consumed and ignored; nothing here panics or loops on any input.

use unicode_width::UnicodeWidthChar;

/// Largest accepted dimension, columns or rows.
pub const MAX_DIM: u16 = 1000;
const MAX_PARAMS: usize = 32;
const MAX_PARAM: u32 = 65535;
const MAX_OSC: usize = 2048;
const ZWJ: char = '\u{200d}';

/// A cell colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Color {
    /// The terminal's default colour.
    #[default]
    Default,
    /// Palette index (0-7 classic, 8-15 bright, 16-255 cube and greys).
    Indexed(u8),
    /// 24-bit colour.
    Rgb(u8, u8, u8),
}

/// Attribute flag bits for [`Attrs::flags`].
pub mod flag {
    /// Bold.
    pub const BOLD: u8 = 1;
    /// Faint.
    pub const FAINT: u8 = 2;
    /// Italic.
    pub const ITALIC: u8 = 4;
    /// Underline.
    pub const UNDERLINE: u8 = 8;
    /// Blink.
    pub const BLINK: u8 = 16;
    /// Reverse video.
    pub const REVERSE: u8 = 32;
    /// Invisible.
    pub const INVISIBLE: u8 = 64;
    /// Strikethrough.
    pub const STRIKE: u8 = 128;
}

/// Rendition of a cell (SGR state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Attrs {
    /// Foreground.
    pub fg: Color,
    /// Background.
    pub bg: Color,
    /// Bits of [`flag`].
    pub flags: u8,
}

/// One grid cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Base character.
    pub ch: char,
    /// Combining marks and joined characters appended after `ch`.
    pub comb: Option<Box<str>>,
    /// 1 or 2 for a character; 0 for the right half of a wide character.
    pub width: u8,
    /// Rendition.
    pub attrs: Attrs,
}

impl Cell {
    /// A blank cell with the given rendition.
    pub fn blank(attrs: Attrs) -> Self {
        Cell {
            ch: ' ',
            comb: None,
            width: 1,
            attrs,
        }
    }

    /// True for a space with default rendition and no marks.
    pub fn is_plain_blank(&self) -> bool {
        self.ch == ' ' && self.comb.is_none() && self.width == 1 && self.attrs == Attrs::default()
    }

    /// Appends the cell's text (nothing for a continuation cell).
    pub fn push_text(&self, out: &mut String) {
        if self.width == 0 {
            return;
        }
        out.push(self.ch);
        if let Some(c) = &self.comb {
            out.push_str(c);
        }
    }
}

/// Terminal modes that matter to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM (?1): application cursor keys.
    pub app_cursor: bool,
    /// DECOM (?6): origin mode.
    pub origin: bool,
    /// DECAWM (?7): autowrap.
    pub autowrap: bool,
    /// ?12: cursor blink (stored only).
    pub cursor_blink: bool,
    /// ?47/?1047/?1049: the alternate screen is active.
    pub alt_screen: bool,
    /// Mouse reporting level: 0 off, else 1000, 1002 or 1003.
    pub mouse_mode: u16,
    /// Mouse encoding: 0 default, else 1005, 1006 or 1015.
    pub mouse_enc: u16,
    /// ?2004: bracketed paste.
    pub bracketed_paste: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            app_cursor: false,
            origin: false,
            autowrap: true,
            cursor_blink: false,
            alt_screen: false,
            mouse_mode: 0,
            mouse_enc: 0,
            bracketed_paste: false,
        }
    }
}

/// One-shot happenings reported to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenEvent {
    /// BEL.
    Bell,
    /// The window title changed (OSC 0 or 2).
    TitleChanged(String),
    /// The icon name changed (OSC 0 or 1).
    IconNameChanged(String),
}

#[derive(Clone, Debug)]
struct SavedCursor {
    row: usize,
    col: usize,
    pending_wrap: bool,
    pen: Attrs,
    origin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscInter,
    CsiParam,
    CsiInter,
    CsiIgnore,
    Osc,
    StrIgnore,
    /// ESC seen inside a string; `true` if it was an OSC.
    StrEsc(bool),
}

#[derive(Clone, Debug)]
struct Parser {
    state: State,
    params: Vec<u32>,
    cur: u32,
    any: bool,
    marker: u8,
    has_inter: bool,
    esc_inter: u8,
    osc: Vec<u8>,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

impl Parser {
    fn new() -> Self {
        Parser {
            state: State::Ground,
            params: Vec::new(),
            cur: 0,
            any: false,
            marker: 0,
            has_inter: false,
            esc_inter: 0,
            osc: Vec::new(),
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    fn clear_csi(&mut self) {
        self.params.clear();
        self.cur = 0;
        self.any = false;
        self.marker = 0;
        self.has_inter = false;
    }
}

/// A terminal screen: cell grid, cursor, modes, title.
#[derive(Debug)]
pub struct Framebuffer {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    row: usize,
    col: usize,
    pending_wrap: bool,
    cursor_visible: bool,
    pen: Attrs,
    saved: Option<SavedCursor>,
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    modes: Modes,
    /// The inactive (main) grid while the alternate screen is shown.
    alt: Option<Vec<Cell>>,
    title: String,
    icon_name: String,
    last_char: Option<char>,
    join_pending: bool,
    parser: Parser,
    events: Vec<ScreenEvent>,
}

impl Clone for Framebuffer {
    fn clone(&self) -> Self {
        Framebuffer {
            cols: self.cols,
            rows: self.rows,
            cells: self.cells.clone(),
            row: self.row,
            col: self.col,
            pending_wrap: self.pending_wrap,
            cursor_visible: self.cursor_visible,
            pen: self.pen,
            saved: self.saved.clone(),
            top: self.top,
            bottom: self.bottom,
            tabs: self.tabs.clone(),
            modes: self.modes,
            alt: self.alt.clone(),
            title: self.title.clone(),
            icon_name: self.icon_name.clone(),
            last_char: self.last_char,
            join_pending: self.join_pending,
            parser: self.parser.clone(),
            events: self.events.clone(),
        }
    }

    /// Copies `src` into `self` reusing the grid's allocation: a recycled
    /// buffer makes a state copy a memcpy instead of a malloc of the whole
    /// screen (which, above the allocator's mmap threshold, is an mmap, page
    /// faults and an munmap per copy).
    fn clone_from(&mut self, src: &Self) {
        self.cols = src.cols;
        self.rows = src.rows;
        self.cells.clone_from(&src.cells);
        self.row = src.row;
        self.col = src.col;
        self.pending_wrap = src.pending_wrap;
        self.cursor_visible = src.cursor_visible;
        self.pen = src.pen;
        self.saved.clone_from(&src.saved);
        self.top = src.top;
        self.bottom = src.bottom;
        self.tabs.clone_from(&src.tabs);
        self.modes = src.modes;
        self.alt.clone_from(&src.alt);
        self.title.clone_from(&src.title);
        self.icon_name.clone_from(&src.icon_name);
        self.last_char = src.last_char;
        self.join_pending = src.join_pending;
        self.parser = src.parser.clone();
        self.events.clone_from(&src.events);
    }
}

fn clamp_dim(v: u16) -> usize {
    usize::from(v.clamp(1, MAX_DIM))
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c % 8 == 0 && c > 0).collect()
}

impl Framebuffer {
    /// A blank screen, cursor at home and visible, default modes.
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (clamp_dim(cols), clamp_dim(rows));
        Framebuffer {
            cols,
            rows,
            cells: vec![Cell::blank(Attrs::default()); cols * rows],
            row: 0,
            col: 0,
            pending_wrap: false,
            cursor_visible: true,
            pen: Attrs::default(),
            saved: None,
            top: 0,
            bottom: rows - 1,
            tabs: default_tabs(cols),
            modes: Modes::default(),
            alt: None,
            title: String::new(),
            icon_name: String::new(),
            last_char: None,
            join_pending: false,
            parser: Parser::new(),
            events: Vec::new(),
        }
    }

    // ---- accessors ----

    /// Columns.
    pub fn cols(&self) -> u16 {
        self.cols as u16
    }
    /// Rows.
    pub fn rows(&self) -> u16 {
        self.rows as u16
    }
    /// The cell at (row, col); panics only if out of range.
    pub fn cell(&self, row: u16, col: u16) -> &Cell {
        &self.cells[usize::from(row) * self.cols + usize::from(col)]
    }
    /// A row of cells.
    pub fn row_cells(&self, row: u16) -> &[Cell] {
        let r = usize::from(row);
        &self.cells[r * self.cols..(r + 1) * self.cols]
    }
    /// Cursor row, 0-based.
    pub fn cursor_row(&self) -> u16 {
        self.row as u16
    }
    /// Cursor column, 0-based.
    pub fn cursor_col(&self) -> u16 {
        self.col as u16
    }
    /// True when the cursor is shown.
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }
    /// True after writing the last column, before the next character wraps.
    pub fn pending_wrap(&self) -> bool {
        self.pending_wrap
    }
    /// Terminal modes.
    pub fn modes(&self) -> &Modes {
        &self.modes
    }
    /// Window title.
    pub fn title(&self) -> &str {
        &self.title
    }
    /// Icon name.
    pub fn icon_name(&self) -> &str {
        &self.icon_name
    }
    /// Overwrites one cell with `cell`. Display-side edit for an overlay
    /// (predicted echo) drawn on a *copy* of a server state; ignored when
    /// out of range. It does not interpret anything or touch the cursor.
    pub fn set_cell(&mut self, row: u16, col: u16, cell: Cell) {
        let (r, c) = (usize::from(row), usize::from(col));
        if r < self.rows && c < self.cols {
            self.cells[r * self.cols + c] = cell;
        }
    }
    /// Moves the cursor (clamped to the grid); same purpose as [`Framebuffer::set_cell`].
    pub fn set_cursor(&mut self, row: u16, col: u16) {
        self.row = usize::from(row).min(self.rows - 1);
        self.col = usize::from(col).min(self.cols - 1);
        self.pending_wrap = false;
    }
    /// Text of a row, continuation cells skipped, trailing blanks trimmed.
    pub fn row_text(&self, row: u16) -> String {
        let mut s = String::new();
        for c in self.row_cells(row) {
            c.push_text(&mut s);
        }
        s.truncate(s.trim_end_matches(' ').len());
        s
    }
    /// All rows as text, joined by newlines.
    pub fn text(&self) -> String {
        (0..self.rows())
            .map(|r| self.row_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Equality of what the host can see and what [`crate::screen::repaint`] mirrors:
    /// size, cells, cursor position and visibility, title, icon, and the modes
    /// application cursor keys, mouse mode and encoding, bracketed paste. The
    /// pending-wrap flag, autowrap, origin, blink and alternate-screen flags are not compared.
    pub fn display_eq(&self, o: &Framebuffer) -> bool {
        self.cols == o.cols
            && self.rows == o.rows
            && self.cells == o.cells
            && self.row == o.row
            && self.col == o.col
            && self.cursor_visible == o.cursor_visible
            && self.title == o.title
            && self.icon_name == o.icon_name
            && self.modes.app_cursor == o.modes.app_cursor
            && self.modes.mouse_mode == o.modes.mouse_mode
            && self.modes.mouse_enc == o.modes.mouse_enc
            && self.modes.bracketed_paste == o.modes.bracketed_paste
    }

    /// Takes the events accumulated by [`Framebuffer::feed`].
    pub fn take_events(&mut self) -> Vec<ScreenEvent> {
        std::mem::take(&mut self.events)
    }

    // ---- resize ----

    /// Resizes, keeping the top-left content. Cursor clamped, margins and tab stops reset.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (nc, nr) = (clamp_dim(cols), clamp_dim(rows));
        if nc == self.cols && nr == self.rows {
            return;
        }
        let (oc, or) = (self.cols, self.rows);
        self.cells = resized(&self.cells, oc, or, nc, nr);
        if let Some(a) = &mut self.alt {
            *a = resized(a, oc, or, nc, nr);
        }
        self.cols = nc;
        self.rows = nr;
        self.row = self.row.min(nr - 1);
        self.col = self.col.min(nc - 1);
        self.pending_wrap = false;
        self.top = 0;
        self.bottom = nr - 1;
        self.tabs = default_tabs(nc);
        if let Some(s) = &mut self.saved {
            s.row = s.row.min(nr - 1);
            s.col = s.col.min(nc - 1);
        }
        for r in 0..nr {
            self.fix_row(r);
        }
    }

    // ---- feeding ----

    /// Interprets host bytes.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        if self.parser.utf8_need > 0 {
            if b & 0xc0 == 0x80 {
                let p = &mut self.parser;
                p.utf8[p.utf8_len] = b;
                p.utf8_len += 1;
                p.utf8_need -= 1;
                if p.utf8_need == 0 {
                    let buf = p.utf8;
                    let len = p.utf8_len;
                    let ch = std::str::from_utf8(&buf[..len])
                        .ok()
                        .and_then(|s| s.chars().next())
                        .unwrap_or('\u{fffd}');
                    self.print(ch);
                }
                return;
            }
            self.parser.utf8_need = 0;
            self.print('\u{fffd}');
        }
        match self.parser.state {
            State::Ground => match b {
                0x00..=0x1f => self.control(b),
                0x20..=0x7e => self.print_ascii(b),
                0x7f => {}
                0x80..=0xff => {
                    let need = match b {
                        0xc2..=0xdf => 1,
                        0xe0..=0xef => 2,
                        0xf0..=0xf4 => 3,
                        _ => 0,
                    };
                    if need == 0 {
                        self.print('\u{fffd}');
                    } else {
                        let p = &mut self.parser;
                        p.utf8[0] = b;
                        p.utf8_len = 1;
                        p.utf8_need = need;
                    }
                }
            },
            State::Escape => self.escape_byte(b),
            State::EscInter => match b {
                0x20..=0x2f => {}
                0x18 | 0x1a => self.parser.state = State::Ground,
                0x1b => self.parser.state = State::Escape,
                0x30..=0x7e => {
                    self.parser.state = State::Ground;
                    if self.parser.esc_inter == b'#' && b == b'8' {
                        self.decaln();
                    }
                }
                _ => {}
            },
            State::CsiParam | State::CsiInter | State::CsiIgnore => self.csi_byte(b),
            State::Osc => match b {
                0x07 => {
                    self.osc_end();
                    self.parser.state = State::Ground;
                }
                0x18 | 0x1a => self.parser.state = State::Ground,
                0x1b => self.parser.state = State::StrEsc(true),
                0x00..=0x1f => {}
                _ => {
                    if self.parser.osc.len() < MAX_OSC {
                        self.parser.osc.push(b);
                    }
                }
            },
            State::StrIgnore => match b {
                0x18 | 0x1a => self.parser.state = State::Ground,
                0x1b => self.parser.state = State::StrEsc(false),
                _ => {}
            },
            State::StrEsc(is_osc) => {
                if is_osc {
                    self.osc_end();
                }
                self.parser.state = State::Escape;
                if b != b'\\' {
                    self.escape_byte(b);
                } else {
                    self.parser.state = State::Ground;
                }
            }
        }
    }

    fn control(&mut self, b: u8) {
        self.join_pending = false;
        match b {
            0x07 => self.events.push(ScreenEvent::Bell),
            0x08 => {
                self.pending_wrap = false;
                self.col = self.col.saturating_sub(1);
            }
            0x09 => {
                self.pending_wrap = false;
                self.col = self.next_tab(self.col);
            }
            0x0a..=0x0c => {
                self.pending_wrap = false;
                self.index();
            }
            0x0d => {
                self.pending_wrap = false;
                self.col = 0;
            }
            0x18 | 0x1a => self.parser.state = State::Ground,
            0x1b => {
                self.parser.state = State::Escape;
            }
            _ => {}
        }
    }

    fn escape_byte(&mut self, b: u8) {
        self.join_pending = false;
        let ground = |s: &mut Self| s.parser.state = State::Ground;
        match b {
            0x00..=0x1f => self.control(b),
            0x20..=0x2f => {
                self.parser.esc_inter = b;
                self.parser.state = State::EscInter;
            }
            b'[' => {
                self.parser.clear_csi();
                self.parser.state = State::CsiParam;
            }
            b']' => {
                self.parser.osc.clear();
                self.parser.state = State::Osc;
            }
            b'P' | b'X' | b'^' | b'_' => self.parser.state = State::StrIgnore,
            b'7' => {
                self.save_cursor();
                ground(self);
            }
            b'8' => {
                self.restore_cursor();
                ground(self);
            }
            b'D' => {
                self.pending_wrap = false;
                self.index();
                ground(self);
            }
            b'E' => {
                self.pending_wrap = false;
                self.index();
                self.col = 0;
                ground(self);
            }
            b'M' => {
                self.pending_wrap = false;
                self.reverse_index();
                ground(self);
            }
            b'H' => {
                self.tabs[self.col] = true;
                ground(self);
            }
            b'c' => {
                self.reset();
                ground(self);
            }
            _ => ground(self),
        }
    }

    fn csi_byte(&mut self, b: u8) {
        match b {
            0x18 | 0x1a => self.parser.state = State::Ground,
            0x1b => self.parser.state = State::Escape,
            0x00..=0x1f => self.control(b),
            0x7f => {}
            0x80..=0xff => {
                self.parser.state = State::Ground;
                self.byte(b);
            }
            _ => match self.parser.state {
                State::CsiParam => match b {
                    b'0'..=b'9' => {
                        let p = &mut self.parser;
                        p.cur = (p.cur * 10 + u32::from(b - b'0')).min(MAX_PARAM);
                        p.any = true;
                    }
                    b';' | b':' => {
                        let p = &mut self.parser;
                        if p.params.len() < MAX_PARAMS {
                            p.params.push(p.cur);
                        }
                        p.cur = 0;
                        p.any = true;
                    }
                    0x3c..=0x3f => {
                        if self.parser.any || self.parser.marker != 0 {
                            self.parser.state = State::CsiIgnore;
                        } else {
                            self.parser.marker = b;
                        }
                    }
                    0x20..=0x2f => {
                        self.parser.has_inter = true;
                        self.parser.state = State::CsiInter;
                    }
                    _ => self.csi_final(b),
                },
                State::CsiInter => match b {
                    0x20..=0x2f => {}
                    0x30..=0x3f => self.parser.state = State::CsiIgnore,
                    _ => self.csi_final(b),
                },
                _ => {
                    if (0x40..=0x7e).contains(&b) {
                        self.parser.state = State::Ground;
                    }
                }
            },
        }
    }

    fn csi_final(&mut self, f: u8) {
        self.parser.state = State::Ground;
        if self.parser.any && self.parser.params.len() < MAX_PARAMS {
            let c = self.parser.cur;
            self.parser.params.push(c);
        }
        self.dispatch_csi(f);
    }

    fn osc_end(&mut self) {
        let data = std::mem::take(&mut self.parser.osc);
        let Some(semi) = data.iter().position(|&b| b == b';') else {
            return;
        };
        let Some(ps) = std::str::from_utf8(&data[..semi])
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            return;
        };
        let text: String = String::from_utf8_lossy(&data[semi + 1..])
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if matches!(ps, 0 | 2) && text != self.title {
            self.title = text.clone();
            self.events.push(ScreenEvent::TitleChanged(text.clone()));
        }
        if matches!(ps, 0 | 1) && text != self.icon_name {
            self.icon_name = text.clone();
            self.events.push(ScreenEvent::IconNameChanged(text));
        }
    }

    // ---- parameters ----

    fn raw(&self, i: usize) -> u32 {
        self.parser.params.get(i).copied().unwrap_or(0)
    }

    fn arg(&self, i: usize, def: usize) -> usize {
        match self.raw(i) {
            0 => def,
            v => v as usize,
        }
    }

    // ---- CSI ----

    fn dispatch_csi(&mut self, f: u8) {
        self.join_pending = false;
        if self.parser.has_inter {
            return;
        }
        if self.parser.marker == b'?' {
            if f == b'h' || f == b'l' {
                let params = std::mem::take(&mut self.parser.params);
                for p in params {
                    self.set_private(p, f == b'h');
                }
            }
            return;
        }
        if self.parser.marker != 0 {
            return;
        }
        if !matches!(f, b'm' | b'h' | b'l') {
            self.pending_wrap = false;
        }
        let n = self.arg(0, 1);
        let (cols, rows) = (self.cols, self.rows);
        match f {
            b'A' => {
                let lim = if self.row >= self.top { self.top } else { 0 };
                self.row = self.row.saturating_sub(n).max(lim);
            }
            b'B' | b'e' => {
                let lim = if self.row <= self.bottom {
                    self.bottom
                } else {
                    rows - 1
                };
                self.row = (self.row + n).min(lim);
            }
            b'C' | b'a' => self.col = (self.col + n).min(cols - 1),
            b'D' => self.col = self.col.saturating_sub(n),
            b'E' => {
                let lim = if self.row <= self.bottom {
                    self.bottom
                } else {
                    rows - 1
                };
                self.row = (self.row + n).min(lim);
                self.col = 0;
            }
            b'F' => {
                let lim = if self.row >= self.top { self.top } else { 0 };
                self.row = self.row.saturating_sub(n).max(lim);
                self.col = 0;
            }
            b'G' | b'`' => self.col = (n - 1).min(cols - 1),
            b'H' | b'f' => self.cup(self.arg(0, 1), self.arg(1, 1)),
            b'd' => {
                let c = self.col;
                self.cup(n, c + 1);
            }
            b'I' => {
                for _ in 0..n.min(cols) {
                    self.col = self.next_tab(self.col);
                }
            }
            b'Z' => {
                for _ in 0..n.min(cols) {
                    self.col = self.prev_tab(self.col);
                }
            }
            b'J' => match self.raw(0) {
                0 => {
                    self.erase_cells(self.row, self.col, cols);
                    for r in self.row + 1..rows {
                        self.erase_cells(r, 0, cols);
                    }
                }
                1 => {
                    for r in 0..self.row {
                        self.erase_cells(r, 0, cols);
                    }
                    self.erase_cells(self.row, 0, self.col + 1);
                }
                2 => {
                    for r in 0..rows {
                        self.erase_cells(r, 0, cols);
                    }
                }
                _ => {} // 3: scrollback only; we keep none
            },
            b'K' => match self.raw(0) {
                0 => self.erase_cells(self.row, self.col, cols),
                1 => self.erase_cells(self.row, 0, self.col + 1),
                2 => self.erase_cells(self.row, 0, cols),
                _ => {}
            },
            b'L' if (self.top..=self.bottom).contains(&self.row) => {
                self.scroll_down_region(self.row, self.bottom, n);
                self.col = 0;
            }
            b'M' if (self.top..=self.bottom).contains(&self.row) => {
                self.scroll_up_region(self.row, self.bottom, n);
                self.col = 0;
            }
            b'@' => {
                let n = n.min(cols - self.col);
                let base = self.row * cols;
                self.cells[base + self.col..base + cols].rotate_right(n);
                let b = self.blank();
                for c in self.col..self.col + n {
                    self.cells[base + c] = b.clone();
                }
                self.fix_row(self.row);
            }
            b'P' => {
                let n = n.min(cols - self.col);
                let base = self.row * cols;
                self.cells[base + self.col..base + cols].rotate_left(n);
                let b = self.blank();
                for c in cols - n..cols {
                    self.cells[base + c] = b.clone();
                }
                self.fix_row(self.row);
            }
            b'X' => self.erase_cells(self.row, self.col, (self.col + n).min(cols)),
            b'S' => self.scroll_up_region(self.top, self.bottom, n),
            b'T' if self.parser.params.len() <= 1 => {
                self.scroll_down_region(self.top, self.bottom, n);
            }
            b'g' => match self.raw(0) {
                0 => self.tabs[self.col] = false,
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            b'b' => {
                if let Some(c) = self.last_char {
                    for _ in 0..n.min(MAX_PARAM as usize) {
                        self.print(c);
                    }
                }
            }
            b'm' => self.sgr(),
            b'r' => {
                let (t, b) = (self.arg(0, 1), self.arg(1, rows));
                if t < b && b <= rows {
                    self.top = t - 1;
                    self.bottom = b - 1;
                    self.cup(1, 1);
                }
            }
            b's' if self.parser.params.is_empty() => {
                self.save_cursor();
            }
            b'u' if self.parser.params.is_empty() => {
                self.restore_cursor();
            }
            _ => {}
        }
    }

    fn sgr(&mut self) {
        let ps = std::mem::take(&mut self.parser.params);
        if ps.is_empty() {
            self.pen = Attrs::default();
            return;
        }
        let mut i = 0;
        while i < ps.len() {
            let p = ps[i];
            let pen = &mut self.pen;
            match p {
                0 => *pen = Attrs::default(),
                1 => pen.flags |= flag::BOLD,
                2 => pen.flags |= flag::FAINT,
                3 => pen.flags |= flag::ITALIC,
                4 => pen.flags |= flag::UNDERLINE,
                5 | 6 => pen.flags |= flag::BLINK,
                7 => pen.flags |= flag::REVERSE,
                8 => pen.flags |= flag::INVISIBLE,
                9 => pen.flags |= flag::STRIKE,
                22 => pen.flags &= !(flag::BOLD | flag::FAINT),
                23 => pen.flags &= !flag::ITALIC,
                24 => pen.flags &= !flag::UNDERLINE,
                25 => pen.flags &= !flag::BLINK,
                27 => pen.flags &= !flag::REVERSE,
                28 => pen.flags &= !flag::INVISIBLE,
                29 => pen.flags &= !flag::STRIKE,
                30..=37 => pen.fg = Color::Indexed((p - 30) as u8),
                39 => pen.fg = Color::Default,
                40..=47 => pen.bg = Color::Indexed((p - 40) as u8),
                49 => pen.bg = Color::Default,
                90..=97 => pen.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => pen.bg = Color::Indexed((p - 100 + 8) as u8),
                38 | 48 => {
                    let mut color = None;
                    match ps.get(i + 1) {
                        Some(5) => {
                            if let Some(&n) = ps.get(i + 2) {
                                color = Some(Color::Indexed(n.min(255) as u8));
                            }
                            i += 2;
                        }
                        Some(2) => {
                            if let (Some(&r), Some(&g), Some(&b)) =
                                (ps.get(i + 2), ps.get(i + 3), ps.get(i + 4))
                            {
                                color = Some(Color::Rgb(
                                    r.min(255) as u8,
                                    g.min(255) as u8,
                                    b.min(255) as u8,
                                ));
                            }
                            i += 4;
                        }
                        _ => {}
                    }
                    if let Some(c) = color {
                        if p == 38 {
                            pen.fg = c;
                        } else {
                            pen.bg = c;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_private(&mut self, p: u32, on: bool) {
        match p {
            1 => self.modes.app_cursor = on,
            6 => {
                self.modes.origin = on;
                self.cup(1, 1);
            }
            7 => {
                self.modes.autowrap = on;
                if !on {
                    self.pending_wrap = false;
                }
            }
            12 => self.modes.cursor_blink = on,
            25 => self.cursor_visible = on,
            47 | 1047 => self.set_alt(on, false),
            1049 => self.set_alt(on, true),
            1000 | 1002 | 1003 => {
                if on {
                    self.modes.mouse_mode = p as u16;
                } else if self.modes.mouse_mode == p as u16 {
                    self.modes.mouse_mode = 0;
                }
            }
            1005 | 1006 | 1015 => {
                if on {
                    self.modes.mouse_enc = p as u16;
                } else if self.modes.mouse_enc == p as u16 {
                    self.modes.mouse_enc = 0;
                }
            }
            2004 => self.modes.bracketed_paste = on,
            _ => {}
        }
    }

    fn set_alt(&mut self, on: bool, with_cursor: bool) {
        if on {
            if self.alt.is_some() {
                return;
            }
            if with_cursor {
                self.save_cursor();
            }
            let blank = vec![Cell::blank(Attrs::default()); self.cols * self.rows];
            self.alt = Some(std::mem::replace(&mut self.cells, blank));
            self.modes.alt_screen = true;
        } else if let Some(main) = self.alt.take() {
            self.cells = main;
            self.modes.alt_screen = false;
            if with_cursor {
                self.restore_cursor();
            }
        }
    }

    // ---- cursor and scrolling ----

    fn cup(&mut self, r: usize, c: usize) {
        let (lo, hi) = if self.modes.origin {
            (self.top, self.bottom)
        } else {
            (0, self.rows - 1)
        };
        self.row = (lo + r.max(1) - 1).min(hi);
        self.col = (c.max(1) - 1).min(self.cols - 1);
    }

    fn save_cursor(&mut self) {
        self.saved = Some(SavedCursor {
            row: self.row,
            col: self.col,
            pending_wrap: self.pending_wrap,
            pen: self.pen,
            origin: self.modes.origin,
        });
    }

    fn restore_cursor(&mut self) {
        match self.saved.clone() {
            Some(s) => {
                self.row = s.row.min(self.rows - 1);
                self.col = s.col.min(self.cols - 1);
                self.pending_wrap = s.pending_wrap;
                self.pen = s.pen;
                self.modes.origin = s.origin;
            }
            None => {
                self.row = 0;
                self.col = 0;
                self.pending_wrap = false;
                self.pen = Attrs::default();
                self.modes.origin = false;
            }
        }
    }

    fn reset(&mut self) {
        let events = std::mem::take(&mut self.events);
        let (title, icon) = (
            std::mem::take(&mut self.title),
            std::mem::take(&mut self.icon_name),
        );
        *self = Framebuffer::new(self.cols as u16, self.rows as u16);
        self.events = events;
        self.title = title;
        self.icon_name = icon;
    }

    fn decaln(&mut self) {
        let a = Attrs::default();
        self.cells.fill(Cell {
            ch: 'E',
            comb: None,
            width: 1,
            attrs: a,
        });
    }

    fn index(&mut self) {
        if self.row == self.bottom {
            self.scroll_up_region(self.top, self.bottom, 1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.row == self.top {
            self.scroll_down_region(self.top, self.bottom, 1);
        } else if self.row > 0 {
            self.row -= 1;
        }
    }

    fn next_tab(&self, col: usize) -> usize {
        (col + 1..self.cols)
            .find(|&i| self.tabs[i])
            .unwrap_or(self.cols - 1)
    }

    fn prev_tab(&self, col: usize) -> usize {
        (0..col).rev().find(|&i| self.tabs[i]).unwrap_or(0)
    }

    fn blank(&self) -> Cell {
        Cell::blank(Attrs {
            bg: self.pen.bg,
            ..Attrs::default()
        })
    }

    fn scroll_up_region(&mut self, a: usize, b: usize, n: usize) {
        let n = n.min(b - a + 1);
        let cols = self.cols;
        self.cells[a * cols..(b + 1) * cols].rotate_left(n * cols);
        let blank = self.blank();
        for r in b + 1 - n..=b {
            self.cells[r * cols..(r + 1) * cols].fill(blank.clone());
        }
    }

    fn scroll_down_region(&mut self, a: usize, b: usize, n: usize) {
        let n = n.min(b - a + 1);
        let cols = self.cols;
        self.cells[a * cols..(b + 1) * cols].rotate_right(n * cols);
        let blank = self.blank();
        for r in a..a + n {
            self.cells[r * cols..(r + 1) * cols].fill(blank.clone());
        }
    }

    // ---- cell writing ----

    /// Blanks halves of wide characters cut by the range `c0..c1` of `row`.
    fn clear_wide_edges(&mut self, row: usize, c0: usize, c1: usize) {
        let base = row * self.cols;
        let blank = self.blank();
        if c0 > 0 && c0 < self.cols && self.cells[base + c0].width == 0 {
            self.cells[base + c0 - 1] = blank.clone();
        }
        if c1 > 0 && c1 < self.cols && self.cells[base + c1].width == 0 {
            self.cells[base + c1] = blank;
        }
    }

    fn erase_cells(&mut self, row: usize, c0: usize, c1: usize) {
        let c1 = c1.min(self.cols);
        if c0 >= c1 {
            return;
        }
        self.clear_wide_edges(row, c0, c1);
        let blank = self.blank();
        let base = row * self.cols;
        self.cells[base + c0..base + c1].fill(blank);
    }

    /// Repairs orphaned halves of wide characters in a row.
    fn fix_row(&mut self, row: usize) {
        let base = row * self.cols;
        for c in 0..self.cols {
            let w = self.cells[base + c].width;
            let bad = (w == 2 && (c + 1 >= self.cols || self.cells[base + c + 1].width != 0))
                || (w == 0 && (c == 0 || self.cells[base + c - 1].width != 2));
            if bad {
                self.cells[base + c] = Cell::blank(Attrs {
                    bg: self.pen.bg,
                    ..Attrs::default()
                });
            }
        }
    }

    /// Index of the cell holding the last written character, if any.
    fn prev_cell(&self) -> Option<usize> {
        let c = if self.pending_wrap {
            self.col
        } else if self.col > 0 {
            self.col - 1
        } else {
            return None;
        };
        let base = self.row * self.cols;
        Some(if self.cells[base + c].width == 0 && c > 0 {
            base + c - 1
        } else {
            base + c
        })
    }

    fn combine(&mut self, ch: char) {
        let Some(i) = self.prev_cell() else { return };
        let cell = &mut self.cells[i];
        let mut s = cell.comb.take().map(String::from).unwrap_or_default();
        s.push(ch);
        cell.comb = Some(s.into_boxed_str());
        self.join_pending = ch == ZWJ;
    }

    /// `print` for a printable ASCII byte: width 1 by definition, so no width
    /// lookup and none of the wide/combining handling. A pending ZWJ join takes
    /// the general path.
    fn print_ascii(&mut self, b: u8) {
        if self.join_pending {
            self.print(char::from(b));
            return;
        }
        self.last_char = Some(char::from(b));
        if self.pending_wrap {
            self.pending_wrap = false;
            if self.modes.autowrap {
                self.col = 0;
                self.index();
            }
        }
        let (row, col) = (self.row, self.col);
        let base = row * self.cols + col;
        if (col > 0 && self.cells[base].width == 0)
            || (col + 1 < self.cols && self.cells[base + 1].width == 0)
        {
            self.clear_wide_edges(row, col, col + 1);
        }
        let cell = &mut self.cells[base];
        cell.ch = char::from(b);
        cell.width = 1;
        cell.attrs = self.pen;
        cell.comb = None;
        self.col += 1;
        if self.col >= self.cols {
            self.col = self.cols - 1;
            self.pending_wrap = self.modes.autowrap;
        }
    }

    fn print(&mut self, ch: char) {
        let Some(w) = ch.width() else {
            return; // C0/C1 control character that slipped through
        };
        if w == 0 {
            self.combine(ch);
            return;
        }
        if self.join_pending {
            self.join_pending = false;
            if self.prev_cell().is_some() {
                self.combine(ch);
                return;
            }
        }
        self.last_char = Some(ch);
        let cols = self.cols;
        if w > cols {
            return;
        }
        if self.pending_wrap {
            self.pending_wrap = false;
            if self.modes.autowrap {
                self.col = 0;
                self.index();
            }
        }
        if self.col + w > cols {
            if self.modes.autowrap {
                let (row, col) = (self.row, self.col);
                self.erase_cells(row, col, cols);
                self.col = 0;
                self.index();
            } else {
                self.col = cols - w;
            }
        }
        let (row, col) = (self.row, self.col);
        self.clear_wide_edges(row, col, col + w);
        let base = row * cols + col;
        self.cells[base] = Cell {
            ch,
            comb: None,
            width: w as u8,
            attrs: self.pen,
        };
        if w == 2 {
            self.cells[base + 1] = Cell {
                ch: ' ',
                comb: None,
                width: 0,
                attrs: self.pen,
            };
        }
        self.col += w;
        if self.col >= cols {
            self.col = cols - 1;
            self.pending_wrap = self.modes.autowrap;
        }
    }
}

fn resized(old: &[Cell], oc: usize, or: usize, nc: usize, nr: usize) -> Vec<Cell> {
    let mut v = vec![Cell::blank(Attrs::default()); nc * nr];
    for r in 0..or.min(nr) {
        for c in 0..oc.min(nc) {
            v[r * nc + c] = old[r * oc + c].clone();
        }
    }
    v
}
