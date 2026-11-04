#!/usr/bin/env bash
# moche resource measurement: footprint, CPU, RSS, wakeups vs the stock mosh-client.
# Needs: bash, python3 (stdlib), tmux, a stock mosh-server (local, over ssh, or already running).
# Platform specifics live in perf.py behind platform checks; this script only parses
# arguments, locates binaries and hands over. See docs/PERF.md.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
label="" moche="" stock="auto" clients="moche,stock" scenarios="" reps=3 idle=60 typing=30
out="$here/out" mosh_server="mosh-server" server_host="" server_port="" key_env=""
server_ssh="" lang="" build=0
ssh_opts=()

usage() {
  cat <<'EOF'
usage: measure.sh --label NAME [options]

  --label NAME          host label used in the report (e.g. mac-arm64, linux-aarch64-vm)
  --moche PATH          moche binary (default: ../../target/perf/release/moche, else moche on PATH)
  --stock PATH|none     stock mosh-client (default: PATH lookup; absent = baseline skipped)
  --build               build moche first: CARGO_TARGET_DIR=target/perf cargo build --release -p moche-cli --locked
  --scenarios LIST      comma list of: startup,idle,typing,paste,flood,colors,steady (default all)
  --reps N              repetitions per scenario/client (default 3; medians are reported)
  --idle-secs N         idle window length (default 60)
  --typing-secs N       typing window length, ~10 chars/s (default 30)
  --clients LIST        moche,stock (default both)
  --out DIR             output root (default tools/perf/out); results go in DIR/<label>-<timestamp>/

server (default: --local-server, a fresh stock mosh-server per run on loopback)
  --local-server        start mosh-server locally (default)
  --mosh-server PATH    mosh-server binary for local/ssh modes (default: mosh-server)
  --server-ssh TARGET   start a fresh mosh-server per run over ssh on TARGET (needs --server-host)
  --ssh-opt OPT         extra ssh argument, repeatable (e.g. --ssh-opt -p --ssh-opt 2222)
  --server-host HOST    address the clients send UDP to (ssh and static modes)
  --server-port PORT    static mode: an already running server; key read from --key-env
  --key-env VAR         env var holding the 22-char session key (default MOSH_KEY)
  --lang LOCALE         UTF-8 locale for server and clients (default: en_US.UTF-8, else C.UTF-8)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --label) label="$2"; shift 2 ;;
    --moche) moche="$2"; shift 2 ;;
    --stock) stock="$2"; shift 2 ;;
    --build) build=1; shift ;;
    --scenarios) scenarios="$2"; shift 2 ;;
    --reps) reps="$2"; shift 2 ;;
    --idle-secs) idle="$2"; shift 2 ;;
    --typing-secs) typing="$2"; shift 2 ;;
    --clients) clients="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --local-server) shift ;;
    --mosh-server) mosh_server="$2"; shift 2 ;;
    --server-ssh) server_ssh="$2"; shift 2 ;;
    --ssh-opt) ssh_opts+=("$2"); shift 2 ;;
    --server-host) server_host="$2"; shift 2 ;;
    --server-port) server_port="$2"; shift 2 ;;
    --key-env) key_env="$2"; shift 2 ;;
    --lang) lang="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[ -n "$label" ] || { echo "--label is required" >&2; usage >&2; exit 2; }
case "$(uname -s)" in
  Darwin|Linux) ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 2 ;;
esac
command -v tmux >/dev/null || { echo "tmux not found" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 1; }

repo="$(cd "$here/../.." && pwd)"
if [ "$build" = 1 ]; then
  (cd "$repo" && CARGO_TARGET_DIR=target/perf cargo build --release -p moche-cli --locked)
fi
if [ -z "$moche" ]; then
  if [ -x "$repo/target/perf/release/moche" ]; then moche="$repo/target/perf/release/moche"
  elif command -v moche >/dev/null; then moche="$(command -v moche)"
  else echo "no moche binary: pass --moche PATH or --build" >&2; exit 1; fi
fi
[ -x "$moche" ] || { echo "moche not executable: $moche" >&2; exit 1; }

case "$stock" in
  auto) stock="$(command -v mosh-client || true)" ;;
  none) stock="" ;;
esac

dir="$out/$label-$(date -u +%Y%m%dT%H%M%SZ)"
args=(--label "$label" --moche "$moche" --stock "$stock" --clients "$clients"
      --reps "$reps" --idle-secs "$idle" --typing-secs "$typing" --out "$dir"
      --mosh-server "$mosh_server")
[ -z "$scenarios" ] || args+=(--scenarios "$scenarios")
[ -z "$server_host" ] || args+=(--server-host "$server_host")
[ -z "$server_port" ] || args+=(--server-port "$server_port")
[ -z "$key_env" ] || args+=(--key-env "$key_env")
[ -z "$server_ssh" ] || args+=(--server-ssh "$server_ssh")
[ -z "$lang" ] || args+=(--lang "$lang")
for o in ${ssh_opts[@]+"${ssh_opts[@]}"}; do args+=("--ssh-opt=$o"); done

mkdir -p "$out"
echo "results: $dir"
python3 -I "$here/perf.py" "${args[@]}"
python3 -I "$here/report.py" "$dir" -o "$dir/report.md"
echo "report: $dir/report.md"
