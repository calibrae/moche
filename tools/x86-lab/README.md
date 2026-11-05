# moche x86 lab (rootless QEMU/KVM, x86_64)

Two guests on a Linux host with `/dev/kvm`, no root, no libvirt, no host changes. Everything lives in one
directory (`$LAB`, default `~/moche-lab2`). Mirrors `tools/vm-lab/` (the Mac/lima lab).

| guest | image | role | ssh (loopback only) |
|---|---|---|---|
| `a` | Debian 13 `generic` (has kernel modules, so `sch_netem`) | stock `mosh-server` | `127.0.0.1:2201` |
| `b` | Ubuntu 24.04 cloud image (`linux-modules-extra` if netem is missing) | client side, moche under test | `127.0.0.1:2202` |

2 vCPU, 2 GiB, thin qcow2 overlays (10 GiB virtual) on the downloaded images. NIC 1 `mgmt` is qemu user-mode
(internet for apt, `hostfwd` ssh bound to 127.0.0.1). NIC 2 `link0` is a point-to-point L2 link between the guests
(`-netdev stream` over loopback TCP), static `<a-link>/24` and `<b-link>/24` (192.0.2.x). Netem goes on `link0` of
both guests (one-way D per side, RTT = 2D + ~1 ms); control always goes over `mgmt`, so a dead `link0` never strands a guest.

## Recreate from nothing
1. `mkdir -p $LAB/img` and download, checking the publisher's SHA files: `debian-13-generic-amd64.qcow2`
   (cloud.debian.org/images/cloud/trixie/latest, SHA512SUMS) and `noble-server-cloudimg-amd64.img`
   (cloud-images.ubuntu.com/noble/current, SHA256SUMS) into `$LAB/img/`.
2. Needs: `qemu-system-x86_64` with kvm, `qemu-img`, `xorriso`, `ssh-keygen`, `scp`, python3. No root.
3. `./lab.sh create` generates a throwaway ssh key in `$LAB/keys`, the overlays and the cloud-init NoCloud seed ISOs
   (user `lab`, key only, passwordless sudo, packages: mosh tmux tcpdump strace iproute2 iptables nftables vim htop
   python3 ethtool curl git rsync).
4. `./lab.sh start`, wait about 2 min for cloud-init (`./lab.sh ssh a cloud-init status --wait`), `./lab.sh addrs`.
5. Optional inter-guest key for `moche user@host` from b to a: generate in b, append to a's `authorized_keys`.
6. Binaries: put `moche` into `~/moche-lab/bin/<variant>/moche` in b (`./lab.sh push b SRC DST`).
7. `./lab.sh stop` keeps images and overlays; `./lab.sh destroy` removes overlays, seeds and keys only.

Commands: `status | create | start | stop | ssh a|b [cmd] | push a|b SRC [DST] | pull a|b SRC [DST] | addrs | destroy`.
Drivers: `tools/x86-lab/guest/{vmw,lat,ghost}.py` (copied to `~/moche-lab/` in b) and `orch/bat.py` (run on the host).
Perf: `tools/perf/measure.sh --server-ssh lab@<a-link> --server-host <a-link>` run inside b.
