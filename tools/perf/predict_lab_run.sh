#!/bin/bash
# Orchestrates tools/perf/predict_lab.py in the VM lab (tools/vm-lab/README.md). Runs on the lima host.
#
#   predict_lab_run.sh lat|scen|scen2|cost|wire|clean [cond-filter-regex]
#   (lat default: every condition; scen2: alternate-screen and contradiction scenarios, in the order run for docs/predict.md)
#
# Netem is applied on eth0 of BOTH VMs, so a per-side delay D gives RTT = 2*D (plus the lab's own
# base RTT of about 1 ms). Jitter J is in ms ("delay D J"), loss is per direction.
# The VMs are controlled over their vzNAT addresses, never over the shaped data path.
#
# Required environment (kept out of the repo):
#   VM_A, VM_B      commands that run a shell command on the server/client VM, e.g.
#                   VM_A="ssh -i KEY -o IdentitiesOnly=yes user@ADDR"  (command appended, stdin closed)
#   PRED_ENV        file on the client VM that exports PRED_A_IP PRED_B_IP PRED_KEY PRED_MOCHE PRED_SRV_DIR
#   OUT             local result directory (default ./pred-results)
set -u
: "${VM_A:?}" "${VM_B:?}" "${PRED_ENV:?}"
OUT="${OUT:-./pred-results}"; mkdir -p "$OUT"
vma() { $VM_A "$@" </dev/null; }
vmb() { $VM_B "$@" </dev/null; }

# name:one-way-delay-ms:jitter-ms:loss-percent   (RTT = 2 * delay)
CONDS="rtt0:0:0:0 rtt30:15:0:0 rtt60:30:0:0 rtt100:50:0:0 rtt200:100:0:0 rtt400:200:0:0
j20_60:30:6:0 j20_100:50:10:0 j20_200:100:20:0 j20_400:200:40:0 l10_100:50:0:10 l10_200:100:0:10 jl_200:100:20:10
rtt40:20:0:0 rtt50:25:0:0 rtt80:40:0:0 rtt90:45:0:0 rtt120:60:0:0"
CLIENTS="moche-adaptive moche-never moche-always stock"

netem() { # delay jitter loss
  local d=$1 j=$2 l=$3 cmd
  if [ "$d" = 0 ] && [ "$l" = 0 ]; then cmd='sudo tc qdisc del dev eth0 root 2>/dev/null; true'
  else cmd="sudo tc qdisc replace dev eth0 root netem delay ${d}ms ${j}ms loss ${l}%"; fi
  vma "$cmd"; vmb "$cmd"
}
clean() {
  vma 'sudo tc qdisc del dev eth0 root 2>/dev/null; tmux -L srv kill-server 2>/dev/null; tmux -L nest kill-server 2>/dev/null; true'
  vmb 'sudo tc qdisc del dev eth0 root 2>/dev/null; sudo pkill -INT strace; sudo pkill tcpdump; tmux -L plcl kill-server 2>/dev/null; true'
}
trap 'clean' EXIT

each_cond() { # filter fn
  local f=${1:-.}; shift
  for c in $CONDS; do
    IFS=: read -r name d j l <<<"$c"
    echo "$name" | grep -Eq "$f" || continue
    netem "$d" "$j" "$l"; sleep 1
    "$@" "$name"
  done
  netem 0 0 0
}
py() { # mode client name-suffix args...  (runs on the client VM, 20 min cap)
  local mode=$1 cl=$2 suf=$3; shift 3
  local f="res/${mode}_${cl}_${suf}.json"
  vmb "cd ~/pred && mkdir -p res && [ -s $f ] || ( . $PRED_ENV; python3 predict_lab.py $mode --client $cl --out $f --tag $suf $* )" 2>&1 | cut -c1-400
}
fetch() { vmb 'tar -C ~/pred -cf - res' | tar -C "$OUT" -xf -; }

case "${1:-}" in
  lat)  lat1() { for cl in $CLIENTS; do py latency $cl $1; done; }
        each_cond "${2:-^(rtt0|rtt30|rtt60|rtt100|rtt200|rtt400|j20_.*|l10_.*|jl_200)$}" lat1; fetch ;;
  scen) SCENS="shell_edit wrap password vim_insert_normal cat_echo slow_echo transform nested_tmux resize ctrl_c_enter paste200"
        scen1() { for n in $SCENS; do for cl in moche-adaptive stock; do py scenario $cl ${n}_$1 --name $n; done; done
                  py scenario moche-always password_$1 --name password; }
        each_cond "${2:-^(rtt100|jl_200)$}" scen1; fetch ;;
  scen2) SCENS2="vim_plain nested_plain transform2 slow_echo2"
        scen2f() { for n in $SCENS2; do for cl in moche-adaptive stock; do py scenario $cl ${n}_$1 --name $n; done; done
                   py scenario moche-always vim_plain_$1 --name vim_plain; }
        each_cond "${2:-^(rtt100|jl_200)$}" scen2f; fetch ;;
  cost) cost1() { for cl in moche-adaptive moche-never stock; do py cost $cl $1; done; }
        each_cond "${2:-^rtt100$}" cost1; fetch ;;
  wire) wire1() { for r in 1 2 3; do for cl in moche-never moche-adaptive moche-always stock; do py wire $cl $1_r$r --lines 8; done; done; }
        each_cond "${2:-^rtt100$}" wire1; fetch ;;
  clean) clean ;;
  *) echo "usage: $0 lat|scen|scen2|cost|wire|clean [cond-regex]" >&2; exit 2 ;;
esac
echo "RUN-$1-DONE"
