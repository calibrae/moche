# moche resource measurements (footprint, CPU, RSS)

`tools/perf/` measures `moche --direct` against the stock `mosh-client` (baseline) on the
same stock `mosh-server`, from outside the client process, and produces JSON per run plus a
Markdown report. The same tool runs unchanged on macOS and Linux. This file holds the
results per platform and how to reproduce them.

## What is measured

Each scenario runs 3 times per client (a fresh server and a fresh client every time, driven
through a private tmux server `-L mochperf`, so identical on macOS and Linux); the report
gives the median and the min-max.

| scenario | what it does | pane |
|---|---|---|
| startup | launch to first non-empty paint, then 1 s settle | 80x24 |
| idle | 60 s of heartbeats only (`--idle-secs`) | 80x24 |
| typing | 30 s at 10 chars/s, one `send-keys` per char (`--typing-secs`) | 80x24 |
| paste | 1 MiB of random base64 pasted into `cat > file`, sha256 checked on the server | 80x24 |
| flood | `yes \| head -c 5000000` | 80x24 |
| colors | 5 passes of a 58x39-cell 256-colour (fg+bg) dump | 200x60 |
| steady | `seq 1 200000` | 80x24 |

The CPU/RSS window is the scenario action itself (for idle: the 60 s after a 3 s settle; for
startup: from launch to first paint + 1 s). Total run time is about 12 minutes.

Per run and client: wall time; CPU user+sys and average % of one core; peak and final RSS;
PSS (Linux); physical footprint and its lifetime peak (macOS); threads; context switches per
second (a wakeup proxy; on macOS also package-idle + interrupt wakeups per second, plus
instructions and cycles retired); page faults; open fds. For moche also `--stats` (datagrams
and bytes sent/received). Static facts: binary size as built and stripped, dynamic
dependencies, and segment sizes (`size`).

### How each number is obtained (sampled from outside, no wrapper)

The client pid is the tmux pane pid (the pane command is `exec <client> ...`, so it is the
client itself; if not, the tool walks down with `pgrep -P`).

| | Linux | macOS |
|---|---|---|
| CPU user/sys | `/proc/<pid>/stat` utime+stime (tick = `SC_CLK_TCK`, usually 10 ms) | `proc_pid_rusage(RUSAGE_INFO_V4)` via `ctypes` (ns resolution after timebase conversion) |
| RSS now / peak | `VmRSS` / `VmHWM` of `/proc/<pid>/status` (peak is exact) | `proc_pidinfo(PROC_PIDTASKINFO)` resident size; peak is the max of 0.5 s samples (approximate) |
| footprint | PSS from `/proc/<pid>/smaps_rollup` (final snapshot) | `ri_phys_footprint` and `ri_lifetime_max_phys_footprint` (the numbers the `footprint` tool shows; the lifetime peak is exact) |
| threads | `num_threads` | `pti_threadnum` |
| context switches | `voluntary/nonvoluntary_ctxt_switches` summed over `/proc/<pid>/task/*` | `pti_csw` (total, not split) |
| wakeups | not measured | `ri_pkg_idle_wkups + ri_interrupt_wkups` |
| page faults | stat minflt/majflt | `pti_faults` / `pti_pageins` (reported as minor/major) |
| fds | `ls /proc/<pid>/fd` | `proc_pidinfo(PROC_PIDLISTFDS)` |
| fallback | | `ps -o rss,vsz,utime,stime` (10 ms CPU resolution, no peak) if `ctypes`/libproc fails |

No `/usr/bin/time` wrapper and no `sudo`: the sampler only reads the client's own process.
Peak RSS is process lifetime (it includes start-up and earlier scenario phases of the same
client run, which is one fresh client per run, so it is the peak of that run).

## Reading the numbers: caveats

- `bulk` scenarios (flood, colors, steady) are short on loopback: mosh drops intermediate
  frames, so the client sees only a few states and the windows last 0.2-2 s. Their CPU is
  tiny and quantised; read ratios loosely and prefer paste and typing for throughput.
- Wall times in bulk/paste scenarios include the tmux polling interval (50 ms) and the
  server-side work, so they measure the whole pipeline, not the client alone.
