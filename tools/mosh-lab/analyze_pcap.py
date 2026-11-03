#!/usr/bin/env python3
"""Decode a mosh capture (cap.pcap + session.json) and print one line per datagram.

Dev tool, independent of the Rust example (it exists because AES-OCB3 is the only
thing needed and `cryptography` gives it). Usage:
    analyze_pcap.py <capture-dir> [--full]
Needs: pip install cryptography. Layout per docs/observations.md.
"""
import json, struct, sys, zlib, collections
from base64 import b64decode
from cryptography.hazmat.primitives.ciphers.aead import AESOCB3

PORT = 60000


def pcap(buf):
    le = buf[:4] == b"\xd4\xc3\xb2\xa1"
    e = "<" if le else ">"
    link = struct.unpack(e + "I", buf[20:24])[0]
    off, out = 24, []
    while off + 16 <= len(buf):
        s, us, incl, _ = struct.unpack(e + "IIII", buf[off:off + 16])
        f = buf[off + 16:off + 16 + incl]
        off += 16 + incl
        l2 = {1: 14, 0: 4, 108: 4, 113: 16}[link]
        ip = f[l2:]
        ihl = (ip[0] & 15) * 4
        udp = ip[ihl:]
        sp, dp, ln = struct.unpack(">HHH", udp[:6])
        out.append((s + us / 1e6, dp == PORT, udp[8:ln]))
    return out


def varint(b, p):
    v = s = 0
    while True:
        x = b[p]; p += 1
        v |= (x & 0x7F) << s
        if not x & 0x80:
            return v, p
        s += 7


def fields(b):
    p, out = 0, []
    while p < len(b):
        t, p = varint(b, p)
        f, w = t >> 3, t & 7
        if w == 0:
            v, p = varint(b, p); out.append((f, v))
        elif w == 2:
            l, p = varint(b, p); out.append((f, b[p:p + l])); p += l
        else:
            break
    return out


def esc(b):
    return "".join({27: "\\e", 13: "\\r", 10: "\\n"}.get(c, chr(c) if 32 <= c < 127 else "\\x%02x" % c) for c in b)


def instr(v):
    d = dict(ver=0, old=0, new=0, ack=0, thr=0, chaff=0, diff=b"")
    for f, x in fields(v):
        k = {1: "ver", 2: "old", 3: "new", 4: "ack", 5: "thr", 6: "diff"}.get(f)
        if k: d[k] = x
        if f == 7: d["chaff"] = len(x)
    els = []
    for f, e in fields(d["diff"]):
        for g, x in fields(e):
            if g == 2:
                k = dict(fields(x)).get(4, b"")
                els.append("bytes[%d]'%s'" % (len(k), esc(k[:48])))
            elif g == 3:
                m = dict(fields(x)); els.append("resize(%sx%s)" % (m.get(5), m.get(6)))
            elif g == 7:
                els.append("echo_ack(%s)" % dict(fields(x)).get(8))
            else:
                els.append("?e%d" % g)
    d["els"] = els
    d["difflen"] = len(d["diff"])
    d["raw"] = v
    return d


def main():
    cd = sys.argv[1]
    full = "--full" in sys.argv
    sess = json.load(open(cd + "/session.json"))
    key = b64decode(sess["key"] + "==")
    aead = AESOCB3(key)
    pk = pcap(open(cd + "/cap.pcap", "rb").read())
    t0 = pk[0][0]
    frags = collections.defaultdict(dict)
    rows = []
    for n, (t, to_srv, d) in enumerate(pk, 1):
        pt = aead.decrypt(b"\0\0\0\0" + d[:8], d[8:], None)
        ctr = struct.unpack(">Q", d[:8])[0] & ~(1 << 63)
        ts, tr, fid, fl = struct.unpack(">HHQH", pt[:14])
        fin, fnum, body = fl >> 15, fl & 0x7FFF, pt[14:]
        frags[(to_srv, fid)][fnum] = (fin, body)
        rows.append(dict(n=n, t=t - t0, cs=to_srv, wire=len(d), ctr=ctr, ts=ts, tr=tr, fid=fid, fin=fin, fnum=fnum, blen=len(body), body=body))
    # reassembly: concatenate by fragment number, inflate the whole
    done = {}
    for k, fr in frags.items():
        nums = sorted(fr)
        ok = nums == list(range(len(nums))) and fr[nums[-1]][0] == 1
        blob = b"".join(fr[i][1] for i in nums)
        try:
            done[k] = (ok, instr(zlib.decompress(blob)), len(blob))
        except Exception as ex:
            done[k] = (ok, None, len(blob))
    for r in rows:
        k = (r["cs"], r["fid"])
        ok, ins, blen = done[k]
        # does this fragment alone inflate?
        try:
            zlib.decompressobj().decompress(r["body"]); alone = "zok"
        except Exception:
            alone = "znot"
        r["line"] = "#%03d t=%8.3f %s w=%4d ctr=%-4d ts=%5d tr=%5d fid=%-4d f%d%s blen=%-4d %s" % (
            r["n"], r["t"], "C>S" if r["cs"] else "S>C", r["wire"], r["ctr"], r["ts"], r["tr"], r["fid"], r["fnum"],
            "F" if r["fin"] else "-", r["blen"], alone)
        if r["fnum"] == 0 or r["fin"]:
            pass
        if ins and (r["fin"] or r["fnum"] == 0):
            pass
        if ins and r["fnum"] == max(f for f in frags[k]):
            r["line"] += " | msg(total_z=%d, frags=%d) old=%d new=%d ack=%d thr=%d chaff=%d diff=%d %s" % (
                blen, len(frags[k]), ins["old"], ins["new"], ins["ack"], ins["thr"], ins["chaff"], ins["difflen"], " ".join(ins["els"]))
        elif not ins:
            r["line"] += " | (no inflate of reassembly)"
    open(cd + "/decoded.txt", "w").write("\n".join(r["line"] for r in rows) + "\n")
    allb = b"".join(done[k][1]["raw"] for k in done if done[k][1])
    open(cd + "/inflated_all.bin", "wb").write(allb)
    # stats
    print("capture", sess["name"], "packets", len(rows), "duration %.1fs" % rows[-1]["t"])
    for cs in (True, False):
        rr = [r for r in rows if r["cs"] == cs]
        if not rr: continue
        ws = [r["wire"] for r in rr]
        cn = [r["ctr"] for r in rr]
        gaps = sorted(set(range(min(cn), max(cn) + 1)) - set(cn))
        print(" %s: n=%d bytes=%d wire min/max/mean=%d/%d/%.0f  ctr %d..%d missing(lost before capture)=%d" % (
            "C>S" if cs else "S>C", len(rr), sum(ws), min(ws), max(ws), sum(ws) / len(ws), min(cn), max(cn), len(gaps)))
    nz = [r for r in rows if r["fnum"] != 0]
    nf = [r for r in rows if not r["fin"]]
    print(" frag_num!=0: %d  non-final: %d  distinct msgs with >1 frag: %d" % (
        len(nz), len(nf), sum(1 for k, v in frags.items() if len(v) > 1)))
    if nz:
        for k, v in sorted(frags.items(), key=lambda kv: kv[0][1]):
            if len(v) > 1:
                print("   msg fid=%d %s frags=%d sizes=%s finals=%s total_z=%d" % (
                    k[1], "C>S" if k[0] else "S>C", len(v), [len(v[i][1]) for i in sorted(v)], [v[i][0] for i in sorted(v)], done[k][2]))
    if full:
        print("\n".join(r["line"] for r in rows))


main()
