//! Pure UI logic: exit-key detection and the "connection lost" line.

/// Ctrl-^ (0x1e), the escape key.
const ESCAPE: u8 = 0x1e;

/// Detects Ctrl-^ `.` across read boundaries.
#[derive(Default)]
pub struct ExitKey {
    pending: bool,
}

impl ExitKey {
    /// Filters input: returns the bytes to forward and whether to quit.
    /// Ctrl-^ Ctrl-^ forwards one literal Ctrl-^; Ctrl-^ x forwards both.
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.pending {
                self.pending = false;
                match b {
                    b'.' => return (out, true),
                    ESCAPE => out.push(ESCAPE),
                    _ => out.extend([ESCAPE, b]),
                }
            } else if b == ESCAPE {
                self.pending = true;
            } else {
                out.push(b);
            }
        }
        (out, false)
    }
}

/// Silence after which the status line appears.
pub const SILENCE_MS: u64 = 5000;

#[derive(Debug, PartialEq, Eq)]
pub enum StatusUpdate {
    None,
    Show(String),
    Clear,
}

/// Decides when to show, refresh or clear the status line.
#[derive(Default)]
pub struct Status {
    shown: Option<String>,
}

impl Status {
    /// The text currently on screen, if any.
    pub fn shown(&self) -> Option<&str> {
        self.shown.as_deref()
    }

    /// `silent_ms`: time since the server was last heard; `desync`: a diff
    /// could not be applied. Silence wins over desync.
    pub fn update(&mut self, silent_ms: u64, desync: bool) -> StatusUpdate {
        let want = if silent_ms >= SILENCE_MS {
            Some(format!(
                "moche: connection lost, last contact {} s ago",
                silent_ms / 1000
            ))
        } else if desync {
            Some("moche: out of sync".to_string())
        } else {
            None
        };
        if want == self.shown {
            return StatusUpdate::None;
        }
        self.shown = want.clone();
        match want {
            Some(t) => StatusUpdate::Show(t),
            None => StatusUpdate::Clear,
        }
    }
}

/// Escape bytes drawing `text` on the bottom row without moving the cursor.
pub fn draw_status(text: &str, cols: u16, rows: u16) -> Vec<u8> {
    let text: String = text.chars().take(usize::from(cols)).collect();
    format!("\x1b7\x1b[{rows};1H\x1b[7m{text}\x1b[0m\x1b[K\x1b8").into_bytes()
}

/// Escape bytes erasing the bottom row, then `repaint` (may be empty).
pub fn clear_status(repaint: &[u8], rows: u16) -> Vec<u8> {
    let mut v = format!("\x1b7\x1b[{rows};1H\x1b[2K").into_bytes();
    v.extend_from_slice(repaint);
    v.extend_from_slice(b"\x1b8");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_key_simple_and_split() {
        let mut k = ExitKey::default();
        assert_eq!(k.feed(b"ab\x1e."), (b"ab".to_vec(), true));
        let mut k = ExitKey::default();
        assert_eq!(k.feed(b"a\x1e"), (b"a".to_vec(), false));
        assert_eq!(k.feed(b".z"), (Vec::new(), true));
    }

    #[test]
    fn exit_key_other_follow_ups() {
        let mut k = ExitKey::default();
        assert_eq!(k.feed(b"\x1ex"), (b"\x1ex".to_vec(), false));
        assert_eq!(k.feed(b"\x1e\x1e."), (b"\x1e.".to_vec(), false));
        assert_eq!(k.feed(b"."), (b".".to_vec(), false));
    }

    #[test]
    fn status_lifecycle() {
        let mut s = Status::default();
        assert_eq!(s.update(1000, false), StatusUpdate::None);
        assert_eq!(s.update(4999, false), StatusUpdate::None);
        assert_eq!(
            s.update(5000, false),
            StatusUpdate::Show("moche: connection lost, last contact 5 s ago".into())
        );
        assert_eq!(s.update(5400, false), StatusUpdate::None);
        assert!(matches!(s.update(6000, false), StatusUpdate::Show(t) if t.contains("6 s")));
        assert_eq!(s.update(10, false), StatusUpdate::Clear);
        assert_eq!(s.update(20, false), StatusUpdate::None);
        assert_eq!(
            s.update(20, true),
            StatusUpdate::Show("moche: out of sync".into())
        );
        assert!(matches!(s.update(7000, true), StatusUpdate::Show(t) if t.contains("lost")));
        assert_eq!(s.update(7000, false), StatusUpdate::None);
        assert_eq!(s.update(0, false), StatusUpdate::Clear);
    }

    #[test]
    fn draw_truncates() {
        let d = String::from_utf8(draw_status("abcdef", 3, 24)).unwrap();
        assert!(d.contains("abc\x1b[0m") && !d.contains("abcd"));
        assert!(d.starts_with("\x1b7\x1b[24;1H"));
    }
}