- Loopback means no loss, no latency: SRTT is about 0 and the frame interval is at its
  minimum. A real WiFi/WireGuard path changes datagram counts, not the footprint.
- The server is the same stock mosh-server for both clients; only the client differs.
- The moche `--stats` byte counters are moche-only (stock prints none).
- `fds`: moche shows 9 against 5 for stock (stderr is redirected to a file for `--stats`,
  plus the socket and inherited descriptors); constant across scenarios, so not a leak, but
  not investigated further.

## Running it

Prerequisites: bash, python3 (stdlib only), tmux, a stock mosh-server and (for the baseline)
`mosh-client` on PATH. A missing `mosh-client` skips the baseline cleanly.

macOS or Linux, local server (the server is started per run on loopback with a fake
`SSH_CONNECTION`, like the e2e test):

```sh
tools/perf/measure.sh --build --label mac-arm64
```

`--build` runs `CARGO_TARGET_DIR=target/perf cargo build --release -p moche-cli --locked`
(the binary lands in `target/perf/release/moche`; both `out/` and `target/perf/` are
gitignored). Useful switches: `--scenarios idle,paste`, `--reps 1`, `--idle-secs 10`,
`--typing-secs 10`, `--clients moche`, `--moche PATH`, `--stock PATH|none`, `--lang LOCALE`
(default `en_US.UTF-8`, else `C.UTF-8`).

Output: `tools/perf/out/<label>-<UTC timestamp>/` with `env.json`, `static.json`,
`runs/<scenario>-<client>-<rep>.json` and `report.md`. Regenerate or compare several hosts
with `python3 -I tools/perf/report.py DIR_A DIR_B -o report.md`.

Only the dedicated tmux server (`tmux -L mochperf`) and the mosh-servers the tool starts are
ever touched or killed; everything is cleaned up at exit.

### Linux run in the VM lab (client VM `moche-b`, server VM `moche-a`)

The client VM needs `tmux`, `python3`, `mosh` (for `mosh-client`), and a moche binary built
there (it has the Rust toolchain; `moche-cli` as built by `cargo build --release -p moche-cli
--locked`). The server VM only needs `mosh-server` and ssh access from the client VM, over
the VM<->VM (user-v2) network, see `tools/vm-lab/README.md`.

```sh
# on the host: copy the tool and the sources (or just the binary) to moche-b
rsync -a tools/perf/ <user>@<b-ip>:perf/            # or: limactl copy / scp -r
# on moche-b, with the moche checkout or a prebuilt binary:
cd perf
./measure.sh --label linux-aarch64-vm --moche /path/to/moche \
    --server-ssh <user>@<a-u2-ip> --server-host <a-u2-ip>
```

`--server-ssh` starts a fresh `mosh-server` on moche-a over ssh for every run (key-based ssh
from b to a, `BatchMode`), and stops it afterwards; `--server-host` is the address the UDP
goes to. If ssh is awkward, start one server by hand on moche-a
(`mosh-server new -s -c 256 -l LANG=en_US.UTF-8 -- /bin/sh`, note the port and key) and run:

```sh
export MOSH_KEY=<22-char key>
./measure.sh --label linux-aarch64-vm --moche /path/to/moche \
    --server-host <a-u2-ip> --server-port <port> --key-env MOSH_KEY
```

That static mode reuses one server session for all runs (the shell state carries over, but
every scenario is self-contained) and does not kill it.

To run client and server both on the Linux VM instead, drop the server options (default
local server on loopback): `./measure.sh --label linux-aarch64-local --moche /path/to/moche`.

Then copy `out/<label>-*/` back and paste `report.md` below, or run `report.py` on both
result directories.

Linux specifics that differ from macOS: PSS and exact `VmHWM` peak are available, context
switches are split voluntary/involuntary, but there is no wakeup counter or
instructions/cycles, and the footprint columns stay empty. The Linux code paths were checked
by reading and `bash -n`/`py_compile` only, not executed on this Mac; if a column is empty
on the first Linux run, look at `runs/*.json` for the raw samples first.

## macOS arm64

