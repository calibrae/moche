#!/usr/bin/env python3
"""golden_report.py BEFORE.txt AFTER.txt: before/after report of two golden.rs outputs
(tools/perf/golden-diff.sh --report). Counts the client datagrams (`tx` lines) per phase of the
scenario and, for the 10 keys/s typing phase, the delay from each key (t multiple of 100 in
100..12100 ms) to the first datagram at or after it, plus the datagram spacing."""
import re, statistics, sys

PHASES = [("typing 10 keys/s, one-way 2 ms (0-12.1 s)", 0, 12100), ("burst + idle (12.1-16 s)", 12100, 16000),
          ("lossy 15 ms, 150 kB paste, 40 kB paste (16-40 s)", 16000, 40000),
          ("one-way 30 ms: resize, ls, 110 kB repaint, idle (40-60 s)", 40000, 60001)]


def load(p):
    tx = []
    for l in open(p):
        m = re.match(r"(\d+) tx len=(\d+) ", l)
        if m:
            tx.append((int(m.group(1)), int(m.group(2))))
    return tx


def main():
    b, a = load(sys.argv[1]), load(sys.argv[2])
    print("| phase | datagrams before | after | bytes before | after |")
    print("|---|---|---|---|---|")
    for name, lo, hi in PHASES:
        fb = [x for x in b if lo <= x[0] < hi]
        fa = [x for x in a if lo <= x[0] < hi]
        print("| %s | %d | %d | %d | %d |" % (name, len(fb), len(fa), sum(x[1] for x in fb), sum(x[1] for x in fa)))
    print("| total | %d | %d | %d | %d |" % (len(b), len(a), sum(x[1] for x in b), sum(x[1] for x in a)))
    keys = list(range(100, 12100, 100))
    for label, tx in (("before", b), ("after", a)):
        d = []
        for k in keys:
            n = next((t for t, _ in tx if t >= k), None)
            if n is not None:
                d.append(n - k)
        small = [x for x in tx if x[0] < 12100]
        print("%s: key -> first datagram: median %d ms, max %d ms; typing-phase datagrams %d (%.2f per key)" % (
            label, statistics.median(d), max(d), len(small), len(small) / len(keys)))


main()
