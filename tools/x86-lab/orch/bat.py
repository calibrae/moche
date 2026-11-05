#!/usr/bin/env python3
"""bat.py: regression battery of the x86 lab, run ON the lab host (drives both guests through lab.sh ssh, the
user-mode control NIC; netem is on link0 of BOTH guests, one-way D per side so RTT = 2D + ~1 ms base).
  bat.py wire  RTT...            datagrams per key, stock vs moche never/adaptive (env RUNS=2, CLIENTS=a,b,..)
  bat.py lat   RTT...            echo latency (env LATSCRIPT=lat2.py, LATGAP=seconds between keys, default 0.1)
  bat.py b1    N                 B1 loop at 10 % loss each way
  bat.py b2    N [D_MS]          B2 loop: 1 MiB base64 paste, 10 % loss (+ D_MS one way), server under strace
  bat.py paste                   lossless pastes (env PASTE_SIZES=raw bytes,.. KINDS=new,stock)
  bat.py flood                   floods, moche vs stock panes (env FLOODSTEPS=yes,seq,colors); panes saved in out/
  bat.py roam [0|1]              NIC down 20 s, back with a NEW address (1 = a key typed during the outage; env ROAMKIND)
  bat.py ack   RTT...            one key then silence, 10 % loss (env NNEW=12 NSTOCK=6); needs guest dec.py
  bat.py ghost RTT N             password prompt ghost check (env GHOSTMODE=adaptive|always)
Client variants are directories ~/moche-lab/bin/<variant>/moche in guest b (default variant "new", env NEWBIN).
Results are appended as JSON lines to $LAB/out/<cmd>.jsonl and logged to stdout. Each call must stay in the background
when driven through a tool with a ~60 s limit (nohup). LAB defaults to ~/moche-lab2; the guest addresses are the lab's
documentation-range placeholders."""
import json, os, re, shlex, subprocess, sys, time
from collections import Counter

LAB = os.environ.get("LAB", os.path.expanduser("~/moche-lab2"))
OUT = LAB + "/out"
A_IP, B_IP = "192.0.2.1", "192.0.2.2"
BIN = "~/moche-lab/bin"
NEW = os.environ.get("NEWBIN", "new")


