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
instructions/cycles, and the footprint columns stay empty. The Linux code paths first ran on 2026-11-04 (see the Linux section; two small fixes were needed:
schedstat for exact CPU time and an lscpu fallback for the CPU model); on a new platform, if a column is
empty, look at `runs/*.json` for the raw samples first.

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
  matters on the phone (it is the only place moche is not better or equal). Checked on Linux with
  tcpdump: stock sends exactly the same number of datagrams, so it is not an excess (see the Linux section).
- **Startup**: first paint in 0.17 s versus 0.34 s; 8 ms versus 44 ms of CPU. One moche
  run took 0.94 s to first paint (min-max 0.15-0.94): an outlier, probably the first server
  frame lost or delayed, not reproduced in the other 2 runs, nor in 20 runs per client on Linux
  (max 0.137 s; see the Linux section).
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

Measured 2026-11-04, `linux-aarch64-vm`: client VM `moche-b` (Ubuntu 24.04, kernel 6.8, 2 vCPU of an
Apple host under Virtualization.framework, 2 GiB RAM, tmux 3.4, python 3.12), server VM `moche-a`
(Debian 13, stock mosh-server 1.4.0) over the VM-to-VM user-v2 network (a real virtual network, RTT
about 1.5 ms: not loopback). moche 0.1.0 (commit b26ebe8) is the release artefact
`moche-0.1.0-aarch64-unknown-linux-musl` built on a second Mac with `tools/release/` (static, no
shared libraries); baseline is the distro `mosh-client` 1.4.0 (dynamic, glibc). Tool run:
`--server-ssh` (fresh server per run), 3 repetitions, idle 60 s, typing 30 s. All 42 runs passed
(paste sha256 matched every time). The CPU figures on Linux come from `/proc/<pid>/schedstat`
(exact on-CPU nanoseconds); the user/sys split below it still comes from 10 ms ticks and is coarse.

### Medians, moche vs stock mosh-client (Linux VM) and the macOS figures

| scenario | metric | moche (Linux) | stock (Linux) | ratio Linux | ratio macOS |
|---|---|---:|---:|---:|---:|
| startup | launch to first paint (s) | 0.122 | 0.269 | 0.45 | 0.49 |
| startup | CPU (s) | 0.003 | 0.004 | 0.70 | 0.19 |
| idle 60 s | CPU (s) / % of a core | 0.027 / 0.05 | 0.009 / 0.02 | 3.0 | 1.4 |
| idle 60 s | peak RSS (VmHWM) / PSS (MB) | 1.1 / 0.9 | 13.9 / 9.8 | 0.08 / 0.09 | 0.17 / 0.22 (footprint) |
| idle 60 s | voluntary ctx switches /s | 0.7 | 0.6 | 1.1 | 1.6 |
| idle 60 s | traffic (moche) | 24 datagrams out, 1.7 KB each way | | | 26 out |
| typing 30 s | CPU (s) / % of a core | 0.494 / 1.57 | 0.184 / 0.58 | 2.7 | 1.4 |
| typing 30 s | minor page faults | 58 325 | 1 426 | 41 | 2.1 |
| typing 30 s | peak RSS / PSS (MB) | 1.4 / 1.1 | 14.0 / 9.9 | 0.10 / 0.11 | 0.21 / 0.25 |
| typing 30 s | datagrams for 300 keys | 306 out, 604 in | (same on the wire, see below) | | 413 out, 638 in |
| paste 1 MiB | wall (s) | 0.46 | 0.40 | 1.15 | 1.15 |
| paste 1 MiB | CPU (s) | 0.037 | 0.207 | 0.18 | 0.15 |
| paste 1 MiB | peak RSS / PSS (MB) | 3.5 / 3.2 | 182.6 (max 241.9) / 178.6 | 0.02 | 0.02 (stock 199, max 732) |
| flood 5 MB | wall (s) / CPU (s) | 0.74 / 0.004 | 0.70 / 0.002 | | 1.67 / 0.002 |
| colors 200x60 | CPU (s) | 0.004 | 0.008 | 0.45 | 0.45 |
| colors 200x60 | peak RSS / PSS (MB) | 3.0 / 2.7 | 15.7 / 11.7 | 0.19 / 0.23 | 0.36 / 0.57 |
| steady `seq` | wall (s) / CPU (s) | 0.13 / 0.002 | 0.18 / 0.002 | 0.69 | 4.6 |

Static: moche 0.63 MB on disk (stripped by the build), `ldd`: not a dynamic executable (no PT_INTERP,
no DT_NEEDED; also runs under `env -i`, from an empty directory and inside a chroot holding only the
binary), segments text 613 KB, data 17 KB, bss 42 KB. Stock mosh-client 0.33 MB but 9 shared libraries
(libtinfo, libprotobuf, libcrypto, libz, libstdc++, libm, libgcc_s, libc, ld.so).

