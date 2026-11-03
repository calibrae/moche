#!/bin/bash
# Gate: fails if any static library given still holds LLVM bitcode or debug info.
# Checks every object with otool -l (no __LLVM segment, no __bitcode section, no
# __DWARF segment) and scans the raw archive for the bitcode magics.
# Usage: check-no-bitcode.sh libfoo.a [libbar.a ...]
set -uo pipefail
[ $# -ge 1 ] || { echo "usage: $0 lib.a ..." >&2; exit 2; }
fail=0
for a in "$@"; do
  [ -f "$a" ] || { echo "FAIL $a: no such file" >&2; fail=1; continue; }
  n=$(ar t "$a" | grep -vc '^__\.SYMDEF')
  size=$(stat -f %z "$a" 2>/dev/null || stat -c %s "$a")
  dump=$(otool -l "$a" 2>&1)
  if [ -z "$dump" ] || echo "$dump" | grep -q "otool.*can't open\|not an object"; then
    echo "FAIL $a: otool cannot read it" >&2; fail=1; continue
  fi
  llvm=$(echo "$dump" | grep -cE '^ +(segname __LLVM|sectname __bitcode)')
  dwarf=$(echo "$dump" | grep -cE '^ +segname __DWARF')
  magic=$(python3 -I - "$a" <<'PY'
import sys
b = open(sys.argv[1], "rb").read()
print(b.count(b"BC\xc0\xde") + b.count(b"\xde\xc0\x17\x0b"))
PY
)
  if [ "$llvm" -ne 0 ] || [ "$dwarf" -ne 0 ] || [ "$magic" -ne 0 ]; then
    echo "FAIL $a: $n objects, $size bytes; __LLVM/__bitcode entries=$llvm, __DWARF segments=$dwarf, bitcode magic hits=$magic" >&2
    fail=1
  else
    echo "PASS $a: $n objects, $size bytes, no bitcode, no debug info"
  fi
done
exit $fail
