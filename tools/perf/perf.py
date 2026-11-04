"""moche resource measurement driver (called by measure.sh; python3 stdlib + tmux only).

Runs scenarios against a real stock mosh-server with `moche --direct` and the stock
`mosh-client`, driven through a dedicated tmux server (-L mochperf), and samples the client
process from outside (Linux: /proc; macOS: libproc via ctypes, ps as fallback).
Writes one JSON per run + env.json + static.json under --out.  See docs/PERF.md.
"""
import argparse, base64, hashlib, json, os, platform, random, re, shlex, shutil, signal
import struct, subprocess, sys, tempfile, threading, time

SOCK = "mochperf"
SESSION = "mp"
SCENARIOS = ["startup", "idle", "typing", "paste", "flood", "colors", "steady"]
IS_LINUX = platform.system() == "Linux"
IS_MAC = platform.system() == "Darwin"


# ---------------------------------------------------------------- process sampling
class Sampler:
    """snap(pid) -> dict of cumulative counters (None when the process is gone)."""

    def __init__(self):
        self.lib = None
        if IS_MAC:
            try:
                import ctypes
                self.ct = ctypes
                self.lib = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
                tb = (ctypes.c_uint32 * 2)()
                self.lib.mach_timebase_info(tb)
                self.tb = (tb[0], tb[1])
            except Exception:
                self.lib = None
        if IS_LINUX:
            self.hz = os.sysconf("SC_CLK_TCK")

    def snap(self, pid, deep=False):
        try:
            if IS_LINUX:
                return self._linux(pid, deep)
            if IS_MAC and self.lib:
                return self._mac(pid)
            return self._ps(pid)
        except (OSError, ValueError, IndexError):
            return None

    def _linux(self, pid, deep):
        base = "/proc/%d" % pid
        with open(base + "/stat") as f:
            s = f.read()
        fl = s[s.rindex(")") + 2:].split()  # fl[0] is field 3 (state)
        minflt, majflt = int(fl[7]), int(fl[9])
        utime, stime, nthr = int(fl[11]), int(fl[12]), int(fl[17])
        st = {}
        with open(base + "/status") as f:
            for line in f:
                k, _, v = line.partition(":")
                st[k] = v.strip()
        vol = invol = 0
        try:  # per-thread counters, summed (the pid-level file only shows the main thread)
            for t in os.listdir(base + "/task"):
                with open("%s/task/%s/status" % (base, t)) as f:
                    for line in f:
                        if line.startswith("voluntary_ctxt_switches"):
                            vol += int(line.split()[1])
                        elif line.startswith("nonvoluntary_ctxt_switches"):
                            invol += int(line.split()[1])
        except OSError:
            pass
        kb = lambda k: int(st[k].split()[0]) if k in st else None
        d = {
            "t": time.time(),
            "cpu_user_s": utime / self.hz, "cpu_sys_s": stime / self.hz,
            "rss_kb": kb("VmRSS"), "peak_rss_kb": kb("VmHWM"), "vsz_kb": kb("VmSize"),
            "threads": nthr, "vol_cs": vol, "invol_cs": invol,
            "faults_minor": minflt, "faults_major": majflt,
            "fds": len(os.listdir(base + "/fd")),
        }
        if deep:
            try:
                with open(base + "/smaps_rollup") as f:
                    for line in f:
                        if line.startswith("Pss:"):
                            d["pss_kb"] = int(line.split()[1])
            except OSError:
                pass
        return d

    def _mac(self, pid):
        ct, lib = self.ct, self.lib
        ru = ct.create_string_buffer(512)
        if lib.proc_pid_rusage(pid, 4, ru) != 0:  # RUSAGE_INFO_V4
            return None
        v = struct.unpack_from("<%dQ" % ((512 - 16) // 8), ru.raw, 16)
        ti = ct.create_string_buffer(96)
        if lib.proc_pidinfo(pid, 4, ct.c_uint64(0), ti, 96) != 96:  # PROC_PIDTASKINFO
            return None
        t = struct.unpack_from("<6Q12i", ti.raw)
        n, dn = self.tb
        ns = lambda x: x * n / dn / 1e9
        want = lib.proc_pidinfo(pid, 1, ct.c_uint64(0), None, 0)  # PROC_PIDLISTFDS (size hint)
        fdb = ct.create_string_buffer(max(want, 8) + 4096)
        nfd = lib.proc_pidinfo(pid, 1, ct.c_uint64(0), fdb, len(fdb))
        return {
            "t": time.time(),
            "cpu_user_s": ns(v[0]), "cpu_sys_s": ns(v[1]),
            "wakeups": v[2] + v[3],  # package-idle + interrupt wakeups
            "rss_kb": t[1] // 1024, "vsz_kb": t[0] // 1024,
            "footprint_kb": v[7] // 1024, "peak_footprint_kb": v[28] // 1024,
            "instructions": v[29], "cycles": v[30],
            "threads": t[15], "csw": t[14],  # total context switches (not split)
            "faults_minor": t[7], "faults_major": t[8],
            "fds": nfd // 8 if nfd >= 0 else None,
        }

    def _ps(self, pid):
        out = subprocess.run(["ps", "-o", "rss=,vsz=,utime=,stime=", "-p", str(pid)],
                             capture_output=True, text=True).stdout.split()
        if len(out) < 4:
            return None

        def tm(x):
            m, s = x.split(":")
            return int(m) * 60 + float(s)
        return {"t": time.time(), "rss_kb": int(out[0]), "vsz_kb": int(out[1]),
                "cpu_user_s": tm(out[2]), "cpu_sys_s": tm(out[3])}


# ---------------------------------------------------------------- tmux helpers
def tmux(*args, check=False):
    r = subprocess.run(["tmux", "-L", SOCK, "-f", "/dev/null", *args],
                       capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError("tmux %s: %s" % (args[0], r.stderr.strip()))
    return r


def pane_text():
    return tmux("capture-pane", "-p", "-J", "-t", SESSION).stdout


def session_alive():
    return tmux("has-session", "-t", SESSION).returncode == 0


def send_line(text):
    tmux("send-keys", "-t", SESSION, "-l", "--", text)
    tmux("send-keys", "-t", SESSION, "Enter")


def find_client_pid(client_name):
    r = tmux("list-panes", "-t", SESSION, "-F", "#{pane_pid}")
    pid = int(r.stdout.split()[0])
    # exec replaces the shell, so pane_pid is normally the client itself; else walk down.
    for _ in range(5):
        comm = subprocess.run(["ps", "-o", "comm=", "-p", str(pid)],
                              capture_output=True, text=True).stdout.strip()
        if os.path.basename(comm).startswith(client_name):
            return pid
        kids = subprocess.run(["pgrep", "-P", str(pid)], capture_output=True, text=True).stdout.split()
        if not kids:
            break
        pid = int(kids[0])
    return pid


# ---------------------------------------------------------------- server handling
class Server:
    def __init__(self, a, lang):
        self.a, self.lang = a, lang
        self.pid = None
        self.host = self.port = self.key = None

    def start(self):
        a = self.a
        if a.server_port:  # static server, already running
            self.host, self.port = a.server_host, a.server_port
            self.key = os.environ.get(a.key_env or "MOSH_KEY")
            if not self.key:
                raise RuntimeError("static server needs the key in $%s" % (a.key_env or "MOSH_KEY"))
            return
        cmd = [a.mosh_server, "new", "-s", "-c", "256", "-l", "LANG=" + self.lang, "--", "/bin/sh"]
        env = dict(os.environ)
        if a.server_ssh:
            cmd = ["ssh", "-o", "BatchMode=yes", *a.ssh_opt, a.server_ssh,
                   " ".join(shlex.quote(c) for c in cmd)]
            self.host = a.server_host
        else:
            env["SSH_CONNECTION"] = "127.0.0.1 1 127.0.0.1 22"  # makes -s bind loopback
            self.host = "127.0.0.1"
        r = subprocess.run(cmd, env=env, capture_output=True, text=True,
                           stdin=subprocess.DEVNULL, timeout=30)
        m = re.search(r"^MOSH CONNECT (\d+) (\S{22})$", r.stdout, re.M)
        if not m:
            raise RuntimeError("no MOSH CONNECT: %s %s" % (r.stdout[:200], r.stderr[:200]))
        self.port, self.key = m.group(1), m.group(2)
        m2 = re.search(r"pid = (\d+)", r.stdout + r.stderr)
        self.pid = int(m2.group(1)) if m2 else None

    def stop(self):
        if not self.pid:
            return  # only servers we started are ever killed
        if self.a.server_ssh:
            subprocess.run(["ssh", "-o", "BatchMode=yes", *self.a.ssh_opt, self.a.server_ssh,
                            "kill %d" % self.pid], capture_output=True, timeout=30)
        else:
            try:
                os.kill(self.pid, signal.SIGTERM)
            except OSError:
                pass
        self.pid = None


# ---------------------------------------------------------------- one run
class Run:
    def __init__(self, a, client, binpath, scenario, rep, lang, sampler):
        self.a, self.client, self.bin = a, client, binpath
        self.scenario, self.rep, self.lang, self.sm = scenario, rep, lang, sampler
        self.samples = []
        self.pid = None
        self.stop_evt = threading.Event()
        self.counter = 0
        self.stats_file = None
        self.t0 = 0.0

    def _poll(self):  # background polling: peak RSS on macOS, coarse timeline
        while not self.stop_evt.wait(self.a.poll):
            s = self.sm.snap(self.pid)
            if s:
                self.samples.append(s)

    def snap(self, deep=False):
        s = self.sm.snap(self.pid, deep)
        if s:
            self.samples.append(s)
        return s

    def marked(self, cmd, timeout=120):
        """Run cmd on the server shell and wait for its end marker. Returns (wall_s, pane)."""
        self.counter += 1
        n = "%d%d" % (os.getpid() % 1000, self.counter)
        t0 = time.time()
        send_line("%s; printf 'MP%%sOK\\n' %s" % (cmd, n))
        want = "MP%sOK" % n
        while time.time() - t0 < timeout:
            txt = pane_text()
            if want in txt:
                return time.time() - t0, txt
            if not session_alive():
                raise RuntimeError("client exited")
            time.sleep(0.05)
        raise RuntimeError("timeout waiting for marker (%s)" % cmd[:40])

    def wait_ready(self):
        t0 = time.time()
        while time.time() - t0 < 30:
            try:
                self.marked(":", timeout=3)
                return
            except RuntimeError as e:
                if "client exited" in str(e):
                    raise
        raise RuntimeError("shell never became ready")

    def launch(self, srv, cols, rows):
        self.stats_file = tempfile.mktemp(prefix="mperf-stats-")
        env = ["-e", "MOSH_KEY=" + srv.key, "-e", "LANG=" + self.lang, "-e", "LC_ALL=" + self.lang]
        if self.client == "moche":
            cmd = "exec %s --stats --direct %s %s 2>%s" % (
                shlex.quote(self.bin), srv.host, srv.port, shlex.quote(self.stats_file))
        else:
            cmd = "exec %s %s %s" % (shlex.quote(self.bin), srv.host, srv.port)
        t0 = time.time()
        tmux("new-session", "-d", "-s", SESSION, "-x", str(cols), "-y", str(rows), *env, cmd, check=True)
        deadline = time.time() + 5
        while time.time() < deadline and not self.pid:
            try:
                self.pid = find_client_pid("moche" if self.client == "moche" else "mosh-client")
            except (IndexError, ValueError):
                time.sleep(0.005)
        if not self.pid:
            raise RuntimeError("client pid not found")
        return t0

    def quit(self):
        """Ctrl-^ . quits both clients; returns when the pane is gone."""
        if not session_alive():
            return
        tmux("send-keys", "-t", SESSION, "C-^")
        time.sleep(0.2)
        tmux("send-keys", "-t", SESSION, "-l", ".")
        for _ in range(60):
            if not session_alive():
                return
            time.sleep(0.1)
        tmux("kill-session", "-t", SESSION)

    # -- scenarios; each returns (window_start_snap, window_end_snap, extra dict)
    def sc_startup(self, srv):
        first = None
        while time.time() - self.t0 < 30:
            if pane_text().strip():
                first = time.time() - self.t0
                break
            if not session_alive():
                raise RuntimeError("client exited before first paint")
            time.sleep(0.01)
        if first is None:
            raise RuntimeError("no paint within 30 s")
        time.sleep(1.0)  # let the connection settle; its CPU is part of the window
        zero = {"t": self.t0, "cpu_user_s": 0.0, "cpu_sys_s": 0.0}
        return zero, self.snap(True), {"first_paint_s": first}

    def sc_idle(self, srv):
        self.wait_ready()
        time.sleep(3)
        w0 = self.snap()
        time.sleep(self.a.idle_secs)
        return w0, self.snap(True), {}

    def sc_typing(self, srv):
        self.wait_ready()
        time.sleep(1)
        text = "the quick brown fox jumps over the lazy dog "
        n = int(self.a.typing_secs * 10)
        typed = "".join(text[i % len(text)] for i in range(n))
        w0 = self.snap()
        start = time.time()
        for i, ch in enumerate(typed):
            tmux("send-keys", "-t", SESSION, "-l", "--", ch)
            delay = start + (i + 1) * 0.1 - time.time()
            if delay > 0:
                time.sleep(delay)
        time.sleep(1.5)
        ok = typed[-20:] in pane_text().replace("\n", "")
        w1 = self.snap(True)
        tmux("send-keys", "-t", SESSION, "C-u")
        return w0, w1, {"chars": n, "tail_seen": ok}

    def sc_paste(self, srv):
        self.wait_ready()
        b64 = base64.encodebytes(random.Random(1234).randbytes(786432))  # ~1 MiB, 76-col lines
        want = hashlib.sha256(b64).hexdigest()
        buf = tempfile.mktemp(prefix="mperf-paste-")
        with open(buf, "wb") as f:
            f.write(b64)
        fname = "/tmp/mperf-%d" % os.getpid()
        try:
            send_line("cat > " + fname)
            time.sleep(0.5)
            w0 = self.snap()
            t0 = time.time()
            tmux("load-buffer", "-b", "mperf", buf, check=True)
            tmux("paste-buffer", "-d", "-b", "mperf", "-t", SESSION, check=True)
            tmux("send-keys", "-t", SESSION, "C-d")
            _, txt = self.marked("( sha256sum %s || shasum -a 256 %s ) 2>&1; rm -f %s" % (
                fname, fname, fname), timeout=300)
            wall = time.time() - t0
            w1 = self.snap(True)
        finally:
            os.unlink(buf)
        m = re.search(r"\b([0-9a-f]{64})\s+\S*mperf-", txt)
        return w0, w1, {"bytes": len(b64), "sha_ok": bool(m) and m.group(1) == want,
                        "paste_wall_s": wall}

    def _bulk(self, cmd, timeout=180):
        self.wait_ready()
        time.sleep(1)
        w0 = self.snap()
        wall, _ = self.marked(cmd, timeout)
        return w0, self.snap(True), {"marker_wall_s": wall}

    def sc_flood(self, srv):
        return self._bulk("yes | head -c 5000000")

    def sc_steady(self, srv):
        return self._bulk("seq 1 200000")

    def sc_colors(self, srv):  # 5 passes of 58 rows x 39 cells x 5 cols, fg+bg 256-colour
        cmd = ("k=0; while [ $k -lt 5 ]; do r=0; while [ $r -lt 58 ]; do c=0; "
               "while [ $c -lt 39 ]; do n=$(( (r*39+c+k*7) % 256 )); "
               "printf '\\033[38;5;%d;48;5;%dm%4d \\033[0m' $(( (n+128)%256 )) $n $n; "
               "c=$((c+1)); done; printf '\\n'; r=$((r+1)); done; k=$((k+1)); done")
        return self._bulk(cmd)

    def execute(self, srv):
        cols, rows = (200, 60) if self.scenario == "colors" else (80, 24)
        res = {"label": self.a.label, "scenario": self.scenario, "client": self.client,
               "rep": self.rep, "ok": False, "cols": cols, "rows": rows}
        poller = None
        try:
            self.t0 = self.launch(srv, cols, rows)
            poller = threading.Thread(target=self._poll, daemon=True)
            poller.start()
            w0, w1, extra = getattr(self, "sc_" + self.scenario)(srv)
            res.update(extra)
            res["ok"] = bool(extra.get("sha_ok", True)) and bool(extra.get("tail_seen", True))
            if w0 and w1:
                res["metrics"] = window_metrics(w0, w1, self.samples)
        except Exception as e:  # keep going: a failed run is data too
            res["error"] = "%s: %s" % (type(e).__name__, e)
        finally:
            self.stop_evt.set()
            if poller:
                poller.join(2)
            self.quit()
            if self.client == "moche" and self.stats_file and os.path.exists(self.stats_file):
                with open(self.stats_file) as f:
                    res["moche_stats"] = {k: int(v) for k, v in re.findall(r"(\w+)=(\d+)", f.read())}
            if self.stats_file and os.path.exists(self.stats_file):
                os.unlink(self.stats_file)
        return res


def window_metrics(w0, w1, samples):
    wall = w1["t"] - w0["t"]
    cpu = lambda s: s["cpu_user_s"] + s["cpu_sys_s"]
    d = {"wall_s": wall,
         "cpu_user_s": w1["cpu_user_s"] - w0["cpu_user_s"],
         "cpu_sys_s": w1["cpu_sys_s"] - w0["cpu_sys_s"],
         "cpu_s": cpu(w1) - cpu(w0),
         "cpu_pct_of_core": 100.0 * (cpu(w1) - cpu(w0)) / wall if wall > 0 else None}
    inwin = [s for s in samples + [w1] if w0["t"] <= s["t"] <= w1["t"] + 1e-6]
    rss = [s["rss_kb"] for s in samples + [w1] if s.get("rss_kb")]
    d["final_rss_kb"] = w1.get("rss_kb")
    # Linux VmHWM is the exact process-lifetime peak; macOS: max of the sampled RSS (approximate)
    d["peak_rss_exact"] = w1.get("peak_rss_kb") is not None
    d["peak_rss_kb"] = w1.get("peak_rss_kb") or (max(rss) if rss else None)
    for k in ("pss_kb", "footprint_kb", "peak_footprint_kb", "vsz_kb", "threads", "fds"):
        d[k] = w1.get(k)
    d["threads_max"] = max([s["threads"] for s in inwin if s.get("threads")] or [None])
    for k, name in (("vol_cs", "vol_cs_per_s"), ("invol_cs", "invol_cs_per_s"),
                    ("csw", "cs_per_s"), ("wakeups", "wakeups_per_s"),
                    ("faults_minor", "faults_minor_per_s"), ("faults_major", "faults_major_per_s")):
        if k in w1 and k in w0 and wall > 0:
            d[name] = (w1[k] - w0[k]) / wall
            d[k + "_total"] = w1[k] - w0[k]
    if "instructions" in w1 and "instructions" in w0:
        d["instructions"] = w1["instructions"] - w0["instructions"]
        d["cycles"] = w1["cycles"] - w0["cycles"]
    return d


# ---------------------------------------------------------------- static facts / env
def sh(cmd):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        return (r.stdout + r.stderr).strip()
    except (OSError, subprocess.TimeoutExpired):
        return ""


def static_facts(paths):
    out = {}
    for name, p in paths.items():
        if not p:
            continue
        real = shutil.which(p) or p
        d = {"path_basename": os.path.basename(real), "size_bytes": os.path.getsize(real)}
        tmpd = tempfile.mkdtemp(prefix="mperf-strip-")
        try:
            sp = os.path.join(tmpd, "stripped")
            shutil.copy(real, sp)
            if subprocess.run(["strip", sp], capture_output=True).returncode == 0:
                d["stripped_size_bytes"] = os.path.getsize(sp)
        except OSError:
            pass
        finally:
            shutil.rmtree(tmpd, ignore_errors=True)
        if IS_MAC:
            d["deps"] = [l.strip().split(" (")[0] for l in sh(["otool", "-L", real]).splitlines()[1:]]
            d["segments"] = sh(["size", "-m", real])
        else:
            d["deps"] = [l.strip() for l in sh(["ldd", real]).splitlines()]
            d["segments"] = sh(["size", real])
        d["file"] = sh(["file", "-b", real])
        out[name] = d
    return out


def environment(a, moche, stock, lang):
    e = {"label": a.label, "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
         "os": platform.system(), "kernel": platform.release(), "machine": platform.machine(),
         "python": platform.python_version(), "tmux": sh(["tmux", "-V"]), "lang": lang,
         "cores": os.cpu_count()}
    if IS_MAC:
        e["os_version"] = "macOS " + sh(["sw_vers", "-productVersion"])
        e["cpu_model"] = sh(["sysctl", "-n", "machdep.cpu.brand_string"])
        e["ram_bytes"] = int(sh(["sysctl", "-n", "hw.memsize"]) or 0)
    else:
        osr = {}
        try:
            with open("/etc/os-release") as f:
                for line in f:
                    k, _, v = line.strip().partition("=")
                    osr[k] = v.strip('"')
        except OSError:
            pass
        e["os_version"] = osr.get("PRETTY_NAME", "Linux")
        model = ""
        try:
            with open("/proc/cpuinfo") as f:
                for line in f:
                    if ":" in line and line.split(":")[0].strip() in ("model name", "Model", "Hardware"):
                        model = line.split(":", 1)[1].strip()
                        break
        except OSError:
            pass
        if not model:
            for l in sh(["lscpu"]).splitlines():
                if l.startswith("Model name"):
                    model = l.split(":", 1)[1].strip()
        e["cpu_model"] = model or "unknown"
        try:
            with open("/proc/meminfo") as f:
                e["ram_bytes"] = int(f.readline().split()[1]) * 1024
        except OSError:
            e["ram_bytes"] = 0
    e["moche_version"] = (sh([moche, "--version"]).splitlines() or [""])[0]
    e["mosh_client_version"] = (sh([stock, "--version"]).splitlines() or [""])[0] if stock else None
    ms = shutil.which(a.mosh_server)
    e["mosh_server_version"] = (sh([ms, "--version"]).splitlines() or [""])[0] if ms else None
    e["server_mode"] = "ssh" if a.server_ssh else ("static" if a.server_port else "local")
    return e


def pick_lang():
    have = {l.lower().replace("-", "") for l in sh(["locale", "-a"]).split()}
    for cand in ("en_US.UTF-8", "C.UTF-8"):
        if cand.lower().replace("-", "") in have:
            return cand
    return "en_US.UTF-8"


# ---------------------------------------------------------------- main
def main():
    p = argparse.ArgumentParser()
    p.add_argument("--label", required=True)
    p.add_argument("--moche", required=True)
    p.add_argument("--stock", default="")
    p.add_argument("--clients", default="moche,stock")
    p.add_argument("--scenarios", default=",".join(SCENARIOS))
    p.add_argument("--reps", type=int, default=3)
    p.add_argument("--idle-secs", type=float, default=60)
    p.add_argument("--typing-secs", type=float, default=30)
    p.add_argument("--poll", type=float, default=0.5)
    p.add_argument("--out", required=True)
    p.add_argument("--mosh-server", default="mosh-server")
    p.add_argument("--server-host", default="")
    p.add_argument("--server-port", default="")
    p.add_argument("--key-env", default="")
    p.add_argument("--server-ssh", default="")
    p.add_argument("--ssh-opt", action="append", default=[])
    p.add_argument("--lang", default="")
    a = p.parse_args()

    if not shutil.which("tmux"):
        raise SystemExit("tmux not found")
    scen = a.scenarios.split(",")
    bad = [s for s in scen if s not in SCENARIOS]
    if bad:
        raise SystemExit("unknown scenario %s (have %s)" % (bad, SCENARIOS))
    bins = {"moche": a.moche, "stock": a.stock}
    clients = [c for c in a.clients.split(",") if c]
    if "stock" in clients and not (a.stock and shutil.which(a.stock)):
        print("stock mosh-client not found: skipping the baseline", file=sys.stderr)
        clients = [c for c in clients if c != "stock"]
    if not a.server_port and not a.server_ssh and not shutil.which(a.mosh_server):
        raise SystemExit("mosh-server not found (use --mosh-server, --server-ssh or --server-port)")
    if (a.server_ssh or a.server_port) and not a.server_host:
        raise SystemExit("--server-ssh/--server-port need --server-host (address the clients send UDP to)")
    lang = a.lang or pick_lang()

    runs_dir = os.path.join(a.out, "runs")
    os.makedirs(runs_dir, exist_ok=True)
    env = environment(a, a.moche, a.stock if "stock" in clients else "", lang)
    with open(os.path.join(a.out, "env.json"), "w") as f:
        json.dump(env, f, indent=2)
    with open(os.path.join(a.out, "static.json"), "w") as f:
        json.dump(static_facts({"moche": a.moche, "mosh-client": a.stock if "stock" in clients else ""}),
                  f, indent=2)
    print("environment:", json.dumps(env), flush=True)

    sm = Sampler()
    tmux("kill-server")  # only our own -L mochperf socket
    try:
        for rep in range(1, a.reps + 1):
            for sc in scen:
                for cl in clients:
                    srv = Server(a, lang)
                    try:
                        srv.start()
                        res = Run(a, cl, bins[cl], sc, rep, lang, sm).execute(srv)
                    except Exception as e:
                        res = {"label": a.label, "scenario": sc, "client": cl, "rep": rep,
                               "ok": False, "error": "%s: %s" % (type(e).__name__, e)}
                    finally:
                        srv.stop()
                    with open(os.path.join(runs_dir, "%s-%s-%d.json" % (sc, cl, rep)), "w") as f:
                        json.dump(res, f, indent=2)
                    m = res.get("metrics", {})
                    print("%-8s %-6s #%d ok=%s wall=%s cpu=%s rss_peak_kb=%s %s" % (
                        sc, cl, rep, res["ok"],
                        "%.2f" % m["wall_s"] if m else "-", "%.3f" % m["cpu_s"] if m else "-",
                        m.get("peak_rss_kb"), res.get("error", "")), flush=True)
    finally:
        tmux("kill-server")


if __name__ == "__main__":
    main()
