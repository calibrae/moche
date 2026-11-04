#!/bin/bash
# Inspect a release binary (or a tarball containing it) for a target.
# Usage: tools/release/verify.sh <tarball-or-binary> <target>
# Linux musl: ELF with no PT_INTERP / DT_NEEDED and no glibc references.
# macOS: lists linked libraries (only system dylibs allowed). Native binaries are also run (--version, --help).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/env.sh"
[ $# -eq 2 ] || { echo "usage: $0 <tarball-or-binary> <target>" >&2; exit 2; }
IN="$1"; T="$2"; check_target "$T"
W=$(mktemp -d); trap 'rm -rf "${W:?}"' EXIT
case "$IN" in
  *.tar.gz) tar -xzf "$IN" -C "$W"; BIN=$(ls "$W"/*/moche); EXTRA=1;;
  *) BIN="$IN"; EXTRA=0;;
esac
[ -f "$BIN" ] || { echo "no binary" >&2; exit 1; }
fail=0
echo "== $T: $(wc -c < "$BIN" | tr -d ' ') bytes, sha256 $(shasum -a 256 "$BIN" | cut -d' ' -f1)"
file "$BIN" || true

if [ "$EXTRA" = 1 ]; then
  d=$(dirname "$BIN")
  for f in moche LICENSE-MIT LICENSE-APACHE THIRD_PARTY.md SECURITY.md README.txt; do
    [ -f "$d/$f" ] || { echo "FAIL tarball lacks $f"; fail=1; }
  done
  [ -x "$d/moche" ] || { echo "FAIL moche not executable"; fail=1; }
fi

case "$T" in
  *-linux-musl)
    python3 -I - "$BIN" "$T" <<'PY' || fail=1
import struct, sys
data = open(sys.argv[1], "rb").read()
target = sys.argv[2]
ok = True
def bad(m):
    global ok; ok = False; print("FAIL", m)
if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
    sys.exit("FAIL not a 64-bit little-endian ELF")
e_type, e_machine = struct.unpack_from("<HH", data, 16)
want = {"x86_64": 62, "aarch64": 183}[target.split("-")[0]]
if e_machine != want: bad(f"e_machine {e_machine}, want {want}")
e_phoff, = struct.unpack_from("<Q", data, 32)
e_phentsize, e_phnum = struct.unpack_from("<HH", data, 54)
types = [struct.unpack_from("<I", data, e_phoff + i * e_phentsize)[0] for i in range(e_phnum)]
names = {1: "PT_LOAD", 2: "PT_DYNAMIC", 3: "PT_INTERP", 6: "PT_PHDR", 7: "PT_TLS"}
print("ELF type", e_type, "(2=EXEC static, 3=DYN/static-pie)", "program headers:", sorted({names.get(t, hex(t)) for t in types}))
if 3 in types: bad("has PT_INTERP (dynamic loader)")
if 2 in types: print("note: PT_DYNAMIC present (static-pie self-relocation); checking DT_NEEDED")
for i in range(e_phnum):
    o = e_phoff + i * e_phentsize
    p_type, _, p_off, _, _, p_filesz = struct.unpack_from("<IIQQQQ", data, o)
    if p_type == 2:
        for j in range(0, p_filesz, 16):
            tag, _ = struct.unpack_from("<qQ", data, p_off + j)
            if tag == 1: bad("DT_NEEDED entry (links a shared library)")
            if tag == 0: break
for needle in (b"GLIBC_", b"ld-linux", b"libc.so.6", b"libgcc_s.so"):
    if needle in data: bad(f"reference to {needle.decode()}")
print("static check:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PY
    ;;
  aarch64-apple-darwin)
    echo "-- otool -L"; otool -L "$BIN"
    bad=$(otool -L "$BIN" | tail -n +2 | awk '{print $1}' | grep -vE '^(/usr/lib/|/System/Library/)' || true)
    if [ -n "$bad" ]; then echo "FAIL non-system libraries: $bad"; fail=1; else echo "links only system libraries: PASS"; fi
    echo "-- codesign"; codesign -dv "$BIN" 2>&1 | grep -E 'Identifier|flags|Signature|TeamIdentifier' || true
    ;;
esac

native=0
case "$T-$(uname -s)-$(uname -m)" in
  aarch64-apple-darwin-Darwin-arm64) native=1;;
  x86_64-unknown-linux-musl-Linux-x86_64|aarch64-unknown-linux-musl-Linux-aarch64) native=1;;
esac
if [ "$native" = 1 ]; then
  echo "-- run"; "$BIN" --version
  "$BIN" --help | head -5
  "$BIN" --version | grep -qE "^moche [0-9]+\.[0-9]+\.[0-9]+ \([^ ,]+, $T\)$" || { echo "FAIL version string"; fail=1; }
fi
[ "$fail" = 0 ] && echo "verify $T: OK" || { echo "verify $T: FAILED"; exit 1; }
