#!/usr/bin/env python3
"""Classifies the failing runs of a predict_e2e campaign.

usage: predict_e2e_classify.py CAMPAIGN_DIR LABEL
Reads LABEL.log (rc != 0 lines), stalls.log (tools/perf/stall_ticker.py) and each failing
run's artefacts (info.json, events.txt, moche.trace). Classes:
  a  startup stall / harness timing: a host stall (ticker gap, or a sample gap above 100 ms)
     fell inside the run's typing window, or a datagram sat more than 100 ms between the
     client's sendto and the relay's recv (info.json `loopback_stall_ms`, since round 11; older
     artefacts: the relay/send delay computed from moche.trace and relay.log)
  b  real echo by the server (typed before echo went off)
  c  GHOST (overlay cell painted with no server echo): needs a code reading
  ?  none of the above: needs a manual look at the artefacts
"""
import glob, json, os, re, sys

d, label = sys.argv[1], sys.argv[2]
stalls = []
try:
    for ln in open(os.path.join(d, "stalls.log")):
        if not ln.startswith("#"):
            t, g = ln.split()
            stalls.append((float(t), int(g)))
except OSError:
    pass
def relay_delays(a):
    """For each key in moche.trace, the time until the relay saw the next client datagram
    (the SSP send, or a relay that stopped serving). Returns the worst (ms, wall time)."""
    keys, cs = [], []
    try:
        for ln in open(os.path.join(a, "moche.trace")):
            p = ln.split(" ", 2)
            if p[1] == "on_input":
                keys.append(float(p[0]))
        for ln in open(os.path.join(a, "relay.log")):
            t, dr, _ = ln.split()
            if dr == "C":
                cs.append(float(t))
    except (OSError, IndexError, ValueError):
        return 0, 0
    worst = (0, 0)
    for k in keys:
        nxt = next((c for c in cs if c >= k - 0.003), None)
        if nxt is not None and (nxt - k) * 1000 > worst[0]:
            worst = (round((nxt - k) * 1000), k)
    return worst


for ln in open(os.path.join(d, label + ".log")):
    m = re.match(r"run=(\d+) pw=(\S+) rc=(\d+) secs=\S+ t0=(\S+) t1=(\S+) (.*)", ln)
    if not m or m.group(3) == "0":
        continue
    n, pw, rc, t0, t1, msg = m.groups()
    t0, t1 = float(t0), float(t1)
    inwin = [(t, g) for t, g in stalls if t0 <= t <= t1]
    dirs = sorted(glob.glob(os.path.join(d, label, "run-%s-*" % n)))
    cls, why = "?", ""
    for a in dirs:
        try:
            info = json.load(open(os.path.join(a, "info.json")))
        except OSError:
            continue
        gap = info.get("max_sample_gap_ms", 0)
        if info.get("class") == "GHOST" or rc == "3":
            cls = "c"
        elif info.get("class") == "real_echo" or info.get("class") == "REAL_ECHO_AFTER_MARKER":
            cls = "b"
        elif inwin or gap > 100 or info.get("stalls"):
            cls = "a"
        elif info.get("loopback_stall_ms", 0) > 100:
            cls = "a"
            why += " [datagram sat %d ms between client sendto and relay recv]" % info["loopback_stall_ms"]
        elif relay_delays(a)[0] > 150:
            cls = "a"
            why += " [relay/send delay %d ms at %.3f]" % relay_delays(a)
        why += " [%s gap=%sms stalls=%s %s]" % (os.path.basename(a), gap, info.get("stalls"), info.get("failure", info.get("class", "")))
    print("run %s pw=%s rc=%s class=%s ticker_stalls=%s%s\n    %s" % (n, pw, rc, cls, inwin, why, msg[:300]))