Measured 2026-11-04, `mac-arm64`: Apple M1 (8 cores), 16 GiB, macOS 26.4.1, moche 0.1.0
(commit dbc02f8, release build), stock mosh 1.4.0 (client and server, Homebrew), tmux 3.8,
server on loopback. All 42 runs passed (the paste sha256 matched every time).

### Medians, moche vs stock mosh-client

| scenario | metric | moche | stock | moche / stock |
|---|---|---:|---:|---:|
| startup | launch to first paint (s) | 0.166 | 0.336 | 0.49 |
| startup | CPU (s) | 0.008 | 0.044 | 0.19 |
| idle 60 s | CPU (s) / % of a core | 0.010 / 0.02 | 0.007 / 0.01 | 1.4 |
| idle 60 s | peak RSS (MB) | 2.5 | 14.7 | 0.17 |
| idle 60 s | physical footprint (MB) | 1.5 | 7.0 | 0.22 |
| idle 60 s | ctx switches /s, wakeups /s | 1.4, 0.4 | 0.8, 0.3 | |
| idle 60 s | traffic (moche) | 26 datagrams out, 1.8 KB each way | | |
| typing 30 s | CPU (s) / % of a core | 0.132 / 0.42 | 0.095 / 0.30 | 1.4 |
| typing 30 s | peak RSS / footprint (MB) | 3.2 / 1.8 | 15.0 / 7.4 | 0.2 / 0.25 |
| paste 1 MiB | wall (s) | 0.79 | 0.69 | 1.15 |
| paste 1 MiB | CPU (s) | 0.052 | 0.350 | 0.15 |
| paste 1 MiB | peak RSS (MB) | 4.3 | 199 (max 732) | 0.02 |
| paste 1 MiB | peak footprint (MB) | 3.6 | 204 (max 781) | 0.02 |
| flood 5 MB | wall (s) / CPU (s) | 1.67 / 0.002 | 1.69 / 0.001 | |
| colors 200x60 | CPU (s) | 0.013 | 0.028 | 0.45 |
| colors 200x60 | peak RSS / footprint (MB) | 6.4 / 5.7 | 17.8 / 10.1 | 0.36 / 0.57 |
| steady `seq` | wall (s) / CPU (s) | 1.19 / 0.006 | 0.26 / 0.002 | |

Static: moche 0.54 MB on disk (already stripped by the release profile; `strip` changes
nothing), 2 dynamic libs (`libSystem`, `libiconv`), `__TEXT` 480 KB. Stock mosh-client
0.39 MB but it links `libprotobuf` (Homebrew), `libncurses`, `libz`, `libc++` on top of
`libSystem`.

### Reading

- **Idle CPU is about zero**: 6-11 ms of CPU over 60 s (0.02 % of a core), 1.4 context
  switches and 0.4 wakeups per second, one heartbeat datagram every ~2.3 s each way. The
  stock client is in the same noise band (7 ms); the 1.4x ratio is a 3 ms difference and
  not significant at the resolution of 3 runs. No timer busy-loop, one thread.
- **Memory is 5-6x smaller**: 2.5 MB RSS and 1.5 MB footprint at idle versus 14.7 MB and
  7.0 MB. After heavy output moche stays under 7 MB; the stock client's RSS rises to 15-18 MB.
- **Paste is the stand-out**: the stock client balloons to ~200 MB RSS (up to 732 MB in one
  run) and burns 7x more CPU for 1 MiB (probably because it buffers the pasted input; not investigated); moche stays at
  4.3 MB (838 KB sent for the paste in 688 datagrams). Wall time is on par
  (0.79 s vs 0.69 s on loopback).
- **Typing costs moche about 1.4x the stock CPU** (0.13 s vs 0.095 s over 30 s, mostly
  system time: 0.072 vs 0.039 s), still 0.4 % of a core. Context switches and wakeups per
  second are equal (about 46/s and 13/s, which is the 10 keystrokes/s plus replies). moche
  sent 413 datagrams for 300 keystrokes, more than one per key: worth a look if battery
  matters on the phone (it is the only place moche is not better or equal).
- **Startup**: first paint in 0.17 s versus 0.34 s; 8 ms versus 44 ms of CPU. One moche
  run took 0.94 s to first paint (min-max 0.15-0.94): an outlier, probably the first server
  frame lost or delayed, not reproduced in the other 2 runs; check on Linux before
  concluding.
