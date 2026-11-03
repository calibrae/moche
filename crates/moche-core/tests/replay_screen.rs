//! Replays real mosh-server 1.4.0 captures through ssp::Transport into Screen.
//! Skipped (with a note) when the capture files are not present.

use std::path::{Path, PathBuf};

use moche_core::crypto::Session;
use moche_core::network::Cipher;
use moche_core::screen::{repaint, Color, Framebuffer, Screen, ScreenEvent};
use moche_core::ssp::{Event, Transport};

struct Pkt {
    ms: u64,
    to_server: bool,
    data: Vec<u8>,
}

fn rd32(b: &[u8], le: bool) -> u32 {
    let a = [b[0], b[1], b[2], b[3]];
    if le {
        u32::from_le_bytes(a)
    } else {
        u32::from_be_bytes(a)
    }
}

/// Classic pcap, Ethernet or loopback, IPv4/UDP; times relative to the first packet.
fn parse_pcap(buf: &[u8], server_port: u16) -> Vec<Pkt> {
    let le = buf[0] == 0xd4;
    let link = rd32(&buf[20..], le);
    let l2 = match link {
        1 => 14,
        0 | 108 => 4,
        _ => panic!("link type {link}"),
    };
    let (mut off, mut out, mut t0) = (24, Vec::new(), None);
    while off + 16 <= buf.len() {
        let sec = u64::from(rd32(&buf[off..], le));
        let usec = u64::from(rd32(&buf[off + 4..], le));
        let incl = rd32(&buf[off + 8..], le) as usize;
        let f = &buf[off + 16..off + 16 + incl];
        off += 16 + incl;
        let ip = &f[l2..];
        let udp = &ip[(ip[0] & 15) as usize * 4..];
        let dp = u16::from_be_bytes([udp[2], udp[3]]);
        let len = u16::from_be_bytes([udp[4], udp[5]]) as usize;
        let us = sec * 1_000_000 + usec;
        let t0 = *t0.get_or_insert(us);
        out.push(Pkt {
            ms: (us - t0) / 1000,
            to_server: dp == server_port,
            data: udp[8..len].to_vec(),
        });
    }
    out
}

fn key_of(json: &str) -> String {
    let p = json.find("\"key\"").unwrap();
    let r = &json[p + 5..];
    let a = r.find('"').unwrap() + 1;
    let b = a + r[a..].find('"').unwrap();
    r[a..b].to_string()
}

fn lab() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/mosh-lab/.state/captures")
}

struct Replay {
    screen: Screen,
    /// Every state after each applied diff, in order.
    states: Vec<Framebuffer>,
    events: Vec<ScreenEvent>,
    shutdown: bool,
}

fn replay(dir: &Path) -> Option<Replay> {
    let (Ok(cap), Ok(json)) = (
        std::fs::read(dir.join("cap.pcap")),
        std::fs::read_to_string(dir.join("session.json")),
    ) else {
        eprintln!("skipping {}: capture not present", dir.display());
        return None;
    };
    let session = Session::from_base64(&key_of(&json)).unwrap();
    let cipher = Cipher::new(&session);
    let mut t = Transport::new(&session, 0, Box::new(|b| b.fill(0xa5)));
    let mut screen = Screen::new(80, 24);
    let mut out = Replay {
        screen: Screen::new(80, 24),
        states: Vec::new(),
        events: Vec::new(),
        shutdown: false,
    };
    let mut resize: Option<(u64, u16, u16)> = None;
    for p in parse_pcap(&cap, 60000).iter().filter(|p| !p.to_server) {
        cipher.open(&p.data).expect("authentic");
        for e in t.handle_datagram(p.ms.max(1), &p.data) {
            match e {
                Event::HostResize {
                    new_num,
                    width,
                    height,
                } => resize = Some((new_num, width, height)),
                Event::HostDiff {
                    old_num,
                    new_num,
                    bytes,
                } => {
                    let rs = resize.take().filter(|r| r.0 == new_num).map(|r| (r.1, r.2));
                    screen
                        .apply(old_num, new_num, rs, &bytes)
                        .unwrap_or_else(|e| panic!("apply {old_num}->{new_num}: {e}"));
                    out.states.push(screen.latest().clone());
                    screen.discard_below(t.peer_throwaway());
                }
                Event::Shutdown => out.shutdown = true,
                Event::EchoAck(_) => {}
            }
        }
    }
    out.events = screen.take_events();
    out.screen = screen;
    Some(out)
}

