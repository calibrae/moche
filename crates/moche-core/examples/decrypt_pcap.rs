//! Dev tool: decrypt a captured mosh session and dump the structure.
//! Usage: decrypt_pcap <cap.pcap> <session.json>
//! Hand-rolled classic pcap + Ethernet/IPv4/UDP parsing; no GPL code involved.

use aes::Aes128;
use moche_core::crypto::Session;
use ocb3::AeadInOut;
use ocb3::Ocb3;
use ocb3::{Array, KeyInit};

struct Pkt {
    n: usize,
    ts: f64,
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

fn parse_pcap(buf: &[u8], server_port: u16) -> Vec<Pkt> {
    let le = buf[0] == 0xd4;
    let link = rd32(&buf[20..], le);
    eprintln!("pcap: little_endian={le} linktype={link}");
    let mut off = 24;
    let mut out = vec![];
    while off + 16 <= buf.len() {
        let sec = rd32(&buf[off..], le) as f64;
        let usec = rd32(&buf[off + 4..], le) as f64;
        let incl = rd32(&buf[off + 8..], le) as usize;
        let f = &buf[off + 16..off + 16 + incl];
        off += 16 + incl;
        let l2 = match link {
            1 => 14,
            0 | 108 => 4,
            _ => panic!("link type {link}"),
        };
        let ip = &f[l2..];
        assert_eq!(ip[0] >> 4, 4, "not ipv4");
        let ihl = (ip[0] & 15) as usize * 4;
        assert_eq!(ip[9], 17, "not udp");
        let udp = &ip[ihl..];
        let sp = u16::from_be_bytes([udp[0], udp[1]]);
        let dp = u16::from_be_bytes([udp[2], udp[3]]);
        let len = u16::from_be_bytes([udp[4], udp[5]]) as usize;
        let payload = udp[8..len].to_vec();
        let to_server = dp == server_port;
        assert!(to_server || sp == server_port);
        out.push(Pkt {
            n: out.len() + 1,
            ts: sec + usec / 1e6,
            to_server,
            data: payload,
        });
    }
    out
}

fn json_str(s: &str, key: &str) -> String {
    let p = s.find(&format!("\"{key}\"")).unwrap();
    let r = &s[p + key.len() + 2..];
    let a = r.find('"').unwrap() + 1;
    let b = a + r[a..].find('"').unwrap();
    r[a..b].to_string()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Try one layout: returns plaintext if the tag verifies.
fn try_decrypt(key: &[u8; 16], wire: &[u8], tag_len: usize, nonce: [u8; 12]) -> Option<Vec<u8>> {
    let body = &wire[8..];
    if body.len() < tag_len {
        return None;
    }
    let (ct, tag) = body.split_at(body.len() - tag_len);
    let mut buf = ct.to_vec();
    let n: &Array<u8, _> = (&nonce).into();
    let k: &Array<u8, _> = key.into();
    macro_rules! go {
        ($t:ty) => {{
            let c = Ocb3::<Aes128, ocb3::consts::U12, $t>::new(k);
            let tg: &Array<u8, _> = tag.try_into().ok()?;
            c.decrypt_inout_detached(n, b"", buf.as_mut_slice().into(), tg)
                .ok()?;
        }};
    }
    match tag_len {
        16 => go!(ocb3::consts::U16),
        12 => go!(ocb3::consts::U12),
        _ => return None,
    }
    Some(buf)
}

type Layout = (&'static str, fn(&[u8]) -> [u8; 12]);

fn layouts() -> Vec<Layout> {
    fn z4_w(w: &[u8]) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&w[..8]);
        n
    }
    fn w_z4(w: &[u8]) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[..8].copy_from_slice(&w[..8]);
        n
    }
    fn z4_w_clr(w: &[u8]) -> [u8; 12] {
        let mut n = z4_w(w);
        n[4] &= 0x7f;
        n
    }
    fn w_z4_clr(w: &[u8]) -> [u8; 12] {
        let mut n = w_z4(w);
        n[0] &= 0x7f;
        n
    }
    fn z4_w_le(w: &[u8]) -> [u8; 12] {
        let mut n = [0u8; 12];
        for i in 0..8 {
            n[4 + i] = w[7 - i];
        }
        n
    }
    vec![
        ("0000 + wire8", z4_w),
        ("wire8 + 0000", w_z4),
        ("0000 + wire8, dir bit cleared", z4_w_clr),
        ("wire8 + 0000, dir bit cleared", w_z4_clr),
        ("0000 + wire8 byte-reversed", z4_w_le),
    ]
}