- **Surprises**: none against the design. The bulk scenarios (flood, steady) are
  dominated by wall-clock noise on loopback (moche `steady` min 0.25 s, median 1.19 s: the
  state it sees depends on frame pacing, not on its own cost); their CPU is a few ms, not
  conclusive. fds are 9 vs 5 (see caveats).
- Not measurable here: Linux-only PSS and exact RSS peak, voluntary/involuntary split of
  context switches, `/proc` figures; energy (no `powermetrics` without sudo); a real network
  path (loss, latency).

### Full generated report (host `mac-arm64`)

#### Host `mac-arm64`

| | |
|---|---|
| label | mac-arm64 |
| OS | macOS 26.4.1 (Darwin 25.4.0) |
| CPU | Apple M1, 8 cores |
| RAM | 16.0 GiB |
| moche | moche 0.1.0 (dbc02f8, aarch64-apple-darwin) |
| mosh-client (baseline) | mosh-client (mosh 1.4.0) [build mosh 1.4.0] |
| mosh-server | mosh-server (mosh 1.4.0) [build mosh 1.4.0] (local) |
| tmux / python | tmux 3.8 / 3.15.0 |
| locale | en_US.UTF-8 |
| date (UTC) | 2026-11-04T11:44:45Z |

##### Static facts

| binary | size MB (as built) | stripped MB | dynamic deps | file |
|---|---:|---:|---:|---:|
| moche | 0.54 | 0.54 | 2 | Mach-O 64-bit executable arm64 |
| mosh-client | 0.39 | n/a | 5 | Mach-O 64-bit executable arm64 |

`moche` dynamic dependencies:

```
/usr/lib/libiconv.2.dylib
/usr/lib/libSystem.B.dylib
```

`mosh-client` dynamic dependencies:

```
/usr/lib/libSystem.B.dylib
/usr/lib/libncurses.5.4.dylib
/opt/homebrew/opt/protobuf/lib/libprotobuf.36.2.0.dylib
/usr/lib/libz.1.dylib
/usr/lib/libc++.1.dylib
```

##### Startup (launch to first paint, +1 s settle)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| launch to first paint (s) | 0.166 | 0.151-0.935 | 0.336 | 0.325-0.542 | 0.49 |
| wall time (s) | 1.17 | 1.16-1.94 | 1.35 | 1.33-1.55 | 0.87 |
| CPU time user+sys (s) | 0.008 | 0.007-0.009 | 0.044 | 0.042-0.046 | 0.19 |
|   user (s) | 0.003 | 0.003-0.003 | 0.031 | 0.030-0.032 | 0.09 |
|   sys (s) | 0.005 | 0.004-0.006 | 0.013 | 0.012-0.014 | 0.42 |
| CPU, % of one core | 0.62 | 0.46-0.71 | 3.20 | 2.94-3.27 | 0.19 |
| peak RSS (MB) | 2.4 | 2.3-2.6 | 14.3 | 14.3-14.3 | 0.17 |
| final RSS (MB) | 2.4 | 2.3-2.6 | 14.3 | 8.9-14.3 | 0.17 |
| physical footprint (MB, macOS) | 1.7 | 1.7-1.7 | 6.5 | 6.5-6.6 | 0.26 |
| peak footprint (MB, macOS) | 1.7 | 1.7-1.9 | 6.6 | 6.5-6.6 | 0.26 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| bytes sent (moche --stats) | 216 | 208-452 | - | - | - |
| bytes received (moche --stats) | 167 | 159-168 | - | - | - |
| datagrams sent | 3 | 3-6 | - | - | - |
| datagrams received | 2 | 2-2 | - | - | - |

