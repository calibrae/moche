# Security

moche is a client-only mosh core (Rust, C ABI for iOS). It speaks to a stock
`mosh-server` and is a clean reimplementation from the protocol.

## Threat model

- The only secret is the 128-bit session key from `MOSH CONNECT <port> <key>`.
  It is per session, short-lived, and obtained by the host app over an
  already-authenticated SSH connection.
- The key lives in memory only. moche never writes it to disk, never logs it,
  and never puts it in an error message (`moche_last_error`, CLI output).
- The decoded key is zeroized when `moche_session_new` returns and when the
  session is freed. `Session` in moche-core zeroizes on drop. The AES key
  schedule is zeroized (`aes` with the `zeroize` feature).
- The host app must wipe its own copy: the `key_out` buffer of
  `moche_parse_connect` / `moche_find_connect`, and any `Data`/`String` it built.
- The network is hostile. Every datagram is authenticated (AES-128-OCB3) before
  use; failures are dropped silently and do not count as contact. Reassembly,
  inflate and input sizes are bounded (fragments, 8 MiB inflated, 1 MiB connect
  buffer, 16 MiB keys per call).
- The transport gives confidentiality and integrity of the session, not
  anonymity: source address and datagram sizes and timing are visible. Chaff
  random padding blurs sizes a little; it needs no secrecy.
- Out of scope: a compromised host app or device (memory is readable by it),
  and the security of `mosh-server` itself or of the SSH bootstrap.
- `unsafe` exists only in `moche-ffi`, in small blocks with SAFETY comments;
  every export catches panics at the C boundary.

## Known gap

The `ocb3` crate does not zeroize the tables it derives from the key (its
internal L-values), so they are not wiped when a session is dropped. The key
itself and the AES key schedule are. Those tables are key-derived, not the
key; the residue lives in freed heap memory until reused. Tracking upstream.

## Random numbers

moche-ffi uses the `getrandom` crate (0.4.x) for chaff padding. Read from its
source (`src/backends/`) for the locked version 0.4.3:

- macOS: `getentropy(2)` (backend `getentropy.rs`), no file is opened.
- iOS (also tvOS, watchOS, visionOS): `CCRandomGenerateBytes` from CommonCrypto
  (backend `apple_other.rs`), no file is opened. (`getentropy` is not used there.)

No `/dev/urandom` open on Apple targets in the library. If the system source
fails, chaff degrades to zero bytes (it needs no secrecy; nonces are
sequence numbers, not random). `moche-cli` is a dev tool and reads
`/dev/urandom` through `std` for the same chaff.

## Reporting

Report suspected vulnerabilities privately to the maintainer, not in a public
issue. Include the version (`moche_version`), a description and, if you can, a
reproduction (a captured datagram plus a throwaway key is ideal; never send a
real session key). The contact address will be added here before the
repository goes public.
