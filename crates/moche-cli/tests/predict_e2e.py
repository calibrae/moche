"""End-to-end predictive echo: moche against a stock local mosh-server, through a UDP
relay that adds delay (tools/perf/delay_relay.py), watched through tmux.

usage: python3 -I predict_e2e.py <moche-binary> <delay-relay.py> [one-way-ms]
Exit 0 = all checks passed, 77 = skipped (no mosh-server / tmux), other = failure.

Checks, with a 100 ms one-way delay (200 ms RTT) by default:
 1. --predict=adaptive: the first typed character appears only with the server's
    echo (about one RTT), later ones within 40 ms of the keystroke; the final screen
    equals the one of a --predict=never run; no underline residue at the end.
 2. --predict=never: every character takes about one RTT.
 3. A password prompt (stty -echo): not one typed character is ever displayed.
"""
import os, re, signal, socket, subprocess, sys, tempfile, time, shutil

MOCHE, RELAY = sys.argv[1], sys.argv[2]
ONE_WAY = int(sys.argv[3]) if len(sys.argv) > 3 else 100
SOCK = "moche-pe2e-%d" % os.getpid()


def tmux(*a):
    return subprocess.run(["tmux", "-L", SOCK, *a], capture_output=True, text=True).stdout


def capture(sess, esc=False):
    return tmux("capture-pane", "-p" + ("e" if esc else ""), "-t", sess)


def free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def start_server():
    env = dict(os.environ, SSH_CONNECTION="127.0.0.1 1 127.0.0.1 22")
    r = subprocess.run(
        ["mosh-server", "new", "-s", "-c", "256", "-l", "LANG=en_US.UTF-8", "--", "/bin/sh"],
        env=env, capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=20)
    m = re.search(r"^MOSH CONNECT (\d+) (\S{22})$", r.stdout, re.M)
    pid = re.search(r"pid = (\d+)", r.stdout + r.stderr)
    assert m and pid, "no MOSH CONNECT: " + r.stdout + r.stderr
    return int(m.group(1)), m.group(2), int(pid.group(1))


class Session:
    def __init__(self, predict):
        self.port, key, self.spid = start_server()
        lp = free_port()
        self.relay = subprocess.Popen(
            [sys.executable, "-I", RELAY, str(lp), str(self.port), str(ONE_WAY)],
            stdout=subprocess.PIPE)
        self.relay.stdout.readline()
        self.sess = "p"
        cmd = "env MOSH_KEY=%s %s --predict=%s --direct 127.0.0.1 %d" % (key, MOCHE, predict, lp)
        tmux("kill-server")
        tmux("new-session", "-d", "-s", self.sess, "-x", "80", "-y", "24", cmd)
        self.wait(r"\$", 15)
        time.sleep(0.8)
        self.send("PS1='$ '\r")
        time.sleep(1.0)
        for c in "xx":  # let the RTT estimate settle
            self.send(c)
            time.sleep(0.3)
        self.send("\x15")
        time.sleep(1.5)

    def send(self, s):
        tmux("send-keys", "-t", self.sess, "-l", s)

    def wait(self, pat, timeout):
        t0 = time.time()
        while time.time() - t0 < timeout:
            if re.search(pat, capture(self.sess)):
                return
            time.sleep(0.05)
        raise AssertionError("timeout waiting for %r:\n%s" % (pat, capture(self.sess)))

    def line(self):
        """The prompt line as plain text, trailing blanks trimmed."""
        rows = [r for r in capture(self.sess).split("\n") if r.startswith("$ ")]
        return rows[-1].rstrip() if rows else ""

    def type_timeline(self, text, gap, tail=2.0):
        """Types one char per `gap` s; returns per-char (typed at, first shown at)."""
        t0 = time.time()
        sent, seen, i = [], [None] * len(text), 0
        lines = []
        while time.time() - t0 < gap * len(text) + tail:
            now = time.time() - t0
            if i < len(text) and now >= gap * i:
                self.send(text[i])
                sent.append(time.time() - t0)
                i += 1
            ln = self.line()
            lines.append((time.time() - t0, ln))
            for k in range(i):
                if seen[k] is None and ln.startswith(("$ " + text[:k + 1]).rstrip()):
                    seen[k] = time.time() - t0
            time.sleep(0.003)
        return sent, seen, lines

    def close(self):
        tmux("kill-server")
        self.relay.terminate()
        try:
            os.kill(self.spid, signal.SIGTERM)
        except OSError:
            pass


def fmt(xs):
    return "[" + " ".join("-" if x is None else "%d" % round(x * 1000) for x in xs) + "]"


def main():
    if not (shutil.which("mosh-server") and shutil.which("tmux")):
        print("skip: mosh-server or tmux missing")
        return 77
    text, gap = "abcdefghij", 0.25
    results = {}
    for mode in ("adaptive", "never"):
        s = Session(mode)
        try:
            sent, seen, lines = s.type_timeline(text, gap)
            lat = [None if b is None else b - a for a, b in zip(sent, seen)]
            print("%s: latency ms %s" % (mode, fmt(lat)))
            assert all(x is not None for x in lat), "a char never showed up"
            time.sleep(0.5)
            final = s.line()
            results[mode] = final
            assert final == "$ " + text, "final line %r" % final
            assert "\x1b[4m" not in capture(s.sess, True), "underline residue"
            rtt = 2 * ONE_WAY / 1000.0
            if mode == "adaptive":
                assert lat[0] >= rtt * 0.8, "first char shown before the server echo: %s" % fmt(lat)
                late = [i for i in range(3, len(text)) if lat[i] > 0.040]
                assert not late, "chars shown late (more than 40 ms): %s" % fmt(lat)
            else:
                assert all(x >= rtt * 0.8 for x in lat), "never mode showed early: %s" % fmt(lat)
        finally:
            s.close()
    assert results["adaptive"] == results["never"], results

    # password prompt: nothing typed may ever be displayed
    s = Session("adaptive")
    try:
        s.send("stty -echo\r")
        time.sleep(1.5)
        base = s.line()
        sent, seen, lines = s.type_timeline("secret-pass", 0.15, tail=1.5)
        bad = [(round(t, 3), ln) for t, ln in lines if ln != base]
        print("password: %d samples, %d differ from the bare prompt" % (len(lines), len(bad)))
        assert not bad, "a typed char was displayed: %r" % (bad[:3],)
        s.send("\r")
        time.sleep(1.0)
    finally:
        s.close()
    print("predict e2e: OK")
    return 0


sys.exit(main())
