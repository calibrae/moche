"""End-to-end: stock local mosh-server + moche --direct inside a pty.

usage: python3 -I e2e.py <moche-binary> <capture-dir>
Writes <capture-dir>/tty.bin (raw bytes moche wrote to the tty) and
<capture-dir>/checkpoints.tsv (name, byte offset, cols, rows, expectations);
the Rust test replays them into a moche_core Framebuffer and asserts the text.
Expectations: "text" must appear, "!text" must not, "N*text" at least N times.
Exit 0 = ran, 77 = skipped (no mosh-server), other = failure.
"""
import fcntl, os, pty, re, select, shutil, signal, struct, subprocess, sys, tempfile, termios, time

def main():
    moche, cap = sys.argv[1], sys.argv[2]
    os.makedirs(cap, exist_ok=True)
    server_bin = shutil.which("mosh-server") or "/opt/homebrew/bin/mosh-server"
    if not os.access(server_bin, os.X_OK):
        print("skip: mosh-server not found")
        return 77

    scratch = tempfile.mkdtemp(prefix="moche-e2e-")
    env = dict(os.environ, SSH_CONNECTION="127.0.0.1 1 127.0.0.1 22")
    srv = subprocess.run(
        [server_bin, "new", "-s", "-c", "256", "-l", "LANG=en_US.UTF-8", "--", "/bin/sh"],
        env=env, capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=20)
    m = re.search(r"^MOSH CONNECT (\d+) (\S{22})$", srv.stdout, re.M)
    if not m:
        print("FAIL: no MOSH CONNECT:", srv.stdout[:200], srv.stderr[:200])
        return 1
    port, key = m.group(1), m.group(2)
    m2 = re.search(r"pid = (\d+)", srv.stdout + srv.stderr)
    server_pid = int(m2.group(1)) if m2 else None
    assert server_pid, "cannot find server pid"

    child_pid = None
    out = b""
    size = (80, 24)
    checks = []
    try:
        child_pid, fd = pty.fork()
        if child_pid == 0:
            os.environ["MOSH_KEY"] = key
            os.execv(moche, [moche, "--direct", "127.0.0.1", port])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))

        def pump(secs, until=None):
            nonlocal out
            end = time.time() + secs
            while time.time() < end:
                r, _, _ = select.select([fd], [], [], 0.1)
                if r:
                    try:
                        d = os.read(fd, 65536)
                    except OSError:
                        return False
                    if not d:
                        return False
                    out += d
                if until and until in out:
                    return True
            return until is None

        def quiet(max_s=10, gap=1.0):
            """Pump until the output has been silent for `gap` seconds."""
            nonlocal out
            end, last = time.time() + max_s, time.time()
            while time.time() < end and time.time() - last < gap:
                n = len(out)
                pump(0.1)
                if len(out) != n:
                    last = time.time()

        sent_at = 0

        def send(b):
            nonlocal sent_at
            sent_at = len(out)
            os.write(fd, b)

        def check(name, *expect, token=None):
            # Wait for a token to show up in new output, then for quiet.
            if token:
                end = time.time() + 20
                while token not in out[sent_at:] and time.time() < end:
                    pump(0.1)
            quiet()
            checks.append((name, len(out), size[0], size[1], expect))
            print("checkpoint", name)

        assert pump(10, b"\x1b[?1049h"), "no alt screen on start"
        pump(20, b"sh-3.2$")
        quiet(10, 1.5)

        # (a) prompt + echo
        send(b"echo moche-e2e-ok\n")
        check("a_echo", "2*moche-e2e-ok", "sh-", token=b"moche-e2e-ok")

        # (b) colored ls
        send(b"CLICOLOR_FORCE=1 ls -G /usr/bin | head -50\n")
        last = subprocess.run("ls /usr/bin | head -50 | tail -1", shell=True,
                              capture_output=True, text=True).stdout.strip()
        assert out.count(b"\x1b[") > 0
        quiet(10, 1.5)
        check("b_ls", last, "!connection lost", token=last.encode())

        # (c) vim
        send(("cd %s && clear && vim f.txt\n" % scratch).encode())
        quiet(10, 2.0)
        check("c1_vim_drawn", "~", "f.txt", "!moche-e2e-ok", token=b"f.txt")
        send(b"ihello")
        quiet(10, 1.0)
        check("c2_vim_typed", "hello", "INSERT", token=b"INSERT")
        send(b"\x1b")
        time.sleep(0.5)
        send(b":q!\n")
        quiet(10, 1.5)
        send(b"echo vim-done\n")
        check("c3_after_vim", "2*vim-done", "sh-", "!INSERT", token=b"vim-done")
        # Note: the "~" rows of vim stay on screen: mosh-server has no alternate
        # screen, so this is the server's own state, not a moche artefact.

        # (d) resize
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        os.kill(child_pid, signal.SIGWINCH)
        time.sleep(1.0)
        send(b"stty size\n")
        quiet(10, 1.5)
        size = (100, 30)
        check("d_resize", "30 100", "stty size", "!connection lost", token=b"30 100")

        # (e) server suspended for ~8 s, then resumed
        os.kill(server_pid, signal.SIGSTOP)
        try:
            pump(8.5)
        finally:
            os.kill(server_pid, signal.SIGCONT)
        gap_seen = b"connection lost" in out
        checks.append(("e1_gap", len(out), size[0], size[1], ("connection lost",)))
        print("checkpoint e1_gap (status line in raw output: %s)" % gap_seen)
        quiet(10, 2.0)
        send(b"echo after-gap\n")
        check("e2_resumed", "2*after-gap", "!connection lost", token=b"after-gap")

        # exit key, split across two writes
        send(b"\x1e")
        time.sleep(0.3)
        send(b".")
        status = None
        for _ in range(50):
            pump(0.1)
            p, st = os.waitpid(child_pid, os.WNOHANG)
            if p:
                status, child_pid = st, None
                break
        assert status is not None, "moche did not exit on Ctrl-^ ."
        assert os.waitstatus_to_exitcode(status) == 0, "exit status %r" % status
        pump(0.3)
        assert b"\x1b[?1049l" in out, "alt screen not left"
        assert gap_seen, "status line never appeared during the gap"
        print("ok: exit key quit with code 0, alt screen left")
    finally:
        with open(os.path.join(cap, "tty.bin"), "wb") as f:
            f.write(out)
        with open(os.path.join(cap, "checkpoints.tsv"), "w") as f:
            for n, off, c, r, ex in checks:
                f.write("\t".join([n, str(off), str(c), str(r), *ex]) + "\n")
        if child_pid:
            try:
                os.kill(child_pid, signal.SIGKILL)
                os.waitpid(child_pid, 0)
            except OSError:
                pass
        for sig in (signal.SIGCONT, signal.SIGTERM):
            try:
                os.kill(server_pid, sig)
            except OSError:
                pass
        shutil.rmtree(scratch, ignore_errors=True)
    return 0

if __name__ == "__main__":
    try:
        sys.exit(main())
    except AssertionError as e:
        print("FAIL:", e)
        sys.exit(1)
