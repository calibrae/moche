//! How host state reaches the user's terminal.
//!
//! [`Display`] is the seam between the session loop and the screen model.
//! [`ScreenDisplay`] (the default) keeps numbered framebuffers in
//! `moche_core::screen::Screen` and paints minimal diffs. [`Passthrough`]
//! (hidden `--passthrough`) writes host diffs straight out, for debugging.

use std::sync::Arc;

use moche_core::predict::{PredictMode, Predictor};
use moche_core::screen::{repaint, Framebuffer, Screen, ScreenEvent};

/// Consumes events from the transport, produces bytes for the tty.
pub trait Display {
    /// A server state `old` -> `new` (optionally with a size change).
    fn on_host_diff(&mut self, old: u64, new: u64, resize: Option<(u16, u16)>, bytes: &[u8]);
    /// The server will never again diff against a state below `n`.
    fn on_throwaway(&mut self, n: u64);
    /// The local terminal changed size. The server answers with a resize and
    /// a repaint; until then we keep painting the old-size framebuffer
    /// (a smaller terminal clips or wraps it, whatever the terminal does).
    fn on_local_resize(&mut self);
    /// True while a diff we could not apply may have left us out of sync.
    fn out_of_sync(&self) -> bool;
    /// Bytes to write to the tty now (drains pending output).
    fn flush(&mut self) -> Vec<u8>;
    /// Full repaint of the current screen; empty if unsupported.
    fn repaint(&mut self) -> Vec<u8>;
    /// Keystrokes were sent: `input_total` is the transport's cumulative input
    /// byte count after them (for predictive echo).
    fn on_input(&mut self, _now_ms: u64, _keys: &[u8], _srtt_ms: f64, _input_total: u64) {}
    /// The server's echo ack now covers `echoed` input bytes.
    fn on_progress(&mut self, _echoed: u64, _now_ms: u64) {}
    /// Housekeeping (prediction expiry); `silent_ms` since the server was heard.
    fn tick(&mut self, _now_ms: u64, _srtt_ms: f64, _silent_ms: u64) {}
    /// When [`Display::tick`] next has work, in the transport's clock.
    fn next_deadline_ms(&self) -> Option<u64> {
        None
    }
}

/// Writes host bytes through unchanged.
#[derive(Default)]
pub struct Passthrough {
    out: Vec<u8>,
}

impl Display for Passthrough {
    fn on_host_diff(&mut self, _old: u64, _new: u64, _resize: Option<(u16, u16)>, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }
    fn on_throwaway(&mut self, _n: u64) {}
    fn on_local_resize(&mut self) {}
    fn out_of_sync(&self) -> bool {
        false
    }
    fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
    fn repaint(&mut self) -> Vec<u8> {
        Vec::new()
    }
}

/// What the terminal currently shows.
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

/// Framebuffer-backed display.
///
/// Two notions are kept apart: the server's latest state (`screen`, the base of
/// every diff) and what the terminal shows (`painted`, which may include the
/// predictor's overlay). Predictions are only ever drawn into a copy.
pub struct ScreenDisplay {
    screen: Screen,
    predictor: Predictor,
    painted: Painted,
    /// A recycled overlay grid, so an active overlay costs one memcpy per paint.
    spare: Option<Box<Framebuffer>>,
    dirty: bool,
    bell: bool,
    desync: bool,
}

impl ScreenDisplay {
    pub fn new(cols: u16, rows: u16, predict: PredictMode) -> Self {
        ScreenDisplay {
            screen: Screen::new(cols, rows),
            predictor: Predictor::new(predict),
            painted: Painted::None,
            spare: None,
            dirty: true,
            bell: false,
            desync: false,
        }
    }

    fn set_painted(&mut self, p: Painted) {
        if let Painted::Overlay(old) = std::mem::replace(&mut self.painted, p) {
            self.spare = Some(old);
        }
    }
}

impl Display for ScreenDisplay {
    fn on_host_diff(&mut self, old: u64, new: u64, resize: Option<(u16, u16)>, bytes: &[u8]) {
        match self.screen.apply(old, new, resize, bytes) {
            Ok(()) => {
                self.desync = false;
                self.dirty = true;
            }
            // Safety net only: the transport delivers a diff only if its base
            // is held (and never acks one it did not deliver), so this should
            // not trigger with a sane server. Flag it for the status line.
            Err(_) => self.desync = true,
        }
        for ev in self.screen.take_events() {
            // Titles are painted by repaint.
            if ev == ScreenEvent::Bell {
                self.bell = true;
            }
        }
    }
    fn on_throwaway(&mut self, n: u64) {
        self.screen.discard_below(n);
    }
    fn on_local_resize(&mut self) {
        self.predictor.on_resize();
        self.painted = Painted::None;
        self.dirty = true;
    }
    fn out_of_sync(&self) -> bool {
        self.desync
    }
    fn on_input(&mut self, now_ms: u64, keys: &[u8], srtt_ms: f64, input_total: u64) {
        let changed =
            self.predictor
                .on_input(now_ms, keys, self.screen.latest(), srtt_ms, input_total);
        self.dirty |= changed;
    }
    fn on_progress(&mut self, echoed: u64, now_ms: u64) {
        let changed = self
            .predictor
            .on_server_state(self.screen.latest(), echoed, now_ms);
        self.dirty |= changed;
    }
    fn tick(&mut self, now_ms: u64, srtt_ms: f64, silent_ms: u64) {
        let changed = self.predictor.tick(now_ms, srtt_ms, silent_ms);
        self.dirty |= changed;
    }
    fn next_deadline_ms(&self) -> Option<u64> {
        self.predictor.next_deadline_ms()
    }
    fn flush(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if std::mem::take(&mut self.dirty) {
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
                let same = self.painted.get().is_some_and(|p| p.display_eq(&buf));
                if !same {
                    out = repaint(&buf, self.painted.get());
                }
                self.set_painted(Painted::Overlay(buf));
            } else {
                // A frame that changes nothing the terminal shows (echo-ack, ack)
                // costs no tty write.
                let same = match &self.painted {
                    Painted::Server(p) => Arc::ptr_eq(p, &latest) || p.display_eq(&latest),
                    Painted::Overlay(p) => p.display_eq(&latest),
                    Painted::None => false,
                };
                if !same {
                    out = self.screen.repaint(self.painted.get());
                }
                self.set_painted(Painted::Server(latest));
            }
        }
        if std::mem::take(&mut self.bell) {
            out.push(0x07);
        }
        out
    }
    fn repaint(&mut self) -> Vec<u8> {
        self.painted = Painted::None;
        self.dirty = true;
        self.flush()
    }
}

