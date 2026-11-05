#!/usr/bin/env python3
"""dec.py (guest): minimal decoder for the wire format of docs/protocol.md, for the ack-starvation probe.
  dec.py TRACE KEY22 -> prints "t dir new_num ack_num diff_len" for every single-fragment datagram (dir C|S).
TRACE lines: "<time> C|S <size> <hex udp payload>" (vmw.py). Datagram = nonce8 || OCB3(ct || tag16), nonce12 = 0*4 || nonce8,
plaintext = ts(2) ts_reply(2) frag_hdr(10) zlib(TransportInstruction). Multi-fragment messages are printed as FRAG.
Needs pycryptodome (Debian/Ubuntu package python3-pycryptodome, module Cryptodome)."""
import base64, sys, zlib
from Cryptodome.Cipher import AES

trace, key = sys.argv[1], base64.b64decode(sys.argv[2] + "==")


def varints(b):
    i, out = 0, {}
    while i < len(b):
        tag = b[i]; i += 1
        f, wt = tag >> 3, tag & 7
        if wt == 0:
            v = 0; s = 0
            while True:
                c = b[i]; i += 1
                v |= (c & 0x7f) << s; s += 7
                if not c & 0x80:
                    break
            out[f] = v
        elif wt == 2:
            n = 0; s = 0
            while True:
                c = b[i]; i += 1
                n |= (c & 0x7f) << s; s += 7
                if not c & 0x80:
                    break
            out[f] = b[i:i + n]; i += n
        else:
            break
    return out


for line in open(trace):
    p = line.split()
    if len(p) < 4:
        continue
    t, d, raw = p[0], p[1], bytes.fromhex(p[3])
    try:
        c = AES.new(key, AES.MODE_OCB, nonce=b"\0\0\0\0" + raw[:8], mac_len=16)
        pt = c.decrypt_and_verify(raw[8:-16], raw[-16:])
    except Exception:
        print(t, d, "BAD")
        continue
    flags = int.from_bytes(pt[12:14], "big")
    if flags != 0x8000:
        print(t, d, "FRAG")
        continue
    ins = varints(zlib.decompress(pt[14:]))
    print(t, d, ins.get(3, 0), ins.get(4, 0), len(ins.get(6, b"")))