### Reading

- **Memory is 10x smaller**: 1.1 MB peak RSS (0.9 MB PSS) at idle against 13.9 MB (9.8 PSS); after the
  heaviest scenario moche is at 3.5 MB, stock at 15.7 MB (183 MB in paste). Exact `VmHWM` peaks, so
  the Linux ratios (0.08-0.10) are tighter than the sampled macOS ones.
- **Paste behaves the same on Linux**: the stock client balloons to 177-242 MB RSS for a 1 MiB paste
  (median 183 MB, macOS 199 MB) and spends 5.6x the CPU of moche (0.21 s against 0.04 s); moche stays at
  3.5 MB (838 KB sent in 683 datagrams). Wall time is on par (0.46 s against 0.40 s, stock min 0.26 s).
  So the stock-client balloon is not a macOS artefact.
- **Idle is the first place moche is clearly worse**: 27 ms of CPU in 60 s (0.05 % of a core) against
  9 ms (0.02 %), 3x, but still nothing in absolute terms, 0.7 wakeups per second like stock, one
  heartbeat datagram every 2.5 s each way, one thread. The extra CPU is the allocator churn below:
  2 840 minor page faults in the window against 362 for stock (24 heartbeats, about 120 faults each).
- **Typing costs 2.7x the stock CPU on Linux, not 1.4x** (0.49 s against 0.18 s over 30 s, 1.6 % against
  0.6 % of a core; sys time is 0.31 s against 0.08 s). Same wakeup rate as stock (39 against 44
  context switches per second). Cause found (below): the page-fault and mmap churn of the static musl
  allocator. On macOS (system malloc) the same code costs 1.4x.
- **Typing datagram count is not an excess.** `tcpdump` on the client VM, 100 keystrokes at 10/s, same
  server, both clients: moche 101 client-to-server and 200 server-to-client datagrams, stock 101 and 200,
  sizes 65-87 B against 67-85 B. Pattern per key, identical for both: the keystroke datagram goes out
  after the 8 ms collect delay, the server answers with the echo frame about 12 ms after the key and a
  second frame about 50 ms after the first; the client's acknowledgement rides on the next keystroke
  datagram 100 ms later, so there is no standalone ack. In the full run moche sent 306 datagrams for 300
  keys (1.02 per key), not the 413 seen on macOS. Not acks, not resends, not heartbeats, not the 8 ms
  delay: the macOS 1.38 per key was most likely an artefact of the 100 ms key interval racing the ack
  delay timer on loopback (standalone acks when a key arrives a few ms late); stock was not counted on
  macOS, so that part stays a hypothesis.
- **Why typing and idle burn more CPU: allocator churn.** `strace -c` on the moche pid during 100
  keystrokes: 1 406 `mmap` and 1 406 `munmap` (about 14 pairs per keystroke), 401 `ppoll`, 400
  `recvfrom` (half EAGAIN), 101 `sendto`; stock: no `mmap` at all (404 `brk`), 464 `pselect6`. A
  `strace -e mmap,munmap` trace around single keystrokes shows 6 anonymous mappings of 69 632, 86 016,
  36 864, 69 632, 69 632 and 36 864 bytes created and freed on every outgoing datagram, and 2-3 more of
  36-65 KB around each received datagram: whole-screen-sized buffers (an 80x24 grid is 1 920 cells)
  allocated and dropped per event. musl's allocator maps and unmaps blocks of that size every time,
  where glibc and the macOS libc keep them in the heap; the result is a page fault per 4 KB touched
  (58 k faults in typing, 41x stock) and all the extra sys time. Fix directions for the core (not done
  here, `crates/**` untouched): reuse the frame and diff buffers across events (or avoid cloning full
  snapshots when sending a keystroke), or link a MIT/Apache global allocator in the musl builds. Not a
  correctness problem and not visible at the user's level (1.6 % of one core while typing), but it is the
  one thing in the table where moche is worse, and it costs battery on a phone.
- **Startup**: first paint at 0.122 s against 0.269 s, 3 ms against 4 ms of CPU. See the distribution
  below; nothing like the macOS outlier.
- Linux-vs-macOS differences that matter when comparing: the real (virtual) network instead of loopback
  (about 1.5 ms RTT, the server answers the first datagram in 1.3-2 ms); a glibc stock client and a
  musl moche instead of two system-libc binaries; 10 ms scheduler-tick CPU on Linux (replaced by
  schedstat in the totals); exact `VmHWM` and PSS instead of sampled RSS and `phys_footprint`; no
  wakeup, instruction or cycle counters. Bulk scenarios are shorter still (0.1-0.7 s windows).

### Startup distribution, 20 runs per client (Linux VM, no loss)

| client | min | median | p90 | max |
|---|---:|---:|---:|---:|
| moche | 0.119 s | 0.128 s | 0.131 s | 0.137 s |
| stock | 0.261 s | 0.274 s | 0.278 s | 0.280 s |

