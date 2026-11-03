#!/bin/bash
# Runs INSIDE the moche-mosh-lab container (as root, loopback only).
# Usage: capture.sh <name> <scenario> [netem args...]
# scenarios: frag | rtt | loss | misc | kill
# Output: /cap/<name>/{cap.pcap,session.json,timeline.tsv,server.out}
# netem (if any) is applied to lo INSIDE this container's own netns and removed on exit.
set -u
name=$1; scen=$2; shift 2; netem="$*"
d=/cap/$name; rm -rf "$d"; mkdir -p "$d"
TL="$d/timeline.tsv"; : > "$TL"
W=100; H=30; [ "$scen" = frag ] && { W=250; H=80; }
T="tmux -L m"

ev()   { printf '%s\t%s\n' "$(date +%s.%N)" "$1" >> "$TL"; }
key()  { ev "key $*"; $T send-keys -t m "$@"; }
typ()  { local s=$1 i c; for ((i=0;i<${#s};i++)); do c=${s:i:1}; ev "char $c"; $T send-keys -t m -l -- "$c"; sleep 0.1; done; }
line() { ev "line $1"; $T send-keys -t m -l -- "$1"; $T send-keys -t m Enter; }
cleanup() {
  [ -n "${TP:-}" ] && kill -INT "$TP" 2>/dev/null && wait "$TP" 2>/dev/null
  pkill -x mosh-server 2>/dev/null; pkill -x mosh-client 2>/dev/null; $T kill-server 2>/dev/null
  [ -n "$netem" ] && tc qdisc del dev lo root 2>/dev/null
  tc qdisc show dev lo > "$d/qdisc_after.txt" 2>&1
}
trap cleanup EXIT

[ -n "$netem" ] && { tc qdisc add dev lo root netem $netem || exit 1; }
tc qdisc show dev lo > "$d/qdisc_during.txt" 2>&1
tcpdump -i lo -U -w "$d/cap.pcap" udp 2> "$d/tcpdump.log" & TP=$!
sleep 1
SSH_CONNECTION='127.0.0.1 50000 127.0.0.1 22' mosh-server new -s -c 256 -l LANG=en_US.UTF-8 -p 60000 \
  > "$d/server.out" 2>&1 < /dev/null
read -r _ _ port key < <(grep 'MOSH CONNECT' "$d/server.out")
[ -n "${key:-}" ] || { echo "no MOSH CONNECT"; cat "$d/server.out"; exit 1; }
ver=$(mosh-server --version 2>&1 | head -1)
$T kill-server 2>/dev/null
$T -u new-session -d -s m -x $W -y $H "MOSH_KEY=$key exec mosh-client 127.0.0.1 $port"
$T set -t m status off
ev "client started"
sleep 2

case $scen in
frag)
  line 'seq 1 20000'; sleep 4
  line 'bash /cap/colors.sh'; sleep 5
  line 'htop'; sleep 3; key q; sleep 1
  line 'clear'; sleep 1 ;;
rtt)
  sleep 10; ev "idle done"
  line 'echo hello'; sleep 1
  for b in 1 2 3; do typ "abcdefghijklmnopqrst"; sleep 2; key C-u; sleep 2; done
  line 'ls /usr/bin | head -40'; sleep 2 ;;
loss)
  sleep 3
  for b in 1 2; do typ "abcdefghijklmnopqrst"; sleep 2; key C-u; sleep 2; done
  line 'ls --color=always /usr/bin | head -100'; sleep 3
  line 'seq 1 300'; sleep 3
  line 'clear'; sleep 2
  line 'ls --color=always /usr/lib | head -60'; sleep 3
  typ "abcdefghijklmnopqrst"; sleep 2; key C-u; sleep 1
  line 'echo done'; sleep 1
  ev "long idle start"; sleep 20; ev "long idle end" ;;
misc)
  line 'echo 日本語 😀 🎉 ＡＢＣ ñ'; sleep 2
  line "printf 'e\\xcc\\x81 a\\xcc\\x88\\xcc\\x82 \\u0928\\u093f x\\n'"; sleep 2
  line 'bash /cap/misc.sh'; sleep 18 ;;
kill)
  line 'echo before kill'; sleep 2
  ev "SIGKILL mosh-client"; pkill -9 -x mosh-client
  sleep 12
  pgrep -a mosh-server > "$d/server_alive_after_12s.txt" 2>&1; ev "checked server" ;;
esac

if [ "$scen" != kill ]; then
  ev "exit"; line exit
  for i in $(seq 1 60); do pgrep -x mosh-client >/dev/null || break; sleep 0.5; done
  ev "client gone or timeout"
  sleep 1
fi

{
  printf '{"name":"%s","scenario":"%s","key":"%s","port":%s,"server_version":"%s","terminal":{"cols":%s,"rows":%s},"netem_on_lo":"%s","timeline":[' \
    "$name" "$scen" "$key" "$port" "$ver" "$W" "$H" "$netem"
  sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' "$TL" | awk -F'\t' '{ if (NR>1) printf ","; printf "[%s,\"%s\"]", $1, $2 }'
  printf ']}\n'
} > "$d/session.json"