fn varint(b: &[u8], p: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut s = 0;
    loop {
        let x = b[*p];
        *p += 1;
        v |= ((x & 0x7f) as u64) << s;
        if x & 0x80 == 0 {
            return v;
        }
        s += 7;
    }
}

/// Generic protobuf dump (wire format only).
fn pb_dump(b: &[u8], ind: usize, depth: usize) {
    let mut p = 0;
    let pad = " ".repeat(ind);
    while p < b.len() {
        let tag = varint(b, &mut p);
        let (f, w) = (tag >> 3, tag & 7);
        match w {
            0 => println!("{pad}f{f} varint {}", varint(b, &mut p)),
            1 => {
                println!("{pad}f{f} fixed64 {}", hex(&b[p..p + 8]));
                p += 8
            }
            5 => {
                println!("{pad}f{f} fixed32 {}", hex(&b[p..p + 4]));
                p += 4
            }
            2 => {
                let l = varint(b, &mut p) as usize;
                let d = &b[p..p + l];
                p += l;
                let txt: String = d
                    .iter()
                    .take(60)
                    .map(|&c| {
                        if (32..127).contains(&c) {
                            c as char
                        } else {
                            '.'
                        }
                    })
                    .collect();
                println!(
                    "{pad}f{f} bytes[{l}] {} |{txt}|",
                    hex(&d[..d.len().min(40)])
                );
                let _ = depth;
            }
            _ => {
                println!("{pad}f{f} wiretype {w}?");
                return;
            }
        }
    }
}

/// Split a message into (field, wire type, varint value, bytes).
fn pb_fields(b: &[u8]) -> Vec<(u64, u64, u64, Vec<u8>)> {
    let mut p = 0;
    let mut out = vec![];
    while p < b.len() {
        let tag = varint(b, &mut p);
        let (f, w) = (tag >> 3, tag & 7);
        match w {
            0 => out.push((f, w, varint(b, &mut p), vec![])),
            2 => {
                let l = varint(b, &mut p) as usize;
                out.push((f, w, 0, b[p..p + l].to_vec()));
                p += l;
            }
            _ => break,
        }
    }
    out
}

fn esc(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x1b => "\\e".to_string(),
            b'\r' => "\\r".to_string(),
            b'\n' => "\\n".to_string(),
            32..=126 => (c as char).to_string(),
            _ => format!("\\x{c:02x}"),
        })
        .collect()
}

/// One-line decode of a TransportInstruction.
fn summarize(v: &[u8]) -> String {
    let mut ver = 0;
    let (mut old, mut new, mut ack, mut thr, mut chaff) = (0, 0, 0, 0, 0);
    let mut diff = vec![];
    for (f, _, n, d) in pb_fields(v) {
        match f {
            1 => ver = n,
            2 => old = n,
            3 => new = n,
            4 => ack = n,
            5 => thr = n,
            6 => diff = d,
            7 => chaff = d.len(),
            _ => {}
        }
    }
    let mut els = vec![];
    for (f, _, _, e) in pb_fields(&diff) {
        if f != 1 {
            els.push(format!("?f{f}"));
            continue;
        }
        for (g, _, n, d) in pb_fields(&e) {
            match g {
                2 => {
                    let k = pb_fields(&d)
                        .into_iter()
                        .find(|x| x.0 == 4)
                        .map(|x| x.3)
                        .unwrap_or_default();
                    els.push(format!(
                        "bytes[{}]\"{}\"",
                        k.len(),
                        esc(&k[..k.len().min(40)])
                    ));
                }
                3 => {
                    let m = pb_fields(&d);
                    let g5 = m.iter().find(|x| x.0 == 5).map(|x| x.2);
                    let g6 = m.iter().find(|x| x.0 == 6).map(|x| x.2);
                    els.push(format!("resize({g5:?}x{g6:?})"));
                }
                7 => {
                    let n2 = pb_fields(&d).into_iter().find(|x| x.0 == 8).map(|x| x.2);
                    els.push(format!("echo_ack({n2:?})"));
                }
                _ => els.push(format!("?e{g}:{n}")),
            }
        }
    }
    format!(
        "ver={ver} old={old} new={new} ack={ack} thr={thr} chaff={chaff} diff_len={} [{}]",
        diff.len(),
        els.join(", ")
    )
}

