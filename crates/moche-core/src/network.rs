//! Datagram layer: sealing, fragments, compression, reassembly and RTT.
//!
//! Wire layout (docs/observations.md): `nonce8 | OCB3(plaintext) | tag16`, with
//! plaintext `ts u16 | ts_reply u16 | frag_id u64 | flags u16 | payload`, all
//! big-endian. The serialized instruction is zlib-compressed once, cut into
//! [`FRAGMENT_PAYLOAD`]-byte pieces, and each piece is sealed on its own.

use aes::Aes128;
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use ocb3::aead::{Aead, KeyInit, Payload};
use ocb3::Ocb3;
use std::collections::BTreeMap;

use crate::crypto::{
    build_nonce, split_wire_nonce, Session, DIRECTION_BIT, TAG_LEN, WIRE_NONCE_LEN,
};

/// Payload bytes per full fragment (observed: datagram 1252 = 8 + 4 + 10 + 1214 + 16).
pub const FRAGMENT_PAYLOAD: usize = 1214;

/// Plaintext bytes before the payload: ts, ts_reply, frag id, flags.
const INNER_HEADER: usize = 14;

/// Largest datagram moche sends: nonce + tag + inner header + a full fragment (1252).
/// Under the 1280-byte IPv6 minimum MTU, so it never fragments at IP level.
pub const MAX_DATAGRAM: usize = WIRE_NONCE_LEN + TAG_LEN + INNER_HEADER + FRAGMENT_PAYLOAD;

/// Most fragments accepted or produced for one message: 2048 x 1214 bytes, about
/// 2.4 MB compressed. This is moche's choice, not an observed server value. A
/// 1000x1000 full repaint is at worst a few MB of VT text that compresses well
/// below that. (The wire field allows up to 32768.) Our own sender stays far
/// below it: one instruction carries at most `ssp::MAX_STATE_BYTES` of input.
pub(crate) const MAX_FRAGMENTS: u16 = 2048;

/// Largest inflated instruction accepted (guards against zlib bombs). 16 MiB is
/// well above a worst-case 1000x1000 repaint with an SGR change on every cell.
const MAX_INFLATED: u64 = 16 << 20;

/// Message groups kept while waiting for missing fragments.
const MAX_GROUPS: usize = 8;

/// Timestamp reply value meaning "nothing to reply to".
pub const NO_REPLY: u16 = 0xffff;

/// Errors from the datagram layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// Datagram or plaintext shorter than its fixed header.
    Short,
    /// Authentication tag did not verify.
    Auth,
    /// Inconsistent or out-of-range fragment numbering.
    BadFragment,
    /// Not a valid zlib stream, or larger than the inflate limit.
    Inflate,
    /// Message needs more fragments than the format allows.
    TooLarge,
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NetError::Short => "datagram too short",
            NetError::Auth => "authentication failed",
            NetError::BadFragment => "malformed fragment",
            NetError::Inflate => "bad or oversized compressed data",
            NetError::TooLarge => "message too large to fragment",
        })
    }
}

impl std::error::Error for NetError {}

/// Which way a datagram travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client to server (top nonce bit clear).
    ToServer,
    /// Server to client (top nonce bit set).
    ToClient,
}

/// One fragment of a compressed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// Message id, shared by all fragments of one message.
    pub id: u64,
    /// Position, from 0.
    pub num: u16,
    /// True on the last fragment.
    pub last: bool,
    /// Slice of the compressed message.
    pub data: Vec<u8>,
}

/// The decrypted body of a datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inner {
    /// Sender clock, milliseconds mod 65536.
    pub ts: u16,
    /// Echo of the peer's latest timestamp plus hold time, if any.
    pub ts_reply: Option<u16>,
    /// The fragment carried.
    pub frag: Fragment,
}

