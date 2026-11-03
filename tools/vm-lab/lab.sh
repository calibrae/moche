#!/bin/bash
# moche VM lab helper. Run on the lima host. Only touches instances moche-a / moche-b.
export PATH=/opt/homebrew/bin:/usr/local/bin:$PATH
DIR="$(cd "$(dirname "$0")" && pwd)"
pick() { case "$1" in a|moche-a) echo moche-a;; b|moche-b) echo moche-b;; *) echo "use a or b" >&2; exit 2;; esac; }
case "$1" in
  status) limactl list | awk 'NR==1 || $1 ~ /^moche-[ab]$/' ;;
  addrs)
    for n in moche-a moche-b; do
      echo "$n lima0(vzNAT, host<->VM): $(limactl shell $n -- ip -4 -o addr show lima0 </dev/null | awk '{print $4}')"
      echo "$n eth0 (user-v2, VM<->VM): $(limactl shell $n -- ip -4 -o addr show eth0 </dev/null | awk '{print $4}')"
    done ;;
  shell) n=$(pick "$2"); shift 2; limactl shell "$n" "$@" ;;
  create) # create + start from the yamls next to this script (one at a time: key-gen race)
    for n in moche-a moche-b; do limactl start --name=$n --tty=false "$DIR/$n.yaml" </dev/null || exit 1; done ;;
  start) limactl start moche-a --tty=false </dev/null; limactl start moche-b --tty=false </dev/null ;;
  stop) limactl stop moche-a </dev/null; limactl stop moche-b </dev/null ;;
  destroy) limactl delete -f moche-a </dev/null; limactl delete -f moche-b </dev/null ;;
  *) echo "usage: $0 status|addrs|shell a|b [cmd]|create|start|stop|destroy" >&2; exit 2 ;;
esac
