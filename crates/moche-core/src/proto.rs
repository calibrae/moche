//! Hand-rolled protobuf codec for the SSP instruction and its diff payload.
//!
//! Field numbers are the observed ones (docs/observations.md, U7). Only the
//! wire types needed are produced; unknown fields are skipped on decode.

/// Protocol version carried in field 1 of every instruction.
pub const PROTOCOL_VERSION: u64 = 2;

/// Maximum chaff length put on the wire.
pub const MAX_CHAFF: usize = 16;

/// Decoding failure: the input is truncated or not valid protobuf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("malformed protobuf message")
    }
}

impl std::error::Error for DecodeError {}

/// One transport instruction (both directions).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Instruction {
    /// Field 1, must be [`PROTOCOL_VERSION`].
    pub version: u64,
    /// Field 2: the state the diff applies to.
    pub old_num: u64,
    /// Field 3: the state the diff produces (`u64::MAX` = shutdown).
    pub new_num: u64,
    /// Field 4: highest peer state number received.
    pub ack_num: u64,
    /// Field 5: highest own state number the peer acked.
    pub throwaway_num: u64,
    /// Field 6: serialized diff (see [`encode_diff`]); may be empty.
    pub diff: Vec<u8>,
    /// Field 7: random padding, ignored by the receiver.
    pub chaff: Vec<u8>,
}

/// One element of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    /// Keystrokes (client to server) or host VT bytes (server to client).
    Data(Vec<u8>),
    /// Terminal size in columns and rows.
    Resize { width: u16, height: u16 },
    /// Server only: the client state number whose keystrokes were echoed.
    EchoAck(u64),
}

// ---- low level ---------------------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_uint(out: &mut Vec<u8>, field: u64, v: u64) {
    put_varint(out, field << 3);
    put_varint(out, v);
}

fn put_bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    /// Fixed-width value: skipped, not interpreted.
    Other,
}

/// Iterator over the top-level fields of a message.
struct Fields<'a> {
    buf: &'a [u8],
}

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = *self.buf.get(i).ok_or(DecodeError)?;
            if i == 9 && b > 1 {
                return Err(DecodeError);
            }
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                self.buf = &self.buf[i + 1..];
                return Ok(v);
            }
        }
        Err(DecodeError)
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], DecodeError> {
        let n = usize::try_from(n).map_err(|_| DecodeError)?;
        if n > self.buf.len() {
            return Err(DecodeError);
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buf.is_empty() {
            return None;
        }
        Some(self.field())
    }
}

impl<'a> Fields<'a> {
    fn field(&mut self) -> Result<(u64, Value<'a>), DecodeError> {
        let tag = self.varint()?;
        let value = match tag & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Value::Other
            }
            2 => {
                let n = self.varint()?;
                Value::Bytes(self.take(n)?)
            }
            5 => {
                self.take(4)?;
                Value::Other
            }
            _ => return Err(DecodeError),
        };
        Ok((tag >> 3, value))
    }
}

fn fields(buf: &[u8]) -> Fields<'_> {
    Fields { buf }
}

// ---- instruction -------------------------------------------------------

impl Instruction {
    /// Serializes the instruction. Field 6 is always written, even when
    /// empty (heartbeats carry `32 00`); chaff only when non-empty.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.diff.len() + self.chaff.len());
        put_uint(&mut out, 1, self.version);
        put_uint(&mut out, 2, self.old_num);
        put_uint(&mut out, 3, self.new_num);
        put_uint(&mut out, 4, self.ack_num);
        put_uint(&mut out, 5, self.throwaway_num);
        put_bytes(&mut out, 6, &self.diff);
        if !self.chaff.is_empty() {
            put_bytes(&mut out, 7, &self.chaff);
        }
        out
    }

    /// Parses an instruction. Missing fields are zero/empty, unknown fields
    /// are skipped, truncated or malformed input is an error.
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let mut i = Instruction::default();
        for f in fields(buf) {
            match f? {
                (1, Value::Varint(v)) => i.version = v,
                (2, Value::Varint(v)) => i.old_num = v,
                (3, Value::Varint(v)) => i.new_num = v,
                (4, Value::Varint(v)) => i.ack_num = v,
                (5, Value::Varint(v)) => i.throwaway_num = v,
                (6, Value::Bytes(b)) => i.diff = b.to_vec(),
                (7, Value::Bytes(b)) => i.chaff = b.to_vec(),
                _ => {}
            }
        }
        Ok(i)
    }
}

// ---- diff --------------------------------------------------------------

/// Serializes diff elements (the content of instruction field 6).
pub fn encode_diff(elements: &[Element]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in elements {
        let mut inner = Vec::new();
        match e {
            Element::Data(b) => {
                let mut m = Vec::new();
                put_bytes(&mut m, 4, b);
                put_bytes(&mut inner, 2, &m);
            }
            Element::Resize { width, height } => {
                let mut m = Vec::new();
                put_uint(&mut m, 5, u64::from(*width));
                put_uint(&mut m, 6, u64::from(*height));
                put_bytes(&mut inner, 3, &m);
            }
            Element::EchoAck(n) => {
                let mut m = Vec::new();
                put_uint(&mut m, 8, *n);
                put_bytes(&mut inner, 7, &m);
            }
        }
        put_bytes(&mut out, 1, &inner);
    }
    out
}