fn pb_field2(b: &[u8], want: u64) -> Option<Vec<u8>> {
    let mut p = 0;
    while p < b.len() {
        let tag = varint(b, &mut p);
        let (f, w) = (tag >> 3, tag & 7);
        match w {
            0 => {
                varint(b, &mut p);
            }
            1 => p += 8,
            5 => p += 4,
            2 => {
                let l = varint(b, &mut p) as usize;
                if f == want {
                    return Some(b[p..p + l].to_vec());
                }
                p += l;
            }
            _ => return None,
        }
    }
    None
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pcap = std::fs::read(&a[1]).unwrap();
    let js = std::fs::read_to_string(&a[2]).unwrap();
    let sess = Session::from_base64(&json_str(&js, "key")).unwrap();
    let pkts = parse_pcap(&pcap, 60000);
    println!("{} packets", pkts.len());

    // counters
    for dir in [true, false] {
        let c: Vec<u64> = pkts
            .iter()
            .filter(|p| p.to_server == dir)
            .map(|p| u64::from_be_bytes(p.data[..8].try_into().unwrap()))
            .collect();
        println!(
            "dir to_server={dir}: first counters {:x?}",
            &c[..4.min(c.len())]
        );
    }

    // layouts
    for tag_len in [16usize, 12] {
        for (name, f) in layouts() {
            let ok = pkts
                .iter()
                .filter(|p| try_decrypt(sess.key(), &p.data, tag_len, f(&p.data)).is_some())
                .count();
            println!("layout [{name}] tag={tag_len}: {ok}/{} verify", pkts.len());
        }
    }

    // Winner: 0000 + wire8, tag 16 (checked above). Decode.
    let t0 = pkts[0].ts;
    let mut inflated_all: Vec<(usize, bool, Vec<u8>)> = vec![];
    for p in &pkts {
        let pt = match try_decrypt(sess.key(), &p.data, 16, layouts()[0].1(&p.data)) {
            Some(x) => x,
            None => {
                println!("#{} FAILED", p.n);
                continue;
            }
        };
        let ctr = u64::from_be_bytes(p.data[..8].try_into().unwrap()) & !(1 << 63);
        println!(
            "#{:02} t={:.3} {} wire={} ctr={} pt_len={} pt={}",
            p.n,
            p.ts - t0,
            if p.to_server { "C->S" } else { "S->C" },
            p.data.len(),
            ctr,
            pt.len(),
            hex(&pt[..pt.len().min(32)])
        );
        if pt.len() >= 4 {
            let ts = u16::from_be_bytes([pt[0], pt[1]]);
            let tr = u16::from_be_bytes([pt[2], pt[3]]);
            println!("     ts={ts} ts_reply={tr}");
        }
        if pt.len() >= 14 {
            let fid = u64::from_be_bytes(pt[4..12].try_into().unwrap());
            let fl = u16::from_be_bytes([pt[12], pt[13]]);
            println!(
                "     frag_id={fid} final={} frag_num={}",
                fl >> 15,
                fl & 0x7fff
            );
            match inflate(&pt[14..]) {
                Some(v) => {
                    println!(
                        "     inflated {} bytes (zlib starts at plaintext offset 14)",
                        v.len()
                    );
                    inflated_all.push((p.n, p.to_server, v));
                }
                None => println!("     inflate failed"),
            }
        }
    }
    if let Ok(dir) = std::env::var("MOCHE_DUMP_DIR") {
        for (n, c, v) in &inflated_all {
            let _ = std::fs::write(
                format!("{dir}/{n:02}_{}.bin", if *c { "cs" } else { "sc" }),
                v,
            );
        }
    }
    println!("\n=== summaries ===");
    for (n, c, v) in &inflated_all {
        println!(
            "#{n:02} {} {}",
            if *c { "C->S" } else { "S->C" },
            summarize(v)
        );
    }
    if std::env::var("MOCHE_FULL").is_err() {
        return;
    }
    for (n, c, v) in &inflated_all {
        println!(
            "--- #{n} {} inflated {} bytes: {}",
            if *c { "C->S" } else { "S->C" },
            v.len(),
            hex(&v[..v.len().min(48)])
        );
        pb_dump(v, 2, 0);
        if let Some(d) = pb_field2(v, 6) {
            println!("   diff field6:");
            pb_dump(&d, 6, 0);
        }
    }
}

fn inflate(b: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = vec![];
    flate2::read::ZlibDecoder::new(b)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}
