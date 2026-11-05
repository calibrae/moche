#!/usr/bin/env python3
"""predict_lab.py: real-network validation of moche's predictive local echo against the stock
mosh-client. Runs INSIDE the client VM of the VM lab (tools/vm-lab/README.md); the server VM runs a
stock mosh-server. Netem is set by the caller (tools/perf/predict_lab_run.sh), not here.

  python3 predict_lab.py latency  --client C --out F.json [--lines N] [--seed S] [--gap MS]
  python3 predict_lab.py scenario --client C --name N --out F.json
  python3 predict_lab.py cost     --client C --out F.json
  python3 predict_lab.py wire     --client C --out F.json
  C = moche-adaptive | moche-never | moche-always | stock

Environment (nothing is hard-coded, the repo may go public):
  PRED_A_IP   server address on the data path     PRED_B_IP  client address on the data path
  PRED_KEY    ssh key of the client VM for the server VM (user PRED_A_USER, default current user)
  PRED_MOCHE  moche binary on the client VM
  PRED_SRV_DIR  directory on the server VM holding plain.sh, wrap.sh, srv.conf (written by the runner)

Method: the client runs in a tmux (dedicated socket); a tmux control-mode pipe sends keys and takes
`capture-pane -p -e` snapshots every 10 ms (about 0.1 ms each). A char is "visible" at the first
snapshot whose prompt row starts with the typed prefix; attributes are parsed from the SGR codes.
Server truth: the server side runs inside a tmux whose pane is captured over ssh.
Stock mosh is a black box: only its behaviour is observed.
"""
import argparse, json, os, random, re, statistics, subprocess, sys, time

A = os.environ.get("PRED_A_IP", "")
B = os.environ.get("PRED_B_IP", "")
KEY = os.environ.get("PRED_KEY", "")
MOCHE = os.environ.get("PRED_MOCHE", "moche")
AUSER = os.environ.get("PRED_A_USER", os.environ.get("USER", "root"))
SRVDIR = os.environ.get("PRED_SRV_DIR", "~/pred")
SSH = ["ssh", "-i", KEY, "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=no", "-o", "BatchMode=yes",
       "-o", "ControlMaster=auto", "-o", "ControlPath=/tmp/pred-ssh-%C", "-o", "ControlPersist=300",
       AUSER + "@" + A]
GAPS = (0.08, 0.12)  # key gap range for latency typing, set by --gap
CLSOCK = "plcl"
SECRET = "ZQXJKVWPY"
WORDS = ("amber basalt cobalt dune ember fjord garnet harbor indigo jasper kelp lagoon marble nectar "
         "onyx pebble quartz ridge saffron tundra umber velvet willow xenon yarrow zephyr").split()

SGR = re.compile(r"\x1b\[([0-9;:]*)m")


def sh(cmd, **kw):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True, **kw)


def ssh_a(cmd, timeout=90):
    return subprocess.run(SSH + [cmd], capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)


# ---------------------------------------------------------------- tmux control pipe
class Ctl:
    def __init__(self, sock, sess="s"):
        self.p = subprocess.Popen(["tmux", "-L", sock, "-C", "attach", "-t", sess, "-f", "ignore-size"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0)
        self.f = self.p.stdout
        self.sess = sess
        self._read_block()  # the attach command's own (empty) reply

    def _read_block(self):
        out, inb = [], False
        while True:
            l = self.f.readline()
            if not l:
                raise RuntimeError("tmux control pipe closed")
            l = l.decode("utf-8", "replace")
            if l.startswith("%begin"):
                inb = True
            elif l.startswith("%end") or l.startswith("%error"):
                return "".join(out)
            elif inb:
                out.append(l)

    def cmd(self, c):
        self.p.stdin.write((c + "\n").encode())
        self.p.stdin.flush()
        return self._read_block()

    def capture(self, esc=True):
        return self.cmd("capture-pane -p%s -t %s" % ("e" if esc else "", self.sess))

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(2)
        except Exception:
            self.p.kill()


def tq(s):
    """Quote a literal for the tmux command parser."""
    return "'" + s.replace("'", "'\\''") + "'" if "'" not in s else '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


# ---------------------------------------------------------------- screen parsing
def cells(raw):
    """One capture-pane -e row -> list of (char, underlined)."""
    out, ul, pos = [], False, 0

    def put(txt):
        for ch in txt:
            out.append((ch, ul))

    for m in SGR.finditer(raw):
        put(raw[pos:m.start()])
        pos = m.end()
        ps = [p for p in re.split(r"[;:]", m.group(1))] or ["0"]
        i = 0
        while i < len(ps):
            p = ps[i]
            if p in ("", "0"):
                ul = False
            elif p == "4":
                ul = True
            elif p == "24":
                ul = False
            elif p in ("38", "48"):
                i += 2 if i + 1 < len(ps) and ps[i + 1] == "5" else 4
                continue
            i += 1
    put(raw[pos:])
    return out


def rows_of(raw):
    return [cells(r) for r in raw.split("\n")]


def plain(row):
    return "".join(c for c, _ in row)


def norm(txt):
    ls = [l.rstrip() for l in txt.split("\n")]
    while ls and not ls[-1]:
        ls.pop()
    return "\n".join(ls)


def lcp(a, b):
    n = 0
    for x, y in zip(a, b):
        if x != y:
            break
        n += 1
    return n


def best_row(rows, text):
    """(n, cells_offset, row) of the row whose body (after an optional '$ ') shares the longest prefix with text."""
    best = (0, 0, None)
    for r in rows:
        p = plain(r)
        off = 2 if p.startswith("$ ") else 0
        n = lcp(p[off:], text)
        if n > best[0]:
            best = (n, off, r)
    return best


def pct(xs, q):
    if not xs:
        return None
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(q * (len(xs) - 1))))]


