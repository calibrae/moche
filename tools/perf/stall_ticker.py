#!/usr/bin/env python3
"""Host stall logger: sleeps 10 ms in a loop and logs every gap above 100 ms.

usage: stall_ticker.py OUTFILE
Lines: `<unix seconds> <gap ms>`; also a `# start` line and a heartbeat comment every 60 s, so
an empty log can be told from a dead ticker. A separate process (no GIL shared with the
harness): a stall here is a stall of the whole machine (scheduler, App Nap, I/O).
"""
import signal, sys, time

running = True


def stop(*_):
    global running
    running = False


signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
out = open(sys.argv[1], "a", buffering=1)
out.write("# start %.3f\n" % time.time())
last = time.monotonic()
beat = last
while running:
    time.sleep(0.01)
    now = time.monotonic()
    if now - last > 0.1:
        out.write("%.3f %d\n" % (time.time(), round((now - last) * 1000)))
    if now - beat > 60:
        out.write("# alive %.3f\n" % time.time())
        beat = now
    last = now