No outlier in 40 runs: the spread is 18 ms for moche. The macOS run in which moche needed 0.94 s
(against 0.15 s) was therefore not reproduced; `tcpdump` of all 40 flows shows the server answering the
first client datagram in 1.3-2.0 ms every time and no client datagram repeated within the first second,
so the transport's 1 s initial SRTT does not stall a normal connection setup. Whether the macOS outlier
was a lost or late first frame or a slow first shell prompt on the server is not provable from that run.

Extra experiment, a loss test, 20 runs per client with 25 % random loss of client-to-server UDP applied
by `iptables` on the client VM (`tools/perf` unchanged): moche first paint min 0.118, median 0.132,
max 0.634 s; 5 of 20 runs at 0.27-0.29 s, 2 at 0.53 s, 2 at 0.63 s, 11 at 0.12-0.13 s. Never close to
1 s, so a lost opening datagram costs 150-500 ms (the recovery comes from the ack and heartbeat timers,
not from the 1 s SRTT-based resend), which would explain an occasional slow start on a lossy path. The
stock client showed no run above 0.28 s in the same test (its first paint is not tied to its own first
datagram), so it is less sensitive to this loss pattern. `tcpdump` on the sender does not see packets
dropped by the netfilter output chain, so the exact timing of the dropped datagram was not captured;
the flow-by-flow attribution to a client was ambiguous and is not claimed. Worth re-running on the
phone's real path before drawing a conclusion.

### Tool fixes made for this run

- `perf.py`: Linux CPU totals now come from `/proc/<pid>/schedstat` (the 10 ms tick showed 0.000 for idle
  and startup); the CPU model falls back to lscpu `Vendor ID` when the VM reports `-`. Everything else
  in the Linux code paths worked on the first run (PSS, VmHWM, context switches, ssh server mode).
- The Linux branches of `measure.sh` and `perf.py` ran for the first time here and needed nothing else.
  `measure.sh` takes `--ssh-opt VALUE` (not `--ssh-opt=VALUE`).

### Full generated report (host `linux-aarch64-vm`)

#### Host `linux-aarch64-vm`

| | |
|---|---|
| label | linux-aarch64-vm |
| OS | Ubuntu 24.04.5 LTS (Linux 6.8.0-147-generic) |
| CPU | Apple (VM guest, model name not exposed), 2 cores |
| RAM | 1.9 GiB |
| moche | moche 0.1.0 (b26ebe8, aarch64-unknown-linux-musl) |
| mosh-client (baseline) | mosh-client (mosh 1.4.0) [build mosh 1.4.0] |
| mosh-server | mosh-server (mosh 1.4.0) [build mosh 1.4.0] (ssh) |
| tmux / python | tmux 3.4 / 3.12.3 |
| locale | en_US.UTF-8 |
| date (UTC) | 2026-11-04T12:17:35Z |

##### Static facts

| binary | size MB (as built) | stripped MB | dynamic deps | file |
|---|---:|---:|---:|---:|
| moche | 0.63 | 0.63 | 1 | ELF 64-bit LSB executable, ARM aarch64, statically linked, stripped |
| mosh-client | 0.33 | 0.33 | 10 | ELF 64-bit LSB pie executable, ARM aarch64, dynamically linked, stripped |

(`moche`: "not a dynamic executable"; the "1" is that line. `mosh-client` links libtinfo, libprotobuf,
libcrypto, libz, libstdc++, libm, libgcc_s, libc and the loader.)

##### Startup (launch to first paint, +1 s settle)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| launch to first paint (s) | 0.122 | 0.119-0.130 | 0.269 | 0.263-0.274 | 0.45 |
| wall time (s) | 1.13 | 1.12-1.13 | 1.27 | 1.26-1.28 | 0.89 |
| CPU time user+sys (s) | 0.003 | 0.002-0.003 | 0.004 | 0.004-0.004 | 0.70 |
| CPU, % of one core | 0.23 | 0.18-0.28 | 0.29 | 0.29-0.33 | 0.80 |
| peak RSS (MB) | 1.2 | 1.0-1.2 | 13.6 | 13.6-13.6 | 0.09 |
| final RSS (MB) | 0.9 | 0.9-0.9 | 13.6 | 13.6-13.6 | 0.07 |
| PSS (MB, Linux) | 0.9 | 0.9-0.9 | 9.6 | 8.9-9.6 | 0.09 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| bytes sent / received (moche --stats) | 217 / 160 | 211-217 / 155-161 | - | - | - |
| datagrams sent / received | 3 / 2 | 3-3 / 2-2 | - | - | - |

