#!/usr/bin/env python3
"""wire_table.py DIR [--clients a,b,c] [--rtts 5,25,50,100]: summary tables over wire_trace.py runs
(files DIR/<client>_<oneway-ms>)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wire_analyze import analyze

d = sys.argv[1]
cl = (sys.argv[sys.argv.index("--clients") + 1] if "--clients" in sys.argv else "stock,moche-never,moche-adaptive").split(",")
ow = (sys.argv[sys.argv.index("--rtts") + 1] if "--rtts" in sys.argv else "5,25,50,100").split(",")
R = {}
for o in ow:
    for c in cl:
        p = os.path.join(d, "%s_%s" % (c, o))
        if os.path.exists(p):
            R[c, o] = analyze(p)
print("Client datagrams in the typing window (first key .. last key + 1.5 s)")
print("| RTT | client | keys | NEWKEY | RESEND | RESEND* | ACK | EMPTY | total | per key | stall bursts | srv DATA | srv ECHO | srv EMPTY |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
for o in ow:
    for c in cl:
        r = R.get((c, o))
        if not r:
            continue
        print("| %d | %s | %d | %d | %d | %d | %d | %d | %d | %.2f | %d | %d | %d | %d |" % (
            2 * int(o), c, r["keys"], r["W_NEWKEY"], r["W_RESEND"], r["W_RESEND*"], r["W_ACK"], r["W_EMPTY"],
            r["W_total"], r["W_per_key"], r["W_BURST"], r["S_DATA"], r["S_ECHO"], r["S_EMPTY"]))
print()
for k, title in (("collect", "key -> first client datagram carrying it (ms)"), ("key_to_server_data", "key -> first server DATA frame after it (ms, at client edge)"),
                 ("key_to_echo_ack", "key -> echo ack covering it (ms)"), ("dataframe_to_ack_standalone", "server DATA frame -> standalone client ACK (ms)"),
                 ("dataframe_to_ack_piggyback", "server DATA frame -> client datagram that acks it, piggybacked on data (ms)"),
                 ("echoframe_to_next_client", "server ECHO-only frame -> next client datagram (ms)"), ("resend_gap", "gap before a client resend (ms)")):
    print(title)
    for o in ow:
        for c in cl:
            r = R.get((c, o))
            if r:
                print("  RTT %3d %-15s %s" % (2 * int(o), c, r.get(k)))
