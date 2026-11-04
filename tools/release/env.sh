# Shared by tools/release/*.sh (sourced). POSIX-ish bash.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLS="$ROOT/target/tools"
DIST="${DIST:-$ROOT/dist}"
TARGETS="x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin"

check_target() {
  case " $TARGETS " in *" $1 "*) ;; *) echo "unsupported target '$1' (one of: $TARGETS)" >&2; exit 2;; esac
}

version() {
  sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' "$ROOT/Cargo.toml" | head -1
}

# Commit date as SOURCE_DATE_EPOCH unless the caller set it.
export_sde() {
  if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH=$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || date +%s)
  fi
  export SOURCE_DATE_EPOCH
}

bin_path() { echo "$ROOT/target/$1/release/moche"; }