def stat(xs):
    xs = [x for x in xs if x is not None]
    if not xs:
        return {"n": 0}
    return {"n": len(xs), "med": round(statistics.median(xs)), "p90": round(pct(xs, 0.9)), "max": round(max(xs))}


# ---------------------------------------------------------------- session
class Session:
    def __init__(self, client, wrap=False, cols=80, rows=24):
        self.client, self.wrap, self.cols, self.rows = client, wrap, cols, rows
        self.log, self.sent, self.checks = [], [], []
        self.last_raw = None
        self.spid = None
        self.pidc = None

    def start(self):
        sc = "%s/%s.sh" % (SRVDIR, "wrap" if self.wrap else "plain")
        r = ssh_a("SSH_CONNECTION='%s 1 %s 22' mosh-server new -s -c 256 -l LANG=C.UTF-8 -- %s" % (B, A, sc), 90)
        m = re.search(r"^MOSH CONNECT (\d+) (\S{22})$", r.stdout, re.M)
        pm = re.search(r"pid = (\d+)", r.stdout + r.stderr)
        if not m or not pm:
            raise RuntimeError("no MOSH CONNECT: %r %r" % (r.stdout, r.stderr))
        port, key = m.group(1), m.group(2)
        self.spid = int(pm.group(1))
        c = self.client
        if c.startswith("moche-"):
            cmd = "env LC_ALL=C.UTF-8 MOSH_KEY=%s %s --predict=%s --stats --direct %s %s 2>/tmp/pred-stats.txt" % (
                key, MOCHE, c.split("-", 1)[1], A, port)
            self.pat = "%s --predict=%s --stats --direct %s %s" % (os.path.basename(MOCHE), c.split("-", 1)[1], A, port)
        elif c == "stock":
            cmd = "env LC_ALL=C.UTF-8 MOSH_KEY=%s mosh-client %s %s" % (key, A, port)
            self.pat = "mosh-client %s %s" % (A, port)
        else:
            raise ValueError(c)
        sh("tmux -L %s kill-server" % CLSOCK)
        sh("tmux -L %s new-session -d -s s -x %d -y %d %s" % (CLSOCK, self.cols, self.rows, tq(cmd)))
        self.ctl = Ctl(CLSOCK)
        time.sleep(0.3)
        p = sh("pgrep -f %s | head -1" % tq(self.pat)).stdout.strip()
        self.pidc = int(p) if p else None

    def stop(self):
        try:
            self.ctl.close()
        except Exception:
            pass
        sh("tmux -L %s kill-server" % CLSOCK)
        if self.spid:
            ssh_a("kill %d 2>/dev/null; tmux -L srv kill-server 2>/dev/null; tmux -L nest kill-server 2>/dev/null; true" % self.spid, 60)

    # -- sampling
    def sample(self):
        raw = self.ctl.capture(True)
        ts = time.time()
        if raw != self.last_raw:
            self.log.append((ts, raw))
            self.last_raw = raw
        return raw

    def send_chars(self, s, ts=None):
        self.sent.append((ts or time.time(), "c", s))
        self.ctl.cmd("send-keys -t s -l %s" % tq(s))

    def send_key(self, k):
        self.sent.append((time.time(), "k", k))
        self.ctl.cmd("send-keys -t s %s" % k)

    def drive(self, events, tail=2.0, step=0.010):
        """events: sorted list of (t_rel, fn). Runs fn(), sampling every `step` s meanwhile."""
        t0 = time.time()
        i, end = 0, (events[-1][0] if events else 0) + tail
        while True:
            now = time.time() - t0
            while i < len(events) and events[i][0] <= now:
                events[i][1]()
                i += 1
            self.sample()
            now = time.time() - t0
            if now > end and i >= len(events):
                break
            nxt = events[i][0] if i < len(events) else now + step
            time.sleep(max(0.0, min(step, nxt - (time.time() - t0))))

    def wait_prompt(self, timeout=40):
        t = time.time()
        while time.time() - t < timeout:
            raw = self.sample()
            if any(plain(r) == "$" or plain(r).startswith("$ ") for r in rows_of(raw)):
                return True
            time.sleep(0.05)
        raise RuntimeError("no prompt:\n" + self.last_raw)

    def warmup(self, rng):
        pl = Plan(rng)
        for i in range(3):
            pl.type("# warm %d %s" % (i, rng.choice(WORDS)))
            pl.key("Enter", 0.6)
        self.run_plan(pl, tail=3.0)
        self.sent = []

    def run_plan(self, pl, tail=2.0):
        evs = []
        for t, kind, v in pl.ev:
            if kind == "c":
                evs.append((t, lambda v=v: self.send_chars(v)))
            elif kind == "k":
                evs.append((t, lambda v=v: self.send_key(v)))
            elif kind == "mark":
                evs.append((t, lambda v=v: self.checks.append(self.check(*v))))
            elif kind == "call":
                evs.append((t, v))
        self.drive(evs, tail=tail)

    # -- quiescence and truth
    def settle(self, maxwait=30.0, quiet=2.0):
        t0 = time.time()
        last_change = time.time()
        prev = self.last_raw
        while time.time() - t0 < maxwait:
            raw = self.sample()
            if raw != prev:
                prev, last_change = raw, time.time()
            elif time.time() - last_change >= quiet:
                return last_change
            time.sleep(0.05)
        return None

    def truth(self, sock="srv"):
        r = ssh_a("tmux -L %s capture-pane -p -t %s; echo @@; tmux -L %s display -p -t %s '#{cursor_x},#{cursor_y}'" %
                  (sock, sock, sock, sock), 90)
        txt, _, cur = r.stdout.partition("@@\n")
        return norm(txt), cur.strip()

    def check(self, name, sock="srv", redraw=False):
        t_last_key = self.sent[-1][0] if self.sent else time.time()
        lc = self.settle()
        res = {"name": name, "settled": lc is not None, "converge_ms": None if lc is None else round(max(0, lc - t_last_key) * 1000)}
        raw_e = self.last_raw
        disp = norm(self.ctl.capture(False))
        dcur = self.ctl.cmd("display-message -p -t s '#{cursor_x},#{cursor_y}'").strip()
        if redraw:  # truth = what the server redraws on Ctrl-L (alternate-screen programs have no server-side pane)
            self.ctl.cmd("send-keys -t s C-l")
            self.settle()
            tr = norm(self.ctl.capture(False))
            tcur = self.ctl.cmd("display-message -p -t s '#{cursor_x},#{cursor_y}'").strip()
        else:
            tr, tcur = self.truth(sock)
        res.update(text_equal=(disp == tr), cursor_equal=(dcur == tcur), underline_residue=any(u for r in rows_of(raw_e) for _, u in r))
        if disp != tr:
            res["display"], res["truth"] = disp, tr
        if dcur != tcur:
            res["cursor"] = {"display": dcur, "truth": tcur}
        return res


