#!/usr/bin/env bash
# Build and start the mosh lab container. Usage: ./run.sh [start|stop]
set -euo pipefail
NAME=moche-mosh-lab
IMAGE=moche-mosh-lab
HERE=$(cd "$(dirname "$0")" && pwd)
STATE="$HERE/.state"

if [[ "${1:-start}" == "stop" ]]; then
  docker rm -f "$NAME" >/dev/null 2>&1 && echo "container removed" || echo "no container"
  rm -rf "$STATE"; echo "state removed"
  exit 0
fi

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker build -q -t "$IMAGE" "$HERE" >/dev/null

# Throwaway keypair, regenerated each run.
rm -rf "$STATE"; mkdir -p "$STATE"; chmod 700 "$STATE"
ssh-keygen -q -t ed25519 -N '' -C moche-lab -f "$STATE/id_ed25519"

# Random high TCP port for sshd; UDP range published 1:1.
PORT=$(( (RANDOM % 10000) + 40000 ))
docker create --name "$NAME" --cap-add NET_ADMIN --cap-add NET_RAW \
  -p "${PORT}:22/tcp" -p 60000-60010:60000-60010/udp "$IMAGE" >/dev/null
docker cp "$STATE/id_ed25519.pub" "$NAME:/home/moche/.ssh/authorized_keys"
docker start "$NAME" >/dev/null
# fix ownership/mode as root inside the container
docker exec -u 0 "$NAME" sh -c 'chown moche:moche /home/moche/.ssh/authorized_keys && chmod 600 /home/moche/.ssh/authorized_keys'

for _ in $(seq 1 30); do
  docker logs "$NAME" 2>&1 | grep -q "Server listening" && break
  sleep 1
done
docker logs "$NAME" 2>&1 | grep -q "Server listening" || { echo "sshd did not start"; docker logs "$NAME"; exit 1; }

echo "ssh port : $PORT"
echo "key      : $STATE/id_ed25519"
echo "connect  : ssh -i $STATE/id_ed25519 -p $PORT -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null moche@<docker-host>"
echo "stop     : $0 stop"
