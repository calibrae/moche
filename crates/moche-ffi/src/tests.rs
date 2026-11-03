//! Tests of the C layer, driven through the exported functions.

use std::path::Path;
use std::ptr::{null, null_mut};

use moche_core::crypto::Session;
use moche_core::network::{Cipher, Direction};
use moche_core::screen::{Framebuffer, Screen};
use moche_core::ssp::{Event, Transport};

use super::*;

const KEY: &str = "AAECAwQFBgcICQoLDA0ODw";

fn new_session(cols: u16, rows: u16, now: u64) -> *mut MocheSession {
    let mut s = null_mut();
    let rc = moche_session_new(KEY.as_ptr(), KEY.len(), cols, rows, now, &mut s);
    assert_eq!(rc, MOCHE_OK);
    assert!(!s.is_null());
    s
}

fn take(b: &mut MocheBytes) -> Vec<u8> {
    let v = if b.ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: test-only view of a buffer we own, read before freeing.
        unsafe { std::slice::from_raw_parts(b.ptr, b.len) }.to_vec()
    };
    assert_eq!(moche_bytes_free(b), MOCHE_OK);
    assert!(b.ptr.is_null() && b.len == 0 && b.cap == 0);
    v
}

fn last_error() -> String {
    let mut buf = [0u8; 256];
    let n = moche_last_error(buf.as_mut_ptr(), buf.len());
    String::from_utf8(buf[..n.min(255)].to_vec()).unwrap()
}

fn poll(s: *mut MocheSession, now: u64) -> Option<Vec<u8>> {
    let mut b = MocheBytes::EMPTY;
    match moche_session_poll_transmit(s, now, &mut b) {
        MOCHE_OK => Some(take(&mut b)),
        MOCHE_NONE => {
            assert!(b.ptr.is_null());
            None
        }
        c => panic!("poll_transmit -> {c}"),
    }
}

