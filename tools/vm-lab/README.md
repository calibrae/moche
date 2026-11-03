# moche VM lab

Two small lima VMs on the Mac that hosts the lab, to test moche against a stock
`mosh-server` with real UDP, loss and roaming. Nothing from the host is mounted into them
and they hold no credentials.

| VM | image | role |
|---|---|---|
| `moche-a` | Debian 13 (trixie) arm64 cloud image | mosh-server side |
| `moche-b` | Ubuntu 24.04 LTS arm64 cloud image | client side, Rust toolchain, builds moche |

Both: `vmType: vz`, 2 vCPU, 2 GiB RAM, 12 GiB disk, `mounts: []`, containerd off. Packages
(mosh 1.4.0, tcpdump, iproute2, iptables, nftables, vim, htop, tmux, curl, git; plus
build-essential in b) are installed by a cloud-init style `provision` script in the yaml.
The default lima user has passwordless sudo.

## Networks (important)

Each VM has two interfaces:

- `lima0` = **vzNAT** (the vzNAT subnet): host <-> VM works (ssh, UDP), but the two VMs
  **cannot reach each other** on it (ARP stays INCOMPLETE: Apple's vzNAT isolates guests).
- `eth0` = **lima user-v2** (the user-v2 subnet, gvisor-tap-vsock, rootless): VM <-> VM
  works, the host cannot address these IPs directly (use `limactl shell` / the ssh port
  forward). It is also the VMs' default route to the internet.

So: moche-b -> moche-a tests use the `eth0` (user-v2) addresses; host -> VM tests use `lima0`.
Switching which interface the client sends from (or `ip link set ... down`) is also a handy
roaming test.

## Create

```sh
# on the lima host, yamls in ~/moche-vmlab (do NOT run both starts at once:
# the first start generates ~/.lima/_config/user and the second races on it)
limactl start --name=moche-a --tty=false moche-a.yaml
limactl start --name=moche-b --tty=false moche-b.yaml
```

or `./lab.sh create`. Debian takes about a minute, Ubuntu similar (images are downloaded and
cached by lima). The yamls only touch instances `moche-a` and `moche-b`.

Do not use `limactl` against anything else on the host: the colima VM there is shared with CI.
(colima keeps its own LIMA_HOME, so `limactl list` normally shows only moche-a/b.)

## Addresses

```sh
./lab.sh addrs
# or: limactl shell moche-a -- ip -4 -br a
```

Below, `<a-vz>`/`<b-vz>` are the `lima0` (vzNAT) addresses and `<a-u2>`/`<b-u2>` the `eth0`
(user-v2) ones.

host -> VM ssh: `ssh -i ~/.lima/_config/user -o IdentitiesOnly=yes <user>@<a-vz>`.

## Smoke test with the stock mosh client (b -> a)

```sh
./lab.sh shell a
  SSH_CONNECTION='<b-u2> 1 <a-u2> 22' mosh-server new -s -c 256 -l LANG=C.UTF-8
  # prints: MOSH CONNECT <port> <key>   (-s binds to the server address in SSH_CONNECTION)
./lab.sh shell b
  tmux new -d -s t -x 100 -y 30 "MOSH_KEY=<key> mosh-client <a-u2> <port>"
  tmux send-keys -t t 'echo vm-ok' Enter; tmux capture-pane -p -t t
```

(`limactl shell` reads stdin: when scripting, redirect `</dev/null` or it eats your script.)
Capture on the server: `sudo tcpdump -ni eth0 udp port <port>`.

## Run moche from moche-b against moche-a

Source gets into b without any credentials: clone on the host, then tar over limactl.

```sh
git clone <moche remote> ~/moche-vmlab/moche
tar -C ~/moche-vmlab/moche -cf - . | limactl shell moche-b -- bash -c 'mkdir -p ~/moche && tar -C ~/moche -xf -'
limactl shell moche-b -- bash -c '. ~/.cargo/env; cd ~/moche && cargo build --release -p moche-cli && cargo test -p moche-core'
```

(rustup minimal profile, stable, was installed in b with
`curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal`.)
Then start a mosh-server in a as above and run the binary in b, in tmux so it has a tty:

```sh
tmux new -d -s m "~/moche/target/release/moche ..."   # see moche --help for the connect args
tmux capture-pane -p -t m
```

A clean release build of `moche-cli` takes a few seconds here; moche-core tests pass.

## Loss, delay, reorder: tc netem (inside a VM)

On the server VM (a) the egress qdisc shapes server -> client traffic; on b it shapes
client -> server. Use the user-v2 interface `eth0`:

```sh
sudo tc qdisc add dev eth0 root netem loss 10% delay 80ms 20ms      # add
sudo tc qdisc change dev eth0 root netem loss 30% delay 200ms       # tweak
sudo tc qdisc del dev eth0 root                                     # remove
```

`tc` is in iproute2; if `netem` is missing on the Ubuntu kernel install
`linux-modules-extra-$(uname -r)`.

## Roaming: source NAT / address change (inside a VM)

Mosh survives the client's source address or port changing. On the client VM (b):

```sh
# rewrite the source of outgoing mosh packets to another source port (new NAT mapping)
sudo iptables -t nat -A POSTROUTING -p udp -d <a-u2> --dport <port> -j SNAT --to-source <b-u2>:50000-50100
# swap the mapping mid-session
sudo iptables -t nat -F POSTROUTING
sudo iptables -t nat -A POSTROUTING -p udp -d <a-u2> --dport <port> -j SNAT --to-source <b-u2>:51000-51100
# or add a second address and move the source
sudo ip addr add <b-u2-alt>/24 dev eth0
sudo iptables -t nat -A POSTROUTING -p udp -d <a-u2> --dport <port> -j SNAT --to-source <b-u2-alt>
```

Verify on a with `tcpdump -ni eth0 udp port <port>` that the peer source changed and the
session kept going. Blackhole test: `sudo iptables -A OUTPUT -p udp --dport <port> -j DROP`
for 30 s, then delete the rule.

## Lifecycle (only moche-a / moche-b)

```sh
./lab.sh status     # list
./lab.sh stop       # limactl stop moche-a moche-b
./lab.sh start
./lab.sh shell a [cmd]
./lab.sh destroy    # limactl delete -f moche-a moche-b
```

`lab.sh` refuses any other instance name.