class Plan:
    def __init__(self, rng):
        self.ev, self.t, self.rng = [], 0.0, rng

    def type(self, text, gap=(0.08, 0.12)):
        for ch in text:
            self.ev.append((self.t, "c", ch))
            self.t += self.rng.uniform(*gap)

    def key(self, k, after=0.1):
        self.ev.append((self.t, "k", k))
        self.t += after

    def wait(self, s):
        self.t += s

    def mark(self, name, sock="srv", redraw=False):
        self.ev.append((self.t, "mark", (name, sock, redraw)))
        self.t += 0.1

    def call(self, fn, after=0.1):
        self.ev.append((self.t, "call", fn))
        self.t += after


# ---------------------------------------------------------------- analysis of a typed text
def track(log, text, sent_times, t_end=None, ok_prefix=0):
    """Per char: first visible time, underline at first sight, time the underline went away.
    log: [(ts, raw)] changes; sent_times: ts of each char of text. Also retractions and wrong time."""
    n_ = len(text)
    vis, ulf, ulg = [None] * n_, [None] * n_, [None] * n_
    retr, prev_n = 0, 0
    wrong = 0.0
    t_start = sent_times[0]
    t_stop = t_end if t_end is not None else float("inf")
    for idx, (ts, raw) in enumerate(log):
        if ts < t_start or ts > t_stop:
            continue
        n, off, row = best_row(rows_of(raw), text)
        if n < prev_n:
            retr += 1
        prev_n = n
        if n > ok_prefix:
            nxt = log[idx + 1][0] if idx + 1 < len(log) else ts
            wrong += max(0.0, min(nxt, t_stop) - ts)
        for k in range(n):
            if sent_times[k] > ts:
                break
            u = row[off + k][1] if off + k < len(row) else False
            if vis[k] is None:
                vis[k], ulf[k] = ts, u
            if ulf[k] and ulg[k] is None and not u and vis[k] is not None and ts > vis[k]:
                ulg[k] = ts
    return {"vis": vis, "ulf": ulf, "ulg": ulg, "retractions": retr, "shown_s": wrong}


