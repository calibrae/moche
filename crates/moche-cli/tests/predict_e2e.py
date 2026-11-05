"""End-to-end predictive echo: moche against a stock local mosh-server, through a UDP
relay that adds delay (tools/perf/delay_relay.py), watched through tmux.

usage: python3 -I predict_e2e.py <moche-binary> <delay-relay.py> [one-way-ms]
Exit 0 = all checks passed, 77 = skipped (no mosh-server / tmux), 1 = failure,
3 = a GHOST: a typed secret character was displayed with no server echo (trace proved).

Checks, with a 100 ms one-way delay (200 ms RTT) by default:
 1. --predict=adaptive: the first typed character appears only with the server's
    echo (about one RTT), later ones within 40 ms of the keystroke; the final screen
    equals the one of a --predict=never run; no underline residue at the end.
 2. --predict=never: every character takes about one RTT.
 3. A password prompt (stty -echo): no typed character is ever displayed.
    The remote prints a marker AFTER stty -echo took effect and the password is typed
    only once the marker and the next prompt are on screen, so the server cannot echo
    it. Then the check is strict: not one sample differs from the bare prompt, and the
    moche predict trace shows no overlay cell. A second variant (`early`) makes the
    remote `sleep 1` before stty -echo and types the secret meanwhile: the server's REAL
    tty echo then shows the first characters (and, by design, the next typed ones are
    predicted until the echo ack contradicts them); that is not a ghost. A ghost is an overlay cell painted while no server echo of the secret had
    been seen; only that fails the early variant (exit 3).

Environment (all optional):
  MOCHE_E2E_ART=<dir>      write artefacts under <dir>/run-<N>-<part>/ (default: temp dir, removed)
  MOCHE_E2E_RUN=<N>        run number, for the artefact directory name
  MOCHE_E2E_WHICH=full|password|timing   which checks (default full)
  MOCHE_E2E_PW=marker|early|both         password variants (default marker)
  MOCHE_E2E_KEEP=1         keep the artefacts of passing runs too
  MOCHE_E2E_RETRY=<n>      retries of the TIMING checks after a harness stall was seen (default 2).
                           The password check is never retried.
Artefacts: per-sample pane timeline (full pane with attributes, on change), relay datagram
log (wall clock, both directions), moche predict trace, the harness event log (keystrokes
with times, markers, stalls), summary.json.
"""
import ast, json, os, re, signal, socket, subprocess, sys, tempfile, time, shutil

MOCHE, RELAY = sys.argv[1], sys.argv[2]
ONE_WAY = int(sys.argv[3]) if len(sys.argv) > 3 else 100
SOCK = "moche-pe2e-%d" % os.getpid()
ART = os.environ.get("MOCHE_E2E_ART")
RUN = os.environ.get("MOCHE_E2E_RUN", "0")
WHICH = os.environ.get("MOCHE_E2E_WHICH", "full")
PWV = os.environ.get("MOCHE_E2E_PW", "marker")
KEEP = bool(os.environ.get("MOCHE_E2E_KEEP"))
RETRY = int(os.environ.get("MOCHE_E2E_RETRY", "2"))
STALL_S = 0.100  # a gap between two samples larger than this is a harness/host stall
SECRET = "secret-pass"
SGR = re.compile(r"\x1b\[[0-9;:]*m")


