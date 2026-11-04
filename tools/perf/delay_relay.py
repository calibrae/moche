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
saw it). Single
client. Stops on SIGTERM/SIGINT. There is no `tc` on macOS; this stands in.
"""
import heapq, random, select, signal, socket, sys, time


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
    while running:
        now = time.monotonic()
        timeout = max(0.0, heap[0][0] - now) if heap else 0.5
        # Cap the wait at 1 ms: on macOS a select() timeout of tens of ms was seen to wake up
        # hundreds of ms to 1.6 s late (timer coalescing), which stalled every datagram.
        r, _, _ = select.select([a, b], [], [], min(timeout, 0.001))
        now = time.monotonic()
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
                trace.write("%.6f S %s\n" % (time.monotonic(), data.hex()))
            try:
                dst.sendto(data, to)
            except OSError:
                pass

    if trace:
        trace.close()


main()