def lines_of_sent(sent):
    """Split the sent record into typed lines [(text, [ts...], t_enter)]."""
    out, cur, ts = [], "", []
    for t, kind, v in sent:
        if kind == "c":
            cur += v
            ts += [t] * len(v)
        elif kind == "k" and v == "Enter":
            if cur:
                out.append((cur, ts, t))
            cur, ts = "", []
    return out


# ---------------------------------------------------------------- latency
def run_latency(a):
    rng = random.Random(a.seed)
    s = Session(a.client)
    res = {"client": a.client, "kind": "latency"}
    try:
        s.start()
        s.wait_prompt()
        s.warmup(rng)
        pl = Plan(rng)
        for i in range(a.lines):
            pl.type("# L%d %s" % (i, " ".join(rng.choice(WORDS) for _ in range(5))), gap=GAPS)
            pl.key("Enter", rng.uniform(0.6, 1.0))
        s.run_plan(pl, tail=3.0)
        chars = []
        retr = 0
        for text, ts, t_enter in lines_of_sent(s.sent):
            tr = track(s.log, text, ts, t_end=t_enter)
            retr += tr["retractions"]
            for k, ch in enumerate(text):
                lat = None if tr["vis"][k] is None else (tr["vis"][k] - ts[k]) * 1000
                ulgone = None if not tr["ulf"][k] or tr["ulg"][k] is None else (tr["ulg"][k] - tr["vis"][k]) * 1000
                chars.append({"k": k, "lat_ms": None if lat is None else round(lat, 1), "ul": bool(tr["ulf"][k]),
                              "ul_gone_ms": None if ulgone is None else round(ulgone, 1), "after_space": k > 0 and text[k - 1] == " ", "ch": ch})
        # a typed space is invisible in capture-pane (trailing blanks are trimmed): not measured
        chars = [c for c in chars if c["ch"] != " "]
        lat_first = [c["lat_ms"] for c in chars if c["k"] == 0]
        lat_k13 = [c["lat_ms"] for c in chars if 1 <= c["k"] <= 3]
        lat_rest = [c["lat_ms"] for c in chars if c["k"] >= 4]
        lat_word = [c["lat_ms"] for c in chars if c["after_space"]]
        allv = [c["lat_ms"] for c in chars]
        seen = [x for x in allv if x is not None]
        res.update({
            "chars": len(chars), "never_visible": len(allv) - len(seen), "retractions": retr,
            "lat_all": stat(allv), "lat_first_of_line": stat(lat_first), "lat_chars_2_4": stat(lat_k13),
            "lat_chars_5plus": stat(lat_rest), "lat_first_of_word": stat(lat_word),
            "within30_per100": round(100.0 * sum(1 for x in seen if x <= 30) / max(1, len(allv)), 1),
            "within60_per100": round(100.0 * sum(1 for x in seen if x <= 60) / max(1, len(allv)), 1),
            "underlined_share": round(sum(1 for c in chars if c["ul"]) / max(1, len(chars)), 3),
            "ul_gone": stat([c["ul_gone_ms"] for c in chars]),
        })
        s.settle()
        res["final_underline_residue"] = any(u for r in rows_of(s.last_raw) for _, u in r)
        res["per_char"] = chars
    except Exception as e:
        res["error"] = "%s: %s" % (type(e).__name__, e)
    finally:
        s.stop()
    return res


