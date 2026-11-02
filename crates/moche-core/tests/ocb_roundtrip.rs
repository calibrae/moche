//! Proves the `ocb3` + `aes` dependencies work. Uses a plain 12-byte nonce,
//! not mosh's nonce layout (unverified).

use aes::Aes128;
use ocb3::aead::{Aead, KeyInit, Payload};
use ocb3::Ocb3;

type Cipher = Ocb3<Aes128, ocb3::consts::U12>;

#[test]
fn round_trip_and_tamper() {
    let key = [7u8; 16];
    let nonce = [1u8; 12];
    let c = Cipher::new(&key.into());
    let ct = c
        .encrypt(
            &nonce.into(),
            Payload {
                msg: b"hello moche",
                aad: b"",
            },
        )
        .unwrap();
    assert_eq!(ct.len(), b"hello moche".len() + 16);
    let pt = c
        .decrypt(&nonce.into(), Payload { msg: &ct, aad: b"" })
        .unwrap();
    assert_eq!(pt, b"hello moche");

    let mut bad = ct.clone();
    bad[0] ^= 1;
    assert!(c
        .decrypt(
            &nonce.into(),
            Payload {
                msg: &bad,
                aad: b""
            }
        )
        .is_err());
}