impl Inner {
    /// Serializes to the plaintext layout.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(INNER_HEADER + self.frag.data.len());
        out.extend_from_slice(&self.ts.to_be_bytes());
        out.extend_from_slice(&self.ts_reply.unwrap_or(NO_REPLY).to_be_bytes());
        out.extend_from_slice(&self.frag.id.to_be_bytes());
        let flags = (u16::from(self.frag.last) << 15) | (self.frag.num & 0x7fff);
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&self.frag.data);
        out
    }

    /// Parses the plaintext layout.
    pub fn decode(b: &[u8]) -> Result<Self, NetError> {
        if b.len() < INNER_HEADER {
            return Err(NetError::Short);
        }
        let u16_at = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        let reply = u16_at(2);
        let mut id = [0u8; 8];
        id.copy_from_slice(&b[4..12]);
        let flags = u16_at(12);
        Ok(Inner {
            ts: u16_at(0),
            ts_reply: (reply != NO_REPLY).then_some(reply),
            frag: Fragment {
                id: u64::from_be_bytes(id),
                num: flags & 0x7fff,
                last: flags & 0x8000 != 0,
                data: b[INNER_HEADER..].to_vec(),
            },
        })
    }
}

/// A successfully authenticated datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// Direction taken from the nonce.
    pub direction: Direction,
    /// 63-bit datagram sequence number.
    pub seq: u64,
    /// Decrypted plaintext (see [`Inner::decode`]).
    pub plaintext: Vec<u8>,
}

/// AES-128-OCB3 sealing and opening of datagrams.
pub struct Cipher(Ocb3<Aes128>);

impl Cipher {
    /// Builds the cipher from the session key.
    pub fn new(session: &Session) -> Self {
        Cipher(Ocb3::new(session.key().into()))
    }

    /// Encrypts `plaintext` as datagram number `seq` (top bit ignored) in
    /// direction `dir`. Never reuse a `(dir, seq)` pair.
    pub fn seal(&self, dir: Direction, seq: u64, plaintext: &[u8]) -> Result<Vec<u8>, NetError> {
        let mut v = seq & !DIRECTION_BIT;
        if dir == Direction::ToClient {
            v |= DIRECTION_BIT;
        }
        let wire = v.to_be_bytes();
        let nonce = build_nonce(&wire);
        let ct = self
            .0
            .encrypt(
                &nonce.into(),
                Payload {
                    msg: plaintext,
                    aad: b"",
                },
            )
            .map_err(|_| NetError::Auth)?;
        let mut out = Vec::with_capacity(WIRE_NONCE_LEN + ct.len());
        out.extend_from_slice(&wire);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Authenticates and decrypts a datagram.
    pub fn open(&self, datagram: &[u8]) -> Result<Opened, NetError> {
        if datagram.len() < WIRE_NONCE_LEN + TAG_LEN {
            return Err(NetError::Short);
        }
        let (wire, body) = datagram.split_at(WIRE_NONCE_LEN);
        let wire: [u8; WIRE_NONCE_LEN] = wire.try_into().map_err(|_| NetError::Short)?;
        let nonce = build_nonce(&wire);
        let plaintext = self
            .0
            .decrypt(
                &nonce.into(),
                Payload {
                    msg: body,
                    aad: b"",
                },
            )
            .map_err(|_| NetError::Auth)?;
        let (to_client, seq) = split_wire_nonce(&wire);
        Ok(Opened {
            direction: if to_client {
                Direction::ToClient
            } else {
                Direction::ToServer
            },
            seq,
            plaintext,
        })
    }
}

/// Reusable zlib compressor. Building a deflate state allocates and zeroes
/// several hundred KiB, so a long-lived sender keeps one and resets it per message
/// (the output is byte-identical to a fresh encoder).
pub struct Deflater {
    z: Compress,
}

impl Default for Deflater {
    fn default() -> Self {
        Self::new()
    }
}

impl Deflater {
    /// A compressor at the default level (same as [`fragment`]).
    pub fn new() -> Self {
        Deflater {
            z: Compress::new(Compression::default(), true),
        }
    }

    /// Compresses a serialized instruction and cuts it into fragments sharing `id`.
    pub fn fragment(&mut self, id: u64, message: &[u8]) -> Result<Vec<Fragment>, NetError> {
        let z = self.compress(message)?;
        let n = z.len().div_ceil(FRAGMENT_PAYLOAD).max(1);
        if n > usize::from(MAX_FRAGMENTS) {
            return Err(NetError::TooLarge);
        }
        Ok((0..n)
            .map(|i| Fragment {
                id,
                num: i as u16,
                last: i + 1 == n,
                data: z
                    .chunks(FRAGMENT_PAYLOAD)
                    .nth(i)
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default(),
            })
            .collect())
    }

