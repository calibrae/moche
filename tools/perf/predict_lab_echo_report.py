#!/usr/bin/env python3
"""predict_lab_echo_report.py [DIR]: pooled key->visible latency per (rtt, gap, client) from the
`latency` runs named lat_rtt<R>_g<GAP>_<client>_s<SEED>.json (predict_lab.py latency --gap GAP)."""
import glob, json, os, re, statistics, sys, collections
d = sys.argv[1] if len(sys.argv) > 1 else "."
cells, runs = collections.defaultdict(list), collections.defaultdict(list)
for f in sorted(glob.glob(os.path.join(d, "lat_rtt*_g*_*_s*.json"))):
    m = re.match(r".*/lat_rtt(\d+)_g(\d+)_(.+)_s(\d+)\.json$", f)
    k = (int(m[1]), int(m[2]), m[3])
    v = sorted(c["lat_ms"] for c in json.load(open(f)).get("per_char", []) if c["lat_ms"] is not None)
    if v:
        cells[k].extend(v)
        runs[k].append(statistics.median(v))
pc = lambda v, q: sorted(v)[min(len(v) - 1, int(q * len(v)))]
order = lambda c: (c != "stock", c)
print("%-5s %-4s %-8s %-5s %-5s %-5s %-5s %s" % ("RTT", "gap", "client", "n", "p50", "p90", "max", "per-run p50"))
for k in sorted(cells, key=lambda k: (k[0], k[1], order(k[2]))):
    v = cells[k]
    print("%-5d %-4d %-8s %-5d %-5.0f %-5.0f %-5.0f %s" % (k[0], k[1], k[2], len(v), statistics.median(v), pc(v, .9), max(v), " ".join("%.0f" % x for x in runs[k])))
