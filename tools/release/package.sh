#!/bin/bash
# Package the built binary into dist/moche-<version>-<target>.tar.gz (deterministic).
# Usage: tools/release/package.sh <target>     (run tools/release/build.sh <target> first)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/env.sh"
[ $# -eq 1 ] || { echo "usage: $0 <target>" >&2; exit 2; }
T="$1"; check_target "$T"; export_sde
BIN=$(bin_path "$T")
[ -f "$BIN" ] || { echo "$BIN missing: run tools/release/build.sh $T" >&2; exit 1; }
V=$(version); NAME="moche-$V-$T"
STAGE=$(mktemp -d); trap 'rm -rf "${STAGE:?}"' EXIT
D="$STAGE/$NAME"; mkdir -p "$D" "$DIST"

install -m 0755 "$BIN" "$D/moche"
install -m 0644 "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/SECURITY.md" "$D/"
install -m 0644 "$ROOT/docs/THIRD_PARTY.md" "$D/THIRD_PARTY.md"
cat > "$D/README.txt" <<TXT
moche $V ($T)
A mosh-compatible client. Clean reimplementation, talks to a stock mosh-server.

  moche user@host            start mosh-server over ssh, then connect over UDP
  moche --direct ...         connect to an already running mosh-server (see --help)
  Ctrl-^ .                   quit the session
  moche --help               all options

The remote host needs mosh-server installed and UDP ports 60000-61000 reachable.
Licences: LICENSE-MIT, LICENSE-APACHE; dependencies in THIRD_PARTY.md. Security: SECURITY.md.
TXT
chmod 0644 "$D/README.txt"

# Deterministic: sorted names, fixed mtime/owner, ustar, gzip -n.
FILES=$(cd "$D" && ls | LC_ALL=C sort)
OUT="$DIST/$NAME.tar.gz"
if tar --version 2>/dev/null | grep -qi gnu; then
  (cd "$STAGE" && for f in $FILES; do echo "$NAME/$f"; done | \
    tar --format=ustar --no-recursion --numeric-owner --owner=0 --group=0 \
        --mtime="@$SOURCE_DATE_EPOCH" -T - -cf -) | gzip -n -9 > "$OUT"
else
  # bsdtar: no --mtime, so stamp the staged files first.
  stamp=$(date -u -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)
  touch -t "$stamp" "$D" "$D"/*
  (cd "$STAGE" && for f in $FILES; do echo "$NAME/$f"; done | \
    tar --format=ustar --numeric-owner --uid 0 --gid 0 --no-xattrs --no-mac-metadata -cf - -T -) | gzip -n -9 > "$OUT"
fi
echo "wrote $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes)"