    /// zlib stream of `message`: the input is fed with no flush, then the stream
    /// is finished, which is what a streaming encoder's `write_all` + `finish` does.
    fn compress(&mut self, message: &[u8]) -> Result<Vec<u8>, NetError> {
        self.z.reset();
        let mut out = Vec::with_capacity(message.len() / 2 + 64);
        let mut input = message;
        while !input.is_empty() {
            if out.capacity() - out.len() < 64 {
                out.reserve(message.len() / 2 + 64);
            }
            let before = self.z.total_in();
            self.z
                .compress_vec(input, &mut out, FlushCompress::None)
                .map_err(|_| NetError::Inflate)?;
            input = &input[(self.z.total_in() - before) as usize..];
        }
        loop {
            if out.capacity() - out.len() < 64 {
                out.reserve(256);
            }
            match self
                .z
                .compress_vec(&[], &mut out, FlushCompress::Finish)
                .map_err(|_| NetError::Inflate)?
            {
                Status::StreamEnd => return Ok(out),
                Status::Ok | Status::BufError => {}
            }
        }
    }
}

/// Compresses a serialized instruction and cuts it into fragments sharing `id`.
pub fn fragment(id: u64, message: &[u8]) -> Result<Vec<Fragment>, NetError> {
    Deflater::new().fragment(id, message)
}

#[derive(Default)]
struct Group {
    parts: BTreeMap<u16, Vec<u8>>,
    last: Option<u16>,
}

/// Collects fragments into messages. Tolerates duplicates and reordering,
/// keeps at most [`MAX_GROUPS`] incomplete messages, and ignores any message
/// with an id at or below the newest one already completed.
pub struct Reassembler {
    groups: BTreeMap<u64, Group>,
    done: Option<u64>,
    /// Reused inflate state (building one allocates tens of KiB).
    z: Decompress,
}

impl Default for Reassembler {
    fn default() -> Self {
        Reassembler {
            groups: BTreeMap::new(),
            done: None,
            z: Decompress::new(true),
        }
    }
}

impl Reassembler {
    /// New empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a fragment. Returns the inflated message once complete,
    /// `Ok(None)` while waiting or for stale/duplicate input.
    pub fn push(&mut self, f: Fragment) -> Result<Option<Vec<u8>>, NetError> {
        if self.done.is_some_and(|d| f.id <= d) {
            return Ok(None);
        }
        if f.num >= MAX_FRAGMENTS {
            return Err(NetError::TooLarge);
        }
        if !self.groups.contains_key(&f.id) && self.groups.len() >= MAX_GROUPS {
            let oldest = self.groups.keys().next().copied().unwrap_or(f.id);
            if f.id < oldest {
                return Ok(None);
            }
            self.groups.remove(&oldest);
        }
        if f.last
            && f.num == 0
            && f.data.len() <= FRAGMENT_PAYLOAD
            && !self.groups.contains_key(&f.id)
        {
            // Whole message in one fragment (the common case): no bookkeeping.
            self.groups.retain(|&id, _| id > f.id);
            self.done = Some(f.id);
            return inflate(&mut self.z, &f.data).map(Some);
        }
        let g = self.groups.entry(f.id).or_default();
        let bad = f.data.len() > FRAGMENT_PAYLOAD
            || (f.last && g.last.is_some_and(|l| l != f.num))
            || (f.last && g.parts.keys().any(|&k| k > f.num))
            || g.last.is_some_and(|l| f.num > l);
        if bad {
            self.groups.remove(&f.id);
            return Err(NetError::BadFragment);
        }
        if f.last {
            g.last = Some(f.num);
        }
        g.parts.entry(f.num).or_insert(f.data);
        let Some(last) = g.last else { return Ok(None) };
        if g.parts.len() != usize::from(last) + 1 {
            return Ok(None);
        }
        let compressed: Vec<u8> = g.parts.values().flatten().copied().collect();
        self.groups.retain(|&id, _| id > f.id);
        self.done = Some(f.id);
        inflate(&mut self.z, &compressed).map(Some)
    }
}

fn inflate(d: &mut Decompress, z: &[u8]) -> Result<Vec<u8>, NetError> {
    d.reset(true);
    let mut out: Vec<u8> = Vec::with_capacity((z.len() * 4).clamp(256, 1 << 16));
    loop {
        if out.capacity() - out.len() < 256 {
            out.reserve(out.len().max(4096));
        }
        let (in0, out0) = (d.total_in(), out.len());
        let status = d
            .decompress_vec(
                &z[(in0 as usize).min(z.len())..],
                &mut out,
                FlushDecompress::None,
            )
            .map_err(|_| NetError::Inflate)?;
        if out.len() as u64 > MAX_INFLATED {
            return Err(NetError::Inflate);
        }
        match status {
            Status::StreamEnd => return Ok(out),
            // No progress with input left or exhausted: truncated stream.
            _ if d.total_in() == in0 && out.len() == out0 && out.capacity() > out.len() => {
                return Err(NetError::Inflate)
            }
            _ => {}
        }
    }
}

/// Smoothed round-trip time estimate (RFC 6298 constants, RTO floor 50 ms).
#[derive(Debug, Clone, Default)]
pub struct Rtt {
    est: Option<(f64, f64)>, // (srtt, rttvar) in ms
}

impl Rtt {
    /// Smoothed RTT in ms; 1000 before the first sample.
    pub fn srtt_ms(&self) -> f64 {
        self.est.map_or(1000.0, |(s, _)| s)
    }

