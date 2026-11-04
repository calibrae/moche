#!/usr/bin/env python3
"""wire_idle.py TRACE...: idle-probe analysis of runs made with `wire_trace.py --idle-probe N --secs 0`.

For each probe keystroke (typed after 2.5 s of silence) prints the client and server datagrams in
the next 1.2 s, then summarises: key -> first client datagram (collection delay), first client
datagram -> resends, first client datagram -> the server frame that acks it, server frame ->
standalone client ack. Times in ms. Uses wire_analyze.load. Probes whose first datagram is more than 30 ms late
(a host stall) are skipped, and datagrams within 3 ms of the previous one are dropped."""
import os, statistics, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import wire_analyze as W


def q(xs):
    if not xs:
        return "n=0"
    s = sorted(xs)
    return "n=%d min=%.0f med=%.0f max=%.0f" % (len(s), s[0], statistics.median(s), s[-1])


def main():
    verbose = "-v" in sys.argv
    for tr in [a for a in sys.argv[1:] if a != "-v"]:
        rows, keys = W.load(tr)
        keys = keys[1:] if False else keys
        coll, resend, ackin, sack, nres, ncl = [], [], [], [], [], []
        for i, k in enumerate(keys):
            win = [r for r in rows if k <= r["t"] < k + 1.2]
            cs = [r for r in win if r["d"] == "C"]
            if not cs or cs[0]["t"] - k > 0.030:
                continue  # none, or a host stall (see wire_analyze BURST): not a protocol decision
            cs = [c for j, c in enumerate(cs) if j == 0 or c["t"] - cs[j - 1]["t"] >= 0.003]
            first = cs[0]
            coll.append((first["t"] - k) * 1000)
            same = [r for r in cs[1:] if r["db"] > 0]
            resend += [(r["t"] - first["t"]) * 1000 for r in same]
            nres.append(len(same))
            ncl.append(len(cs))
            ack = next((r for r in win if r["d"] == "S" and r["ack"] >= first["new"]), None)
            if ack:
                ackin.append((ack["t"] - first["t"]) * 1000)
                nxt = next((r for r in cs if r["t"] > ack["t"]), None)
                if nxt and nxt["db"] == 0:
                    sack.append((nxt["t"] - ack["t"]) * 1000)
            if verbose:
                print("-- key %d" % i)
                for r in win:
                    if r["d"] == "C":
                        print("  %6.1f C old%d new%d ack%d db%d" % ((r["t"] - k) * 1000, r["old"], r["new"], r["ack"], r["db"]))
                    else:
                        print("  %6.1f      S new%d ack%d db%d echo%d dl%d" % ((r["t"] - k) * 1000, r["new"], r["ack"], r["db"], r["echo"], r["dl"]))
        print(os.path.basename(tr))
        print("   key -> first client datagram: ", q(coll))
        print("   client datagrams per probe key: mean %.2f; resends per key mean %.2f" % (statistics.mean(ncl) if ncl else 0, statistics.mean(nres) if nres else 0))
        print("   first datagram -> each resend:", q(resend))
        print("   first datagram -> server frame acking it:", q(ackin))
        print("   acking server frame -> next client datagram if empty (standalone ack):", q(sack))


main()
