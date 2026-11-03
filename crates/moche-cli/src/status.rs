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
    shown_secs: Option<u64>,
}

impl Status {
    pub fn update(&mut self, silent_ms: u64) -> StatusUpdate {
        if silent_ms >= SILENCE_MS {
            let secs = silent_ms / 1000;
            if self.shown_secs == Some(secs) {
                return StatusUpdate::None;
            }
            self.shown_secs = Some(secs);
            StatusUpdate::Show(format!("moche: connection lost, last contact {secs} s ago"))
        } else if self.shown_secs.take().is_some() {
            StatusUpdate::Clear
        } else {
            StatusUpdate::None
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
        assert_eq!(s.update(1000), StatusUpdate::None);
        assert_eq!(s.update(4999), StatusUpdate::None);
        assert_eq!(
            s.update(5000),
            StatusUpdate::Show("moche: connection lost, last contact 5 s ago".into())
        );
        assert_eq!(s.update(5400), StatusUpdate::None);
        assert!(matches!(s.update(6000), StatusUpdate::Show(t) if t.contains("6 s")));
        assert_eq!(s.update(10), StatusUpdate::Clear);
        assert_eq!(s.update(20), StatusUpdate::None);
    }

    #[test]
    fn draw_truncates() {
        let d = String::from_utf8(draw_status("abcdef", 3, 24)).unwrap();
        assert!(d.contains("abc\x1b[0m") && !d.contains("abcd"));
        assert!(d.starts_with("\x1b7\x1b[24;1H"));
    }
}
