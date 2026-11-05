#!/bin/bash
# One vm_wire.py run of the VM lab, driven from the lima host (tools/vm-lab/README.md). Netem is set by the caller.
#   vm_wire_run.sh OUTDIR PREFIX RTT_MS CLIENT RUN [vm_wire.py args...]
# Environment (kept out of the repo): VM_A / VM_B = ssh command prefixes for the server/client VM over the
# control network (stdin is closed), A_IP / B_IP = their addresses on the shaped data path,
# REC=1 runs rec.py (copy it to /tmp/rec.py on the server VM first) as the server command and keeps its log,
# SRVWRAP = optional wrapper for mosh-server (e.g. an strace script). vm_wire.py is expected in ~/moche-lab on VM_B.
set -u
: "${VM_A:?}" "${VM_B:?}" "${A_IP:?}" "${B_IP:?}"
out=$1; pfx=$2; rtt=$3; cl=$4; run=$5; shift 5
mkdir -p "$out"
tag=${pfx}_rtt${rtt}_${cl}_r$run
$VM_A 'pkill mosh-server; rm -f /tmp/rec.log; true' </dev/null >/dev/null
cmd=""; [ -n "${REC:-}" ] && cmd="-- /tmp/rec.py"
read -r PORT KEY < <($VM_A "SSH_CONNECTION='$B_IP 1 $A_IP 22' ${SRVWRAP:-} mosh-server new -s -c 256 -l LANG=C.UTF-8 $cmd 2>&1 | sed -n 's/^MOSH CONNECT //p'" </dev/null)
[ -z "$PORT" ] && { echo "no server for $tag"; exit 1; }
$VM_B "python3 ~/moche-lab/vm_wire.py --client $cl --aip $A_IP --port $PORT --key $KEY --out ~/moche-lab/res/$tag --rtt-ms $rtt $*" </dev/null | tail -1
for e in "" .keys .key .stats .cpu .tailok .idle; do $VM_B "cat ~/moche-lab/res/$tag$e 2>/dev/null" </dev/null > "$out/$tag$e"; done
[ -n "${REC:-}" ] && $VM_A 'cat /tmp/rec.log' </dev/null > "$out/$tag.rec"
$VM_B "rm -f ~/moche-lab/res/$tag*" </dev/null
$VM_A 'pkill mosh-server; true' </dev/null