# ---------------------------------------------------------------- scenarios
def scen_plan(name, s, rng, pl):
    """Fill the plan for one scenario."""
    if name == "shell_edit":
        pl.type("echo alpha beta gamma"); pl.mark("typed")
        for _ in range(6): pl.key("BSpace", 0.09)
        pl.mark("after_bs")
        for _ in range(4): pl.key("Left", 0.09)
        pl.type("XY"); pl.mark("mid_insert")
        for _ in range(2): pl.key("Right", 0.09)
        pl.type("!!"); pl.mark("right_then_type")
        pl.key("C-u"); pl.mark("cleared")
        pl.type("echo done"); pl.key("Enter", 0.5); pl.mark("enter")
    elif name == "wrap":
        pl.type("".join(rng.choice("abcdefghij") for _ in range(110))); pl.mark("wrapped")
        pl.key("C-u"); pl.mark("cleared")
        pl.type("# x" * 30); pl.key("Enter", 0.5); pl.mark("enter")
    elif name == "password":
        pl.type("stty -echo; read x"); pl.key("Enter", 2.0)
        pl.call(lambda: s.checks.append({"name": "secret_start", "t": time.time()}))
        pl.type(SECRET); pl.wait(1.0)
        pl.call(lambda: s.checks.append({"name": "secret_end", "t": time.time()}))
        pl.mark("secret_typed"); pl.key("Enter", 0.5)
        pl.type("stty echo; echo done"); pl.key("Enter", 0.5); pl.mark("after")
    elif name == "vim_insert_normal":
        pl.type("vim -u NONE -N /tmp/pv.txt"); pl.key("Enter", 2.5)
        pl.type("i"); pl.wait(0.3)
        pl.call(lambda: s.checks.append({"name": "ins_start", "t": time.time()}))
        pl.type("the quick brown fox jumps over"); pl.key("Escape", 0.5); pl.mark("insert_done")
        pl.call(lambda: s.checks.append({"name": "norm_start", "t": time.time()}))
        for k in "hjklx": pl.key(k, 0.11)
        pl.type("dd", gap=(0.1, 0.12)); pl.type("u"); pl.type("0"); pl.type("x"); pl.type("p")
        pl.mark("normal_done")
        pl.type("ihello"); pl.key("Escape", 0.4); pl.mark("insert2")
        pl.type(":q!"); pl.key("Enter", 1.0); pl.mark("quit")
    elif name == "vim_plain":  # server runs bash directly: vim really uses the alternate screen
        pl.type("vim -u NONE -N /tmp/pv.txt"); pl.key("Enter", 2.5)
        pl.type("i"); pl.wait(0.3)
        pl.call(lambda: s.checks.append({"name": "ins_start", "t": time.time()}))
        pl.type("the quick brown fox jumps over"); pl.key("Escape", 0.5); pl.mark("insert_done", redraw=True)
        pl.call(lambda: s.checks.append({"name": "norm_start", "t": time.time()}))
        for k in "hjklx": pl.key(k, 0.11)
        pl.type("dd", gap=(0.1, 0.12)); pl.type("u"); pl.type("0"); pl.type("x"); pl.type("p")
        pl.call(lambda: s.checks.append({"name": "norm_end", "t": time.time()}))
        pl.mark("normal_done", redraw=True)
        pl.type("ihello"); pl.key("Escape", 0.4); pl.mark("insert2", redraw=True)
        pl.type(":q!"); pl.key("Enter", 1.0)
    elif name == "nested_plain":  # server runs bash directly: the inner tmux uses the alternate screen
        pl.type("tmux -L nest -f %s/srv.conf new-session -s nest" % SRVDIR); pl.key("Enter", 2.5)
        pl.type("# nested typing line"); pl.mark("typed", sock="nest")
        for _ in range(3): pl.key("BSpace", 0.1)
        pl.mark("bs", sock="nest")
        pl.key("C-u", 0.3); pl.type("echo inner"); pl.key("Enter", 0.8); pl.mark("enter", sock="nest")
        pl.type("exit"); pl.key("Enter", 1.0)
    elif name == "transform2":  # first 3 chars echoed as typed (confirms the epoch), then upper-cased: contradiction
        pl.type("stty -echo; n=0; while IFS= read -rsn1 c; do n=$((n+1)); if [ $n -le 3 ]; then printf %s \"$c\"; else printf %s \"${c^^}\"; fi; done"); pl.key("Enter", 1.5)
        pl.call(lambda: s.checks.append({"name": "t0", "t": time.time()}))
        pl.type("abcdefghijklmnopqrst"); pl.wait(1.5); pl.mark("typed")
        pl.key("C-c", 0.5); pl.type("stty echo"); pl.key("Enter", 0.5); pl.mark("after")
    elif name == "slow_echo2":  # first 3 chars echoed at once (confirms), then 0.3 s per char: backlog
        pl.type("stty -echo; n=0; while IFS= read -rsn1 c; do n=$((n+1)); [ $n -le 3 ] || sleep 0.3; printf %s \"$c\"; done"); pl.key("Enter", 1.5)
        pl.call(lambda: s.checks.append({"name": "t0", "t": time.time()}))
        pl.type("abcdefghijklmnopqrstuvwx"); pl.wait(9.0); pl.mark("drained")
        pl.key("C-c", 0.5); pl.type("stty echo"); pl.key("Enter", 0.5); pl.mark("after")
    elif name == "mid_insert":  # typing inside a line, text to the right of the cursor
        pl.type("echo " + "a" * 20); pl.wait(0.5)
        for _ in range(10): pl.key("Left", 0.09)
        pl.wait(0.5)
        pl.call(lambda: s.checks.append({"name": "t0", "t": time.time()}))
        pl.type("0123456789"); pl.wait(1.5); pl.mark("inserted")
        pl.key("C-u"); pl.mark("cleared")
    elif name == "cat_echo":
        pl.type("cat"); pl.key("Enter", 1.0)
        pl.type("hello cat one"); pl.key("Enter", 0.8)
        pl.type("second line here"); pl.key("Enter", 0.8)
        pl.mark("typed"); pl.key("C-d", 0.5); pl.mark("eof")
    elif name == "slow_echo":
        pl.type("stty -echo; while IFS= read -rsn1 c; do sleep 0.2; printf %s \"$c\"; done"); pl.key("Enter", 1.5)
        pl.call(lambda: s.checks.append({"name": "t0", "t": time.time()}))
        pl.type("abcdefghijklmnopqrstuvwx"); pl.wait(7.0); pl.mark("drained")
        pl.key("C-c", 0.5); pl.type("stty echo"); pl.key("Enter", 0.5); pl.mark("after")
    elif name == "transform":
        pl.type("stty -echo; while IFS= read -rsn1 c; do printf %s \"${c^^}\"; done"); pl.key("Enter", 1.5)
        pl.call(lambda: s.checks.append({"name": "t0", "t": time.time()}))
        pl.type("abcdefghijklmnopqrst"); pl.wait(1.5); pl.mark("typed")
        pl.key("C-c", 0.5); pl.type("stty echo"); pl.key("Enter", 0.5); pl.mark("after")
    elif name == "nested_tmux":
        pl.type("tmux -L nest -f %s/srv.conf new-session -s nest" % SRVDIR); pl.key("Enter", 2.5)
        pl.type("# nested typing line"); pl.mark("typed")
        for _ in range(3): pl.key("BSpace", 0.1)
        pl.mark("bs")
        pl.key("C-u", 0.3); pl.type("echo inner"); pl.key("Enter", 0.8); pl.mark("enter")
        pl.type("exit"); pl.key("Enter", 1.0)
    elif name == "resize":
        pl.type("# resize while typing")
        pl.call(lambda: sh("tmux -L %s resize-window -t s -x 60 -y 20" % CLSOCK))
        pl.type(" line two is typed during the resize")
        pl.call(lambda: sh("tmux -L %s resize-window -t s -x 80 -y 24" % CLSOCK))
        pl.type(" end"); pl.mark("resized"); pl.key("C-u"); pl.mark("cleared")
    elif name == "ctrl_c_enter":
        pl.type("sleep 30"); pl.key("Enter", 1.2)
        pl.type("zzz"); pl.mark("typed_while_running"); pl.key("C-c", 1.2)
        pl.type("echo after cc"); pl.key("Enter", 0.5); pl.mark("after_cc")
        pl.type("# a"); pl.key("Enter", 0.0); pl.type("# b"); pl.key("Enter", 0.0); pl.type("# c"); pl.mark("fast_enter")
        pl.key("C-u"); pl.mark("cleared")
    elif name == "paste200":
        txt = "".join(rng.choice("abcdefghij ") for _ in range(200))
        pl.call(lambda: s.checks.append({"name": "paste_t", "t": time.time()}))
        pl.call(lambda: s.send_chars("# " + txt))
        pl.wait(1.5); pl.mark("pasted_raw"); pl.key("C-u"); pl.mark("cleared")
        pl.call(lambda: (sh("tmux -L %s set-buffer %s" % (CLSOCK, tq("# " + txt))),
                         s.sent.append((time.time(), "k", "paste-p")),
                         sh("tmux -L %s paste-buffer -p -t s" % CLSOCK)))
        pl.wait(1.5); pl.mark("pasted_bracketed"); pl.key("C-u"); pl.mark("cleared2")
    else:
        raise ValueError(name)


