# mosh-lab

Throwaway container with sshd and the stock distro `mosh-server` (Debian package, no
source), for testing moche against a real server. Also has `tcpdump`, `iproute2` (`tc netem`),
`vim`, `htop`, `less`, and a UTF-8 locale. User: `moche`, key-only auth.

## Run

    ./run.sh          # build, start, print ssh port; key goes to .state/ (gitignored)
    ./run.sh stop     # remove container and .state/

Needs a working `docker` CLI (colima is fine). sshd is on a random high TCP port; UDP
60000-60010 is published 1:1.

## Start a mosh-server

    ssh -i .state/id_ed25519 -p <port> moche@<docker-host> \
      'mosh-server new -s -c 256 -l LANG=en_US.UTF-8 -p 60000'

Read the line `MOSH CONNECT <udp-port> <key>`: the 22-char base64 key is the AES-128 key.
Connect the client to `<docker-host>:<udp-port>`. The server detaches from the ssh
session and lives on until idle or killed. Version: `mosh-server --version`.

## Capture

    docker exec -d moche-mosh-lab tcpdump -i any -U -w /tmp/mosh.pcap udp portrange 60000-60010
    # ... run the session ...
    docker exec moche-mosh-lab pkill tcpdump
    docker cp moche-mosh-lab:/tmp/mosh.pcap .state/mosh.pcap

## Network impairment

    docker exec -u 0 moche-mosh-lab tc qdisc add dev eth0 root netem delay 100ms loss 5%

## Colima caveat (UDP)

Verified on colima (Virtualization.Framework, no `--network-address`): the published UDP
ports are NOT reachable from the macOS host (datagrams sent to 127.0.0.1:60000 never reach
the container; TCP/sshd works). Options: run the client in the same docker network (e.g.
`docker run --rm --network container:moche-mosh-lab ...`), run `moche-cli` inside a Linux
container, use a Linux docker host, or start colima with `--network-address` and use the VM
address. Inside the container, tcpdump and mosh-server UDP work fine.