##### Idle (heartbeats only)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 60.01 | 60.01-60.01 | 60.01 | 60.00-60.01 | 1.00 |
| CPU time user+sys (s) | 0.010 | 0.006-0.011 | 0.007 | 0.007-0.010 | 1.39 |
|   user (s) | 0.002 | 0.002-0.003 | 0.003 | 0.003-0.003 | 0.92 |
|   sys (s) | 0.008 | 0.004-0.008 | 0.005 | 0.004-0.007 | 1.64 |
| CPU, % of one core | 0.02 | 0.01-0.02 | 0.01 | 0.01-0.02 | 1.39 |
| peak RSS (MB) | 2.5 | 2.5-2.6 | 14.7 | 14.7-14.8 | 0.17 |
| final RSS (MB) | 2.0 | 1.7-2.4 | 9.0 | 6.2-9.1 | 0.23 |
| physical footprint (MB, macOS) | 1.5 | 1.5-1.8 | 7.0 | 7.0-7.0 | 0.22 |
| peak footprint (MB, macOS) | 2.0 | 1.8-2.2 | 7.2 | 7.1-7.3 | 0.27 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| ctx switches /s (macOS, total) | 1.4 | 0.8-1.8 | 0.8 | 0.7-1.0 | 1.63 |
| wakeups /s (macOS, idle+interrupt) | 0.4 | 0.4-0.4 | 0.3 | 0.3-0.3 | 1.10 |
| minor page faults (window) | 727 | 180-1147 | 357 | 277-676 | 2.04 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| instructions retired (macOS, window) | 14.0 M | 9.6-16.6 M | 6.5 M | 5.8-8.1 M | 2.14 |
| cycles (macOS, window) | 20.6 M | 11.7-24.8 M | 13.9 M | 13.0-15.1 M | 1.48 |
| bytes sent (moche --stats) | 1812 | 1638-1913 | - | - | - |
| bytes received (moche --stats) | 1870 | 1840-1880 | - | - | - |
| datagrams sent | 26 | 24-27 | - | - | - |
| datagrams received | 25 | 24-25 | - | - | - |

##### Typing (~10 chars/s)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 31.53 | 31.53-31.54 | 31.53 | 31.52-31.53 | 1.00 |
| CPU time user+sys (s) | 0.132 | 0.122-0.135 | 0.095 | 0.094-0.100 | 1.39 |
|   user (s) | 0.060 | 0.056-0.063 | 0.058 | 0.055-0.060 | 1.02 |
|   sys (s) | 0.072 | 0.066-0.073 | 0.039 | 0.037-0.040 | 1.85 |
| CPU, % of one core | 0.42 | 0.39-0.43 | 0.30 | 0.30-0.32 | 1.39 |
| peak RSS (MB) | 3.2 | 3.1-3.2 | 15.0 | 14.9-15.1 | 0.21 |
| final RSS (MB) | 3.0 | 3.0-3.1 | 9.5 | 9.5-9.6 | 0.32 |
| physical footprint (MB, macOS) | 1.8 | 1.7-2.0 | 7.4 | 7.4-7.5 | 0.25 |
| peak footprint (MB, macOS) | 2.6 | 2.4-2.7 | 7.7 | 7.5-7.7 | 0.34 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| ctx switches /s (macOS, total) | 45.6 | 43.1-48.2 | 46.7 | 44.8-47.5 | 0.98 |
| wakeups /s (macOS, idle+interrupt) | 13.3 | 12.2-14.2 | 13.6 | 13.1-13.9 | 0.97 |
| minor page faults (window) | 166 | 109-181 | 78 | 64-129 | 2.13 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| instructions retired (macOS, window) | 223.9 M | 222.3-226.2 M | 137.9 M | 137.6-142.0 M | 1.62 |
| cycles (macOS, window) | 229.5 M | 229.2-239.3 M | 189.1 M | 179.4-190.8 M | 1.21 |
| bytes sent (moche --stats) | 33204 | 30971-36678 | - | - | - |
| bytes received (moche --stats) | 53045 | 52086-53909 | - | - | - |
| datagrams sent | 413 | 385-456 | - | - | - |
| datagrams received | 638 | 632-650 | - | - | - |