def run_scenario(a):
    rng = random.Random(a.seed)
    s = Session(a.client, wrap=a.name not in ("vim_plain", "nested_plain"))
    res = {"client": a.client, "kind": "scenario", "name": a.name}
    try:
        s.start()
        s.wait_prompt()
        s.warmup(rng)
        pl = Plan(rng)
        scen_plan(a.name, s, rng, pl)
        s.run_plan(pl, tail=1.0)
        s.settle()
        res["checks"] = [c for c in s.checks if "converge_ms" in c]
        marks = {c["name"]: c["t"] for c in s.checks if "t" in c}
        res["all_equal"] = all(c["text_equal"] and c["cursor_equal"] for c in res["checks"])
        res["residue"] = any(c["underline_residue"] for c in res["checks"])
        res["worst_converge_ms"] = max([c["converge_ms"] or 0 for c in res["checks"]] + [0])
        res["unsettled"] = [c["name"] for c in res["checks"] if not c["settled"]]
        # scenario specific observations
        if a.name == "password":
            t0, t1 = marks["secret_start"], marks["secret_end"]
            leak_chars, leak_s = 0, 0.0
            for i, (ts, raw) in enumerate(s.log):
                if ts < t0:
                    continue
                nxt = s.log[i + 1][0] if i + 1 < len(s.log) else ts
                if ts > t1 + 0.5:
                    break
                n = sum(1 for r in rows_of(raw) for c, _ in r if c in SECRET)
                if n:
                    leak_chars = max(leak_chars, n)
                    leak_s += max(0, min(nxt, t1 + 0.5) - ts)
            res["secret_max_chars_visible"] = leak_chars
            res["secret_visible_seconds"] = round(leak_s, 2)
        elif a.name in ("transform", "slow_echo", "transform2", "slow_echo2"):
            text = "abcdefghijklmnopqrst" if a.name.startswith("transform") else "abcdefghijklmnopqrstuvwx"
            t0 = marks["t0"]
            sent_t = [t for t, k, v in s.sent if k == "c" and t >= t0][: len(text)]
            tr = track(s.log, text, sent_t, ok_prefix=3 if a.name.endswith("2") else 0)
            lat = [None if v is None else round((v - t) * 1000) for v, t in zip(tr["vis"], sent_t)]
            res["lowercase_visible_seconds" if a.name.startswith("transform") else "typed_visible_seconds"] = round(tr["shown_s"], 2)
            res["retractions"] = tr["retractions"]
            res["lat_ms"] = lat
            res["chars_ever_lowercase_visible" if a.name.startswith("transform") else "chars_ever_visible"] = sum(1 for v in tr["vis"] if v is not None)
        elif a.name in ("vim_insert_normal", "vim_plain"):
            t0 = marks["ins_start"]
            text = "the quick brown fox jumps over"
            sent_t = [t for t, k, v in s.sent if k == "c" and t >= t0][: len(text)]
            tr = track(s.log, text, sent_t, t_end=marks["norm_start"])
            res["insert_lat_ms"] = stat([None if v is None else (v - t) * 1000 for v, t, ch in zip(tr["vis"], sent_t, text) if ch != " "])
            res["insert_underlined_share"] = round(sum(1 for u in tr["ulf"] if u) / len(text), 2)
            if "norm_end" in marks:  # NORMAL-mode keys (hjklx dd u 0 x p) must not show ghost chars: count underlined cells
                ule = [sum(1 for r in rows_of(raw) for _, u in r if u) for ts, raw in s.log if marks["norm_start"] <= ts <= marks["norm_end"]]
                res["normal_ul_cells_max"] = max(ule + [0])
                res["normal_ul_entries"] = sum(1 for x in ule if x)
        elif a.name == "mid_insert":
            text = "0123456789"
            t0 = marks["t0"]
            sent_t = [t for t, k, v in s.sent if k == "c" and t >= t0][: len(text)]
            vis, ulf = [None] * 10, [None] * 10
            for ts, raw in s.log:
                if ts < t0:
                    continue
                for r in rows_of(raw):
                    pl_ = plain(r)
                    for k in range(10):
                        key = "echo " + "a" * 10 + text[:k + 1] + "a" * 10
                        pos = pl_.find(key)
                        if vis[k] is None and pos >= 0 and sent_t[k] <= ts:
                            vis[k], ulf[k] = ts, bool(r[pos + 15 + k][1])
            res["insert_lat_ms"] = stat([None if v is None else (v - t) * 1000 for v, t in zip(vis, sent_t)])
            res["insert_underlined_share"] = round(sum(1 for u in ulf if u) / 10, 2)
        elif a.name == "paste200":
            t0 = marks["paste_t"]
            base = max([sum(1 for r in rows_of(raw) for c, _ in r if c != " ") for ts, raw in s.log if ts < t0] + [0])
            first = None
            for ts, raw in s.log:
                if ts >= t0 and sum(1 for r in rows_of(raw) for c, _ in r if c != " ") > base + 20:
                    first = ts
                    break
            res["paste_first_visible_ms"] = None if first is None else round((first - t0) * 1000)
            ul = max([sum(1 for r in rows_of(raw) for _, u in r if u) for ts, raw in s.log if ts >= t0] + [0])
            res["paste_max_underlined_cells"] = ul
        elif a.name in ("shell_edit", "nested_tmux", "ctrl_c_enter", "cat_echo", "wrap", "resize"):
            lines = lines_of_sent(s.sent)
            res["retractions_total"] = sum(track(s.log, t, ts, t_end=te)["retractions"] for t, ts, te in lines)
    except Exception as e:
        res["error"] = "%s: %s" % (type(e).__name__, e)
    finally:
        s.stop()
    return res


