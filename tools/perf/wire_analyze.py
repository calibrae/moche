#!/usr/bin/env python3
"""wire_analyze.py: classify and time the datagrams of a wire_trace.py run.

  wire_analyze.py TRACE [--json]       (needs target/release/examples/trace_decode, built from
                                        crates/moche-core/examples/trace_decode.rs)

Client datagram classes (every datagram the client sent, lossless relay):
  NEWKEY   fresh new_num whose content is longer than anything sent before (new keystroke data)
  RESEND   same new_num as an earlier datagram (resend of an unacked state)
  RESEND*  fresh new_num, content unchanged (resend under a new number)
  ACK      empty diff and its ack_num is new (a standalone delayed ack)
  EMPTY    empty diff, ack_num not new (heartbeat or a repeat)
  BURST    a RESEND within 3 ms of the previous client datagram: not a protocol decision but a
           host stall (a client stuck for 0.5-2 s on this Mac wakes up and re-sends in a burst,
           observed with both clients); reported apart and left out of the per-key count
Server datagram classes: DATA (host bytes), ECHO (echo-ack only, no host bytes), EMPTY (no diff).
"""
import os, statistics, subprocess, sys, json

HERE = os.path.dirname(os.path.abspath(__file__))
DEC = os.path.join(HERE, "../../target/release/examples/trace_decode")


def load(trace):
    key = open(trace + ".key").read().strip()
    out = subprocess.run([DEC, trace, key], capture_output=True, text=True, check=True).stdout
    rows = []
    for l in out.splitlines():
        f = l.split("\t")
        if len(f) < 12 or f[3] in ("UNDECRYPTABLE", "BADINNER", "BADINSTR"):
            continue
        if f[11] != "1":
            continue
        rows.append(dict(t=float(f[0]), d=f[1], seq=int(f[2]), old=int(f[3]), new=int(f[4]), ack=int(f[5]),
                         thr=int(f[6]), dl=int(f[7]), db=int(f[8]), echo=int(f[9])))
    keys = [float(x) for x in open(trace + ".keys").read().split()] if os.path.exists(trace + ".keys") else []
    return rows, keys


def pct(xs, q):
    if not xs:
        return float("nan")
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))]


def dist(xs):
    if not xs:
        return "n=0"
    return "n=%d p10=%.0f med=%.0f p90=%.0f max=%.0f" % (len(xs), pct(xs, .1) * 1000, statistics.median(xs) * 1000, pct(xs, .9) * 1000, max(xs) * 1000)


def analyze(trace):
    rows, keys = load(trace)
    t0 = rows[0]["t"]
    L = {0: 0}            # client state -> absolute input length (data bytes)
    seen_new, maxL, max_ack = set(), 0, 0
    cls = []
    prev_t = None
    for r in rows:
        if r["d"] != "C":
            continue
        base = L.get(r["old"])
        if base is not None and r["new"] not in L:
            L[r["new"]] = base + r["db"]
        ln = L.get(r["new"], maxL)
        if r["db"] > 0:
            if r["new"] in seen_new:
                c = "RESEND"
            elif ln > maxL:
                c = "NEWKEY"
            else:
                c = "RESEND*"
        else:
            c = "ACK" if r["ack"] > max_ack else "EMPTY"
        if c in ("RESEND", "RESEND*") and prev_t is not None and r["t"] - prev_t < 0.003:
            c = "BURST"
        prev_t = r["t"]
        seen_new.add(r["new"])
        maxL = max(maxL, ln)
        max_ack = max(max_ack, r["ack"])
        r["c"], r["L"] = c, ln
        cls.append(c)
    scls = []
    for r in rows:
        if r["d"] == "S":
            r["c"] = "DATA" if r["db"] > 0 else ("ECHO" if r["dl"] > 0 else "EMPTY")
            scls.append(r["c"])
    res = dict(trace=trace, keys=len(keys))
    for c in ("NEWKEY", "RESEND", "RESEND*", "ACK", "EMPTY", "BURST"):
        res["C_" + c] = cls.count(c)
    res["C_total"] = len(cls)
    for c in ("DATA", "ECHO", "EMPTY"):
        res["S_" + c] = scls.count(c)
    res["S_total"] = len(scls)
    # restrict timing to the typing window
    C = [r for r in rows if r["d"] == "C"]
    S = [r for r in rows if r["d"] == "S"]
    if keys:
        w0, w1 = keys[0] - 0.001, keys[-1] + 1.5
        Cw = [r for r in C if w0 <= r["t"] <= w1]
        Sw = [r for r in S if w0 <= r["t"] <= w1]
        for c in ("NEWKEY", "RESEND", "RESEND*", "ACK", "EMPTY", "BURST"):
            res["W_" + c] = sum(1 for r in Cw if r["c"] == c)
        res["W_total"] = len(Cw) - res["W_BURST"]
        res["W_per_key"] = round(res["W_total"] / len(keys), 3)
        # keystroke -> first client datagram carrying it
        coll = []
        for i, k in enumerate(keys):
            for r in C:
                if r["t"] >= k and r["L"] >= i + 1:
                    coll.append(r["t"] - k)
                    break
        res["collect"] = dist(coll)
        res["collect_p25"] = round(pct(coll, .25) * 1000)
        res["collect_vals"] = coll
        # keystroke -> server data frame (first S DATA after first carry) , echo ack
        data_lat, echo_lat, ack_lat = [], [], []
        for i, k in enumerate(keys):
            car = next((r for r in C if r["t"] >= k and r["L"] >= i + 1), None)
            if not car:
                continue
            sd = next((r for r in S if r["t"] >= car["t"] and r["db"] > 0), None)
            if sd:
                data_lat.append(sd["t"] - k)
            se = next((r for r in S if r["t"] >= car["t"] and r["echo"] >= car["new"]), None)
            if se:
                echo_lat.append(se["t"] - k)
        res["key_to_server_data"] = dist(data_lat)
        res["key_to_echo_ack"] = dist(echo_lat)
        # server DATA frame -> first client datagram acking it (any class) and what it was
        ackc = {"ACK": [], "piggy": []}
        for s in Sw:
            if s["db"] == 0:
                continue
            nxt = next((r for r in C if r["t"] >= s["t"] and r["ack"] >= s["new"]), None)
            if nxt:
                ackc["ACK" if nxt["c"] == "ACK" else "piggy"].append(nxt["t"] - s["t"])
        res["dataframe_to_ack_standalone"] = dist(ackc["ACK"])
        res["dataframe_to_ack_piggyback"] = dist(ackc["piggy"])
        # server ECHO-only frame -> next client datagram
        e2n = []
        for s in Sw:
            if s["c"] != "ECHO":
                continue
            nxt = next((r for r in C if r["t"] >= s["t"]), None)
            if nxt:
                e2n.append(nxt["t"] - s["t"])
        res["echoframe_to_next_client"] = dist(e2n)
        gaps = [b["t"] - a["t"] for a, b in zip(Cw, Cw[1:]) if b["c"] in ("RESEND", "RESEND*")]  # BURST excluded
        res["resend_gap"] = dist(gaps)
    return res


if __name__ == "__main__":
    r = analyze(sys.argv[1])
    if "--json" in sys.argv:
        r.pop("collect_vals", None)
        print(json.dumps(r))
    else:
        r.pop("collect_vals", None)
        for k, v in r.items():
            print("%-30s %s" % (k, v))
