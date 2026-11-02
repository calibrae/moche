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

/// TODO(protocol U2-U4): how the 12-byte OCB nonce is built from the packet
/// sequence number and direction bit is unverified in docs/protocol.md.
/// Deliberately unimplemented until confirmed against a real server.
pub fn build_nonce_todo() -> ! {
    unimplemented!("OCB nonce layout unverified, see docs/protocol.md U2-U4")
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
}