##### Idle (heartbeats only)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 60.01 | 60.01-60.01 | 60.01 | 60.00-60.01 | 1.00 |
| CPU time user+sys (s) | 0.027 | 0.026-0.032 | 0.009 | 0.009-0.011 | 3.01 |
| CPU, % of one core | 0.05 | 0.04-0.05 | 0.02 | 0.02-0.02 | 3.01 |
| peak RSS (MB) | 1.1 | 1.1-1.2 | 13.9 | 13.9-13.9 | 0.08 |
| final RSS (MB) | 0.9 | 0.9-0.9 | 13.9 | 13.9-13.9 | 0.06 |
| PSS (MB, Linux) | 0.9 | 0.9-0.9 | 9.8 | 9.1-9.8 | 0.09 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| voluntary ctx switches /s | 0.7 | 0.6-0.7 | 0.6 | 0.5-0.6 | 1.14 |
| involuntary ctx switches /s | 0.0 | 0.0-0.0 | 0.0 | 0.0-0.0 | - |
| minor page faults (window) | 2840 | 2788-2840 | 362 | 362-375 | 7.85 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| bytes sent / received (moche --stats) | 1686 / 1645 | 1678-1688 / 1635-1649 | - | - | - |
| datagrams sent / received | 24 / 23 | 24-24 / 23-23 | - | - | - |

##### Typing (~10 chars/s)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 31.51 | 31.51-31.51 | 31.51 | 31.51-31.52 | 1.00 |
| CPU time user+sys (s) | 0.494 | 0.478-0.501 | 0.184 | 0.171-0.197 | 2.68 |
|   user (s, 10 ms ticks) | 0.180 | 0.150-0.190 | 0.090 | 0.090-0.100 | 2.00 |
|   sys (s, 10 ms ticks) | 0.310 | 0.300-0.330 | 0.080 | 0.080-0.090 | 3.88 |
| CPU, % of one core | 1.57 | 1.52-1.59 | 0.58 | 0.54-0.62 | 2.68 |
| peak RSS (MB) | 1.4 | 1.4-1.4 | 14.0 | 14.0-14.0 | 0.10 |
| final RSS (MB) | 1.1 | 1.1-1.1 | 14.0 | 14.0-14.0 | 0.08 |
| PSS (MB, Linux) | 1.1 | 1.1-1.1 | 9.9 | 9.9-9.9 | 0.11 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| voluntary ctx switches /s | 38.9 | 38.8-39.2 | 43.7 | 43.2-44.6 | 0.89 |
| involuntary ctx switches /s | 0.2 | 0.1-0.3 | 0.1 | 0.1-0.3 | 1.50 |
| minor page faults (window) | 58325 | 58322-58412 | 1426 | 1425-1426 | 40.90 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| open fds (final) | 9 | 9-9 | 5 | 5-5 | 1.80 |
| bytes sent / received (moche --stats) | 24161 / 49473 | 24126-24229 / 49422-49495 | - | - | - |
| datagrams sent / received | 306 / 604 | 306-307 / 604-604 | - | - | - |

##### Paste (1 MiB base64 into cat > file, sha256 checked)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| paste to sha-verified (s) | 0.46 | 0.45-0.46 | 0.40 | 0.26-0.54 | 1.15 |
| CPU time user+sys (s) | 0.037 | 0.034-0.037 | 0.207 | 0.150-0.228 | 0.18 |
| CPU, % of one core | 8.08 | 7.42-8.16 | 52.12 | 42.34-57.69 | 0.16 |
| peak RSS (MB) | 3.5 | 3.4-3.5 | 182.6 | 177.4-241.9 | 0.02 |
| final RSS (MB) | 3.2 | 3.1-3.2 | 182.6 | 177.4-241.9 | 0.02 |
| PSS (MB, Linux) | 3.2 | 3.1-3.2 | 178.6 | 173.3-237.9 | 0.02 |
| threads (final) | 1 | 1-1 | 1 | 1-1 | 1.00 |
| voluntary ctx switches /s | 170.1 | 124.7-323.5 | 81.7 | 62.8-111.2 | 2.08 |
| involuntary ctx switches /s | 187.1 | 159.3-238.2 | 280.0 | 178.2-339.3 | 0.67 |
| minor page faults (window) | 4917 | 4852-4918 | 44346 | 42782-59962 | 0.11 |
| major page faults (window) | 0 | 0-0 | 0 | 0-0 | - |
| bytes sent / received (moche --stats) | 838422 / 34475 | 838270-838691 / 33335-34682 | - | - | - |
| datagrams sent / received | 683 / 46 | 683-687 / 45-46 | - | - | - |