#[test]
fn version() {
    // SAFETY: moche_version returns a NUL-terminated static string.
    let v = unsafe { std::ffi::CStr::from_ptr(moche_version()) };
    assert_eq!(v.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn parse_connect_exports() {
    let mut port = 0u16;
    let mut key = [0xffu8; 23];
    let line = format!("MOSH CONNECT 60001 {KEY}\r\n");
    assert_eq!(
        moche_parse_connect(line.as_ptr(), line.len(), &mut port, key.as_mut_ptr()),
        MOCHE_OK
    );
    assert_eq!(port, 60001);
    assert_eq!(&key[..22], KEY.as_bytes());
    assert_eq!(key[22], 0);

    let noisy = format!("motd\nMOSH CONNECT 61000 {KEY}\n[detached]\n");
    assert_eq!(
        moche_find_connect(noisy.as_ptr(), noisy.len(), &mut port, key.as_mut_ptr()),
        MOCHE_OK
    );
    assert_eq!(port, 61000);

    let (p, k) = (&mut port as *mut u16, key.as_mut_ptr());
    assert_eq!(
        moche_find_connect(b"nope".as_ptr(), 4, p, k),
        MOCHE_ERR_NO_CONNECT
    );
    assert!(last_error().contains("MOSH CONNECT"));
    let bad = b"MOSH CONNECT 5 AAAA\n";
    assert_eq!(
        moche_find_connect(bad.as_ptr(), bad.len(), p, k),
        MOCHE_ERR_BAD_KEY
    );
    let big = vec![b'x'; (1 << 20) + 1];
    assert_eq!(
        moche_find_connect(big.as_ptr(), big.len(), p, k),
        MOCHE_ERR_TOO_LARGE
    );
    assert_eq!(moche_find_connect(null(), 0, p, k), MOCHE_ERR_NO_CONNECT);
    assert_eq!(moche_find_connect(null(), 3, p, k), MOCHE_ERR_ARG);
    assert_eq!(
        moche_parse_connect(line.as_ptr(), line.len(), null_mut(), k),
        MOCHE_ERR_ARG
    );
    assert_eq!(
        moche_parse_connect(line.as_ptr(), line.len(), p, null_mut()),
        MOCHE_ERR_ARG
    );
    // the key never shows up in an error message
    let bad = format!("MOSH CONNECT 5 {}!\n", &KEY[..21]);
    assert_eq!(
        moche_find_connect(bad.as_ptr(), bad.len(), p, k),
        MOCHE_ERR_BAD_KEY
    );
    assert!(!last_error().contains(&KEY[..8]));
}

#[test]
fn new_validates_arguments() {
    let mut s: *mut MocheSession = std::ptr::dangling_mut();
    assert_eq!(
        moche_session_new(KEY.as_ptr(), 21, 80, 24, 0, &mut s),
        MOCHE_ERR_BAD_KEY
    );
    assert!(s.is_null(), "out is nulled on failure");
    assert_eq!(
        moche_session_new(KEY.as_ptr(), 22, 0, 24, 0, &mut s),
        MOCHE_ERR_BAD_SIZE
    );
    assert_eq!(
        moche_session_new(KEY.as_ptr(), 22, 80, 1001, 0, &mut s),
        MOCHE_ERR_BAD_SIZE
    );
    assert_eq!(
        moche_session_new(null(), 22, 80, 24, 0, &mut s),
        MOCHE_ERR_ARG
    );
    assert_eq!(
        moche_session_new(null(), 0, 80, 24, 0, &mut s),
        MOCHE_ERR_BAD_KEY
    );
    assert_eq!(
        moche_session_new(KEY.as_ptr(), 22, 80, 24, 0, null_mut()),
        MOCHE_ERR_ARG
    );
    let bad = b"AAECAwQFBgcICQoLDA0OD!";
    assert_eq!(
        moche_session_new(bad.as_ptr(), 22, 80, 24, 0, &mut s),
        MOCHE_ERR_BAD_KEY
    );
    let nonutf = [0xffu8; 22];
    assert_eq!(
        moche_session_new(nonutf.as_ptr(), 22, 80, 24, 0, &mut s),
        MOCHE_ERR_BAD_KEY
    );
    assert!(!last_error().contains("AAEC"));
}

#[test]
fn null_and_zero_arguments() {
    assert_eq!(moche_session_free(null_mut()), MOCHE_OK);
    assert_eq!(
        moche_session_send_keys(null_mut(), 0, null(), 0),
        MOCHE_ERR_ARG
    );
    assert_eq!(
        moche_session_handle_datagram(null_mut(), 0, null(), 0),
        MOCHE_ERR_ARG
    );
    assert_eq!(moche_bytes_free(null_mut()), MOCHE_ERR_ARG);
    let s = new_session(80, 24, 0);
    // zero-length input is fine, null with length is not
    assert_eq!(moche_session_send_keys(s, 1, null(), 0), MOCHE_OK);
    assert_eq!(moche_session_send_keys(s, 1, null(), 5), MOCHE_ERR_ARG);
    assert_eq!(
        moche_session_handle_datagram(s, 1, null(), 0),
        MOCHE_AUTH_FAILED
    );
    assert_eq!(
        moche_session_handle_datagram(s, 1, null(), 4),
        MOCHE_ERR_ARG
    );
    assert_eq!(moche_session_send_resize(s, 1, 0, 5), MOCHE_ERR_BAD_SIZE);
    assert_eq!(
        moche_session_send_resize(s, 1, 80, 2000),
        MOCHE_ERR_BAD_SIZE
    );
    assert_eq!(moche_session_poll_transmit(s, 1, null_mut()), MOCHE_ERR_ARG);
    assert_eq!(moche_session_take_paint(s, null_mut()), MOCHE_ERR_ARG);
    assert_eq!(moche_session_next_deadline_ms(s, null_mut()), MOCHE_ERR_ARG);
    assert_eq!(moche_session_modes(s, null_mut()), MOCHE_ERR_ARG);
    assert_eq!(moche_session_take_events(s, null_mut()), MOCHE_ERR_ARG);
    assert_eq!(
        moche_session_screen_size(s, null_mut(), null_mut()),
        MOCHE_ERR_ARG
    );
    assert_eq!(
        moche_session_ms_since_last_contact(s, 0, null_mut()),
        MOCHE_ERR_ARG
    );
    assert_eq!(moche_session_srtt_ms(s, null_mut()), MOCHE_ERR_ARG);
    // the session is still healthy after all of that
    assert_eq!(moche_session_force_full_paint(s), MOCHE_OK);
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

#[test]
fn bytes_lifecycle() {
    let mut b = MocheBytes::from_vec(vec![1, 2, 3]);
    assert_eq!((b.len, b.cap >= 3), (3, true));
    assert_eq!(moche_bytes_free(&mut b), MOCHE_OK);
    // second free of the same variable is a no-op
    assert_eq!(moche_bytes_free(&mut b), MOCHE_OK);
    let mut e = MocheBytes::from_vec(Vec::new());
    assert!(e.ptr.is_null());
    assert_eq!(moche_bytes_free(&mut e), MOCHE_OK);
    let mut corrupt = MocheBytes {
        ptr: 8 as *mut u8,
        len: 10,
        cap: 4,
    };
    assert_eq!(moche_bytes_free(&mut corrupt), MOCHE_ERR_ARG);
}

#[test]
fn last_error_truncates_and_reports_length() {
    fail(MOCHE_ERR_ARG, "héllo wörld");
    let full = "héllo wörld";
    assert_eq!(moche_last_error(null_mut(), 0), full.len());
    let mut buf = [0xaau8; 3];
    assert_eq!(moche_last_error(buf.as_mut_ptr(), 3), full.len());
    // 'h' + 'é' (2 bytes) would need 3 bytes plus NUL: cut before the é
    assert_eq!(&buf[..2], b"h\0");
}

#[test]
fn loopback_keys_are_encrypted_for_the_server() {
    let s = new_session(80, 24, 100);
    assert_eq!(
        moche_session_send_keys(s, 100, b"ls\r".as_ptr(), 3),
        MOCHE_OK
    );
    let mut dl = 0u64;
    assert_eq!(moche_session_next_deadline_ms(s, &mut dl), MOCHE_OK);
    assert!(dl >= 100);
    assert!(poll(s, 100).is_none(), "collect interval not elapsed");
    let mut got = Vec::new();
    while let Some(d) = poll(s, dl + 500) {
        got.push(d);
    }
    assert!(!got.is_empty());
    let cipher = Cipher::new(&Session::from_base64(KEY).unwrap());
    let o = cipher.open(&got[0]).expect("client datagram decrypts");
    assert_eq!(o.direction, Direction::ToServer);
    assert!(o.plaintext.len() > 16);
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

/// `#define NAME <int>` from the header text.
fn header_define(name: &str) -> usize {
    let h = include_str!("../include/moche.h");
    let pat = format!("#define {name} ");
    let line = h.lines().find(|l| l.starts_with(&pat)).expect("define");
    line[pat.len()..].trim().parse().expect("integer")
}

#[test]
fn header_max_datagram_matches_core() {
    assert_eq!(
        header_define("MOCHE_MAX_DATAGRAM"),
        moche_core::network::MAX_DATAGRAM
    );
}

#[test]
fn transmitted_datagrams_respect_max_datagram() {
    let s = new_session(80, 24, 0);
    let mut seed = 7u64;
    let big: Vec<u8> = (0..200_000)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        })
        .collect();
    assert_eq!(
        moche_session_send_keys(s, 0, big.as_ptr(), big.len()),
        MOCHE_OK
    );
    let mut n = 0;
    for now in [500, 1000, 2000] {
        while let Some(d) = poll(s, now) {
            assert!(d.len() <= header_define("MOCHE_MAX_DATAGRAM"));
            n += 1;
        }
    }
    assert!(n > 100);
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

/// Same throwaway key and captured server datagram as the Swift known-answer test.
#[test]
fn captured_server_datagram_is_authentic_and_paints() {
    const K: &str = "Ixu2un/jwqLpFzsnDHm5lw";
    const D: &str = "80000000000000077d3eff25273194f8472a9e5058d9ea455e55cff4c586b9b6a44ffe88adddf19b5bc49639fa139e5c49a731d46208d75d1e7e65d9661396";
    let d: Vec<u8> = (0..D.len() / 2)
        .map(|i| u8::from_str_radix(&D[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let mut s = null_mut();
    assert_eq!(
        moche_session_new(K.as_ptr(), K.len(), 80, 24, 0, &mut s),
        MOCHE_OK
    );
    assert_eq!(
        moche_session_handle_datagram(s, 5, d.as_ptr(), d.len()),
        MOCHE_OK
    );
    let mut p = MocheBytes::EMPTY;
    assert_eq!(moche_session_take_paint(s, &mut p), MOCHE_OK);
    assert!(!take(&mut p).is_empty());
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

#[test]
fn garbage_datagrams_are_nonfatal() {
    let s = new_session(80, 24, 0);
    for d in [&[0u8; 5][..], &[7u8; 64][..], &[0xffu8; 1500][..]] {
        assert_eq!(
            moche_session_handle_datagram(s, 5, d.as_ptr(), d.len()),
            MOCHE_AUTH_FAILED
        );
    }
    // wrong direction: a client-bound packet echoed back to us
    let c = Cipher::new(&Session::from_base64(KEY).unwrap());
    let d = c.seal(Direction::ToServer, 1, &[0u8; 32]).unwrap();
    assert_eq!(
        moche_session_handle_datagram(s, 5, d.as_ptr(), d.len()),
        MOCHE_AUTH_FAILED
    );
    let mut ev = 99;
    assert_eq!(moche_session_take_events(s, &mut ev), MOCHE_OK);
    assert_eq!(ev, 0);
    let mut since = 0;
    assert_eq!(
        moche_session_ms_since_last_contact(s, 1500, &mut since),
        MOCHE_OK
    );
    assert_eq!(since, 1500, "garbage is not contact");
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

#[test]
fn time_going_backwards_is_harmless() {
    let s = new_session(80, 24, 10_000);
    assert_eq!(moche_session_send_keys(s, 5, b"x".as_ptr(), 1), MOCHE_OK);
    let _ = poll(s, 1);
    let mut since = 7;
    assert_eq!(
        moche_session_ms_since_last_contact(s, 3, &mut since),
        MOCHE_OK
    );
    assert_eq!(since, 0);
    assert_eq!(
        moche_session_send_keys(s, u64::MAX, b"x".as_ptr(), 1),
        MOCHE_OK
    );
    let _ = poll(s, u64::MAX);
    let mut dl = 0;
    let _ = moche_session_next_deadline_ms(s, &mut dl);
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

#[test]
fn panic_becomes_error_and_kills_only_that_session() {
    let a = new_session(80, 24, 0);
    let b = new_session(80, 24, 0);
    assert_eq!(moche_session_test_panic(a), MOCHE_ERR_PANIC);
    assert!(last_error().contains("test panic"));
    let mut cols = 0;
    let mut rows = 0;
    assert_eq!(
        moche_session_screen_size(a, &mut cols, &mut rows),
        MOCHE_ERR_DEAD
    );
    assert_eq!(
        moche_session_send_keys(a, 0, b"x".as_ptr(), 1),
        MOCHE_ERR_DEAD
    );
    assert_eq!(moche_session_screen_size(b, &mut cols, &mut rows), MOCHE_OK);
    assert_eq!((cols, rows), (80, 24));
    assert_eq!(
        moche_session_free(a),
        MOCHE_OK,
        "a dead session can still be freed"
    );
    assert_eq!(moche_session_free(b), MOCHE_OK);
}

#[test]
fn guard_converts_panics_outside_sessions() {
    assert_eq!(guard(|| panic!("boom {}", 1)), MOCHE_ERR_PANIC);
    assert!(last_error().contains("boom 1"));
}

#[test]
fn modes_and_initial_paint() {
    let s = new_session(40, 10, 0);
    let mut m = MocheModes::default();
    assert_eq!(moche_session_modes(s, &mut m), MOCHE_OK);
    assert_eq!(
        (m.app_cursor_keys, m.mouse_mode, m.cursor_visible),
        (0, 0, 1)
    );
    let mut p = MocheBytes::EMPTY;
    assert_eq!(moche_session_take_paint(s, &mut p), MOCHE_OK);
    let paint = take(&mut p);
    assert!(paint.starts_with(moche_core::screen::PAINT_PREAMBLE.as_bytes()));
    assert_eq!(moche_session_take_paint(s, &mut p), MOCHE_NONE);
    assert_eq!(moche_session_force_full_paint(s), MOCHE_OK);
    assert_eq!(moche_session_take_paint(s, &mut p), MOCHE_OK);
    take(&mut p);
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

// ---- replay of the stock mosh-server captures through the C API ----

fn rd32(b: &[u8], le: bool) -> u32 {
    let a = [b[0], b[1], b[2], b[3]];
    if le {
        u32::from_le_bytes(a)
    } else {
        u32::from_be_bytes(a)
    }
}

/// Server-to-client datagrams of a classic pcap (Ethernet or loopback, IPv4/UDP).
fn server_datagrams(buf: &[u8], server_port: u16) -> Vec<(u64, Vec<u8>)> {
    let le = buf[0] == 0xd4;
    let l2 = match rd32(&buf[20..], le) {
        1 => 14,
        _ => 4,
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
        if dp != server_port {
            out.push(((us - t0) / 1000, udp[8..len].to_vec()));
        }
    }
    out
}

fn json_key(json: &str) -> String {
    let r = &json[json.find("\"key\"").unwrap() + 5..];
    let a = r.find('"').unwrap() + 1;
    r[a..a + r[a..].find('"').unwrap()].to_string()
}

fn replay_through_ffi(dir: &Path) {
    let (Ok(cap), Ok(json)) = (
        std::fs::read(dir.join("cap.pcap")),
        std::fs::read_to_string(dir.join("session.json")),
    ) else {
        eprintln!("skipping {}: capture not present", dir.display());
        return;
    };
    let key = json_key(&json);
    let dgrams = server_datagrams(&cap, 60000);

    // reference: moche-core driven directly
    let session = Session::from_base64(&key).unwrap();
    let mut t = Transport::new(&session, 0, Box::new(|b| b.fill(0)));
    let mut direct = Screen::new(80, 24);
    let mut resize = None;
    for (ms, d) in &dgrams {
        for e in t.handle_datagram((*ms).max(1), d) {
            match e {
                Event::HostResize { width, height, .. } => resize = Some((width, height)),
                Event::HostDiff {
                    old_num,
                    new_num,
                    bytes,
                } => {
                    direct
                        .apply(old_num, new_num, resize.take(), &bytes)
                        .unwrap();
                    direct.discard_below(t.peer_throwaway());
                }
                _ => {}
            }
        }
    }

    // through the C API, painting into a model of the host terminal
    let mut s = null_mut();
    assert_eq!(
        moche_session_new(key.as_ptr(), key.len(), 80, 24, 0, &mut s),
        MOCHE_OK
    );
    let mut host = Framebuffer::new(80, 24);
    let (mut saw_shutdown, mut paints) = (false, 0);
    for (ms, d) in &dgrams {
        assert_eq!(
            moche_session_handle_datagram(s, (*ms).max(1), d.as_ptr(), d.len()),
            MOCHE_OK
        );
        let mut p = MocheBytes::EMPTY;
        if moche_session_take_paint(s, &mut p) == MOCHE_OK {
            paints += 1;
            let (mut c, mut r) = (0, 0);
            assert_eq!(moche_session_screen_size(s, &mut c, &mut r), MOCHE_OK);
            host.resize(c, r);
            host.feed(&take(&mut p));
        }
        let mut ev = 0;
        assert_eq!(moche_session_take_events(s, &mut ev), MOCHE_OK);
        assert_eq!(ev & 8, 0, "never out of sync on a clean capture");
        saw_shutdown |= ev & 16 != 0;
    }
    assert!(paints > 1);
    assert!(
        host.display_eq(direct.latest()),
        "host terminal differs from the model"
    );
    assert_eq!(host.text(), direct.latest().text());
    assert_eq!(saw_shutdown, t_shutdown(&dgrams, &key));
    // a forced repaint rebuilds the same screen on a fresh terminal
    assert_eq!(moche_session_force_full_paint(s), MOCHE_OK);
    let mut p = MocheBytes::EMPTY;
    assert_eq!(moche_session_take_paint(s, &mut p), MOCHE_OK);
    let mut fresh = Framebuffer::new(80, 24);
    fresh.resize(direct.latest().cols(), direct.latest().rows());
    fresh.feed(&take(&mut p));
    assert!(fresh.display_eq(direct.latest()));
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

fn t_shutdown(d: &[(u64, Vec<u8>)], key: &str) -> bool {
    let session = Session::from_base64(key).unwrap();
    let mut t = Transport::new(&session, 0, Box::new(|b| b.fill(0)));
    d.iter().any(|(ms, d)| {
        t.handle_datagram((*ms).max(1), d)
            .contains(&Event::Shutdown)
    })
}

fn lab() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/mosh-lab/.state/captures")
}

#[test]
fn replay_main_capture() {
    replay_through_ffi(&lab());
}

#[test]
fn replay_misc_capture() {
    replay_through_ffi(&lab().join("misc"));
}