def sh(n, cmd, timeout=150):
    r = subprocess.run([LAB + "/lab.sh", "ssh", n, cmd], capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    return r.stdout.strip()


def log(cmd, **kw):
    kw["t"] = time.strftime("%H:%M:%S")
    print(json.dumps(kw), flush=True)
    open("%s/%s.jsonl" % (OUT, cmd), "a").write(json.dumps(kw) + "\n")


def netem(d_ms=0, loss=0):
    for n in "ab":
        if d_ms or loss:
            sh(n, "sudo tc qdisc replace dev link0 root netem delay %dms loss %s%%" % (d_ms, loss))
        else:
            sh(n, "sudo tc qdisc del dev link0 root 2>/dev/null; true")


def start_server(wrap="", tail="", bg=False):
    sh("a", "pkill mosh-server; pkill strace; rm -f /tmp/ms.out /tmp/st.log; true")
    base = "SSH_CONNECTION='%s 1 %s 22' %s mosh-server new -s -c 256 -l LANG=C.UTF-8 %s" % (B_IP, A_IP, wrap, tail)
    if bg:
        sh("a", "nohup env %s > /tmp/ms.out 2>&1 < /dev/null &" % base)
        time.sleep(1.5)
        o = sh("a", "sed -n 's/^MOSH CONNECT //p' /tmp/ms.out")
    else:
        o = sh("a", base + " 2>&1 | sed -n 's/^MOSH CONNECT //p'")
    port, key = o.split()
    return port, key


def client_cmd(kind, port, key, err="/tmp/m.err", predict="never"):
    if kind == "stock":
        return "env MOSH_KEY=%s mosh-client %s %s" % (key, A_IP, port)
    flag = "" if predict == "none" else "--predict=%s" % predict
    return "env MOSH_KEY=%s %s/%s/moche --stats %s --direct %s %s 2>%s" % (key, BIN, kind, flag, A_IP, port, err)


def tm_start(s, cmd, w=100, h=30):
    sh("b", "tmux kill-session -t %s 2>/dev/null; tmux new -d -s %s -x %d -y %d %s" % (s, s, w, h, shlex.quote(cmd)))


def keys(s, *k):
    sh("b", "tmux send-keys -t %s %s" % (s, " ".join(shlex.quote(x) for x in k)))


def pane(s, e=False):
    return sh("b", "tmux capture-pane -p %s -t %s" % ("-e" if e else "", s))


def quit_client(s, err="/tmp/m.err"):
    keys(s, "C-^", ".")
    time.sleep(1.0)
    sh("b", "tmux kill-session -t %s 2>/dev/null; true" % s)
    return sh("b", "cat %s 2>/dev/null | tail -3" % err)


def wait_for(s, pat, secs, step=0.3):
    t0 = time.time()
    while time.time() - t0 < secs:
        if re.search(pat, pane(s), re.M):
            return time.time() - t0
        time.sleep(step)
    return None


def stat_num(txt, name):
    m = re.search(name + r"\D{1,3}(\d+)", txt)
    return int(m.group(1)) if m else None


def jline(o, **extra):
    try:
        r = json.loads(o.splitlines()[-1])
    except Exception:
        r = dict(error=o[-200:])
    r.update(extra)
    return r


def cmd_wire(rtts):
    runs = int(os.environ.get("RUNS", 2))
    for rtt in rtts:
        netem(rtt // 2) if rtt > 1 else netem(0)
        for run in range(1, runs + 1):
            for cl in os.environ.get("CLIENTS", "stock,%s-never,%s-adaptive" % (NEW, NEW)).split(","):
                port, key = start_server()
                tag = "w_rtt%d_%s_r%d" % (rtt, cl, run)
                o = sh("b", "python3 ~/moche-lab/vmw.py --client %s --aip %s --port %s --key %s --out ~/moche-lab/res/%s --rtt-ms %d --seed %d"
                       % (cl, A_IP, port, key, tag, rtt, run), timeout=200)
                log("wire", **jline(o, rtt=rtt, run=run))
    netem(0)


def cmd_lat(rtts):
    script, gap = os.environ.get("LATSCRIPT", "lat2.py"), os.environ.get("LATGAP", "0.1")
    for rtt in rtts:
        netem(rtt // 2)
        for seed in (1, 2):
            for cl in os.environ.get("CLIENTS", "stock,%s-never" % NEW).split(","):
                port, key = start_server()
                o = sh("b", "python3 ~/moche-lab/%s --client %s --aip %s --port %s --key %s --seed %d --gap %s" % (script, cl, A_IP, port, key, seed, gap), timeout=150)
                log("lat", **jline(o, rtt=rtt, seed=seed, gap=gap, script=script))
    netem(0)


def cmd_b1(n):
    netem(0, 10)
    freezes = 0
    for i in range(1, n + 1):
        port, key = start_server()
        tm_start("m", client_cmd(NEW, port, key, predict="never"))
        time.sleep(3)
        keys("m", 'echo RDY""%d' % i, "Enter")
        t = wait_for("m", r"^RDY%d\s*$" % i, 20, 0.2)
        st = quit_client("m")
        if t is None:
            freezes += 1
        log("b1", i=i, ok=t is not None, rdy_after_s=None if t is None else round(t, 2), stats=st[-160:])
    log("b1", summary=True, sessions=n, freezes=freezes)
    netem(0)


def mkpaste(nbytes):
    sh("b", "head -c %d /dev/urandom | base64 -w 76 > /tmp/paste.txt" % nbytes)
    o = sh("b", "wc -c < /tmp/paste.txt; sha256sum < /tmp/paste.txt | cut -c1-64; gzip -c /tmp/paste.txt | wc -c").split()
    return int(o[0]), o[1], int(o[2])


def one_paste(kind, wrap="", tmo=240, loop_ticker=True):
    port, key = start_server(wrap=wrap, bg=bool(wrap))
    tm_start("m", client_cmd(kind, port, key), 200, 50)
    time.sleep(3)
    sh("b", "tmux load-buffer /tmp/paste.txt")
    if loop_ticker:
        keys("m", '( while :; do printf .; sleep 0.2; done ) &', "Enter")
    keys("m", "cat > /tmp/paste.out", "Enter")
    time.sleep(0.5)
    t0 = time.time()
    sh("b", "tmux paste-buffer -r -t m")
    keys("m", "C-d")
    keys("m", 'echo FIN""1', "Enter")
    t = wait_for("m", r"^FIN1\s*$", tmo, 0.5)
    dt = time.time() - t0
    if loop_ticker:
        keys("m", "kill %1", "Enter")
    st = quit_client("m")
    sha = sh("a", "sha256sum < /tmp/paste.out | cut -c1-64")
    alive = sh("a", "pgrep -c mosh-server; true")
    # strace -e trace=process,signal also logs rt_sigaction lines that name SIGABRT: count only real deliveries/deaths
    abrt = sh("a", "grep -c -E -- '--- SIGABRT|killed by SIGABRT' /tmp/st.log 2>/dev/null; true") if wrap else "-"
    return dict(done=t is not None, secs=round(dt, 2), sha=sha, alive=alive, sigabrt=abrt, stats=st[-200:])


def cmd_b2(n, d_ms=0):
    size, sha, gz = mkpaste(775960)
    netem(d_ms, 10)
    for i in range(1, n + 1):
        r = one_paste(NEW, wrap="strace -f -o /tmp/st.log -e trace=process,signal")
        r.update(i=i, bytes=size, gz=gz, sha_ok=r["sha"] == sha, d_ms=d_ms, loss=10)
        r["bytes_sent"] = stat_num(r["stats"], "bytes_sent")
        r["cost"] = round(r["bytes_sent"] / gz, 2) if r["bytes_sent"] else None
        log("b2", **r)
    netem(0)


def cmd_paste():
    for nb in [int(x) for x in os.environ.get("PASTE_SIZES", "775960,3114000").split(",")]:
        size, sha, gz = mkpaste(nb)
        for kind in os.environ.get("KINDS", "%s,stock" % NEW).split(","):
            r = one_paste(kind, loop_ticker=False)
            r.update(kind=kind, bytes=size, gz=gz, sha_ok=r["sha"] == sha)
            # the stock client has no --stats: its bytes_sent here is a stale file, ignore it for kind=stock
            r["bytes_sent"] = stat_num(r["stats"], "bytes_sent")
            r["cost"] = round(r["bytes_sent"] / gz, 3) if r["bytes_sent"] else None
            log("paste", **r)


D1 = [(re.compile(r"\x1b\[9([0-7])m"), "38"), (re.compile(r"\x1b\[10([0-7])m"), "48")]


def norm(t):  # the known D1: SGR 90-97 / 100-107 (ours) against 38;5;8-15 / 48;5;8-15 (stock)
    for rx, p in D1:
        t = rx.sub(lambda m, p=p: "\x1b[%s;5;%dm" % (p, 8 + int(m.group(1))), t)
    return t


def cmd_flood():
    steps = [("yes", "yes | head -c 5000000"), ("seq", "seq 1 1000000"),
             ("colors", "for i in $(seq 0 255); do printf '\\033[48;5;%dm \\033[0m' $i; done; echo")]
    only = os.environ.get("FLOODSTEPS")
    for name, c in steps:
        if only and name not in only.split(","):
            continue
        res = {}
        for kind in (NEW, "stock"):
            port, key = start_server()
            tm_start("f_" + kind, client_cmd(kind, port, key), 250, 80)
            time.sleep(3)
            keys("f_" + kind, "clear", "Enter")
            time.sleep(1)
            t0 = time.time()
            keys("f_" + kind, c + '; echo DONE""1', "Enter")
            t = wait_for("f_" + kind, r"^DONE1\s*$", 90, 0.1)
            dt = time.time() - t0
            time.sleep(1.5)
            res[kind] = (dt if t is not None else None, pane("f_" + kind), pane("f_" + kind, e=True))
            quit_client("f_" + kind)
        a, b = res[NEW], res["stock"]
        for kk, vv in ((NEW, a), ("stock", b)):
            open("%s/flood_%s_%s.txt" % (OUT, name, kk), "w").write(vv[1])
            open("%s/flood_%s_%s.esc" % (OUT, name, kk), "w").write(vv[2])
        log("flood", step=name, new_ms=round(a[0] * 1000) if a[0] else None, stock_ms=round(b[0] * 1000) if b[0] else None,
            text_identical=a[1] == b[1], attrs_identical=a[2] == b[2], attrs_identical_after_D1=norm(a[2]) == norm(b[2]))


def cmd_roam(during="0"):
    port, key = start_server()
    # watchdog on b: restores link and the original address 110 s after arming; control stays on the user-mode NIC
    sh("b", "sudo sh -c 'nohup sh -c \"sleep 110; ip link set link0 up; ip addr flush dev link0; ip addr add %s/24 dev link0\" >/dev/null 2>&1 &'" % B_IP)
    tm_start("m", client_cmd(os.environ.get("ROAMKIND", NEW), port, key))
    time.sleep(3)
    keys("m", 'i=0; while sleep 1; do i=$((i+1)); echo tick$i; done', "Enter")
    time.sleep(8)
    sh("b", "sudo ip link set link0 down")
    t_down = time.time()
    if during == "1":
        time.sleep(10)
        keys("m", "x")
    time.sleep(max(0, 20 - (time.time() - t_down)))
    status = pane("m").splitlines()
    sh("b", "sudo ip addr flush dev link0; sudo ip addr add 192.0.2.12/24 dev link0; sudo ip link set link0 up")
    t_up = time.time()
    ticks = lambda: [int(x) for x in re.findall(r"^tick(\d+)\s*$", pane("m"), re.M)]
    # NOTE: "before" is read after the link-up command returned (one ssh round trip, 0.2-0.5 s): a client that resumes faster
    # than that shows a large `before` and resumed_s=None. Resolution of resumed_s is the ssh poll, about 0.3 s.
    before = max(ticks() or [0])
    resumed = None
    t0 = time.time()
    while time.time() - t0 < 15:
        tk = ticks()
        if tk and max(tk) >= before + 19:
            resumed = time.time() - t_up
            break
        time.sleep(0.05)
    shown = "last contact" in " ".join(status).lower() or "connection lost" in " ".join(status).lower()
    time.sleep(2)
    keys("m", "C-c")
    keys("m", 'echo POST""1', "Enter")
    echo_ok = wait_for("m", r"^POST1\s*$", 10, 0.1)
    typed_visible = ("x" in pane("m")) if during == "1" else None
    st = quit_client("m")
    sh("b", "sudo ip addr flush dev link0; sudo ip addr add %s/24 dev link0; sudo ip link set link0 up" % B_IP)
    log("roam", kind=os.environ.get("ROAMKIND", NEW), key_during_outage=during, status_line_shown=shown, tick_before_up=before,
        resumed_s=None if resumed is None else round(resumed, 2), post_echo_ok=echo_ok is not None,
        typed_in_outage_visible=typed_visible, stats=st[-160:])


def cmd_ack(rtts):
    n_new, n_stock = int(os.environ.get("NNEW", 12)), int(os.environ.get("NSTOCK", 6))
    for rtt in rtts:
        netem(rtt // 2, 10)
        for cl, n in (("%s-never" % NEW, n_new), ("stock", n_stock)):
            for run in range(1, n + 1):
                port, key = start_server()
                tag = "ack_rtt%d_%s_r%d" % (rtt, cl, run)
                sh("b", "python3 ~/moche-lab/vmw.py --client %s --aip %s --port %s --key %s --out ~/moche-lab/res/%s --rtt-ms %d --secs 0 --probes 1 --tail 12.5 --seed %d >/dev/null"
                   % (cl, A_IP, port, key, tag, rtt, run), timeout=120)
                tr = sh("b", "python3 ~/moche-lab/dec.py ~/moche-lab/res/%s %s; echo KEYS; cat ~/moche-lab/res/%s.keys; rm -f ~/moche-lab/res/%s*" % (tag, key, tag, tag))
                body, _, ks = tr.partition("KEYS")
                rows = [l.split() for l in body.splitlines() if l.strip()]
                kt = float(ks.split()[0]) if ks.split() else None
                srv = [(float(r[0]), r[2:]) for r in rows if r[1] == "S" and len(r) == 5 and kt and float(r[0]) >= kt]
                ne = [t - kt for t, v in srv if int(v[2]) > 0]          # server datagrams with a non-empty diff
                nums = [v[0] for t, v in srv if int(v[2]) > 0]
                log("ack", rtt=rtt, client=cl, run=run, nonempty_after_key=len(ne), last_nonempty_s=round(max(ne), 2) if ne else None,
                    srv_dg_after_key=len(srv), max_copies_of_one_state=max(Counter(nums).values()) if nums else 0,
                    srv_nonempty_t=[round(x, 2) for x in ne][:12])
    netem(0)


def cmd_ghost(rtt, n):
    netem(rtt // 2)
    mode = os.environ.get("GHOSTMODE", "adaptive")
    for i in range(1, n + 1):
        port, key = start_server()
        o = sh("b", "python3 ~/moche-lab/ghost.py --cmd %s" % shlex.quote(client_cmd(NEW, port, key, predict=mode)), timeout=120)
        log("ghost", **jline(o, rtt=rtt, i=i, mode=mode))
    netem(0)


if __name__ == "__main__":
    c, a = sys.argv[1], sys.argv[2:]
    os.makedirs(OUT, exist_ok=True)
    if c in ("wire", "lat", "ack"):
        {"wire": cmd_wire, "lat": cmd_lat, "ack": cmd_ack}[c]([int(x) for x in a])
    elif c == "b1":
        cmd_b1(int(a[0]))
    elif c == "b2":
        cmd_b2(int(a[0]), int(a[1]) if len(a) > 1 else 0)
    elif c == "paste":
        cmd_paste()
    elif c == "flood":
        cmd_flood()
    elif c == "roam":
        cmd_roam(a[0] if a else "0")
    elif c == "ghost":
        cmd_ghost(int(a[0]), int(a[1]))
