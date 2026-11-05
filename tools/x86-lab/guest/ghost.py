#!/usr/bin/env python3
"""ghost.py (inside the client guest): password-prompt ghost check. Starts the client (--cmd) in a tmux pane, runs
`read -s -p "pw: " x` on the server, types an 8-letter upper-case secret 100 ms apart while polling the pane every ~20 ms,
and reports the largest number of secret characters ever visible (0 = pass). One JSON line."""
import argparse, json, subprocess, time

ap = argparse.ArgumentParser()
ap.add_argument("--cmd", required=True)
a = ap.parse_args()
S = "ghost"
SECRET = "QXZJKWVU"


def tm(*args):
    return subprocess.run(["tmux"] + list(args), capture_output=True, text=True).stdout


subprocess.run(["tmux", "kill-session", "-t", S], capture_output=True)
tm("new-session", "-d", "-s", S, "-x", "100", "-y", "30", a.cmd)
time.sleep(4)
tm("send-keys", "-t", S, 'read -s -p "pw: " x', "Enter")
time.sleep(2)
base = tm("capture-pane", "-p", "-t", S)
worst, first = 0, None
t_start = time.time()
for ch in SECRET:
    tm("send-keys", "-t", S, ch)
    end = time.time() + 0.1
    while time.time() < end:
        p = tm("capture-pane", "-p", "-t", S)
        c = sum(1 for x in SECRET if x in p)
        if c > worst:
            worst = c
            first = first or round(time.time() - t_start, 2)
        time.sleep(0.02)
end = time.time() + 1.5
while time.time() < end:
    p = tm("capture-pane", "-p", "-t", S)
    worst = max(worst, sum(1 for x in SECRET if x in p))
    time.sleep(0.03)
tm("send-keys", "-t", S, "Enter")
time.sleep(0.5)
tm("send-keys", "-t", S, "C-^", ".")
time.sleep(0.5)
subprocess.run(["tmux", "kill-session", "-t", S], capture_output=True)
print(json.dumps(dict(ghost_chars_max=worst, first_ghost_s=first, prompt_seen="pw:" in base)))