##### Flood (yes | head -c 5000000)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 0.74 | 0.67-0.75 | 0.70 | 0.69-0.72 | 1.05 |
| CPU time user+sys (s) | 0.004 | 0.003-0.005 | 0.002 | 0.002-0.003 | 1.49 |
| CPU, % of one core | 0.54 | 0.46-0.64 | 0.33 | 0.28-0.36 | 1.61 |
| peak RSS (MB) | 1.4 | 1.4-1.4 | 14.4 | 14.4-14.4 | 0.10 |
| PSS (MB, Linux) | 0.9 | 0.9-1.1 | 10.3 | 10.3-10.3 | 0.09 |
| minor page faults (window) | 1634 | 1440-1840 | 207 | 166-248 | 7.89 |
| datagrams sent / received | 9 / 23 | 9-10 / 21-27 | - | - | - |

##### 256-colour dump (200x60 pane, 5 passes)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 0.13 | 0.12-0.13 | 0.13 | 0.13-0.14 | 0.97 |
| CPU time user+sys (s) | 0.004 | 0.004-0.005 | 0.008 | 0.008-0.008 | 0.45 |
| CPU, % of one core | 2.96 | 2.89-3.75 | 6.19 | 6.18-6.48 | 0.48 |
| peak RSS (MB) | 3.0 | 2.9-3.4 | 15.7 | 15.7-15.9 | 0.19 |
| PSS (MB, Linux) | 2.7 | 2.6-3.0 | 11.7 | 11.6-11.8 | 0.23 |
| minor page faults (window) | 995 | 915-1232 | 262 | 245-303 | 3.80 |
| datagrams sent / received | 5 / 10 | 5-5 / 10-12 | - | - | - |

##### Steady output (seq 1 200000)

| metric | moche median (n=3) | moche min-max | stock median (n=3) | stock min-max | moche / stock |
|---|---:|---:|---:|---:|---:|
| wall time (s) | 0.13 | 0.12-0.13 | 0.18 | 0.18-0.19 | 0.69 |
| CPU time user+sys (s) | 0.002 | 0.001-0.002 | 0.002 | 0.002-0.002 | 0.67 |
| CPU, % of one core | 1.24 | 1.07-1.27 | 1.33 | 1.18-1.34 | 0.94 |
| peak RSS (MB) | 1.3 | 1.2-1.4 | 14.5 | 14.4-14.8 | 0.09 |
| PSS (MB, Linux) | 1.2 | 1.1-1.2 | 10.0 | 10.0-10.7 | 0.12 |
| minor page faults (window) | 492 | 350-492 | 212 | 187-277 | 2.32 |
| datagrams sent / received | 5 / 8 | 5-6 / 7-8 | - | - | - |

(Condensed from the generated `report.md`: rows that were identical in both clients, the all-zero
user/sys tick rows of the short scenarios and the dynamic-library address listing are omitted; the raw
`runs/*.json` and the full report stay in the VM lab output directory.)


## CPU optimisation log

Measured 2026-11-04 on the Mac (M1, macOS 26.4, stock mosh-server 1.4.0 on loopback), 3 runs per
scenario, medians; baseline is HEAD b26ebe8 built the same way. Every optimisation below is
wire-neutral and proven so by `tools/perf/golden-diff.sh [REF]`: it runs the deterministic scenario
`crates/moche-core/examples/golden.rs` (300 typed keys, a 150 KB lossy paste with 12 % loss, 40 KB
more, a resize, heartbeats, a 71-fragment server repaint, 120 VT fuzz cases) on REF and on the working
tree and requires the output to be byte-identical: the sealed bytes of all 1561 datagrams with their
emission time, every `next_deadline_ms` change, decoded events, host paints and VT hashes. Result:
IDENTICAL. Bench: `cargo run --release -p moche-ffi --example bench` (ns/op, allocs/op);
allocation sites: `... --example allocsites` (a counting allocator in moche-ffi, the only crate
allowed `unsafe`).

### Where the CPU went (before)

- Typing (30 s, 300 keys): 0.143 s CPU, user 0.065 / sys 0.077, 223 M instructions; sys dominates.
  Per key (instrumented copy of the CLI, not committed): 4.5 poll, 1 read(stdin), 4.2 recvfrom (half
  EAGAIN), 1.4 sendto, 1.8 write(tty), **4.5 ioctl(TIOCGWINSZ)**, **2.8 read(/dev/urandom)**: about 21
  syscalls and 4.5 wakeups per key. `sample` showed time in the ioctl/sendto/recvfrom/madvise
  syscalls and in large `malloc`/`madvise`; user-side hot spots were deflate/inflate state setup and
  framebuffer clones.