/// Picks the display implementation.
pub fn make_display(
    passthrough: bool,
    cols: u16,
    rows: u16,
    predict: PredictMode,
) -> Box<dyn Display> {
    if passthrough {
        Box::new(Passthrough::default())
    } else {
        Box::new(ScreenDisplay::new(cols, rows, predict))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_orders_and_drains() {
        let mut d = Passthrough::default();
        d.on_host_diff(0, 1, None, b"ab");
        d.on_host_diff(1, 2, Some((80, 24)), b"cd");
        assert_eq!(d.flush(), b"abcd");
        assert!(d.flush().is_empty());
    }

    #[test]
    fn screen_display_paints_and_flags_desync() {
        let mut d = ScreenDisplay::new(10, 3, PredictMode::Never);
        d.on_host_diff(0, 1, None, b"hi\x07");
        let out = d.flush();
        assert!(out.windows(2).any(|w| w == b"hi") && out.contains(&7));
        let mut fb = Framebuffer::new(10, 3);
        fb.feed(&out);
        assert!(fb.text().contains("hi"));
        assert!(d.flush().is_empty());
        d.on_host_diff(9, 10, None, b"x");
        assert!(d.out_of_sync());
        d.on_host_diff(1, 2, None, b"!");
        assert!(!d.out_of_sync());
    }

    /// Paints what the host terminal receives into a model terminal.
    fn host(d: &mut ScreenDisplay, term: &mut Framebuffer) {
        let out = d.flush();
        term.feed(&out);
    }

    #[test]
    fn predicted_echo_is_painted_then_reconciled() {
        let mut d = ScreenDisplay::new(20, 4, PredictMode::Adaptive);
        let mut term = Framebuffer::new(20, 4);
        d.on_host_diff(0, 1, None, b"$ ");
        host(&mut d, &mut term);
        // the first char of an epoch is hidden until the server echoes it
        d.on_input(0, b"a", 200.0, 1);
        host(&mut d, &mut term);
        assert_eq!(term.row_text(0), "$");
        d.on_host_diff(1, 2, None, b"a");
        d.on_progress(1, 210);
        host(&mut d, &mut term);
        assert_eq!(term.row_text(0), "$ a");
        // now shown at once, before the server echoes
        d.on_input(300, b"b", 200.0, 2);
        host(&mut d, &mut term);
        assert_eq!(term.row_text(0), "$ ab");
        assert_eq!(term.cursor_col(), 4);
        assert!(
            !term.display_eq(d.screen.latest()),
            "overlay differs from the server"
        );
        // the real echo arrives: identical, and nothing more to paint
        d.on_host_diff(2, 3, None, b"b");
        d.on_progress(2, 500);
        host(&mut d, &mut term);
        assert!(term.display_eq(d.screen.latest()));
        assert!(d.flush().is_empty());
    }

    #[test]
    fn contradicted_prediction_is_erased_from_the_terminal() {
        let mut d = ScreenDisplay::new(20, 4, PredictMode::Adaptive);
        let mut term = Framebuffer::new(20, 4);
        d.on_host_diff(0, 1, None, b"$ ");
        d.on_input(0, b"a", 200.0, 1);
        d.on_host_diff(1, 2, None, b"a");
        d.on_progress(1, 210);
        d.on_input(300, b"s", 200.0, 2);
        host(&mut d, &mut term);
        assert_eq!(term.row_text(0), "$ as");
        // the echo ack covers the key and the server showed nothing for it
        d.on_progress(2, 500);
        host(&mut d, &mut term);
        assert_eq!(term.row_text(0), "$ a");
        assert!(term.display_eq(d.screen.latest()));
        // and an expiry also leaves no residue
        d.on_input(600, b"x", 200.0, 3);
        d.on_host_diff(2, 3, None, b"");
        host(&mut d, &mut term);
        d.tick(700 + 4000, 200.0, 0);
        host(&mut d, &mut term);
        assert!(term.display_eq(d.screen.latest()));
    }

    #[test]
    fn never_mode_paints_exactly_the_server_state() {
        let mut d = ScreenDisplay::new(20, 4, PredictMode::Never);
        let mut term = Framebuffer::new(20, 4);
        d.on_host_diff(0, 1, None, b"$ ");
        d.on_input(0, b"a", 500.0, 1);
        d.on_progress(1, 10);
        host(&mut d, &mut term);
        assert_eq!(term.row_text(0), "$");
        assert!(term.display_eq(d.screen.latest()));
        assert_eq!(d.next_deadline_ms(), None);
    }
}
