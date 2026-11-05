#!/usr/bin/env python3
"""vmw.py (runs INSIDE the client guest): adaptation of tools/perf/vm_wire.py for the x86 lab.
Differences: tcpdump on link0 (not eth0); no trace_decode: the pcap is reduced to "<time> C|S <size> <hex payload>" lines
(dec.py can decode them) and the script prints datagram counts per key itself.
  vmw.py --client stock|<variant>-<never|adaptive|always|none> --aip IP --port P --key K --out PFX
         [--secs 30] [--seed 1] [--probes N] [--tail S] [--idle S] [--rtt-ms R]
Moche binary = $MOCHE_BIN_DIR/<variant>/moche (default ~/moche-lab/bin); mode "none" passes no --predict flag (rc1 has none).
Typing: random lowercase letters, 80-120 ms gaps. --probes N: N isolated keys after typing, each after 3.5 s + 2 RTT of silence.
Writes PFX (trace), PFX.keys, PFX.stats (moche --stats), PFX.cpu, PFX.idle; prints one JSON line. Needs sudo for tcpdump."""
import argparse, fcntl, json, os, pty, random, select, struct, subprocess, termios, time

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
        ulen = struct.unpack(">H", p[udp + 4:udp + 6])[0] - 8
        t = ts + tu / (1e9 if nano else 1e6)
        lines.append((t, "S" if src == sport else "C", ulen, p[udp + 8:udp + 8 + ulen].hex()))
    open(out, "w").writelines("%.6f %s %d %s\n" % l for l in lines)
    return lines


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--client", required=True)
    ap.add_argument("--aip", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--key", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--secs", type=float, default=30)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--rtt-ms", type=float, default=0)
    ap.add_argument("--probes", type=int, default=0)
    ap.add_argument("--idle", type=float, default=0)
    ap.add_argument("--tail", type=float, default=-1)
    ap.add_argument("--maxpkts", type=int, default=12000)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    pcap = a.out + ".pcap"
    subprocess.run("sudo pkill tcpdump; rm -f %s" % pcap, shell=True)
    td = subprocess.Popen(["sudo", "tcpdump", "-ni", "link0", "-U", "-c", str(a.maxpkts), "-w", pcap, "udp", "port", str(a.port)],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    if a.client == "stock":
        cmd = "env MOSH_KEY=%s MOSH_PREDICTION_DISPLAY=%s mosh-client %s %d" % (a.key, os.environ.get("STOCK_PRED", "never"), a.aip, a.port)
    else:
        v, mode = a.client.split("-", 1)
        cmd = "env MOSH_KEY=%s %s/%s/moche --stats %s --direct %s %d 2>%s.stats" % (
            a.key, BIN_DIR, v, ("" if mode == "none" else "--predict=" + mode), a.aip, a.port, a.out)
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe("/bin/sh", ["sh", "-c", "exec " + cmd], dict(os.environ, TERM="xterm-256color", LANG="C.UTF-8"))
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    keys = []

    def pause(secs):
        end = time.monotonic() + secs
        while True:
            left = end - time.monotonic()
            if left <= 0:
                return
            if select.select([fd], [], [], min(left, 0.001))[0]:
                try:
                    os.read(fd, 65536)
                except OSError:
                    return

    def send(ch):
        keys.append(time.time())
        os.write(fd, ch.encode())

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
    ntype = len(keys)
    if a.probes:
        for _ in range(a.probes):
            pause(3.5 + 2 * rtt)
            send(rng.choice(letters))
        pause(a.tail if a.tail >= 0 else 6.0 + 4 * rtt)
    else:
        pause(a.tail if a.tail >= 0 else 2.0 + rtt)
    open(a.out + ".keys", "w").write("".join("%.6f\n" % t for t in keys))
    os.kill(pid, 9)
    os.waitpid(pid, 0)
    subprocess.run("sudo pkill tcpdump", shell=True)
    td.wait()
    lines = pcap_to_trace(pcap, a.out, a.port)
    os.remove(pcap)
    res = dict(client=a.client, keys=ntype, total_keys=len(keys), cpu_ms_per_key=None)
    if ntype:
        lo, hi = keys[0], keys[ntype - 1] + 1.5
        res["client_dg"] = sum(1 for t, d, _, _h in lines if d == "C" and lo <= t <= hi)
        res["dg_per_key"] = round(res["client_dg"] / ntype, 4)
        res["server_dg"] = sum(1 for t, d, _, _h in lines if d == "S" and lo <= t <= hi)
        res["cpu_ms_per_key"] = round(1000.0 * (c1[0] - c0[0]) / os.sysconf("SC_CLK_TCK") / ntype, 3)
    print(json.dumps(res))


main()
