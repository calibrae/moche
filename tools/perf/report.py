"""Markdown report from one or more tools/perf run directories.

usage: python3 -I report.py RUNDIR [RUNDIR...] [-o report.md]
Each RUNDIR holds env.json, static.json and runs/*.json written by measure.sh.
Several directories (one per host) give one section per host label, same layout, so the
numbers can be compared side by side.
"""
import argparse, glob, json, os, statistics, sys

SCEN = [("startup", "Startup (launch to first paint, +1 s settle)"),
        ("idle", "Idle (heartbeats only)"),
        ("typing", "Typing (~10 chars/s)"),
        ("paste", "Paste (1 MiB base64 into cat > file, sha256 checked)"),
        ("flood", "Flood (yes | head -c 5000000)"),
        ("colors", "256-colour dump (200x60 pane, 5 passes)"),
        ("steady", "Steady output (seq 1 200000)")]

# (key, label, scale, format)
ROWS = [("wall_s", "wall time (s)", 1, "%.2f"),
        ("cpu_s", "CPU time user+sys (s)", 1, "%.3f"),
        ("cpu_user_s", "  user (s)", 1, "%.3f"),
        ("cpu_sys_s", "  sys (s)", 1, "%.3f"),
        ("cpu_pct_of_core", "CPU, % of one core", 1, "%.2f"),
        ("peak_rss_kb", "peak RSS (MB)", 1 / 1024, "%.1f"),
        ("final_rss_kb", "final RSS (MB)", 1 / 1024, "%.1f"),
        ("pss_kb", "PSS (MB, Linux)", 1 / 1024, "%.1f"),
        ("footprint_kb", "physical footprint (MB, macOS)", 1 / 1024, "%.1f"),
        ("peak_footprint_kb", "peak footprint (MB, macOS)", 1 / 1024, "%.1f"),
        ("threads", "threads (final)", 1, "%.0f"),
        ("vol_cs_per_s", "voluntary ctx switches /s", 1, "%.1f"),
        ("invol_cs_per_s", "involuntary ctx switches /s", 1, "%.1f"),
        ("cs_per_s", "ctx switches /s (macOS, total)", 1, "%.1f"),
        ("wakeups_per_s", "wakeups /s (macOS, idle+interrupt)", 1, "%.1f"),
        ("faults_minor_total", "minor page faults (window)", 1, "%.0f"),
        ("faults_major_total", "major page faults (window)", 1, "%.0f"),
        ("fds", "open fds (final)", 1, "%.0f"),
        ("instructions", "instructions retired (macOS, window)", 1e-6, "%.1f M"),
        ("cycles", "cycles (macOS, window)", 1e-6, "%.1f M")]
EXTRA = [("first_paint_s", "launch to first paint (s)", "%.3f"),
         ("paste_wall_s", "paste to sha-verified (s)", "%.2f")]
STATS = [("bytes_sent", "bytes sent (moche --stats)"), ("bytes_received", "bytes received (moche --stats)"),
         ("datagrams_sent", "datagrams sent"), ("datagrams_received", "datagrams received")]


def med_rng(vals, fmt):
    vals = [v for v in vals if v is not None]
    if not vals:
        return None
    m = statistics.median(vals)
    return fmt % m, "%s-%s" % (fmt % min(vals), fmt % max(vals)), m


def load(d):
    with open(os.path.join(d, "env.json")) as f:
        env = json.load(f)
    st = {}
    p = os.path.join(d, "static.json")
    if os.path.exists(p):
        with open(p) as f:
            st = json.load(f)
    runs = []
    for p in sorted(glob.glob(os.path.join(d, "runs", "*.json"))):
        with open(p) as f:
            runs.append(json.load(f))
    return env, st, runs


def table(lines, header, rows):
    lines.append("| " + " | ".join(header) + " |")
    lines.append("|" + "|".join(["---"] + ["---:"] * (len(header) - 1)) + "|")
    for r in rows:
        lines.append("| " + " | ".join(r) + " |")
    lines.append("")


