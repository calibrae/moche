#!/usr/bin/env python3
"""wire_trace.py: record the client-edge datagram trace of one typing run, local, no remote host.

  wire_trace.py --client stock|moche-never|moche-adaptive|moche-always --oneway MS --out TRACE
                [--moche PATH] [--secs 30] [--seed 1] [--idle-probe N]

Starts a stock mosh-server (fake SSH_CONNECTION, bash --norc), the delay relay
(tools/perf/delay_relay.py, tracing at the client edge), the client on a pty owned by this script
(no tmux: a tmux control pipe stalled for hundreds of ms in the first runs), then types
random lowercase letters with random 80-120 ms gaps (fixed seed) straight into the pty.
Writes TRACE (relay trace, `ts C|S hex`), TRACE.keys (monotonic time of every keystroke write,
one per line) and TRACE.key (the session key, a throwaway). Stock mosh is a black box: only its
datagrams are observed. With --idle-probe N, N extra keystrokes are typed first, each after 2.5 s
of silence (idle-connection collection delay probe).
"""
import argparse, os, random, re, socket, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))


def sh(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True)


def free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--client", required=True)
    ap.add_argument("--oneway", type=float, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--moche", default=os.path.join(HERE, "../../target/release/moche"))
    ap.add_argument("--secs", type=float, default=30)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--idle-probe", type=int, default=0)
    ap.add_argument("--loss", type=float, default=0)
    ap.add_argument("--train", default="", help="G,K,N: N trains of K keys G ms apart, each after 2.5 s of silence")
    a = ap.parse_args()
    rng = random.Random(a.seed)

    r = sh("SSH_CONNECTION='127.0.0.1 1 127.0.0.1 22' mosh-server new -s -c 256 -l LANG=en_US.UTF-8 -- /bin/bash --norc --noprofile")
    m = re.search(r"^MOSH CONNECT (\d+) (\S{22})$", r.stdout, re.M)
    pm = re.search(r"pid = (\d+)", r.stdout + r.stderr)
    if not m or not pm:
        sys.exit("no MOSH CONNECT: %r %r" % (r.stdout, r.stderr))
    sport, key, spid = m.group(1), m.group(2), int(pm.group(1))
    open(a.out + ".key", "w").write(key + "\n")
    lport = free_port()
    relay = subprocess.Popen([sys.executable, os.path.join(HERE, "delay_relay.py"), str(lport), sport,
                              str(a.oneway), str(a.loss), a.out], stdout=subprocess.PIPE, text=True)
    relay.stdout.readline()
    c = a.client
    if c == "stock":
        cmd = "env LC_ALL=en_US.UTF-8 MOSH_KEY=%s mosh-client 127.0.0.1 %d" % (key, lport)
    else:
        cmd = "env LC_ALL=en_US.UTF-8 MOSH_KEY=%s %s --predict=%s --direct 127.0.0.1 %d" % (
            key, a.moche, c.split("-", 1)[1], lport)
    import fcntl, pty, struct, termios
    pid, fd = pty.fork()
    if pid == 0:
        env = dict(os.environ, TERM="xterm-256color")
        os.execvpe("/bin/sh", ["sh", "-c", "exec " + cmd], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    import select
    keys = []

    def pause(secs):
        """Sleep `secs` while draining the client's screen output (single thread, 1 ms polls)."""
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
        t = time.monotonic()
        os.write(fd, ch.encode())
        keys.append(t)

    pause(2.5 + a.oneway / 100.0)  # session up, first screen, settle
    pause(3.5)
    letters = "abcdefghijklmnopqrstuvwxyz"
    for _ in range(a.idle_probe):
        send(rng.choice(letters))
        pause(2.5)
    if a.train:
        g, kk, nn = a.train.split(",")
        for _ in range(int(nn)):
            for _ in range(int(kk)):
                send(rng.choice(letters))
                pause(float(g) / 1000.0)
            pause(2.5)
    t_end = time.monotonic() + a.secs
    while time.monotonic() < t_end:
        send(rng.choice(letters))
        pause(rng.uniform(0.08, 0.12))
    pause(2.0 + a.oneway / 100.0)
    open(a.out + ".keys", "w").write("".join("%.6f\n" % t for t in keys))
    os.kill(pid, 9)
    os.waitpid(pid, 0)
    relay.terminate()
    relay.wait()
    sh("kill %d" % spid)


main()
