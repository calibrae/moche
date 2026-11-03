#!/bin/bash
# Removes LLVM bitcode and debug info from a static library, member by member.
# Why: rustup's precompiled std objects (linked into every Rust staticlib) carry
# __LLVM,__bitcode/__cmdline and __DWARF; RUSTFLAGS cannot touch them, and App
# Store Connect can reject leftover bitcode. Needs llvm-objcopy (rustup component
# llvm-tools) and Xcode's strip and libtool.
# Usage: sanitize-archive.sh IN.a OUT.a
set -euo pipefail
[ $# -eq 2 ] || { echo "usage: $0 IN.a OUT.a" >&2; exit 2; }
in=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
out=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")

objcopy="${LLVM_OBJCOPY:-}"
if [ -z "$objcopy" ]; then
  sysroot=$(rustc --print sysroot)
  host=$(rustc -vV | sed -n 's/^host: //p')
  objcopy="$sysroot/lib/rustlib/$host/bin/llvm-objcopy"
fi
[ -x "$objcopy" ] || { echo "llvm-objcopy not found: run 'rustup component add llvm-tools' (or set LLVM_OBJCOPY)" >&2; exit 2; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
ar t "$in" | grep -v '^__\.SYMDEF' > list
if [ -n "$(sort list | uniq -d)" ]; then
  echo "sanitize-archive: duplicate member names in $in (ar x would lose some)" >&2
  exit 1
fi
ar x "$in"
while IFS= read -r m; do
  "$objcopy" --remove-section=__LLVM,__bitcode --remove-section=__LLVM,__cmdline \
    --remove-section=__LLVM,__asm --remove-section=__LLVM,__swift_ast "$m" "$m.new"
  mv "$m.new" "$m"
  strip -S "$m" 2>/dev/null
done < list
rm -f "$out"
# shellcheck disable=SC2046
libtool -static -no_warning_for_no_symbols -o "$out" $(cat list)
echo "sanitize-archive: $(wc -l < list | tr -d ' ') objects -> $out"
