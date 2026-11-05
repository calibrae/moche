#!/bin/bash
# moche x86 lab: two rootless QEMU/KVM guests. Everything lives under $LAB (default ~/moche-lab2).
#   a = Debian 13 (stock mosh-server side), b = Ubuntu 24.04 (client side)
#   NIC 1 (mgmt): qemu user-mode, outbound internet, ssh via hostfwd on 127.0.0.1 only
#   NIC 2 (link0): point-to-point L2 link between the guests (qemu -netdev stream over loopback), static
#                  192.0.2.1/24 (a) and 192.0.2.2/24 (b). Netem goes on link0 of both guests.
# usage: lab.sh status|create|start|stop|ssh a|b [cmd...]|push a|b SRC [DST]|pull a|b SRC [DST]|addrs|destroy
# Needs $LAB/img/{debian-13-generic-amd64.qcow2,noble-server-cloudimg-amd64.img} (see README.md).
set -u
LAB=${LAB:-$HOME/moche-lab2}
PORT_A=${PORT_A:-2201}; PORT_B=${PORT_B:-2202}; LINK_PORT=${LINK_PORT:-5601}
KEY=$LAB/keys/lab_key
mkdir -p "$LAB"/{img,disk,seed,run,keys,out,bin,work}

qemu_cmd() { # name ssh_port macsuffix linkmode
  local n=$1 sp=$2 mac=$3 mode=$4 link
  if [ "$mode" = server ]; then link="stream,id=l0,server=on,addr.type=inet,addr.host=127.0.0.1,addr.port=$LINK_PORT"
  else link="stream,id=l0,server=off,addr.type=inet,addr.host=127.0.0.1,addr.port=$LINK_PORT,reconnect-ms=1000"; fi
  qemu-system-x86_64 -name moche-lab2-$n -machine q35,accel=kvm -cpu host -smp 2 -m 2048 \
    -drive file=$LAB/disk/$n.qcow2,if=virtio,format=qcow2 \
    -drive file=$LAB/seed/$n.iso,if=virtio,format=raw,media=cdrom,readonly=on \
    -netdev user,id=m0,hostfwd=tcp:127.0.0.1:$sp-:22 -device virtio-net-pci,netdev=m0,mac=52:54:00:c2:00:0$mac \
    -netdev "$link" -device virtio-net-pci,netdev=l0,mac=52:54:00:c2:01:0$mac \
    -display none -serial file:$LAB/run/$n.serial -monitor none \
    -daemonize -pidfile $LAB/run/$n.pid
}

mkseed() { # name hostname linkip
  local n=$1 h=$2 ip=$3 d=$LAB/work/seed-$1
  mkdir -p $d
  local pub; pub=$(cat $KEY.pub)
  cat > $d/user-data <<EOT
#cloud-config
hostname: $h
users:
  - name: lab
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - $pub
ssh_pwauth: false
package_update: true
packages: [mosh, tmux, tcpdump, strace, iproute2, iptables, nftables, vim, htop, python3, python3-pycryptodome, ethtool, curl, git, rsync, kmod]
runcmd:
  - modprobe sch_netem || { apt-get install -y linux-modules-extra-\$(uname -r) || true; modprobe sch_netem; }
  - touch /var/lib/lab-provisioned
EOT
  cat > $d/meta-data <<EOT
instance-id: moche-lab2-$n-1
local-hostname: $h
EOT
  cat > $d/network-config <<EOT
version: 2
ethernets:
  mgmt:
    match: {macaddress: "52:54:00:c2:00:0$([ $n = a ] && echo 1 || echo 2)"}
    set-name: mgmt
    dhcp4: true
  link0:
    match: {macaddress: "52:54:00:c2:01:0$([ $n = a ] && echo 1 || echo 2)"}
    set-name: link0
    addresses: [$ip/24]
EOT
  xorriso -as mkisofs -quiet -V cidata -J -r -o $LAB/seed/$n.iso $d/user-data $d/meta-data $d/network-config 2>&1 | tail -2
}

alive() { [ -f $LAB/run/$1.pid ] && kill -0 "$(cat $LAB/run/$1.pid)" 2>/dev/null; }

SSHO="-i $KEY -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=$LAB/keys/known_hosts -o LogLevel=ERROR"
port() { [ "$1" = a ] && echo $PORT_A || echo $PORT_B; }
sshg() { local n=$1; shift; ssh $SSHO -o ConnectTimeout=8 -p "$(port $n)" lab@127.0.0.1 "$@"; }

case "${1:-}" in
  create)
    [ -f $KEY ] || ssh-keygen -q -t ed25519 -N '' -C moche-lab2 -f $KEY
    [ -f $LAB/disk/a.qcow2 ] || qemu-img create -q -f qcow2 -F qcow2 -b $LAB/img/debian-13-generic-amd64.qcow2 $LAB/disk/a.qcow2 10G
    [ -f $LAB/disk/b.qcow2 ] || qemu-img create -q -f qcow2 -F qcow2 -b $LAB/img/noble-server-cloudimg-amd64.img $LAB/disk/b.qcow2 10G
    mkseed a moche-a 192.0.2.1; mkseed b moche-b 192.0.2.2
    echo "created; run: lab.sh start" ;;
  start)
    alive a || qemu_cmd a $PORT_A 1 server
    alive b || qemu_cmd b $PORT_B 2 client
    echo started ;;
  stop)
    for n in b a; do
      if alive $n; then sshg $n 'sudo poweroff' </dev/null >/dev/null 2>&1; fi
    done
    for i in $(seq 1 30); do alive a || alive b || break; sleep 1; done
    for n in a b; do alive $n && kill "$(cat $LAB/run/$n.pid)"; done; sleep 1
    echo stopped ;;
  status)
    for n in a b; do alive $n && echo "$n: running pid $(cat $LAB/run/$n.pid)" || echo "$n: stopped"; done
    du -sh $LAB 2>/dev/null; ls -l $LAB/disk ;;
  ssh) n=${2:?a|b}; shift 2; sshg $n "$@" ;;
  push) n=${2:?a|b}; src=${3:?src}; scp -q $SSHO -P "$(port $n)" "$src" lab@127.0.0.1:"${4:-.}" ;;
  pull) n=${2:?a|b}; src=${3:?src}; scp -q $SSHO -P "$(port $n)" lab@127.0.0.1:"$src" "${4:-.}" ;;
  addrs) for n in a b; do echo "== $n"; sshg $n 'ip -4 -br a' </dev/null; done ;;
  destroy)
    "$0" stop; rm -rf $LAB/disk $LAB/seed $LAB/run $LAB/work $LAB/keys; echo "destroyed disks/seeds/keys (images and out kept)" ;;
  *) echo "usage: $0 status|create|start|stop|ssh a|b [cmd]|push a|b SRC [DST]|pull a|b SRC [DST]|addrs|destroy" >&2; exit 2 ;;
esac