class Ghost(AssertionError):
    pass


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
    def __init__(self, predict, art):
        self.art = art
        self.events = []  # (wall, what)
        self.timeline = []  # (wall, full pane with attributes), on change
        self.last_pane = None
        self.samples = 0
        self.max_gap = 0.0
        self.stalls = []  # (wall, gap)
        self.port, key, self.spid = start_server()
        lp = free_port()
        self.relay_log = os.path.join(art, "relay.log")
        self.trace = os.path.join(art, "moche.trace")
        self.relay = subprocess.Popen(
            [sys.executable, "-I", RELAY, str(lp), str(self.port), str(ONE_WAY), "0", self.relay_log],
            stdout=subprocess.PIPE, env=dict(os.environ, MOCHE_RELAY_WALL="1"))
        self.relay.stdout.readline()
        self.sess = "p"
        cmd = "env MOSH_KEY=%s MOCHE_PREDICT_TRACE=%s %s --predict=%s --direct 127.0.0.1 %d" % (
            key, self.trace, MOCHE, predict, lp)
        tmux("kill-server")
        tmux("new-session", "-d", "-s", self.sess, "-x", "80", "-y", "24", cmd)
        self.ev("session start predict=%s" % predict)
        self.wait(r"\$", 15)
        time.sleep(0.8)
        self.send("PS1='$ '\r")
        time.sleep(1.0)
        for c in "xx":  # let the RTT estimate settle
            self.send(c)
            time.sleep(0.3)
        self.send("\x15")
        time.sleep(1.5)
        self.ev("session ready")

    def ev(self, what):
        self.events.append((time.time(), what))

    def send(self, s):
        self.ev("send %r" % s)
        tmux("send-keys", "-t", self.sess, "-l", s)

    def sample(self):
        """One capture: returns the plain prompt line; records the pane when it changed."""
        now = time.time()
        pane = capture(self.sess, True)
        if self.samples:
            gap = now - self.last_t
            if gap > self.max_gap:
                self.max_gap = gap
            if gap > STALL_S:
                self.stalls.append((now, gap))
        self.samples += 1
        self.last_t = now
        if pane != self.last_pane:
            self.timeline.append((now, pane))
            self.last_pane = pane
        plain = SGR.sub("", pane)
        rows = [r for r in plain.split("\n") if r.startswith("$ ")]
        return rows[-1].rstrip() if rows else ""

    def wait(self, pat, timeout):
        t0 = time.time()
        while time.time() - t0 < timeout:
            if re.search(pat, SGR.sub("", capture(self.sess)), re.M):
                return
            time.sleep(0.05)
        raise AssertionError("timeout waiting for %r:\n%s" % (pat, capture(self.sess)))

    def line(self):
        """The prompt line as plain text, trailing blanks trimmed."""
        rows = [r for r in capture(self.sess).split("\n") if r.startswith("$ ")]
        return rows[-1].rstrip() if rows else ""

    def type_timeline(self, text, gap, tail=2.0):
        """Types one char per `gap` s; returns per-char (typed at, first shown at) and the
        samples."""
        t0 = time.time()
        sent, seen, i = [], [None] * len(text), 0
        lines = []
        while time.time() - t0 < gap * len(text) + tail:
            now = time.time() - t0
            if i < len(text) and now >= gap * i:
                self.send(text[i])
                sent.append(time.time() - t0)
                i += 1
            ln = self.sample()
            lines.append((time.time() - t0, ln))
            for k in range(i):
                if seen[k] is None and ln.startswith(("$ " + text[:k + 1]).rstrip()):
                    seen[k] = time.time() - t0
            time.sleep(0.003)
        return sent, seen, lines, t0

    def loopback_stall_ms(self):
        """Worst delay between a client `tx` (moche.trace) and the relay reading that datagram
        (relay.log `C` lines), matched in order (no loss in this relay). Needs `close()` first."""
        tx = [t for t, ev, _ in parse_trace(self.trace) if ev == "tx"]
        cs = []
        try:
            for ln in open(self.relay_log):
                p = ln.split()
                if len(p) == 3 and p[1] == "C":
                    cs.append(float(p[0]))
        except OSError:
            return 0
        worst = 0.0
        for a, b in zip(tx, cs):
            worst = max(worst, b - a)
        return round(worst * 1000)

    def close(self):
        tmux("kill-server")
        self.relay.terminate()
        try:
            self.relay.wait(timeout=5)
        except Exception:
            self.relay.kill()
        try:
            os.kill(self.spid, signal.SIGTERM)
        except OSError:
            pass

    def dump(self):
        with open(os.path.join(self.art, "timeline.txt"), "w") as f:
            for t, pane in self.timeline:
                f.write("=== %.6f\n%s\n" % (t, pane.replace("\x1b", "\\e")))
        with open(os.path.join(self.art, "events.txt"), "w") as f:
            for t, w in self.events:
                f.write("%.6f %s\n" % (t, w))
            for t, g in self.stalls:
                f.write("%.6f STALL sample gap %.0f ms\n" % (t, g * 1000))


def fmt(xs):
    return "[" + " ".join("-" if x is None else "%d" % round(x * 1000) for x in xs) + "]"


