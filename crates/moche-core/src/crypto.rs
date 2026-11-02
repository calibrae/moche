//! Session key handling and (later) the packet cipher.

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};

/// Length in bytes of the session key.
pub const KEY_LEN: usize = 16;

/// Length of the unpadded base64 form printed by the server in `MOSH CONNECT`.
pub const KEY_B64_LEN: usize = 22;

/// Errors decoding a session key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// The string is not 22 characters long.
    BadLength(usize),
    /// The string is not valid unpadded base64.
    BadBase64,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::BadLength(n) => write!(f, "key must be {KEY_B64_LEN} characters, got {n}"),
            KeyError::BadBase64 => write!(f, "key is not valid unpadded base64"),
        }
    }
}

impl std::error::Error for KeyError {}

/// The 128-bit shared secret from `MOSH CONNECT <port> <key>`.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    key: [u8; KEY_LEN],
}

impl Session {
    /// Decodes the 22-character unpadded base64 key.
    pub fn from_base64(s: &str) -> Result<Self, KeyError> {
        let s = s.trim();
        if s.len() != KEY_B64_LEN {
            return Err(KeyError::BadLength(s.len()));
        }
        let bytes = STANDARD_NO_PAD.decode(s).map_err(|_| KeyError::BadBase64)?;
        let key: [u8; KEY_LEN] = bytes.try_into().map_err(|_| KeyError::BadBase64)?;
        Ok(Session { key })
    }

    /// Raw key bytes.
    pub fn key(&self) -> &[u8; KEY_LEN] {
        &self.key
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Session(<redacted>)")
    }
}

/// Length in bytes of the nonce sent in clear at the start of every datagram.
pub const WIRE_NONCE_LEN: usize = 8;

/// Length in bytes of the OCB3 nonce.
pub const OCB_NONCE_LEN: usize = 12;

/// OCB3 tag length in bytes (the last 16 bytes of the datagram).
pub const TAG_LEN: usize = 16;

/// Top bit of the wire nonce: clear for client to server, set for server to client.
pub const DIRECTION_BIT: u64 = 1 << 63;

/// Builds the 12-byte OCB3 nonce from the 8 bytes sent in clear: four zero bytes
/// followed by the wire bytes unchanged (direction bit kept). Verified against a
/// stock mosh-server 1.4.0 capture (docs/observations.md, U2-U4). No associated
/// data, 16-byte tag.
pub fn build_nonce(wire: &[u8; WIRE_NONCE_LEN]) -> [u8; OCB_NONCE_LEN] {
    let mut n = [0u8; OCB_NONCE_LEN];
    n[OCB_NONCE_LEN - WIRE_NONCE_LEN..].copy_from_slice(wire);
    n
}