/// Parses diff elements. Elements of unknown kind are skipped.
pub fn decode_diff(buf: &[u8]) -> Result<Vec<Element>, DecodeError> {
    let mut out = Vec::new();
    for f in fields(buf) {
        let (1, Value::Bytes(el)) = f? else { continue };
        if let Some(e) = decode_element(el)? {
            out.push(e);
        }
    }
    Ok(out)
}

fn decode_element(buf: &[u8]) -> Result<Option<Element>, DecodeError> {
    let mut found = None;
    for f in fields(buf) {
        match f? {
            (2, Value::Bytes(m)) => {
                for g in fields(m) {
                    if let (4, Value::Bytes(b)) = g? {
                        found = Some(Element::Data(b.to_vec()));
                    }
                }
            }
            (3, Value::Bytes(m)) => {
                let (mut width, mut height) = (0u16, 0u16);
                for g in fields(m) {
                    match g? {
                        (5, Value::Varint(v)) => {
                            width = u16::try_from(v).map_err(|_| DecodeError)?
                        }
                        (6, Value::Varint(v)) => {
                            height = u16::try_from(v).map_err(|_| DecodeError)?
                        }
                        _ => {}
                    }
                }
                found = Some(Element::Resize { width, height });
            }
            (7, Value::Bytes(m)) => {
                for g in fields(m) {
                    if let (8, Value::Varint(v)) = g? {
                        found = Some(Element::EchoAck(v));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn captured_client_instruction() {
        // observations.md U7: `echo hi\r` keystrokes, old 3 new 5 ack 3 thr 3, 12 bytes chaff
        let mut wire = unhex(
            "0802100318052003280332 0e0a0c120a22086563686f2068690d3a0c"
                .replace(' ', "")
                .as_str(),
        );
        wire.extend_from_slice(&[7; 12]);
        let i = Instruction::decode(&wire).unwrap();
        assert_eq!((i.version, i.old_num, i.new_num, i.ack_num), (2, 3, 5, 3));
        assert_eq!(i.throwaway_num, 3);
        assert_eq!(i.chaff.len(), 12);
        assert_eq!(
            decode_diff(&i.diff).unwrap(),
            vec![Element::Data(b"echo hi\r".to_vec())]
        );
        assert_eq!(i.encode(), wire);
    }

    #[test]
    fn diff_round_trip() {
        let els = vec![
            Element::Resize {
                width: 100,
                height: 30,
            },
            Element::EchoAck(5),
            Element::Data(vec![0, 1, 2, 255]),
            Element::Data(vec![]),
        ];
        assert_eq!(decode_diff(&encode_diff(&els)).unwrap(), els);
    }

    #[test]
    fn instruction_round_trip_extremes() {
        let i = Instruction {
            version: PROTOCOL_VERSION,
            old_num: 19,
            new_num: u64::MAX,
            ack_num: u64::MAX,
            throwaway_num: 1 << 40,
            diff: vec![9; 300],
            chaff: vec![1; 16],
        };
        let b = i.encode();
        assert_eq!(Instruction::decode(&b).unwrap(), i);
    }

    #[test]
    fn empty_diff_is_written() {
        let i = Instruction {
            version: 2,
            ..Default::default()
        };
        assert!(i.encode().ends_with(&[0x32, 0x00]));
    }

    #[test]
    fn unknown_fields_are_skipped() {
        let mut b = Instruction {
            version: 2,
            new_num: 7,
            ..Default::default()
        }
        .encode();
        put_uint(&mut b, 15, 12345); // varint
        put_bytes(&mut b, 20, b"junk"); // length-delimited
        put_varint(&mut b, (9 << 3) | 5); // fixed32
        b.extend_from_slice(&[1, 2, 3, 4]);
        put_varint(&mut b, (10 << 3) | 1); // fixed64
        b.extend_from_slice(&[0; 8]);
        let i = Instruction::decode(&b).unwrap();
        assert_eq!((i.version, i.new_num), (2, 7));
        // unknown diff element kind
        let mut d = Vec::new();
        let mut el = Vec::new();
        put_uint(&mut el, 30, 1);
        put_bytes(&mut d, 1, &el);
        assert_eq!(decode_diff(&d).unwrap(), vec![]);
    }

    #[test]
    fn truncation_is_rejected_without_panic() {
        let b = Instruction {
            version: 2,
            old_num: 300,
            new_num: u64::MAX,
            diff: encode_diff(&[Element::Data(b"hello".to_vec())]),
            chaff: vec![3; 10],
            ..Default::default()
        }
        .encode();
        let mut rejected = 0;
        for n in 0..b.len() {
            if Instruction::decode(&b[..n]).is_err() {
                rejected += 1;
            }
        }
        assert!(rejected > 0);
        // a cut in the middle of the trailing field must fail
        assert!(Instruction::decode(&b[..b.len() - 1]).is_err());
        let d = encode_diff(&[Element::Resize {
            width: 80,
            height: 24,
        }]);
        for n in 1..d.len() {
            assert!(decode_diff(&d[..n]).is_err(), "cut at {n}");
        }
    }

    #[test]
    fn garbage_never_panics() {
        let mut x = 0x1234_5678_9abc_def1u64;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = (x % 40) as usize;
            let b: Vec<u8> = (0..len)
                .map(|i| (x >> (i % 8 * 8)) as u8 ^ i as u8)
                .collect();
            let _ = Instruction::decode(&b);
            let _ = decode_diff(&b);
        }
    }

    #[test]
    fn overlong_varint_rejected() {
        let b = [
            0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
        ];
        assert!(Instruction::decode(&b).is_err());
        let b = [
            0x08, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
        ];
        assert!(Instruction::decode(&b).is_err());
    }
}