def parse_trace(path):
    """Trace lines as (wall, event, rest)."""
    out = []
    try:
        for ln in open(path):
            p = ln.rstrip("\n").split(" ", 2)
            if len(p) >= 2:
                out.append((float(p[0]), p[1], p[2] if len(p) > 2 else ""))
    except OSError:
        pass
    return out


def secret_prefix(text, n=1):
    """True if a screen row, minus a leading prompt, is a prefix (>= n chars) of the secret."""
    t = text.strip()
    t = t[2:].strip() if t.startswith("$ ") else t
    return len(t) >= n and SECRET.startswith(t)


def password_verdict(trace, t_from):
    """From the moche trace, for the password phase (wall >= t_from): the overlay paints
    and the server rows. Returns dict(overlay_paints, ghosts, real_echo_rows)."""
    srv_echo = False
    paints, ghosts, real = [], [], []
    for t, ev, rest in trace:
        if t < t_from:
            continue
        if ev == "server_diff":
            m = re.search(r'row=(".*")\s*$', rest)
            row = ast.literal_eval(m.group(1)) if m else ""
            if secret_prefix(row):
                srv_echo = True
                real.append((t, row))
        elif ev == "paint":
            m = re.search(r"overlay_cells=(\d+)", rest)
            if m and int(m.group(1)) > 0:
                paints.append((t, rest))
                if not srv_echo:
                    ghosts.append((t, rest))
    return dict(overlay_paints=paints, ghosts=ghosts, real_echo_rows=real)


def timing_part(mode, art, results):
    text, gap = "abcdefghij", 0.25
    s = Session(mode, art)
    info = {}
    try:
        sent, seen, lines, t0 = s.type_timeline(text, gap)
        lat = [None if b is None else b - a for a, b in zip(sent, seen)]
        print("%s: latency ms %s" % (mode, fmt(lat)))
        info["latency_ms"] = fmt(lat)
        info["max_sample_gap_ms"] = round(s.max_gap * 1000)
        info["stalls"] = len(s.stalls)
        try:
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
        except AssertionError as e:
            info["failure"] = str(e).split("\n")[0]
            info["stall_seen"] = bool(s.stalls)
            raise
    finally:
        s.close()
        s.dump()
        # The stalls that matter here are not the harness's own (sample gaps) but datagrams that
        # sat 0.2 to 1.1 s between the client's sendto and the relay's recv (the OS loopback; see
        # docs/observations-round11.md). The client traces every sendto, the relay stamps every
        # datagram it reads, both in wall time: a long gap marks the run as disturbed.
        lb = s.loopback_stall_ms()
        info["loopback_stall_ms"] = lb
        if lb > 100 and "failure" in info:
            info["stall_seen"] = True
        json.dump(info, open(os.path.join(art, "info.json"), "w"))


