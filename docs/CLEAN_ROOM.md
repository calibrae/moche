# Clean-room process, and its limits

moche implements a protocol that stock mosh already implements. mosh is GPLv3; moche is
MIT OR Apache-2.0. This file says how moche was written to keep them apart, how you can
check it, and what the process cannot prove.

## What the behaviour was derived from

- the published mosh paper (Winstein and Balakrishnan, USENIX ATC 2012);
- the mosh.org site and FAQ, and public mosh-devel mailing-list posts;
- RFC 7253 (OCB), RFC 6298 (retransmission timer) and ECMA-48 (terminal language);
- black-box observation of a stock `mosh-server` 1.4.0 (Debian package): packet captures
  decoded with our own code and with generic `protoc --decode_raw`, run in the labs under
  `tools/mosh-lab` and `tools/vm-lab`. The evidence is in `docs/observations*.md`;
- the API reference pages of one permissively licensed third-party package, used only for
  names and sizes, and each of them confirmed by a capture (`docs/protocol.md`, source [6]).

## Rules that were followed

1. No file of mosh's repository (source, `.proto`, tests), no mirror or fork of it, was
   read. The man pages were not fetched.
2. Nothing was copied, translated or closely paraphrased from mosh.
3. Other GPL clients and ports of mosh were not consulted.
4. A claim that no document or capture supports stays in the "Unverified" list of
   `docs/protocol.md` and is isolated behind one named constant or function.
5. Dependencies are MIT, Apache-2.0, BSD, ISC, Zlib or Unicode-3.0 only (`deny.toml`,
   enforced in CI).

## How to verify

`docs/protocol.md` is the contract. Every claim in it is labelled SOURCED (with a numbered
public source), OBSERVED (with the item or section of `docs/observations*.md` that shows
it) or UNVERIFIED, and "moche choice" marks decisions that are ours. You can re-run the
captures against a stock server with the tools in `tools/`, and compare the code with the
claims. The git history shows the order the work happened in: documents and captures
first, code after.

## AI assistance

The code, tests and documents of this repository were written by an AI model (Anthropic's
Claude, driven through Claude Code), directed and reviewed by the maintainer. The model
was instructed not to consult mosh's source code, and the process above reflects that
instruction.

## Contributors

Do not submit code that is derived from mosh's source or from any GPL implementation of
the protocol, and do not read them while working on the matching part of moche. A change
that touches the protocol needs a source, a capture or an observation to cite, as in
`docs/protocol.md`. See [CONTRIBUTING.md](../CONTRIBUTING.md).