/// Splits the wire nonce into (is_server_to_client, sequence number).
pub fn split_wire_nonce(wire: &[u8; WIRE_NONCE_LEN]) -> (bool, u64) {
    let v = u64::from_be_bytes(*wire);
    (v & DIRECTION_BIT != 0, v & !DIRECTION_BIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_key() {
        // 16 bytes 0x00..0x0f
        let s = Session::from_base64("AAECAwQFBgcICQoLDA0ODw").unwrap();
        let want: Vec<u8> = (0u8..16).collect();
        assert_eq!(s.key().as_slice(), want.as_slice());
    }

    #[test]
    fn rejects_bad_length() {
        assert_eq!(Session::from_base64("AAEC"), Err(KeyError::BadLength(4)));
        assert_eq!(
            Session::from_base64("AAECAwQFBgcICQoLDA0ODw=="),
            Err(KeyError::BadLength(24))
        );
    }

    #[test]
    fn rejects_bad_alphabet() {
        assert_eq!(
            Session::from_base64("AAECAwQFBgcICQoLDA0OD!"),
            Err(KeyError::BadBase64)
        );
    }

    #[test]
    fn debug_hides_key() {
        let s = Session::from_base64("AAECAwQFBgcICQoLDA0ODw").unwrap();
        assert!(!format!("{s:?}").contains("AAEC"));
    }

    // Packets from a real mosh-server 1.4.0 loopback capture, throwaway key.
    const VEC_KEY: &str = "Ixu2un/jwqLpFzsnDHm5lw";
    const VEC_C2S: &str = "0000000000000007c83813e43189920f325240ba203ba38f8ce6f00cc2e42686e9503d462fa8910e4d4d8b77cb4e43a034cafb77c580051b7dda83c6";
    const VEC_S2C: &str = "80000000000000077d3eff25273194f8472a9e5058d9ea455e55cff4c586b9b6a44ffe88adddf19b5bc49639fa139e5c49a731d46208d75d1e7e65d9661396";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    fn open(pkt: &[u8]) -> Vec<u8> {
        use ocb3::AeadInOut;
        use ocb3::{Array, KeyInit};
        let s = Session::from_base64(VEC_KEY).unwrap();
        let wire: [u8; WIRE_NONCE_LEN] = pkt[..WIRE_NONCE_LEN].try_into().unwrap();
        let nonce = build_nonce(&wire);
        let (ct, tag) = pkt[WIRE_NONCE_LEN..].split_at(pkt.len() - WIRE_NONCE_LEN - TAG_LEN);
        let mut buf = ct.to_vec();
        let c = ocb3::Ocb3::<aes::Aes128>::new(s.key().into());
        let tag: &Array<u8, _> = tag.try_into().unwrap();
        c.decrypt_inout_detached((&nonce).into(), b"", buf.as_mut_slice().into(), tag)
            .expect("tag verifies");
        buf
    }

    #[test]
    fn nonce_layout() {
        let w = [0x80, 0, 0, 0, 0, 0, 0, 7];
        assert_eq!(build_nonce(&w), [0, 0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 7]);
        assert_eq!(split_wire_nonce(&w), (true, 7));
        assert_eq!(split_wire_nonce(&[0, 0, 0, 0, 0, 0, 0, 7]), (false, 7));
    }

    #[test]
    fn decrypts_captured_client_packet() {
        let pt = open(&unhex(VEC_C2S));
        assert_eq!(pt.len(), 36);
        // timestamp 9832 ms, timestamp reply 9831 ms (big-endian u16 each)
        assert_eq!(&pt[..4], &[0x26, 0x68, 0x26, 0x67]);
        // fragment id 8, final flag set, fragment number 0, then a zlib stream
        assert_eq!(&pt[4..14], &[0, 0, 0, 0, 0, 0, 0, 8, 0x80, 0]);
        assert_eq!(&pt[14..16], &[0x78, 0x9c]);
    }

    #[test]
    fn decrypts_captured_server_packet() {
        let pt = open(&unhex(VEC_S2C));
        assert_eq!(pt.len(), 39);
        // timestamp 12781 ms, no reply (0xffff)
        assert_eq!(&pt[..4], &[0x31, 0xed, 0xff, 0xff]);
        assert_eq!(&pt[12..14], &[0x80, 0]);
    }

    #[test]
    fn wrong_direction_bit_fails() {
        use ocb3::AeadInOut;
        use ocb3::{Array, KeyInit};
        let pkt = unhex(VEC_S2C);
        let s = Session::from_base64(VEC_KEY).unwrap();
        let mut wire: [u8; 8] = pkt[..8].try_into().unwrap();
        wire[0] &= 0x7f;
        let nonce = build_nonce(&wire);
        let (ct, tag) = pkt[8..].split_at(pkt.len() - 8 - TAG_LEN);
        let mut buf = ct.to_vec();
        let c = ocb3::Ocb3::<aes::Aes128>::new(s.key().into());
        let tag: &Array<u8, _> = tag.try_into().unwrap();
        let r = c.decrypt_inout_detached((&nonce).into(), b"", buf.as_mut_slice().into(), tag);
        assert!(r.is_err());
    }
}