##### Paste (1 MiB base64 into cat > file, sha256 checked)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| paste to sha-verified (s) | 0.79 | 0.79-0.79 | 0.69 | 0.62-3.13 | 1.15 |
| wall time (s) | 0.79 | 0.79-0.79 | 0.69 | 0.62-3.13 | 1.15 |
| CPU time user+sys (s) | 0.052 | 0.052-0.053 | 0.350 | 0.331-1.742 | 0.15 |
|   user (s) | 0.029 | 0.029-0.029 | 0.270 | 0.248-1.325 | 0.11 |
|   sys (s) | 0.023 | 0.022-0.024 | 0.082 | 0.080-0.416 | 0.28 |
| CPU, % of one core | 6.56 | 6.53-6.72 | 53.00 | 50.93-55.59 | 0.12 |
| peak RSS (MB) | 4.3 | 4.3-4.5 | 199.1 | 188.9-731.6 | 0.02 |
| final RSS (MB) | 4.3 | 4.3-4.5 | 199.1 | 188.9-517.5 | 0.02 |
| physical footprint (MB, macOS) | 3.6 | 3.6-3.9 | 40.7 | 13.1-411.8 | 0.09 |
| peak footprint (MB, macOS) | 3.6 | 3.6-3.9 | 203.9 | 175.1-780.5 | 0.02 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| ctx switches /s (macOS, total) | 2637.0 | 2616.4-2687.5 | 2573.7 | 2272.5-2925.5 | 1.02 |
| wakeups /s (macOS, idle+interrupt) | 0.0 | 0.0-0.0 | 3.2 | 1.5-4.8 | 0.00 |
| minor page faults (window) | 118 | 117-130 | 12646 | 11142-76603 | 0.01 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| instructions retired (macOS, window) | 354.1 M | 353.3-354.3 M | 3259.5 M | 3077.7-12875.2 M | 0.11 |
| cycles (macOS, window) | 151.0 M | 150.8-154.1 M | 1031.9 M | 976.3-5004.2 M | 0.15 |
| bytes sent (moche --stats) | 838776 | 838726-838786 | - | - | - |
| bytes received (moche --stats) | 19704 | 19692-19858 | - | - | - |
| datagrams sent | 688 | 688-688 | - | - | - |
| datagrams received | 26 | 26-27 | - | - | - |

##### Flood (yes | head -c 5000000)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 1.67 | 1.64-2.39 | 1.69 | 1.62-1.71 | 0.99 |
| CPU time user+sys (s) | 0.002 | 0.001-0.006 | 0.001 | 0.001-0.002 | 1.37 |
|   user (s) | 0.001 | 0.000-0.003 | 0.001 | 0.001-0.001 | 1.20 |
|   sys (s) | 0.001 | 0.000-0.004 | 0.001 | 0.001-0.001 | 1.58 |
| CPU, % of one core | 0.10 | 0.05-0.27 | 0.07 | 0.07-0.11 | 1.38 |
| peak RSS (MB) | 2.8 | 2.5-3.1 | 15.3 | 15.3-15.4 | 0.18 |
| final RSS (MB) | 2.8 | 2.5-3.1 | 15.3 | 10.6-15.4 | 0.18 |
| physical footprint (MB, macOS) | 2.1 | 1.8-2.4 | 7.5 | 7.4-7.7 | 0.28 |
| peak footprint (MB, macOS) | 2.1 | 1.8-2.4 | 7.6 | 7.5-7.7 | 0.28 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| ctx switches /s (macOS, total) | 12.0 | 6.1-31.4 | 9.4 | 8.9-11.7 | 1.28 |
| wakeups /s (macOS, idle+interrupt) | 3.0 | 1.8-16.3 | 5.3 | 3.1-5.3 | 0.57 |
| minor page faults (window) | 22 | 2-45 | 40 | 36-44 | 0.55 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| instructions retired (macOS, window) | 4.6 M | 1.9-14.0 M | 3.9 M | 3.7-4.3 M | 1.17 |
| cycles (macOS, window) | 4.1 M | 1.7-17.2 M | 3.4 M | 3.4-3.5 M | 1.19 |
| bytes sent (moche --stats) | 723 | 586-4921 | - | - | - |
| bytes received (moche --stats) | 1596 | 924-2269 | - | - | - |
| datagrams sent | 9 | 7-45 | - | - | - |
| datagrams received | 17 | 9-27 | - | - | - |

