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
    assert!(n > 20); // first state is 16 KiB now (slow start), about 28 fragments
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

#[test]
fn stats_fresh_session_is_zero_and_args_are_checked() {
    let s = new_session(80, 24, 0);
    let mut st = MocheStats {
        queued_bytes: 7,
        send_errors: 7,
        rx_errors: 7,
        rx_gaps: 7,
        reserved: [7; 5],
    };
    assert_eq!(moche_session_stats(s, &mut st), MOCHE_OK);
    assert_eq!(
        (st.queued_bytes, st.send_errors, st.rx_errors, st.rx_gaps),
        (0, 0, 0, 0)
    );
    assert_eq!(st.reserved, [0; 5]);
    assert_eq!(moche_session_stats(s, null_mut()), MOCHE_ERR_ARG);
    assert_eq!(moche_session_stats(null_mut(), &mut st), MOCHE_ERR_ARG);
    assert_eq!(std::mem::size_of::<MocheStats>(), 40);
    assert_eq!(moche_session_free(s), MOCHE_OK);
}

/// A server ack for client state `n`, as a datagram (empty diff).
fn server_ack(srv: &Cipher, seq: u64, n: u64) -> Vec<u8> {
    use moche_core::network::{fragment, Inner};
    use moche_core::proto::{encode_diff, Instruction};
    let i = Instruction {
        version: 2,
        old_num: 0,
        new_num: 1,
        ack_num: n,
        throwaway_num: 0,
        diff: encode_diff(&[]),
        chaff: vec![],
    };
    let frag = fragment(seq + 1, &i.encode()).unwrap().remove(0);
    let inner = Inner {
        ts: 0,
        ts_reply: None,
        frag,
    };
    srv.seal(Direction::ToClient, seq, &inner.encode()).unwrap()
}

/// Highest client state number carried by a client datagram (single-fragment
/// messages only: returns None for a fragment that is not the whole message).
fn client_state_num(c: &Cipher, d: &[u8]) -> Option<u64> {
    use moche_core::network::{Inner, Reassembler};
    use moche_core::proto::Instruction;
    let o = c.open(d).ok()?;
    let inner = Inner::decode(&o.plaintext).ok()?;
    let m = Reassembler::default().push(inner.frag).ok()??;
    Some(Instruction::decode(&m).ok()?.new_num)
}

fn stats_of(s: *mut MocheSession) -> MocheStats {
    let mut st = MocheStats::default();
    assert_eq!(moche_session_stats(s, &mut st), MOCHE_OK);
    st
}