def host_section(d, level):
    env, st, runs = load(d)
    h = "#" * level
    L = ["%s Host `%s`" % (h, env["label"]), ""]
    gb = env.get("ram_bytes", 0) / 2 ** 30
    L += ["| | |", "|---|---|",
          "| label | %s |" % env["label"],
          "| OS | %s (%s %s) |" % (env.get("os_version"), env.get("os"), env.get("kernel")),
          "| CPU | %s, %s cores |" % (env.get("cpu_model"), env.get("cores")),
          "| RAM | %.1f GiB |" % gb,
          "| moche | %s |" % env.get("moche_version"),
          "| mosh-client (baseline) | %s |" % (env.get("mosh_client_version") or "absent, baseline skipped"),
          "| mosh-server | %s (%s) |" % (env.get("mosh_server_version"), env.get("server_mode")),
          "| tmux / python | %s / %s |" % (env.get("tmux"), env.get("python")),
          "| locale | %s |" % env.get("lang"),
          "| date (UTC) | %s |" % env.get("date_utc"), ""]

    if st:
        L += ["%s# Static facts" % h, ""]
        rows = []
        for name, s in st.items():
            rows.append([name, "%.2f" % (s["size_bytes"] / 1e6), "%.2f" % (s.get("stripped_size_bytes", 0) / 1e6)
                         if s.get("stripped_size_bytes") else "n/a",
                         str(len([x for x in s.get("deps", []) if x])), s.get("file", "")])
        table(L, ["binary", "size MB (as built)", "stripped MB", "dynamic deps", "file"], rows)
        for name, s in st.items():
            L += ["`%s` dynamic dependencies:" % name, "", "```"] + s.get("deps", []) + ["```", ""]
            if s.get("segments"):
                L += ["`%s` segment sizes:" % name, "", "```", s["segments"].strip(), "```", ""]

    by = {}
    for r in runs:
        by.setdefault((r["scenario"], r["client"]), []).append(r)
    clients = [c for c in ("moche", "stock") if any(k[1] == c for k in by)]
    for sc, title in SCEN:
        if not any(k[0] == sc for k in by):
            continue
        L += ["%s# %s" % (h, title), ""]
        fails = []
        for c in clients:
            for r in by.get((sc, c), []):
                if not r["ok"]:
                    fails.append("%s rep %s: %s" % (c, r["rep"], r.get("error") or "check failed"))
        header = ["metric"]
        for c in clients:
            n = len(by.get((sc, c), []))
            header += ["%s median (n=%d)" % (c, n), "%s min-max" % c]
        if len(clients) == 2:
            header.append("moche / stock")
        rows = []
        specs = [(k, lab, 1, f, "extra") for k, lab, f in EXTRA] + [
            (k, lab, s, f, "m") for k, lab, s, f in ROWS] + [(k, lab, 1, "%.0f", "stat") for k, lab in STATS]
        for key, lab, scale, fmt, kind in specs:
            cells, meds = [], {}
            for c in clients:
                rs = [r for r in by.get((sc, c), []) if r["ok"] or "metrics" in r]
                if kind == "m":
                    vals = [r["metrics"].get(key) for r in rs if "metrics" in r]
                elif kind == "extra":
                    vals = [r.get(key) for r in rs]
                else:
                    vals = [r.get("moche_stats", {}).get(key) for r in rs]
                vals = [v * scale for v in vals if v is not None]
                f = fmt.replace(" M", "")
                sfx = " M" if fmt.endswith(" M") else ""
                mr = med_rng(vals, f)
                if mr:
                    cells += [mr[0] + sfx, mr[1] + sfx]
                    meds[c] = mr[2]
                else:
                    cells += ["-", "-"]
            if not meds:
                continue
            if len(clients) == 2:
                a, b = meds.get("moche"), meds.get("stock")
                cells.append("%.2f" % (a / b) if a is not None and b else "-")
            rows.append([lab] + cells)
        table(L, header, rows)
        if fails:
            L += ["Failed or unverified runs (excluded from medians when they produced no metrics):", ""]
            L += ["- " + x for x in fails] + [""]
    return L


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("-o", "--out")
    a = ap.parse_args()
    L = ["# moche resource report", ""]
    for d in a.dirs:
        L += host_section(d, 2)
    txt = "\n".join(L) + "\n"
    if a.out:
        with open(a.out, "w") as f:
            f.write(txt)
    else:
        sys.stdout.write(txt)


if __name__ == "__main__":
    main()
