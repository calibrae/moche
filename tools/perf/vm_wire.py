#!/usr/bin/env python3
"""vm_wire.py (runs INSIDE the client VM of the VM lab): typing run against an already running stock
mosh-server, datagram trace taken with tcpdump on eth0 (the client edge of the netem path), converted to
the `ts C|S hex` format of delay_relay.py so wire_analyze.py / trace_decode can read it.

  vm_wire.py --client stock|new-never|old-adaptive|... --aip IP --port P --key K --out PFX
             [--secs 30] [--seed 1] [--rtt-ms R] [--idle-probe N] [--probes N] [--tail S] [--idle S]
  client = stock | <variant>-<never|adaptive|always>; the moche binary is $MOCHE_BIN_DIR/<variant>/moche
           (default ~/moche-lab/bin), so two builds can be compared in the same session.
Writes PFX (trace), PFX.keys (wall time of every keystroke), PFX.key (throwaway session key), PFX.stats
(moche --stats), PFX.cpu (CPU ticks and context switches over the typing window), PFX.idle (same over
--idle seconds before typing), PFX.tailok (crude: last 30 typed chars found in the client's output).
Typing: random lowercase letters with 80-120 ms gaps (as wire_trace.py). --idle-probe N: N keys, each after
2.5 s of silence, before the typing. --probes N: N isolated keys, each after 3.5 s + 2 RTT of silence, after
the typing (loss-recovery probes; with --secs 0 they are the whole run). --tail: final pause.
Needs sudo for tcpdump (capture limited by -c).
"""
import argparse, fcntl, json, os, pty, random, re, select, struct, subprocess, termios, time

BIN_DIR = os.environ.get("MOCHE_BIN_DIR", os.path.expanduser("~/moche-lab/bin"))


def pcap_to_trace(pcap, out, sport):
    d = open(pcap, "rb").read()
    magic = d[:4]
    e = "<" if magic in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1") else ">"
    nano = magic in (b"\x4d\x3c\xb2\xa1", b"\xa1\xb2\x3c\x4d")
    o, lines = 24, []
    while o + 16 <= len(d):
        ts, tu, cl, _ = struct.unpack(e + "IIII", d[o:o + 16])
        o += 16
        p = d[o:o + cl]
        o += cl
        if len(p) < 42 or p[12:14] != b"\x08\x00":
            continue
        udp = 14 + (p[14] & 0xf) * 4
        src, _dst = struct.unpack(">HH", p[udp:udp + 4])
        t = ts + tu / (1e9 if nano else 1e6)
        lines.append("%.6f %s %s\n" % (t, "S" if src == sport else "C", p[udp + 8:].hex()))
    open(out, "w").writelines(lines)
    return len(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--client", required=True)
    ap.add_argument("--aip", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--key", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--secs", type=float, default=30)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--idle-probe", type=int, default=0)
    ap.add_argument("--rtt-ms", type=float, default=0)
    ap.add_argument("--probes", type=int, default=0)
    ap.add_argument("--idle", type=float, default=0)
    ap.add_argument("--tail", type=float, default=-1)
    ap.add_argument("--maxpkts", type=int, default=12000)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    pcap = a.out + ".pcap"
    subprocess.run("sudo pkill tcpdump; rm -f %s" % pcap, shell=True)
    td = subprocess.Popen(["sudo", "tcpdump", "-ni", "eth0", "-c", str(a.maxpkts), "-w", pcap, "udp", "port", str(a.port)],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    open(a.out + ".key", "w").write(a.key + "\n")
    if a.client == "stock":
        cmd = "env MOSH_KEY=%s mosh-client %s %d" % (a.key, a.aip, a.port)
    else:
        v, mode = a.client.split("-", 1)
        cmd = "env MOSH_KEY=%s %s/%s/moche --stats --predict=%s --direct %s %d 2>%s.stats" % (
            a.key, BIN_DIR, v, mode, a.aip, a.port, a.out)
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe("/bin/sh", ["sh", "-c", "exec " + cmd], dict(os.environ, TERM="xterm-256color", LANG="C.UTF-8"))
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    keys, typed, outbuf = [], [], bytearray()

    def pause(secs):
        end = time.monotonic() + secs
        while True:
            left = end - time.monotonic()
            if left <= 0:
                return
            if select.select([fd], [], [], min(left, 0.001))[0]:
                try:
                    b = os.read(fd, 65536)
                    if len(outbuf) < 8000000:
                        outbuf.extend(b)
                except OSError:
                    return

    def send(ch):
        t = time.time()
        os.write(fd, ch.encode())
        keys.append(t)
        typed.append(ch)

    def cpu():
        f = open("/proc/%d/stat" % pid).read().rsplit(")", 1)[1].split()
        ctx = 0
        for t in os.listdir("/proc/%d/task" % pid):
            try:
                for l in open("/proc/%d/task/%s/status" % (pid, t)):
                    if "ctxt_switches" in l:
                        ctx += int(l.split()[1])
            except OSError:
                pass
        return int(f[11]) + int(f[12]), ctx

    rtt = a.rtt_ms / 1000.0
    pause(3.0 + 2 * rtt)
    pause(3.0 + rtt)
    letters = "abcdefghijklmnopqrstuvwxyz"
    for _ in range(a.idle_probe):
        send(rng.choice(letters))
        pause(2.5)
    if a.idle:
        ci0, wi0 = cpu(), time.time()
        pause(a.idle)
        ci1, wi1 = cpu(), time.time()
        open(a.out + ".idle", "w").write(json.dumps(dict(ticks=ci1[0] - ci0[0], ctx=ci1[1] - ci0[1], secs=wi1 - wi0)) + "\n")
    c0, w0 = cpu(), time.time()
    t_end = time.monotonic() + a.secs
    while time.monotonic() < t_end:
        send(rng.choice(letters))
        pause(rng.uniform(0.08, 0.12))
    c1, w1 = cpu(), time.time()
    open(a.out + ".cpu", "w").write(json.dumps(dict(ticks=c1[0] - c0[0], ctx=c1[1] - c0[1], secs=w1 - w0, keys=len(keys))) + "\n")
    if a.probes:
        for _ in range(a.probes):
            pause(3.5 + 2 * rtt)
            send(rng.choice(letters))
        pause(6.0 + 4 * rtt)
    else:
        pause(a.tail if a.tail >= 0 else 2.0 + rtt)
    txt = re.sub(rb"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[()][0-9A-B]|[\x00-\x1f]", b"", bytes(outbuf)).decode("utf-8", "replace")
    open(a.out + ".tailok", "w").write(("1" if "".join(typed)[-30:] in txt else "0") + "\n")
    open(a.out + ".keys", "w").write("".join("%.6f\n" % t for t in keys))
    os.kill(pid, 9)
    os.waitpid(pid, 0)
    subprocess.run("sudo pkill tcpdump", shell=True)
    td.wait()
    n = pcap_to_trace(pcap, a.out, a.port)
    os.remove(pcap)
    print("trace", a.out, "datagrams", n, "keys", len(keys))


main()
