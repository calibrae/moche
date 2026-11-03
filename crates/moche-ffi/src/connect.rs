//! Parsing of the `MOSH CONNECT <port> <key>` line. Pure, no FFI.

use moche_core::crypto::{Session, KEY_B64_LEN};

/// Longest buffer `find` will scan (ssh banners and motd are far smaller).
pub const MAX_SCAN: usize = 1 << 20;

/// Why a connect line was refused. Never carries the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    /// Input larger than [`MAX_SCAN`].
    TooLarge,
    /// No `MOSH CONNECT` line, or the line is malformed (port, token count).
    NotFound,
    /// The line is there but the key is not a valid 22-character key.
    BadKey,
}

/// A parsed connect line.
pub struct Connect {
    /// UDP port, 1..=65535.
    pub port: u16,
    /// The 22 ASCII key characters.
    pub key: [u8; KEY_B64_LEN],
}

impl Drop for Connect {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.key);
    }
}

/// Parses exactly one line (whitespace and CR/LF around it are ignored).
pub fn parse_line(line: &[u8]) -> Result<Connect, ConnectError> {
    let line = line.trim_ascii();
    let mut tok = line
        .split(|b| b.is_ascii_whitespace())
        .filter(|t| !t.is_empty());
    let (Some(a), Some(b), Some(port), Some(key), None) =
        (tok.next(), tok.next(), tok.next(), tok.next(), tok.next())
    else {
        return Err(ConnectError::NotFound);
    };
    if a != b"MOSH" || b != b"CONNECT" {
        return Err(ConnectError::NotFound);
    }
    if port.is_empty() || port.len() > 5 || !port.iter().all(u8::is_ascii_digit) {
        return Err(ConnectError::NotFound);
    }
    let port: u32 = port.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0'));
    let port = u16::try_from(port)
        .ok()
        .filter(|&p| p != 0)
        .ok_or(ConnectError::NotFound)?;
    let key: [u8; KEY_B64_LEN] = key.try_into().map_err(|_| ConnectError::BadKey)?;
    let c = Connect { port, key };
    if !key_ok(&c.key) {
        return Err(ConnectError::BadKey);
    }
    Ok(c)
}

/// True if `key` is 22 characters of valid unpadded base64 for 16 bytes.
pub fn key_ok(key: &[u8]) -> bool {
    std::str::from_utf8(key)
        .ok()
        .is_some_and(|s| Session::from_base64(s).is_ok())
}

/// Scans a multi-line buffer for the connect line; the first valid one wins.
/// A malformed `MOSH CONNECT` line is remembered so the error is precise when
/// no valid line follows.
pub fn find(buf: &[u8]) -> Result<Connect, ConnectError> {
    if buf.len() > MAX_SCAN {
        return Err(ConnectError::TooLarge);
    }
    let mut worst = ConnectError::NotFound;
    for line in buf.split(|&b| b == b'\n') {
        if !line.trim_ascii_start().starts_with(b"MOSH CONNECT") {
            continue;
        }
        match parse_line(line) {
            Ok(c) => return Ok(c),
            Err(ConnectError::BadKey) => worst = ConnectError::BadKey,
            Err(_) => {}
        }
    }
    Err(worst)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODw";

    #[test]
    fn plain_and_crlf() {
        let c = parse_line(format!("MOSH CONNECT 60001 {KEY}").as_bytes()).unwrap();
        assert_eq!((c.port, &c.key[..]), (60001, KEY.as_bytes()));
        let c = parse_line(format!("  MOSH CONNECT\t7 {KEY}\r\n").as_bytes()).unwrap();
        assert_eq!(c.port, 7);
    }

    #[test]
    fn noisy_buffer() {
        let b = format!(
            "Welcome\r\nlast login\r\n\r\nMOSH CONNECT 60002 {KEY}\r\nmosh-server (mosh 1.4.0)\n[detached]\n"
        );
        assert_eq!(find(b.as_bytes()).unwrap().port, 60002);
    }

    #[test]
    fn first_valid_wins_and_garbage_before_is_skipped() {
        let b = format!("MOSH CONNECT 99999 {KEY}\nMOSH CONNECT 61000 {KEY}\n");
        assert_eq!(find(b.as_bytes()).unwrap().port, 61000);
    }

    #[test]
    fn missing_line() {
        assert_eq!(find(b"hello\nworld\n").err(), Some(ConnectError::NotFound));
        assert_eq!(find(b"").err(), Some(ConnectError::NotFound));
        assert_eq!(
            find(b"mosh connect 1 AAECAwQFBgcICQoLDA0ODw").err(),
            Some(ConnectError::NotFound)
        );
    }

    #[test]
    fn bad_keys() {
        for k in [
            "short",
            "AAECAwQFBgcICQoLDA0OD!",
            "AAECAwQFBgcICQoLDA0ODwAA",
            "",
        ] {
            let r = find(format!("MOSH CONNECT 1 {k}\n").as_bytes());
            let want = if k.is_empty() {
                ConnectError::NotFound // only three tokens
            } else {
                ConnectError::BadKey
            };
            assert_eq!(r.err(), Some(want), "key {k:?}");
        }
        // non-UTF-8 key bytes
        assert_eq!(
            parse_line(b"MOSH CONNECT 1 \xff\xfeAAECAwQFBgcICQoL").err(),
            Some(ConnectError::BadKey)
        );
    }

    #[test]
    fn ports() {
        for p in [
            "0", "65536", "99999", "100000", "-1", "+5", "1e3", "0x10", "",
        ] {
            let r = parse_line(format!("MOSH CONNECT {p} {KEY}").as_bytes());
            assert!(r.is_err(), "port {p:?}");
        }
        assert_eq!(
            parse_line(format!("MOSH CONNECT 65535 {KEY}").as_bytes())
                .unwrap()
                .port,
            65535
        );
        assert_eq!(
            parse_line(format!("MOSH CONNECT 00080 {KEY}").as_bytes())
                .unwrap()
                .port,
            80
        );
    }

    #[test]
    fn extra_tokens_rejected() {
        assert!(parse_line(format!("MOSH CONNECT 1 {KEY} extra").as_bytes()).is_err());
    }

    #[test]
    fn huge_input() {
        let big = vec![b'a'; MAX_SCAN + 1];
        assert_eq!(find(&big).err(), Some(ConnectError::TooLarge));
        // at the limit, a long noisy buffer still works
        let mut b = vec![b'x'; MAX_SCAN - 100];
        b.push(b'\n');
        b.extend_from_slice(format!("MOSH CONNECT 5 {KEY}\n").as_bytes());
        assert_eq!(find(&b).unwrap().port, 5);
        // one giant line without a newline is just noise
        assert_eq!(
            find(&vec![b'M'; MAX_SCAN]).err(),
            Some(ConnectError::NotFound)
        );
    }
}
