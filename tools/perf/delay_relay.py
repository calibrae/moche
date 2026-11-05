#!/usr/bin/env python3
"""Local UDP relay that adds a fixed one-way delay in each direction (loopback only).

usage: delay_relay.py LISTEN_PORT SERVER_PORT ONE_WAY_MS [LOSS_PERCENT [TRACE_FILE]]

A client talks to 127.0.0.1:LISTEN_PORT; the relay forwards to the server at
127.0.0.1:SERVER_PORT and back, holding every datagram ONE_WAY_MS in each
direction (so the round trip is twice that). Optional random loss. With TRACE_FILE every
datagram is logged at the CLIENT edge of the delay as `<time.monotonic()> <C|S> <hex>`:
C when the client sent it (logged when the relay receives it, before the delay), S when
the client receives it (logged when the relay hands it over, after the delay). Dropped
datagrams are logged too (C as sent; an S that is lost is not logged, the client never
saw it). It also logs `<time> L <ms>` when a loop iteration came back more than 20 ms late.
Single client. Stops on SIGTERM/SIGINT. There is no `tc` on macOS; this stands in.
"""
import heapq, os, random, select, signal, socket, sys, time

# MOCHE_RELAY_WALL=1: timestamp the trace with time.time() (Unix seconds) instead of
# time.monotonic(), to line it up with other logs.
CLOCK = time.time if os.environ.get("MOCHE_RELAY_WALL") else time.monotonic


def main():
    listen, server, one_way = int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3]) / 1000.0
    loss = float(sys.argv[4]) / 100.0 if len(sys.argv) > 4 else 0.0
    a = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)  # client side
    a.bind(("127.0.0.1", listen))
    b = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)  # server side
    b.bind(("127.0.0.1", 0))
    trace = open(sys.argv[5], "w", buffering=1 << 16) if len(sys.argv) > 5 else None
    client = None
    heap, seq = [], 0
    running = True

    def stop(*_):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    print("relay ready", flush=True)
    # Wait at most 1 ms: on macOS a select() timeout of tens of ms was seen to wake up hundreds of
    # ms late (timer coalescing). Round 11 tried a zero-timeout busy poll instead (while datagrams
    # are in flight and 2 s after): it made no difference to the start-up stalls, which are
    # datagrams delayed in the OS loopback between sendto and recv, so the plain 1 ms wait stays.
    # Loop lateness is logged (`L` lines) to prove the relay itself was running on time.
    last_iter = CLOCK()
    wait = 0.0
    while running:
        now = CLOCK()
        if trace and now - last_iter - wait > 0.020:
            trace.write("%.6f L %.1f\n" % (now, (now - last_iter - wait) * 1000))
        last_iter = now
        timeout = max(0.0, heap[0][0] - now) if heap else 0.5
        wait = min(timeout, 0.001)
        r, _, _ = select.select([a, b], [], [], wait)
        now = CLOCK()
        for s in r:
            data, addr = s.recvfrom(65535)
            if s is a:
                client = addr
                if trace:
                    trace.write("%.6f C %s\n" % (now, data.hex()))
            if random.random() < loss:
                continue
            if s is a:
                dst, to = b, ("127.0.0.1", server)
            else:
                if client is None:
                    continue
                dst, to = a, client
            seq += 1
            heapq.heappush(heap, (now + one_way, seq, dst, to, data))
        while heap and heap[0][0] <= now:
            _, _, dst, to, data = heapq.heappop(heap)
            if trace and dst is a:
                trace.write("%.6f S %s\n" % (CLOCK(), data.hex()))
            try:
                dst.sendto(data, to)
            except OSError:
                pass

    if trace:
        trace.close()


main()