# ---------------------------------------------------------------- cost
def proc_cpu(pid):
    f = open("/proc/%d/stat" % pid).read().rsplit(")", 1)[1].split()
    return int(f[11]), int(f[12])  # utime, stime in ticks


def proc_ctx(pid):
    tot = 0
    for t in os.listdir("/proc/%d/task" % pid):
        try:
            for l in open("/proc/%d/task/%s/status" % (pid, t)):
                if "ctxt_switches" in l:
                    tot += int(l.split()[1])
        except OSError:
            pass
    return tot


def proc_rss(pid):
    d = {}
    for l in open("/proc/%d/status" % pid):
        if l.startswith(("VmRSS", "VmHWM")):
            d[l.split(":")[0]] = int(l.split()[1])
    return d


def typing_plan(rng, secs):
    pl = Plan(rng)
    while pl.t < secs:
        pl.type("# " + " ".join(rng.choice(WORDS) for _ in range(6)))
        pl.key("Enter", rng.uniform(0.3, 0.6))
    return pl


def run_cost(a):
    rng = random.Random(a.seed)
    s = Session(a.client)
    res = {"client": a.client, "kind": "cost"}
    try:
        s.start()
        s.wait_prompt()
        s.warmup(rng)
        pid = s.pidc
        assert pid, "no client pid"
        time.sleep(1)
        u0, k0 = proc_cpu(pid); c0 = proc_ctx(pid); t0 = time.time()
        pl = typing_plan(rng, a.secs)
        nkeys = sum(1 for e in pl.ev if e[1] in ("c", "k"))
        s.run_plan(pl, tail=1.0)
        dt = time.time() - t0
        u1, k1 = proc_cpu(pid); c1 = proc_ctx(pid)
        res.update(keys=nkeys, secs=round(dt, 1), cpu_user_ms=(u1 - u0) * 10, cpu_sys_ms=(k1 - k0) * 10,
                   cpu_ms_total=(u1 - u0 + k1 - k0) * 10, cpu_ms_per_key=round((u1 - u0 + k1 - k0) * 10 / nkeys, 2),
                   wakeups_per_s=round((c1 - c0) / dt, 1), rss_kb=proc_rss(pid))
        # strace window: mmap-family calls per key
        s.sent = []
        outf = "/tmp/pred-strace.txt"
        sh("rm -f %s" % outf)
        st = subprocess.Popen(["sudo", "strace", "-f", "-c", "-o", outf, "-e", "trace=mmap,munmap,mremap,brk,madvise", "-p", str(pid)],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.0)
        pl = typing_plan(rng, a.secs * 0.7)
        nk2 = sum(1 for e in pl.ev if e[1] in ("c", "k"))
        s.run_plan(pl, tail=1.0)
        sh("sudo pkill -INT strace")
        try:
            st.wait(10)
        except Exception:
            pass
        counts = {}
        if os.path.exists(outf):
            for l in open(outf):
                p = l.split()
                if len(p) >= 5 and p[-1] in ("mmap", "munmap", "mremap", "brk", "madvise"):
                    counts[p[-1]] = int(p[3])
        res["strace_keys"] = nk2
        res["strace_counts"] = counts
        res["strace_mmap_family_per_key"] = round(sum(counts.get(k, 0) for k in ("mmap", "munmap", "mremap")) / nk2, 3)
        sh("rm -f %s" % outf)
    except Exception as e:
        res["error"] = "%s: %s" % (type(e).__name__, e)
    finally:
        sh("sudo pkill -INT strace")
        s.stop()
    return res