- Allocations per event (counting allocator, 80x24): outgoing datagram 7 allocations >= 4 KiB
  (65 712, 85 196, 4 320, 33 026, 65 536, 65 536, 32 768 B: the miniz compressor state built for each
  datagram); incoming frame 4 (32 768 + 43 296: the inflate state; 61 440 x 2: `Screen::apply`
  clone and the display's `shown` clone, 384 000 x 2 at 200x60). These match the sizes the Linux
  strace shows.
- Micro-benchmarks before: compress of a keystroke instruction 6.3 us and 352 KB allocated; heartbeat
  frame 7.2 us / 352 KB; small server frame `handle_datagram` 3.1 us / 77 KB; `Screen::apply` 80x24
  3.0 us / 61 KB, 200x60 19 us / 384 KB, 400x120 80 us / 1.5 MB.

### Implemented

| change | effect (measured) |
|---|---|
| `network::Deflater` held by the Transport; `Compress::reset` instead of a new encoder per datagram (same bytes: unit test vs a fresh streaming encoder on 45 inputs up to 300 KB) | keystroke/heartbeat frame 352 KB -> 0.9 KB allocated, 0 allocations >= 4 KiB; time unchanged (reset still clears the tables, 5-6 us) |
| `Reassembler` keeps a `Decompress`; single-fragment messages skip the group map and the copy (unit test vs the old `ZlibDecoder` on good, truncated, corrupt and trailing-garbage input) | small server frame 3.1 us -> 1.0 us, 77 KB -> 1 KB |
| `Transport::receive` returns the sequence number and events; FFI `Core::datagram` used to open every datagram twice | one OCB3 open (about 0.24 us / 100 B, 2.9 us / 1252 B) saved per received datagram; FFI path only |
| `Screen` states are `Arc<Framebuffer>`; an empty diff (echo-ack, ack) shares the base instead of copying; `latest_shared()`; the display/FFI keep the painted state as an `Arc` | no copy for the 2nd server frame per key; no `shown` clone |
| `Screen` recycles discarded grids (`Framebuffer::clone_from`, at most 2 spare) | `Screen::apply` 80x24 3.0 -> 2.2 us, 200x60 19 -> 13.7 us, 400x120 80 -> 57 us; 61 KB -> 0.4 KB allocated; **steady state: 0 allocations >= 1 KiB for a server frame, an echo-ack frame, a keystroke frame and an ack frame, at 80x24 and 200x60** |
| CLI: paint skipped when the screen shown is unchanged (`ptr_eq` / `display_eq`) | about 0.8 tty write per key |
| CLI: window size read only after a self-pipe wake (SIGWINCH), not on every loop turn | 4.5 ioctl per key -> 0 |
| CLI: `/dev/urandom` through a 512 B `BufReader` (2 reads per frame before) | 2.8 read per key -> about 0.1 |
| VT: printable-ASCII fast path in the ground state | within noise (5.4 -> 5.1 ms per 100 KB); kept, verified by the VT fuzz in the golden run |

### Before / after on the Mac (medians of 3)

| scenario | metric | before | after | change |
|---|---|---:|---:|---:|
| typing 30 s | CPU user + sys (s) | 0.143 (0.065 + 0.077) | 0.112 (0.048 + 0.062) | -22 % |
| typing 30 s | instructions / cycles (M) | 223 / 239 | 170 / 184 | -24 % / -23 % |
| typing 30 s | peak RSS (KB) | 3232 | 2704 | |
| paste 1 MiB | CPU (s) | 0.051 | 0.052 | none (bound by VT scrolling and zlib) |
| idle 60 s | CPU (s) | 0.013 | 0.008 | -38 % |
| idle 60 s | instructions (M) | 14 | 6 | -57 % |

Measured spread is 0.102-0.114 s for typing after against 0.140-0.152 before; datagram counts
vary 385-469 per run in both and are not affected by these changes. On macOS the system malloc
caches big blocks, so the allocation fix shows up as fewer instructions, not page faults; the
Linux/musl effect (mmap + munmap + page faults per block) is not measurable here.

### Not done / not found

- The per-byte VT cost is dominated by scrolling: `rotate_left` of the whole grid per line feed
  (about 3 us at 80x24, 17 us at 200x60, 1.4 ns per cell). A row-indirection (a permutation vector
  of row indices) would make a scroll O(1); not done (touches about 15 sites in `vt.rs`, no
  effect on typing). Paste is already 0.15x stock CPU.
- Re-compressing a resent state (cache) is not wire-neutral: each resend draws fresh chaff, and the
  chaff is inside the compressed message. Proposal: compress the diff once per state and append the
  chaff outside the deflate stream (needs a stock-server check). Only helps lossy links (16 KiB
  resend 206 us).
- Wakeups (4.5 per key) and per-datagram syscalls are set by the protocol timing (8 ms collection,
  100 ms ack delay, server frames 12 and 60 ms after a key). The datagram pattern per key is the
  same for the stock client (1.0-1.3 client datagrams per key through a loopback relay, 2.2 server
  datagrams per key for both).

### Proposals (not implemented: they change what or when we send)

1. Ack delay 100 -> about 108-110 ms. Observed in isolated-key traces: moche's standalone ack leaves
   104-108 ms after the first server frame, stock's 111-115 ms. At 10 keys/s plus tmux jitter the next
   keystroke lands at 108-128 ms, so the 100 ms timer fires first and costs a datagram, a wake and
   a poll: moche 1.45-1.56 client datagrams per key against 1.29 for stock through the relay (macOS
   loopback only; on Linux the counts are equal, so low value there). Expected: -0.2 datagram, -1 wake
   and about -8 syscalls per key on macOS. Risk: the server's retransmission of its own frame if the
   ack is late (stays far below its resend interval at RTT 1-50 ms; not tested at high RTT). Lab test:
   relay or tcpdump typing at 10 and 5 keys/s at RTT 1, 50, 200 ms, 3 values (100, 108, 116),
   counting client datagrams per key and server frames with no effect (duplicates). My A/B attempt on
   loopback was inside the noise (+-0.3 per key), so this is a hypothesis.
2. Chaff only in the first send of a state, or none. Saves 2 RNG draws per frame and about 8 B; the
   stock server never needed it? Untested (protocol.md U12). Negligible CPU; skip unless a test shows
   the server accepts chaffless frames.
3. A musl-friendly allocator (mimalloc or rpmalloc, both MIT, built from C by `cc`; with the zig
   cross toolchain it needs the C source compiled for `aarch64-linux-musl` and adds a few hundred KB
   to the 0.63 MB binary: unverified here, no new dependency was added). Not needed after the fix
   above: the steady state allocates nothing over 1 KiB, so there is nothing left for musl to
   mmap. If a Linux strace still shows mappings, a tiny arena for the three per-frame buffers (the
   two grids, the compressor) is simpler than a global allocator; try that first.

### Linux re-check (to run in the lab VM)

Build the musl release from this tree (`tools/release/build.sh` as in docs/RELEASING.md), copy the
binary to moche-b, then:

```sh
cd perf && ./measure.sh --label linux-aarch64-vm-opt --moche /path/to/moche \
    --server-ssh <user>@<a-u2-ip> --server-host <a-u2-ip> --scenarios typing,idle --reps 3
# page faults and mappings per keystroke (expect no mmap/munmap per key, minor faults near stock's):
strace -f -c -p $(pgrep -x moche) -e trace=mmap,munmap,brk,mprotect   # type 100 keys, then Ctrl-C
```

Compare typing CPU (was 0.494 s), minor page faults (58 325) and idle CPU (0.027 s) with the
numbers in the Linux section; a good result is typing near 0.2 s and a few thousand faults.

### Linux A/B, same VM, same day

Measured 2026-11-04 in the lab VM, 3 repetitions per cell, typing 30 s, idle 60 s, paste 1 MiB,
startup; medians, with min-max in brackets for the cells that matter. Two release artefacts built
with `tools/release/build.sh aarch64-unknown-linux-musl` (rustc 1.99.0, pinned zig 0.17.1) from fresh
clones: baseline `61b6da1` (`moche 0.1.0 (61b6da1, aarch64-unknown-linux-musl)`, 631 680 B) and new
`bf5951d` (`moche 0.1.0 (bf5951d, aarch64-unknown-linux-musl)`, 631 392 B). Client VM `moche-b`
(2 vCPU, 2 GiB), server VM `moche-a` (stock mosh-server 1.4.0) over the VM-to-VM network, fresh server
per run, `tools/perf/measure.sh --server-ssh` once per moche binary (label `linux-aarch64-vm-ab-base`
and `-new`, scenarios typing,idle,paste,startup). The stock `mosh-client` 1.4.0 is measured in both
runs, so it appears twice: the two stock columns are the same binary a few minutes apart, i.e. the
run-to-run noise of the lab. All 48 runs passed (paste sha256 matched every time). The baseline column
reproduces the earlier Linux section (typing 0.44-0.50 s, 58 k faults), so the lab did not drift.

| scenario | metric | moche baseline `61b6da1` | moche new `bf5951d` | stock (run 1) | stock (run 2) |
|---|---|---:|---:|---:|---:|
| typing 30 s | CPU total (s) | 0.442 [0.413-0.497] | **0.155 [0.144-0.167]** | 0.173 [0.166-0.196] | 0.219 [0.192-0.252] |
| typing 30 s | CPU user / sys (s, 10 ms ticks) | 0.16 / 0.27 | 0.08 / 0.07 | 0.09 / 0.08 | 0.10 / 0.10 |
| typing 30 s | minor / major page faults | 58 324 / 0 | **32 / 0** | 1 426 / 0 | 1 425 / 0 |
| typing 30 s | peak RSS / PSS (MB) | 1.4 / 1.1 | 1.3 / 1.3 | 14.0 / 9.9 | 14.0 / 9.9 |
| typing 30 s | voluntary ctx switches /s | 38.8 | 38.6 | 43.4 | 43.7 |
| typing 30 s | datagrams out / in (300 keys) | 306-307 / 604 | 306 / 604 | | |
| idle 60 s | CPU total (s) | 0.023 [0.022-0.028] | **0.007 [0.006-0.008]** | 0.009 | 0.010 |
| idle 60 s | minor faults | 2 840 | **0** | 370 | 362 |
| idle 60 s | peak RSS / PSS (MB) | 1.2 / 0.9 | 1.2 / 1.2 | 13.9 / 9.8 | 13.9 / 9.8 |
| idle 60 s | voluntary ctx switches /s | 0.7 | 0.7 | 0.5 | 0.6 |
| paste 1 MiB | wall (s) | 0.462 | 0.452 | 0.509 | 0.420 |
| paste 1 MiB | CPU total (s) | 0.034 | 0.035 | 0.243 | 0.204 |
| paste 1 MiB | minor faults | 4 848 | 2 088 | 45 033 | 53 025 |
| paste 1 MiB | peak RSS (MB) | 3.5 | 3.7 | 185 | 215 |
| startup | launch to first paint (s) | 0.121 | 0.129 | 0.269 | 0.274 |
| startup | CPU total (s) | 0.002 | 0.002 | 0.004 | 0.004 |

`strace -f -c -p <pid> -e trace=mmap,munmap,brk,mprotect` over 100 typed keys at 10/s (one session per
binary, same server):

| client | mmap | munmap | brk | mprotect | per key |
|---|---:|---:|---:|---:|---|
| moche baseline | 1 412 | 1 412 | 0 | 0 | 14.1 mmap + 14.1 munmap |
| moche new | **1** | 0 | 0 | 0 | 0.01 mmap |
| stock | 0 | 0 | 477 | 0 | 4.8 brk (heap grow/shrink, no mmap) |

A second short `strace -f -tt -e trace=mmap,munmap,brk` around 5 keys at 0.5 s spacing shows 2 lines for
the new build in total (one `munmap` and one `mmap`, both 98 304 B, 0.6 s apart: a single 96 KiB
buffer freed and re-created, not per key; I did not identify it and it is not worth chasing), against the
stock client's `brk` 4-step grow/shrink on every key.

#### Interpretation

- **The allocator churn is gone and the CPU followed.** Typing CPU fell from 0.44 s to 0.155 s (-65 %),
  sys time from 0.27 s to 0.07 s, minor faults from 58 324 to 32 (the whole 30 s window), mmap/munmap
  from 14 pairs per key to none. Idle CPU fell from 23 ms to 7 ms in 60 s and idle faults from 2 840 to
  0. That is the musl mmap/munmap + page-fault cost the earlier section pinned on whole-screen-sized
  buffers, removed by reusing the zlib state and the recycled grids.
- **moche is now at or below stock on every CPU line on Linux.** Typing 0.155 s against 0.173 s and
  0.219 s (ratio 0.7-0.9, was 2.7), idle 7 ms against 9-10 ms, paste 0.035 s against 0.20-0.24 s (the
  0.15x of before is unchanged), startup 2 ms against 4 ms. It also makes 11 % fewer wakeups than
  stock in typing (38.6 against 43.5 per second) and 45x fewer minor faults (32 against 1 425).
  Datagram counts are identical to the baseline (306 out, 604 in), so the change is wire-neutral here
  too.
- **Paste** is unchanged in CPU and wall time (it is bound by VT scrolling and zlib, not by the
  allocator); its page faults halved (4 848 to 2 088, peak RSS 3.5 to 3.7 MB).
- **Startup** first paint 0.129 s against 0.121 s is inside the 18 ms spread seen in the 20-run
  distribution of the earlier section (0.119-0.137 s); not a regression.
- **PSS went up while RSS stayed flat** (typing 1.1 to 1.3 MB, idle 0.9 to 1.2 MB): the baseline
  returned its big blocks to the kernel after each event, the new build keeps the recycled grids and the
  compressor state resident. It is 0.2-0.3 MB, against 9.8 MB for stock.
- **Remaining gap: none worth a project.** What is left per key is about 0.5 ms of CPU for ~39 wakeups
  per second (poll, recv, send, tty write) set by the protocol timing, not by allocation; sys time is
  now the same as stock's. Further CPU work on Linux would mean row-indirection scrolling for bulk output
  (paste is already 0.15-0.17x stock) or a smaller zlib reset cost, with gains of a few ms per session.
  I would stop here and spend effort elsewhere. The musl-allocator proposal (3 above) is moot.
- Setup note: `measure.sh --server-ssh` from the client VM needs a key the server VM trusts; the lab
  had none (the earlier run's key had been removed), so a throwaway ed25519 key was generated in
  `moche-b` and authorised in `moche-a`, passed with `--ssh-opt -i --ssh-opt KEY --ssh-opt -o --ssh-opt IdentitiesOnly=yes
  --ssh-opt -o --ssh-opt StrictHostKeyChecking=no`, and removed at the end. No change to `tools/` was needed.