def password_part(variant, art):
    s = Session("adaptive", art)
    info = {"variant": variant}
    try:
        if variant == "early":
            # the remote sleeps before it turns echo off (a host stall): the secret is typed
            # while the tty still echoes it, so the server's REAL echo shows the first
            # characters; that is legitimate, only a character shown with no server echo
            # at all is a ghost
            s.send("sleep 1; stty -echo; printf '\\nMK%s\\n' 7\r")
            time.sleep(0.05)
            secret_rest = SECRET
            t_from = time.time() - 0.01
            s.ev("password phase starts (early: echo still on)")
        else:
            s.send("stty -echo; printf 'MK%s\\n' 7\r")
            s.wait(r"^MK7$", 20)
            s.ev("marker seen")
            s.wait(r"^MK7\n\$$", 20)
            s.ev("prompt after marker seen")
            time.sleep(1.0)
            secret_rest = SECRET
            t_from = time.time() - 0.01
            s.ev("password phase starts")
        base = s.line()
        info["base"] = base
        sent, seen, lines, t0 = s.type_timeline(secret_rest, 0.15, tail=1.5)
        if variant == "early":
            s.wait(r"^MK7$", 20)
            time.sleep(1.0)
        if variant == "early":
            # no stable base (the prompt only returns after the sleep): a sample is a secret
            # on screen when some pane row holds at least two secret characters
            bad = []
            for t, pane in s.timeline:
                if t >= t_from:
                    for r in SGR.sub("", pane).split("\n"):
                        if secret_prefix(r, 2):
                            bad.append((round(t - t_from, 3), r.rstrip()))
                            break
        else:
            bad = [(round(t, 3), ln) for t, ln in lines if ln != base]
        info["samples"] = len(lines)
        info["differ"] = len(bad)
        info["max_sample_gap_ms"] = round(s.max_gap * 1000)
        info["stalls"] = len(s.stalls)
        s.send("\r")
        time.sleep(1.0)
        s.close()
        verdict = password_verdict(parse_trace(s.trace), t_from)
        info["overlay_paints"] = len(verdict["overlay_paints"])
        info["ghost_paints"] = len(verdict["ghosts"])
        info["server_echo_rows"] = len(verdict["real_echo_rows"])
        print("password[%s]: %d samples, %d differ from the bare prompt, %d overlay paints, "
              "%d server echo rows" % (variant, len(lines), len(bad), info["overlay_paints"],
                                      info["server_echo_rows"]))
        if verdict["ghosts"]:
            info["class"] = "GHOST"
            raise Ghost("GHOST: overlay painted with no server echo: %r" % (verdict["ghosts"][:2],))
        if variant == "marker":
            if verdict["real_echo_rows"]:
                info["class"] = "REAL_ECHO_AFTER_MARKER"
                raise AssertionError("the server echoed a typed char although stty -echo "
                                     "had printed its marker: %r" % (verdict["real_echo_rows"][:2],))
            if verdict["overlay_paints"] or bad:
                info["class"] = "OVERLAY_OR_PANE_DIFF_NO_ECHO"
                raise Ghost("a typed char was displayed: %r %r" % (bad[:3], verdict["overlay_paints"][:2]))
        else:
            if bad and not verdict["real_echo_rows"]:
                info["class"] = "PANE_DIFF_NO_SERVER_ECHO"
                raise Ghost("pane differs with no server echo: %r" % (bad[:3],))
            info["class"] = "real_echo" if verdict["real_echo_rows"] else "clean"
        info.setdefault("class", "clean")
    finally:
        s.close()
        s.dump()
        json.dump(info, open(os.path.join(art, "info.json"), "w"))


def run_part(name, fn, base):
    """Runs one check with its own artefact dir; keeps it on failure. Returns (exit, err)."""
    art = os.path.join(base, "run-%s-%s" % (RUN, name))
    os.makedirs(art, exist_ok=True)
    err = None
    try:
        fn(art)
    except Ghost as e:
        err = (3, e)
    except AssertionError as e:
        err = (1, e)
    if err is None and not KEEP:
        shutil.rmtree(art, ignore_errors=True)
    return err


def main():
    if not (shutil.which("mosh-server") and shutil.which("tmux")):
        print("skip: mosh-server or tmux missing")
        return 77
    tmp = None
    base = ART
    if not base:
        tmp = tempfile.mkdtemp(prefix="moche-pe2e-")
        base = tmp
    os.makedirs(base, exist_ok=True)
    rc = 0
    try:
        if WHICH in ("full", "timing"):
            results = {}
            for mode in ("adaptive", "never"):
                for attempt in range(RETRY + 1):
                    err = run_part("%s%s" % (mode, "" if attempt == 0 else "-retry%d" % attempt),
                                   lambda a, m=mode: timing_part(m, a, results), base)
                    if err is None:
                        break
                    info = {}
                    try:
                        info = json.load(open(os.path.join(
                            base, "run-%s-%s%s" % (RUN, mode, "" if attempt == 0 else "-retry%d" % attempt),
                            "info.json")))
                    except OSError:
                        pass
                    if info.get("stall_seen") and attempt < RETRY:
                        print("harness stall during the timing check, retrying: %s" % err[1])
                        continue
                    print("FAIL %s: %s" % (mode, err[1]))
                    return err[0]
            assert results.get("adaptive") == results.get("never"), results
        if WHICH in ("full", "password"):
            variants = ["marker", "early"] if PWV == "both" else [PWV]
            for v in variants:
                err = run_part("password-%s" % v, lambda a, v=v: password_part(v, a), base)
                if err:
                    print("FAIL password[%s]: %s" % (v, err[1]))
                    return err[0]
        print("predict e2e: OK")
        return rc
    finally:
        if tmp:
            shutil.rmtree(tmp, ignore_errors=True)


sys.exit(main())