    /// Round-trip variation in ms; half the first-sample RTT before any (so
    /// 500 before the first sample).
    pub fn rttvar_ms(&self) -> f64 {
        self.est.map_or(500.0, |(_, v)| v)
    }

    /// Retransmission timeout in ms: `srtt + 4 * rttvar`, clamped to 50..=1000.
    pub fn rto_ms(&self) -> f64 {
        self.est
            .map_or(1000.0, |(s, v)| (s + 4.0 * v).clamp(50.0, 1000.0))
    }

    /// Feeds one RTT sample in ms.
    pub fn sample(&mut self, r: f64) {
        self.est = Some(match self.est {
            None => (r, r / 2.0),
            Some((s, v)) => (0.875 * s + 0.125 * r, 0.75 * v + 0.25 * (s - r).abs()),
        });
    }

    /// RTT from a received `ts_reply` (already hold-adjusted by the peer) at
    /// local time `now_ms`. Implausible values (over 10 s) are discarded.
    pub fn sample_from_reply(&mut self, now_ms: u64, ts_reply: u16) {
        let r = now_ms as u16;
        let r = r.wrapping_sub(ts_reply);
        if r <= 10_000 {
            self.sample(f64::from(r));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "Ixu2un/jwqLpFzsnDHm5lw";
    const VEC_S2C: &str = "80000000000000077d3eff25273194f8472a9e5058d9ea455e55cff4c586b9b6a44ffe88adddf19b5bc49639fa139e5c49a731d46208d75d1e7e65d9661396";

    fn cipher() -> Cipher {
        Cipher::new(&Session::from_base64(KEY).unwrap())
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    fn lcg(seed: &mut u64) -> u8 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 33) as u8
    }

    #[test]
    fn opens_captured_datagram() {
        let o = cipher().open(&unhex(VEC_S2C)).unwrap();
        assert_eq!(o.direction, Direction::ToClient);
        assert_eq!(o.seq, 7);
        let i = Inner::decode(&o.plaintext).unwrap();
        assert_eq!((i.ts, i.ts_reply), (12781, None));
        assert!(i.frag.last && i.frag.num == 0);
    }

    #[test]
    fn seal_matches_capture_bit_for_bit() {
        let c = cipher();
        let pkt = unhex(VEC_S2C);
        let o = c.open(&pkt).unwrap();
        assert_eq!(
            c.seal(Direction::ToClient, o.seq, &o.plaintext).unwrap(),
            pkt
        );
    }

    #[test]
    fn seal_open_round_trip_and_tamper() {
        let c = cipher();
        let d = c.seal(Direction::ToServer, 42, b"hello").unwrap();
        assert_eq!(d.len(), 8 + 5 + 16);
        assert_eq!(&d[..8], &42u64.to_be_bytes());
        let o = c.open(&d).unwrap();
        assert_eq!(
            (o.direction, o.seq, o.plaintext.as_slice()),
            (Direction::ToServer, 42, &b"hello"[..])
        );
        let mut bad = d.clone();
        bad[9] ^= 1;
        assert_eq!(c.open(&bad), Err(NetError::Auth));
        let mut flipped = d.clone();
        flipped[0] |= 0x80; // flip the direction bit
        assert_eq!(c.open(&flipped), Err(NetError::Auth));
        assert_eq!(c.open(&d[..20]), Err(NetError::Short));
    }

    #[test]
    fn inner_round_trip() {
        let i = Inner {
            ts: 65535,
            ts_reply: Some(12),
            frag: Fragment {
                id: u64::MAX - 3,
                num: 0x7fff,
                last: true,
                data: vec![1, 2, 3],
            },
        };
        assert_eq!(Inner::decode(&i.encode()).unwrap(), i);
        assert_eq!(Inner::decode(&[0; 13]), Err(NetError::Short));
    }

    #[test]
    fn small_message_is_one_fragment() {
        let f = fragment(9, b"hello hello hello").unwrap();
        assert_eq!(f.len(), 1);
        assert!(f[0].last && f[0].num == 0 && f[0].id == 9);
        assert_eq!(&f[0].data[..2], &[0x78, 0x9c]);
        let mut r = Reassembler::new();
        assert_eq!(r.push(f[0].clone()).unwrap().unwrap(), b"hello hello hello");
    }

    #[test]
    fn large_incompressible_shuffled_with_duplicate() {
        let mut s = 7u64;
        let msg: Vec<u8> = (0..9000).map(|_| lcg(&mut s)).collect();
        let frags = fragment(5, &msg).unwrap();
        assert!(frags.len() >= 7);
        assert!(frags[..frags.len() - 1]
            .iter()
            .all(|f| f.data.len() == FRAGMENT_PAYLOAD && !f.last));
        assert!(frags.last().unwrap().last);
        // deterministic shuffle: reverse, then swap a few, plus a duplicate
        let mut order: Vec<usize> = (0..frags.len()).rev().collect();
        order.swap(0, 2);
        order.insert(3, order[1]);
        let mut r = Reassembler::new();
        let mut out = None;
        for (k, &i) in order.iter().enumerate() {
            let res = r.push(frags[i].clone()).unwrap();
            if k + 1 < order.len() {
                // the last arrival may be a duplicate; completion happens once
                if res.is_some() {
                    out = res;
                }
            } else if res.is_some() {
                out = res;
            }
        }
        assert_eq!(out.unwrap(), msg);
        // sealed datagrams stay within the observed maximum
        let c = cipher();
        for f in &frags {
            let inner = Inner {
                ts: 1,
                ts_reply: None,
                frag: f.clone(),
            };
            let d = c
                .seal(Direction::ToClient, f.num.into(), &inner.encode())
                .unwrap();
            assert!(d.len() <= 1252);
        }
    }

    #[test]
    fn stale_groups_dropped_when_newer_completes() {
        let mut s = 3u64;
        let big: Vec<u8> = (0..4000).map(|_| lcg(&mut s)).collect();
        let old = fragment(1, &big).unwrap();
        let new = fragment(2, b"fresh").unwrap();
        let mut r = Reassembler::new();
        assert!(r.push(old[0].clone()).unwrap().is_none());
        assert_eq!(r.push(new[0].clone()).unwrap().unwrap(), b"fresh");
        for f in &old[1..] {
            assert!(r.push(f.clone()).unwrap().is_none());
        }
        // duplicate of the completed one is ignored too
        assert!(r.push(new[0].clone()).unwrap().is_none());
        assert!(r.groups.is_empty());
    }

    #[test]
    fn malformed_fragments_rejected() {
        let mut r = Reassembler::new();
        let f = |num, last, len| Fragment {
            id: 1,
            num,
            last,
            data: vec![0; len],
        };
        assert_eq!(r.push(f(MAX_FRAGMENTS, false, 1)), Err(NetError::TooLarge));
        assert_eq!(r.push(f(0, false, 1215)), Err(NetError::BadFragment));
        // two different final fragments
        assert!(r.push(f(2, true, 5)).unwrap().is_none());
        assert_eq!(r.push(f(3, true, 5)), Err(NetError::BadFragment));
        // fragment beyond the final one
        let mut r = Reassembler::new();
        assert!(r.push(f(1, true, 5)).unwrap().is_none());
        assert_eq!(r.push(f(2, false, 5)), Err(NetError::BadFragment));
        // not zlib
        let mut r = Reassembler::new();
        assert_eq!(r.push(f(0, true, 20)), Err(NetError::Inflate));
    }

    #[test]
    fn group_count_is_bounded() {
        let mut r = Reassembler::new();
        for id in 1..=100u64 {
            let _ = r.push(Fragment {
                id,
                num: 0,
                last: false,
                data: vec![1],
            });
            assert!(r.groups.len() <= MAX_GROUPS);
        }
    }

    #[test]
    fn inflate_bomb_rejected() {
        let big = vec![0u8; (MAX_INFLATED + 10) as usize];
        let frags = fragment(1, &big).unwrap();
        let mut r = Reassembler::new();
        let mut res = Ok(None);
        for f in frags {
            res = r.push(f);
        }
        assert_eq!(res, Err(NetError::Inflate));
    }

    /// The previous implementation (a fresh streaming encoder per message),
    /// kept as the oracle for the reusable one.
    fn oracle_compress(message: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(message).unwrap();
        enc.finish().unwrap()
    }

    fn oracle_inflate(z: &[u8]) -> Result<Vec<u8>, ()> {
        use std::io::Read;
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(z)
            .take(MAX_INFLATED + 1)
            .read_to_end(&mut out)
            .map_err(|_| ())?;
        if out.len() as u64 > MAX_INFLATED {
            return Err(());
        }
        Ok(out)
    }

    #[test]
    fn reused_deflater_matches_fresh_encoder_byte_for_byte() {
        let mut d = Deflater::new();
        let mut s = 99u64;
        // sizes around block, window and fragment boundaries; text and noise
        for (k, len) in [
            0usize, 1, 2, 30, 100, 1213, 1214, 1215, 4096, 16384, 32768, 32769, 65536, 100_000,
            300_000,
        ]
        .into_iter()
        .cycle()
        .take(45)
        .enumerate()
        {
            let msg: Vec<u8> = (0..len)
                .map(|i| match k % 3 {
                    0 => lcg(&mut s),
                    1 => b"abc def \x1b[31m"[i % 13],
                    _ => (lcg(&mut s) & 0x0f) + b'a',
                })
                .collect();
            let z = oracle_compress(&msg);
            let frags = d.fragment(7, &msg).unwrap();
            let joined: Vec<u8> = frags.iter().flat_map(|f| f.data.iter().copied()).collect();
            assert_eq!(joined, z, "len {len} kind {}", k % 3);
            assert_eq!(fragment(7, &msg).unwrap(), frags);
        }
    }

    #[test]
    fn inflate_matches_streaming_decoder_on_good_truncated_and_corrupt_input() {
        let mut r = Reassembler::new();
        let mut s = 5u64;
        for len in [0usize, 10, 1000, 50_000, 200_000] {
            let msg: Vec<u8> = (0..len).map(|_| lcg(&mut s) & 0x3f).collect();
            let z = oracle_compress(&msg);
            let mut cases: Vec<Vec<u8>> = vec![
                z.clone(),
                z[..z.len() / 2].to_vec(),
                z[..z.len() - 1].to_vec(),
            ];
            let mut bad = z.clone();
            if bad.len() > 10 {
                let mid = bad.len() / 2;
                bad[mid] ^= 0x55;
            }
            cases.push(bad);
            cases.push([z.as_slice(), b"trailing"].concat());
            for c in cases {
                assert_eq!(
                    inflate(&mut r.z, &c).ok(),
                    oracle_inflate(&c).ok(),
                    "len {len}, input {} B",
                    c.len()
                );
            }
        }
    }

    #[test]
    fn rtt_estimation() {
        let mut r = Rtt::default();
        assert_eq!(r.srtt_ms(), 1000.0);
        r.sample(100.0);
        assert_eq!(r.srtt_ms(), 100.0);
        assert_eq!(r.rto_ms(), 300.0);
        for _ in 0..200 {
            r.sample(1.0);
        }
        assert!(r.srtt_ms() < 2.0);
        assert_eq!(r.rto_ms(), 50.0);
        // wraparound: now = 70000 (16-bit 4464), reply 4400 -> 64 ms
        let mut r = Rtt::default();
        r.sample_from_reply(70_000, 4400);
        assert_eq!(r.srtt_ms(), 64.0);
        // implausible sample ignored
        let mut r = Rtt::default();
        r.sample_from_reply(100, 5000);
        assert_eq!(r.srtt_ms(), 1000.0);
    }
}