# ---------------------------------------------------------------- wire
def run_wire(a):
    rng = random.Random(a.seed)
    res = {"client": a.client, "kind": "wire"}
    out = "/tmp/pred-tcpdump.txt"
    td = None
    s = Session(a.client)
    try:
        sh("rm -f %s" % out)
        td = subprocess.Popen(["sudo", "tcpdump", "-n", "-q", "-tt", "-l", "-i", "eth0", "udp and src host %s and dst host %s" % (B, A)],
                              stdout=open(out, "w"), stderr=subprocess.DEVNULL)
        time.sleep(1.0)
        s.start()
        s.wait_prompt()
        time.sleep(3)
        s.sent = []
        pl = Plan(rng)
        for i in range(a.lines):
            pl.type("# W%d %s" % (i, " ".join(rng.choice(WORDS) for _ in range(5))))
            pl.key("Enter", 0.7)
        s.run_plan(pl, tail=3.0)
        time.sleep(1.0)
        sh("sudo pkill -INT tcpdump")
        time.sleep(0.5)
        if s.pidc:
            sh("kill -TERM %d" % s.pidc)
            time.sleep(1.5)
        sizes = []
        for l in open(out):
            m = re.search(r"UDP, length (\d+)", l)
            if m:
                sizes.append(int(m.group(1)))
        res.update(datagrams=len(sizes), bytes=sum(sizes), sizes_sorted=sorted(sizes))
        if a.client.startswith("moche") and os.path.exists("/tmp/pred-stats.txt"):
            res["stats"] = open("/tmp/pred-stats.txt").read().strip()
    except Exception as e:
        res["error"] = "%s: %s" % (type(e).__name__, e)
    finally:
        sh("sudo pkill -INT tcpdump")
        s.stop()
        sh("rm -f %s /tmp/pred-stats.txt" % out)
    return res


def main():
    global GAPS
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["latency", "scenario", "cost", "wire"])
    ap.add_argument("--client", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--name", default="")
    ap.add_argument("--lines", type=int, default=6)
    ap.add_argument("--secs", type=float, default=30)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--tag", default="")
    ap.add_argument("--gap", type=float, default=100.0, help="mean key gap in ms for latency typing (+-20%%)")
    a = ap.parse_args()
    GAPS = (a.gap * 0.0008, a.gap * 0.0012)
    for k in ("PRED_A_IP", "PRED_B_IP", "PRED_KEY"):
        if not os.environ.get(k):
            sys.exit("set " + k)
    res = {"latency": run_latency, "scenario": run_scenario, "cost": run_cost, "wire": run_wire}[a.mode](a)
    res["tag"] = a.tag
    res["t"] = time.strftime("%Y-%m-%dT%H:%M:%S")
    with open(a.out, "w") as f:
        json.dump(res, f)
    brief = {k: v for k, v in res.items() if k not in ("per_char", "checks", "sizes_sorted")}
    print(json.dumps(brief)[:1500])


if __name__ == "__main__":
    main()