#[test]
fn stats_queued_bytes_grows_with_paste_and_drains_on_ack() {
    let s = new_session(80, 24, 0);
    let cipher = Cipher::new(&Session::from_base64(KEY).unwrap());
    let big = vec![b'a'; 200_000];
    assert_eq!(
        moche_session_send_keys(s, 0, big.as_ptr(), big.len()),
        MOCHE_OK
    );
    assert_eq!(stats_of(s).queued_bytes, big.len() as u64);

    let (mut now, mut seq) = (0u64, 0u64);
    let mut last = stats_of(s).queued_bytes;
    let mut shrank = false;
    for _ in 0..2000 {
        now += 250;
        let mut top = 0;
        while let Some(d) = poll(s, now) {
            top = top.max(client_state_num(&cipher, &d).unwrap_or(0));
        }
        if top > 0 {
            let ack = server_ack(&cipher, seq, top);
            seq += 1;
            assert_eq!(
                moche_session_handle_datagram(s, now, ack.as_ptr(), ack.len()),
                MOCHE_OK
            );
        }
        let q = stats_of(s).queued_bytes;
        assert!(q <= last, "queue never grows by itself");
        shrank |= q < last;
        last = q;
        if q == 0 {
            break;
        }
    }
    assert!(shrank, "acks must drain the queue");
    assert_eq!(last, 0, "queue drains to zero");
    assert_eq!(stats_of(s).send_errors, 0);
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

// ---- predictive echo ----

/// Simulated server for one session: sends numbered states with an optional
/// echo ack, as datagrams sealed with the session key.
struct Srv {
    cipher: Cipher,
    seq: u64,
    num: u64,
}

impl Srv {
    fn new() -> Srv {
        Srv {
            cipher: Cipher::new(&Session::from_base64(KEY).unwrap()),
            seq: 0,
            num: 0,
        }
    }

    /// Next state: `host` bytes (VT) and an echo ack for client state `echo`.
    fn state(
        &mut self,
        host: &[u8],
        echo: Option<u64>,
        ack: u64,
        ts_reply: Option<u16>,
    ) -> Vec<u8> {
        use moche_core::network::{fragment, Inner};
        use moche_core::proto::{encode_diff, Element, Instruction};
        let mut els = Vec::new();
        if let Some(n) = echo {
            els.push(Element::EchoAck(n));
        }
        if !host.is_empty() {
            els.push(Element::Data(host.to_vec()));
        }
        let i = Instruction {
            version: 2,
            old_num: self.num,
            new_num: self.num + 1,
            ack_num: ack,
            throwaway_num: 0,
            diff: encode_diff(&els),
            chaff: vec![],
        };
        self.num += 1;
        self.seq += 1;
        let frag = fragment(self.seq, &i.encode()).unwrap().remove(0);
        let inner = Inner {
            ts: 0,
            ts_reply,
            frag,
        };
        self.cipher
            .seal(Direction::ToClient, self.seq, &inner.encode())
            .unwrap()
    }
}

/// Client side of a scripted session: the C session plus a model of the host
/// terminal that every paint is fed to.
struct Rig {
    s: *mut MocheSession,
    srv: Srv,
    client: Cipher,
    term: Framebuffer,
    paints: Vec<Vec<u8>>,
    /// What the server framebuffer holds (fed with the same host bytes).
    truth: Framebuffer,
    /// Highest client state number sent so far (acked by every server state).
    sent: u64,
    /// When set, every server state carries an RTT sample of this many ms.
    rtt: Option<u64>,
}

impl Rig {
    fn new(cols: u16, rows: u16) -> Rig {
        Rig {
            s: new_session(cols, rows, 0),
            srv: Srv::new(),
            client: Cipher::new(&Session::from_base64(KEY).unwrap()),
            term: Framebuffer::new(cols, rows),
            paints: Vec::new(),
            truth: Framebuffer::new(cols, rows),
            sent: 0,
            rtt: None,
        }
    }

    fn paint(&mut self) {
        let mut p = MocheBytes::EMPTY;
        match moche_session_take_paint(self.s, &mut p) {
            MOCHE_OK => {
                let v = take(&mut p);
                self.term.feed(&v);
                self.paints.push(v);
            }
            MOCHE_NONE => {}
            c => panic!("take_paint -> {c}"),
        }
    }

    /// The server sends `host` (echo-acking `echo`); the pump paints.
    fn server(&mut self, now: u64, host: &[u8], echo: Option<u64>) {
        let reply = self.rtt.map(|r| (now.wrapping_sub(r) & 0xffff) as u16);
        let d = self.srv.state(host, echo, self.sent, reply);
        self.truth.feed(host);
        assert_eq!(
            moche_session_handle_datagram(self.s, now, d.as_ptr(), d.len()),
            MOCHE_OK
        );
        self.pump(now);
    }

    /// Types `keys`; returns the client state number that carries them.
    fn type_keys(&mut self, now: u64, keys: &[u8]) -> u64 {
        assert_eq!(
            moche_session_send_keys(self.s, now, keys.as_ptr(), keys.len()),
            MOCHE_OK
        );
        let mut dl = 0;
        assert_eq!(moche_session_next_deadline_ms(self.s, &mut dl), MOCHE_OK);
        let mut top = 0;
        while let Some(d) = poll(self.s, dl.max(now) + 1) {
            top = top.max(client_state_num(&self.client, &d).unwrap_or(0));
        }
        self.sent = self.sent.max(top);
        self.pump(now);
        top
    }

    fn pump(&mut self, now: u64) {
        while poll(self.s, now).is_some() {}
        self.paint();
    }

    fn row0(&self) -> String {
        self.term.row_text(0)
    }

    fn set_mode(&mut self, m: i32) {
        assert_eq!(moche_session_set_predict_mode(self.s, m), MOCHE_OK);
    }

    fn deadline(&self) -> Option<u64> {
        let mut dl = 0;
        match moche_session_next_deadline_ms(self.s, &mut dl) {
            MOCHE_OK => Some(dl),
            MOCHE_NONE => None,
            c => panic!("deadline -> {c}"),
        }
    }

    fn pstats(&self) -> MochePredictStats {
        let mut p = MochePredictStats::default();
        assert_eq!(moche_session_predict_stats(self.s, &mut p), MOCHE_OK);
        p
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        moche_session_free(self.s);
    }
}

#[test]
fn predict_stats_layout_and_arguments() {
    assert_eq!(std::mem::size_of::<MochePredictStats>(), 36);
    assert_eq!(std::mem::align_of::<MochePredictStats>(), 4);
    let r = Rig::new(20, 4);
    let p = r.pstats();
    assert_eq!(
        (p.predictions, p.confirmed, p.contradicted, p.expired),
        (0, 0, 0, 0)
    );
    assert_eq!(
        (p.displaying, p.mode, p.pad, p.reserved),
        (0, 0, [0; 2], [0; 4])
    );
    for bad in [3, -1, i32::MIN, i32::MAX] {
        assert_eq!(moche_session_set_predict_mode(r.s, bad), MOCHE_ERR_ARG);
        assert!(last_error().contains("predict mode"));
    }
    assert_eq!(r.pstats().mode, 0, "a refused mode changes nothing");
    assert_eq!(moche_session_set_predict_mode(null_mut(), 1), MOCHE_ERR_ARG);
    let mut p = MochePredictStats::default();
    assert_eq!(
        moche_session_predict_stats(null_mut(), &mut p),
        MOCHE_ERR_ARG
    );
    assert_eq!(moche_session_predict_stats(r.s, null_mut()), MOCHE_ERR_ARG);
    r.set_mode_check();
}

impl Rig {
    fn set_mode_check(&self) {
        for (m, want) in [(1, 1), (2, 2), (0, 0)] {
            assert_eq!(moche_session_set_predict_mode(self.s, m), MOCHE_OK);
            assert_eq!(self.pstats().mode, want);
        }
    }
}

/// The scripted session: a prompt, then typing with echoes and acks.
fn script(r: &mut Rig) {
    r.server(10, b"$ ", None);
    let n = r.type_keys(100, b"a");
    r.server(300, b"a", Some(n));
    let n = r.type_keys(400, b"b");
    r.server(600, b"b", Some(n));
    let n = r.type_keys(700, b"\r");
    r.server(900, b"\r\nout\r\n$ ", Some(n));
}

#[test]
fn default_mode_paints_exactly_the_server_state() {
    // golden: a model screen fed the same diffs and repainted by moche-core
    let mut golden = Rig::new(20, 4);
    script(&mut golden);
    let mut model = Screen::new(20, 4);
    let mut prev: Option<Framebuffer> = None;
    let mut expect = Vec::new();
    for (old, new, bytes) in [
        (0, 1, &b"$ "[..]),
        (1, 2, b"a"),
        (2, 3, b"b"),
        (3, 4, b"\r\nout\r\n$ "),
    ] {
        model.apply(old, new, None, bytes).unwrap();
        let out = model.repaint(prev.as_ref());
        // the initial paint comes before any datagram: handled below
        expect.push(out);
        prev = Some(model.latest().clone());
    }
    // paints from the C session (the very first one is the blank initial screen)
    // are those after each server state; the echo-only states of the script
    // change nothing visible, so compare the visible ones
    let got = &golden.paints;
    assert!(got.len() >= 4, "{} paints", got.len());
    assert!(golden.term.display_eq(&golden.truth));
    assert_eq!(&got[got.len() - 1], &expect[3]);
    assert_eq!(&got[got.len() - 2], &expect[2]);

    // explicitly NEVER is byte-identical to never calling it
    let mut a = Rig::new(20, 4);
    let mut b = Rig::new(20, 4);
    b.set_mode(0);
    script(&mut a);
    script(&mut b);
    assert_eq!(a.paints, b.paints);
    assert_eq!(b.pstats().predictions, 0);
    // and ADAPTIVE changed something while the same script ran
    let mut c = Rig::new(20, 4);
    c.set_mode(1);
    script(&mut c);
    assert_ne!(a.paints, c.paints, "adaptive paints the overlay");
    assert!(c.pstats().predictions > 0);
    assert!(c.term.display_eq(&c.truth), "and converges");
    assert_eq!(c.pstats().displaying, 0);
}

#[test]
fn adaptive_shows_prediction_before_the_echo_and_converges() {
    let mut r = Rig::new(20, 4);
    r.set_mode(1);
    r.server(10, b"$ ", None);
    assert_eq!(r.row0(), "$");
    // srtt is 1000 before a sample: high. The first char of an epoch is hidden.
    let n = r.type_keys(100, b"a");
    assert_eq!(r.row0(), "$", "first char waits for the server echo");
    r.server(300, b"a", Some(n));
    assert_eq!(r.row0(), "$ a");
    // now a typed char shows at once, before any server echo
    let n = r.type_keys(400, b"b");
    assert_eq!(r.row0(), "$ ab", "predicted before the echo");
    assert!(!r.term.display_eq(&r.truth));
    assert_eq!(r.term.cursor_col(), 4);
    let ps = r.pstats();
    assert!(ps.displaying == 1 && ps.predictions == 2 && ps.mode == 1);
    // the paint that showed it is a normal paint
    let last = r.paints.last().unwrap();
    assert!(last.starts_with(b"\x1b[?25l\x1b[0m\x1b[r\x1b[?6l\x1b[?7h"));
    assert!(!last.windows(4).any(|w| w == b"\x1b[2J"), "partial paint");
    // the real echo arrives: identical to the server, nothing left over
    r.server(600, b"b", Some(n));
    assert!(r.term.display_eq(&r.truth));
    assert_eq!(r.pstats().displaying, 0);
    assert!(r.pstats().confirmed >= 2);
    // nothing more to paint
    let before = r.paints.len();
    r.paint();
    assert_eq!(r.paints.len(), before);
}

#[test]
fn password_like_silence_shows_nothing_in_adaptive() {
    let mut r = Rig::new(20, 4);
    r.set_mode(1);
    r.server(10, b"Password:", None);
    let mut now = 100;
    for k in [b"s", b"e", b"c", b"r", b"e", b"t"] {
        let n = r.type_keys(now, k);
        assert_eq!(r.row0(), "Password:", "typed secret never shown");
        // the server consumes the key and echoes nothing, acking the input
        r.server(now + 200, b"", Some(n));
        assert_eq!(r.row0(), "Password:");
        assert!(r.term.display_eq(&r.truth));
        now += 300;
    }
    assert_eq!(r.pstats().displaying, 0);
    assert_eq!(r.pstats().confirmed, 0);
}

#[test]
fn deadline_includes_the_predictor_and_timer_wake_removes_expiry() {
    let mut r = Rig::new(20, 4);
    r.rtt = Some(80); // srtt 80 ms: predicts (>30), no underline (<85)
    r.set_mode(1);
    r.server(10, b"$ ", None);
    let n = r.type_keys(100, b"a");
    r.server(300, b"a", Some(n));
    r.type_keys(400, b"b");
    assert_eq!(r.row0(), "$ ab");
    // the server acks the datagram (the transport has nothing to resend) but
    // does not echo: the prediction stays pending
    r.server(450, b"", None);
    assert_eq!(r.row0(), "$ ab");
    while poll(r.s, 700).is_some() {}
    // the same session with predictions off: only the transport's heartbeat
    let mut quiet = Rig::new(20, 4);
    quiet.rtt = Some(80);
    quiet.server(10, b"$ ", None);
    let n = quiet.type_keys(100, b"a");
    quiet.server(300, b"a", Some(n));
    quiet.type_keys(400, b"b");
    quiet.server(450, b"", None);
    while poll(quiet.s, 700).is_some() {}
    let transport_only = quiet.deadline().expect("heartbeat");
    let with = r.deadline().expect("deadline");
    assert!(transport_only > 3400, "test premise: {transport_only}");
    // the predictor expires the shown 'b' at 400 + 3000, earlier
    assert_eq!(with, 3400, "predictor expiry wins");
    // the host wakes at the deadline and runs its pump: poll, then take_paint
    while poll(r.s, with).is_some() {}
    r.paint();
    assert_eq!(r.row0(), "$ a", "expired prediction removed by take_paint");
    assert!(r.pstats().expired >= 1);
    assert!(r.term.display_eq(&r.truth));
}

#[test]
fn resize_and_mode_change_clear_the_overlay() {
    let mut r = Rig::new(20, 4);
    r.set_mode(1);
    r.server(10, b"$ ", None);
    let n = r.type_keys(100, b"a");
    r.server(300, b"a", Some(n));
    r.type_keys(400, b"b");
    assert_eq!(r.row0(), "$ ab");
    // switching off removes the overlay at the next paint
    r.set_mode(0);
    r.paint();
    assert_eq!(r.row0(), "$ a");
    assert!(r.term.display_eq(&r.truth));
    // a forced full repaint includes the overlay currently displayed
    r.set_mode(1);
    let n = r.type_keys(500, b"c");
    r.server(700, b"c", Some(n));
    r.type_keys(800, b"d");
    assert_eq!(r.row0(), "$ acd");
    assert_eq!(moche_session_force_full_paint(r.s), MOCHE_OK);
    let mut fresh = Framebuffer::new(20, 4);
    let mut p = MocheBytes::EMPTY;
    assert_eq!(moche_session_take_paint(r.s, &mut p), MOCHE_OK);
    let v = take(&mut p);
    assert!(v.windows(4).any(|w| w == b"\x1b[2J"));
    fresh.feed(&v);
    assert_eq!(fresh.row_text(0), "$ acd");
    r.term.feed(&v);
    // a local resize drops predictions
    assert_eq!(moche_session_send_resize(r.s, 900, 20, 4), MOCHE_OK);
    r.paint();
    assert_eq!(r.pstats().displaying, 0);
    assert_eq!(r.row0(), "$ ac", "the prediction is gone");
}
