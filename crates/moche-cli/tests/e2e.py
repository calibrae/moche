"""End-to-end: stock local mosh-server + moche --direct inside a pty.

usage: python3 -I e2e.py <path-to-moche-binary>
Exit 0 = pass, 77 = skipped (no mosh-server), other = failure.
"""
import fcntl, os, pty, re, select, shutil, signal, struct, subprocess, sys, termios, time

def main():
    moche = sys.argv[1]
    server_bin = shutil.which("mosh-server") or "/opt/homebrew/bin/mosh-server"
    if not os.access(server_bin, os.X_OK):
        print("skip: mosh-server not found")
        return 77

    env = dict(os.environ, SSH_CONNECTION="127.0.0.1 1 127.0.0.1 22")
    srv = subprocess.run(
        [server_bin, "new", "-s", "-c", "256", "-l", "LANG=en_US.UTF-8", "--", "/bin/sh"],
        env=env, capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=20)
    m = re.search(r"^MOSH CONNECT (\d+) (\S{22})$", srv.stdout, re.M)
    if not m:
        print("FAIL: no MOSH CONNECT:", srv.stdout[:200], srv.stderr[:200])
        return 1
    port, key = m.group(1), m.group(2)
    pid = None
    m2 = re.search(r"pid = (\d+)", srv.stdout + srv.stderr)
    server_pid = int(m2.group(1)) if m2 else None

    child_pid = None
    try:
        child_pid, fd = pty.fork()
        if child_pid == 0:
            os.environ["MOSH_KEY"] = key
            os.execv(moche, [moche, "--direct", "127.0.0.1", port])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        out = b""

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

        def send(b):
            os.write(fd, b)

        # entered alt screen + shell prompt (sh prints $ or %)
        assert pump(10, b"\x1b[?1049h"), "no alt screen on start"
        time.sleep(1.5)
        pump(1)
        assert re.search(rb"[$#%] ", out), f"no prompt: {out[-300:]!r}"
        print("ok: prompt appeared")

        send(b"echo moche-e2e-$((20+2+0))\n")
        assert pump(8, b"moche-e2e-22"), f"no echo output: {out[-300:]!r}"
        print("ok: command output received")

        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        os.kill(child_pid, signal.SIGWINCH)
        time.sleep(0.5)
        send(b"stty size\n")
        assert pump(8, b"30 100"), f"resize not applied remotely: {out[-300:]!r}"
        print("ok: resize propagated (stty size 30 100)")

        # exit key, split across two writes
        send(b"\x1e")
        time.sleep(0.3)
        send(b".")
        deadline = time.time() + 5
        status = None
        while time.time() < deadline:
            pump(0.2)
            p, st = os.waitpid(child_pid, os.WNOHANG)
            if p:
                status = st
                child_pid = None
                break
        assert status is not None, "moche did not exit on Ctrl-^ ."
        assert os.waitstatus_to_exitcode(status) == 0, f"exit status {status}"
        pump(0.3)
        assert b"\x1b[?1049l" in out, "alt screen not left"
        print("ok: exit key quit with code 0, alt screen left")
        print("PASS")
        return 0
    finally:
        if child_pid:
            try:
                os.kill(child_pid, signal.SIGKILL)
                os.waitpid(child_pid, 0)
            except OSError:
                pass
        if server_pid:
            try:
                os.kill(server_pid, signal.SIGTERM)
            except OSError:
                pass

if __name__ == "__main__":
    try:
        sys.exit(main())
    except AssertionError as e:
        print("FAIL:", e)
        sys.exit(1)
