//! Replays real mosh-server 1.4.0 captures through the client transport.
//! Skipped (with a note) when the capture files are not present.

use std::path::{Path, PathBuf};

use moche_core::crypto::Session;
use moche_core::network::{Cipher, Direction, Inner, Reassembler};
use moche_core::proto::Instruction;
use moche_core::ssp::{Event, Transport, SHUTDOWN_NUM};

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

/// Fires everything due up to `until`, collecting datagrams.
fn pump(t: &mut Transport, until: u64, out: &mut Vec<Vec<u8>>) {
    for _ in 0..10_000 {
        match t.next_deadline_ms() {
            Some(d) if d <= until => {
                let before = out.len();
                while let Some(p) = t.poll_transmit(d.max(1)) {
                    out.push(p);
                }
                assert!(
                    out.len() > before,
                    "deadline {d} passed but nothing to send"
                );
            }
            _ => return,
        }
    }
    panic!("pump did not settle");
}

/// Replays one capture; returns (events, produced datagrams).
fn replay(dir: &Path, pcap: &str) -> Option<(Vec<Event>, Vec<Vec<u8>>, Session)> {
    let (Ok(cap), Ok(json)) = (
        std::fs::read(dir.join(pcap)),
        std::fs::read_to_string(dir.join("session.json")),
    ) else {
        eprintln!("skipping {}: capture not present", dir.display());
        return None;
    };
    let session = Session::from_base64(&key_of(&json)).unwrap();
    let cipher = Cipher::new(&session);
    let mut t = Transport::new(&session, 0, Box::new(|b| b.fill(0xa5)));
    t.send_resize(0, 100, 30);

    let (mut events, mut out) = (Vec::new(), Vec::new());
    let mut last = 0;
    for (k, p) in parse_pcap(&cap, 60000).iter().enumerate() {
        if p.to_server {
            continue;
        }
        // every server datagram must authenticate with our key, direction S->C
        let o = cipher
            .open(&p.data)
            .unwrap_or_else(|e| panic!("packet {k}: {e}"));
        assert_eq!(o.direction, Direction::ToClient);
        pump(&mut t, p.ms, &mut out);
        let now = p.ms.max(1);
        events.extend(t.handle_datagram(now, &p.data));
        last = now;
        // type something now and then so the sender path is exercised
        if k % 7 == 0 {
            t.send_keys(now, b"x");
        }
    }
    pump(&mut t, last + 5000, &mut out);
    Some((events, out, session))
}

fn check(dir: &Path, pcap: &str, expect_shutdown: bool) {
    let Some((events, out, session)) = replay(dir, pcap) else {
        return;
    };
    // events: states strictly increasing, bases never ahead of the target
    let mut prev = 0;
    let mut diffs = 0;
    for e in &events {
        if let Event::HostDiff {
            old_num, new_num, ..
        } = e
        {
            assert!(*new_num > prev && old_num < new_num);
            prev = *new_num;
            diffs += 1;
        }
    }
    assert!(diffs > 3, "too few host diffs: {diffs}");
    let shut = events.contains(&Event::Shutdown);
    assert_eq!(shut, expect_shutdown);
    if expect_shutdown {
        assert_eq!(events.last(), Some(&Event::Shutdown));
        assert_eq!(prev, SHUTDOWN_NUM);
    }

    // our own output: authentic client->server datagrams carrying valid instructions
    let cipher = Cipher::new(&session);
    assert!(!out.is_empty());
    let (mut last_seq, mut last_ack, mut last_ids) = (None::<u64>, 0, 0);
    let mut seen_data = false;
    for d in &out {
        let o = cipher.open(d).expect("our datagram authenticates");
        assert_eq!(o.direction, Direction::ToServer);
        assert!(last_seq.is_none_or(|s| o.seq > s), "seq must increase");
        last_seq = Some(o.seq);
        let inner = Inner::decode(&o.plaintext).unwrap();
        assert!(inner.frag.id > last_ids);
        last_ids = inner.frag.id;
        let msg = Reassembler::new().push(inner.frag).unwrap().unwrap();
        let i = Instruction::decode(&msg).unwrap();
        assert_eq!(i.version, 2);
        assert!(i.ack_num >= last_ack, "ack must progress monotonically");
        assert!(i.old_num <= i.new_num && i.chaff.len() <= 16);
        last_ack = i.ack_num;
        seen_data |= i.diff.len() > 2;
    }
    assert!(seen_data);
    assert_eq!(last_ack, prev, "final ack covers everything delivered");
}

#[test]
fn main_capture_with_shutdown() {
    check(&lab(), "cap.pcap", true);
}

#[test]
fn misc_capture() {
    check(&lab().join("misc"), "cap.pcap", true);
}

#[test]
fn misc_kill_capture_heartbeats_only() {
    // The client was killed: the server only sends heartbeats afterwards.
    let dir = lab().join("misc_kill");
    check(&dir, "cap.pcap", false);
}
