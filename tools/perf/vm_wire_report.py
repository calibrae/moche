#!/usr/bin/env python3
"""vm_wire_report.py: tables from vm_wire.py traces (needs target/release/examples/trace_decode, see wire_analyze.py).

  vm_wire_report.py ab    DIR PREFIX     datagrams per key, RESEND share, key->datagram and key->echo-ack, per (rtt, client)
  vm_wire_report.py loss  DIR PREFIX N   last N keys of each trace are isolated probes: key -> first server DATA datagram,
                                         keys delivered (needs the rec.py .rec file), longest server DATA gap while typing
  vm_wire_report.py ack   DIR PREFIX     one key then silence: last non-empty server datagram after the key, repeats
Trace names: DIR/PREFIX_rtt<R>_<client>_r<N> (+ .key .keys .rec).
"""
import collections, glob, os, re, statistics, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import wire_analyze as wa

ORDER = {"stock": 0, "old-never": 1, "new-never": 2, "new-adaptive": 3}


def traces(d, pfx):
    for f in sorted(glob.glob(os.path.join(d, "%s_rtt*_r[0-9]*" % pfx))):
        m = re.match(r".*/%s_rtt(\d+)_(.+)_r(\d+)$" % re.escape(pfx), f)
        if m:
            yield f, int(m.group(1)), m.group(2)


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))] if xs else float("nan")


def med(s):
    m = re.search(r"med=(\d+)", s)
    return float(m.group(1)) if m else float("nan")


def ab(d, pfx):
    rows = collections.defaultdict(list)
    for f, rtt, cl in traces(d, pfx):
        r = wa.analyze(f)
        rows[(rtt, cl)].append(dict(pk=r["W_per_key"], rs=100.0 * (r["W_RESEND"] + r["W_RESEND*"]) / max(1, r["keys"]),
                                    ack=r["W_ACK"], col=med(r["collect"]), echo=med(r["key_to_echo_ack"])))
    print("%-5s %-13s %-22s %-8s %-9s %-6s %-8s %-8s" % ("RTT", "client", "per_key (runs)", "median", "resend%", "ack", "collect", "echo_ack"))
    for k in sorted(rows, key=lambda k: (k[0], ORDER.get(k[1], 9))):
        L = rows[k]
        print("%-5d %-13s %-22s %-8.3f %-9.1f %-6.0f %-8.0f %-8.0f" % (
            k[0], k[1], " ".join("%.3f" % x["pk"] for x in L), statistics.median(x["pk"] for x in L),
            statistics.median(x["rs"] for x in L), statistics.median(x["ack"] for x in L),
            statistics.median(x["col"] for x in L), statistics.median(x["echo"] for x in L)))


def loss(d, pfx, npr):
    cells = collections.defaultdict(lambda: dict(runs=0, keys=0, got=0, gap=[], probe=[], miss=0))
    for f, rtt, cl in traces(d, pfx):
        rws, keys = wa.load(f)
        C = cells[(rtt, cl)]
        C["runs"] += 1
        C["keys"] += len(keys)
        if os.path.exists(f + ".rec"):
            C["got"] += sum(1 for _ in open(f + ".rec"))
        typ, probes = keys[:-npr], keys[-npr:]
        S = [r for r in rws if r["d"] == "S" and r["db"] > 0]
        ts = [r["t"] for r in S if typ and typ[0] <= r["t"] <= typ[-1] + 1.0]
        C["gap"].append(max([b - a for a, b in zip(ts, ts[1:])] + [0]))
        for k in probes:
            nx = next((r["t"] for r in S if r["t"] >= k), None)
            if nx is None or nx - k > 8:
                C["miss"] += 1
            else:
                C["probe"].append((nx - k) * 1000)
    print("%-4s %-11s %-4s %-14s | probe key->echo ms: n p50 p90 max mean missing | max S DATA gap while typing (s)" % ("RTT", "client", "runs", "keys got/sent"))
    for k in sorted(cells, key=lambda k: (k[0], ORDER.get(k[1], 9))):
        C = cells[k]
        P = C["probe"]
        print("%-4d %-11s %-4d %-14s | %3d %5.0f %5.0f %5.0f %5.0f %d | %.2f" % (
            k[0], k[1], C["runs"], "%d/%d" % (C["got"], C["keys"]), len(P), statistics.median(P), pct(P, .9), max(P),
            statistics.mean(P), C["miss"], max(C["gap"])))


def ack(d, pfx):
    cells = collections.defaultdict(list)
    for f, rtt, cl in traces(d, pfx):
        rws, keys = wa.load(f)
        k = keys[0]
        S = [r for r in rws if r["d"] == "S" and r["t"] >= k]
        ne = [r for r in S if r["dl"] > 0]
        cnt = collections.Counter(r["new"] for r in ne)
        cells[(rtt, cl)].append(dict(last=(ne[-1]["t"] - k) if ne else -1, rep=max(cnt.values()) if cnt else 0,
                                     s10=sum(1 for r in S if r["t"] - k <= 10), late=sum(1 for r in ne if r["t"] - k > 2.0)))
    print("%-5s %-13s %-4s %-26s %-18s %-16s %-14s" % ("RTT", "client", "n", "last non-empty S (s) med/max", "traces with S>2s", "max repeat med/max", "S in 10 s med/max"))
    for (rtt, cl), L in sorted(cells.items(), key=lambda kv: (kv[0][0], ORDER.get(kv[0][1], 9))):
        ln = [x["last"] for x in L]
        print("%-5d %-13s %-4d %-26s %-18s %-16s %-14s" % (
            rtt, cl, len(L), "%.2f / %.2f" % (statistics.median(ln), max(ln)), "%d" % sum(1 for x in L if x["late"]),
            "%d / %d" % (statistics.median(x["rep"] for x in L), max(x["rep"] for x in L)),
            "%d / %d" % (statistics.median(x["s10"] for x in L), max(x["s10"] for x in L))))


if __name__ == "__main__":
    a = sys.argv
    if a[1] == "ab":
        ab(a[2], a[3])
    elif a[1] == "loss":
        loss(a[2], a[3], int(a[4]))
    elif a[1] == "ack":
        ack(a[2], a[3])
    else:
        sys.exit(__doc__)
