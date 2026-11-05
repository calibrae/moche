#!/usr/bin/env python3
"""rec.py: server-side key recorder for the VM lab. Run as the mosh-server command
(`mosh-server new ... -- /tmp/rec.py`): raw tty, logs "<time_ns> <byte hex>" per received byte to
/tmp/rec.log and echoes the byte back, so the client screen behaves like a shell echo and every key
that reached the server is counted."""
import os, sys, time, tty
tty.setraw(0)
log = open("/tmp/rec.log", "w", buffering=1)
sys.stdout.write("REC> "); sys.stdout.flush()
while True:
    b = os.read(0, 1)
    if not b:
        break
    log.write("%d %s\n" % (time.time_ns(), b.hex()))
    os.write(1, b)