##### 256-colour dump (200x60 pane, 5 passes)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 0.29 | 0.23-1.17 | 0.24 | 0.23-0.28 | 1.17 |
| CPU time user+sys (s) | 0.013 | 0.012-0.013 | 0.028 | 0.028-0.030 | 0.45 |
|   user (s) | 0.010 | 0.007-0.010 | 0.027 | 0.027-0.028 | 0.38 |
|   sys (s) | 0.003 | 0.003-0.005 | 0.001 | 0.001-0.002 | 1.96 |
| CPU, % of one core | 4.62 | 1.02-5.46 | 11.56 | 10.80-12.14 | 0.40 |
| peak RSS (MB) | 6.4 | 6.3-7.9 | 17.8 | 17.8-18.0 | 0.36 |
| final RSS (MB) | 6.4 | 6.3-7.9 | 17.8 | 17.8-18.0 | 0.36 |
| physical footprint (MB, macOS) | 5.7 | 5.6-7.2 | 10.1 | 10.0-10.2 | 0.57 |
| peak footprint (MB, macOS) | 5.7 | 5.6-7.2 | 10.1 | 10.0-10.2 | 0.57 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| ctx switches /s (macOS, total) | 1664.3 | 296.8-2104.4 | 1281.2 | 1144.4-1355.5 | 1.30 |
| wakeups /s (macOS, idle+interrupt) | 10.5 | 0.0-44.6 | 8.6 | 8.2-10.8 | 1.21 |
| minor page faults (window) | 148 | 139-239 | 124 | 124-137 | 1.19 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| instructions retired (macOS, window) | 151.1 M | 102.6-152.4 M | 405.4 M | 402.9-405.9 M | 0.37 |
| cycles (macOS, window) | 36.2 M | 34.5-36.4 M | 83.7 M | 82.8-84.5 M | 0.43 |
| bytes sent (moche --stats) | 709 | 630-5784 | - | - | - |
| bytes received (moche --stats) | 22440 | 22214-118853 | - | - | - |
| datagrams sent | 7 | 6-56 | - | - | - |
| datagrams received | 27 | 24-133 | - | - | - |

##### Steady output (seq 1 200000)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 1.19 | 0.25-1.25 | 0.26 | 0.24-0.30 | 4.61 |
| CPU time user+sys (s) | 0.006 | 0.001-0.006 | 0.002 | 0.002-0.002 | 2.64 |
|   user (s) | 0.002 | 0.001-0.002 | 0.002 | 0.001-0.002 | 1.37 |
|   sys (s) | 0.004 | 0.000-0.004 | 0.001 | 0.001-0.001 | 6.00 |
| CPU, % of one core | 0.48 | 0.42-0.49 | 0.85 | 0.60-0.91 | 0.57 |
| peak RSS (MB) | 2.8 | 2.7-2.8 | 14.9 | 14.7-15.9 | 0.19 |
| final RSS (MB) | 2.8 | 2.7-2.8 | 10.5 | 10.5-15.9 | 0.27 |
| physical footprint (MB, macOS) | 2.3 | 2.0-2.5 | 8.1 | 8.0-8.2 | 0.28 |
| peak footprint (MB, macOS) | 2.3 | 2.0-2.5 | 8.1 | 8.0-8.2 | 0.28 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| ctx switches /s (macOS, total) | 58.8 | 55.9-63.6 | 69.6 | 57.0-82.9 | 0.84 |
| wakeups /s (macOS, idle+interrupt) | 38.6 | 7.9-40.7 | 8.3 | 7.7-10.1 | 4.66 |
| minor page faults (window) | 45 | 15-51 | 67 | 65-86 | 0.67 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| instructions retired (macOS, window) | 11.4 M | 3.9-12.4 M | 10.3 M | 10.3-10.5 M | 1.10 |
| cycles (macOS, window) | 16.4 M | 2.7-17.1 M | 4.6 M | 4.5-5.9 M | 3.60 |
| bytes sent (moche --stats) | 5649 | 591-6065 | - | - | - |
| bytes received (moche --stats) | 2500 | 2288-2592 | - | - | - |
| datagrams sent | 51 | 7-55 | - | - | - |
| datagrams received | 15 | 14-15 | - | - | - |


## Linux aarch64 (lab VM)

_Not run yet. Run the commands in "Linux run in the VM lab" above, then paste the medians
table and the generated `report.md` here._
