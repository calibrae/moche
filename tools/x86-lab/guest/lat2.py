#!/usr/bin/env python3
"""lat2.py (inside the client guest): echo latency key -> visible, measured on a real terminal emulator (tmux) instead of by
matching bytes, so a client that repaints earlier text cannot produce false early matches (an earlier byte-matching version,
lat.py, was found biased for moche at 10 keys/s and was dropped).
  lat2.py --client stock|<variant>-never --aip IP --port P --key K [--n 100] [--gap 0.1] [--seed 1]
Runs the client in a tmux pane, types N random lowercase letters GAP s apart (+-30 %) at the shell prompt, polls capture-pane
(about 5 ms per poll) and counts the lowercase letters on the pane; key k is "visible" at the first poll where the count is
>= baseline + k (a count only grows while typing, so several keys in flight are handled). Resolution about 5-10 ms, same bias
for every client. One JSON line."""
import argparse, json, os, random, re, subprocess, time

BIN_DIR = os.environ.get("MOCHE_BIN_DIR", os.path.expanduser("~/moche-lab/bin"))
ap = argparse.ArgumentParser()
ap.add_argument("--client", required=True)
ap.add_argument("--aip", required=True)
ap.add_argument("--port", required=True)
ap.add_argument("--key", required=True)
ap.add_argument("--n", type=int, default=100)
ap.add_argument("--gap", type=float, default=0.1)
ap.add_argument("--seed", type=int, default=1)
a = ap.parse_args()
rng = random.Random(a.seed)
S = "lat2"
if a.client == "stock":
    cmd = "env MOSH_KEY=%s MOSH_PREDICTION_DISPLAY=never mosh-client %s %s" % (a.key, a.aip, a.port)
else:
    v, mode = a.client.split("-", 1)
    cmd = "env MOSH_KEY=%s %s/%s/moche %s --direct %s %s 2>/dev/null" % (a.key, BIN_DIR, v, "" if mode == "none" else "--predict=" + mode, a.aip, a.port)


def tm(*args):
    return subprocess.run(["tmux"] + list(args), capture_output=True, text=True).stdout


def count():
    return len(re.findall(r"[a-z]", tm("capture-pane", "-p", "-t", S)))


subprocess.run(["tmux", "kill-session", "-t", S], capture_output=True)
tm("new-session", "-d", "-s", S, "-x", "100", "-y", "30", cmd)
time.sleep(6)
base = count()
sent, vis = [], {}
letters = "abcdefghijklmnopqrstuvwxyz"


def poll_until(t_end):
    while True:
        c = count() - base
        now = time.monotonic()
        for k in range(1, len(sent) + 1):
            if k not in vis and c >= k:
                vis[k] = now
        if now >= t_end:
            return


for i in range(a.n):
    sent.append(time.monotonic())
    tm("send-keys", "-t", S, "-l", rng.choice(letters))
    poll_until(time.monotonic() + a.gap * rng.uniform(0.7, 1.3))
poll_until(time.monotonic() + 3.0)
tm("send-keys", "-t", S, "C-^", ".")
time.sleep(0.5)
subprocess.run(["tmux", "kill-session", "-t", S], capture_output=True)
lat = sorted((vis[k] - sent[k - 1]) * 1000 for k in vis)
q = lambda p: round(lat[min(len(lat) - 1, int(p * len(lat)))], 1) if lat else None
print(json.dumps(dict(client=a.client, typed=len(sent), matched=len(lat), min=q(0), p50=q(0.5), p90=q(0.9), max=q(1.0))))