/// repaint(None) and repaint(Some(prev)) round trips on every state.
fn check_repaints(states: &[Framebuffer]) {
    let mut host: Option<Framebuffer> = None;
    let mut prev: Option<&Framebuffer> = None;
    for (i, fb) in states.iter().enumerate() {
        let mut fresh = Framebuffer::new(fb.cols(), fb.rows());
        fresh.feed(&repaint(fb, None));
        assert!(fresh.display_eq(fb), "full repaint mismatch at state {i}");

        let mut h = host
            .take()
            .unwrap_or_else(|| Framebuffer::new(fb.cols(), fb.rows()));
        h.resize(fb.cols(), fb.rows());
        h.feed(&repaint(fb, prev));
        assert!(
            h.display_eq(fb),
            "incremental repaint mismatch at state {i}"
        );
        host = Some(h);
        prev = Some(fb);
    }
}

fn any_state_has(r: &Replay, needle: &str) -> bool {
    r.states.iter().any(|f| f.text().contains(needle))
}

#[test]
fn main_capture_shows_hi() {
    let Some(r) = replay(&lab()) else { return };
    assert!(r.shutdown);
    assert!(r.states.len() > 3);
    // the final screen has scrolled past `echo hi`; some earlier state shows the output
    assert!(
        r.states
            .iter()
            .any(|f| (0..f.rows()).any(|row| f.row_text(row) == "hi")),
        "no state shows the output line `hi`"
    );
    let f = r.screen.latest();
    assert!((0..f.rows()).any(|row| f.row_text(row).contains("exit")));
    check_repaints(&r.states);
}

#[test]
fn misc_capture_features() {
    let Some(r) = replay(&lab().join("misc")) else {
        return;
    };
    assert!(r.shutdown);
    assert_eq!(r.screen.latest().cols(), 100);
    assert!(any_state_has(&r, "日本語 😀 🎉 ＡＢＣ ñ"), "wide line");
    assert!(any_state_has(&r, "alt screen"));
    assert!(any_state_has(&r, "ital"));
    let f = r.screen.latest();
    assert_eq!(f.title(), "moche title two");
    assert!(r.events.contains(&ScreenEvent::Bell));
    assert!(r
        .events
        .contains(&ScreenEvent::TitleChanged("moche title zero".into())));
    // truecolor cell from the last script line
    let tc = r.states.iter().any(|f| {
        (0..f.rows()).any(|row| {
            f.row_cells(row)
                .iter()
                .any(|c| c.ch == 't' && c.attrs.fg == Color::Rgb(10, 200, 30))
        })
    });
    assert!(tc, "truecolor text");
    // combining marks arrived as marks on their base cell
    let comb = r.states.iter().any(|f| {
        (0..f.rows()).any(|row| {
            f.row_cells(row)
                .iter()
                .any(|c| c.ch == 'e' && c.comb.as_deref() == Some("\u{301}"))
        })
    });
    assert!(comb, "e + U+0301");
    // some state had mouse mode and bracketed paste on, and the cursor hidden
    assert!(r
        .states
        .iter()
        .any(|f| f.modes().mouse_mode == 1000 && f.modes().mouse_enc == 1006));
    assert!(r.states.iter().any(|f| f.modes().bracketed_paste));
    assert!(r.states.iter().any(|f| !f.cursor_visible()));
    check_repaints(&r.states);
}

#[test]
fn misc_kill_capture() {
    let Some(r) = replay(&lab().join("misc_kill")) else {
        return;
    };
    assert!(!r.shutdown);
    assert!(any_state_has(&r, "before kill"));
    check_repaints(&r.states);
}
