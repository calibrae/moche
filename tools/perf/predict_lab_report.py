#!/usr/bin/env python3
"""Markdown tables from the JSON files written by predict_lab.py / predict_lab_run.sh.
usage: predict_lab_report.py RESULTS_DIR    (the directory holding latency_*.json, scenario_*.json, ...)
"""
import glob, json, os, re, statistics, sys

d = sys.argv[1]
R = [json.load(open(f)) for f in sorted(glob.glob(os.path.join(d, "*.json")))]
ORDER = ["moche-adaptive", "moche-never", "moche-always", "stock"]
COND_ORDER = ["rtt0", "rtt30", "rtt40", "rtt50", "rtt60", "rtt80", "rtt90", "rtt100", "rtt120", "rtt200", "rtt400",
              "j20_60", "j20_100", "j20_200", "j20_400", "l10_100", "l10_200", "jl_200",
              "rtt200_s8", "rtt200_s9", "j20_400_s8", "j20_400_s9", "j20_400_s10"]


WORDS = ("amber basalt cobalt dune ember fjord garnet harbor indigo jasper kelp lagoon marble nectar "
         "onyx pebble quartz ridge saffron tundra umber velvet willow xenon yarrow zephyr").split()


def old_texts(seed=7, lines=6):
    """Typed text of a latency run made before per_char carried the char (same rng sequence as predict_lab.py)."""
    import random
    rng = random.Random(seed)
    for i in range(3):
        for _ in "# warm %d %s" % (i, rng.choice(WORDS)):
            rng.uniform(0.08, 0.12)
    out = []
    for i in range(lines):
        t = "# L%d %s" % (i, " ".join(rng.choice(WORDS) for _ in range(5)))
        for _ in t:
            rng.uniform(0.08, 0.12)
        rng.uniform(0.6, 1.0)
        out.append(t)
    return "".join(out)


def st(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return {"n": 0}
    return {"n": len(xs), "med": round(statistics.median(xs)), "p90": round(xs[min(len(xs) - 1, int(round(0.9 * (len(xs) - 1))))]),
            "max": round(xs[-1])}


def fix_latency(r):
    """Typed spaces are invisible in capture-pane (trailing blanks trimmed): drop them from the statistics."""
    pc = r.get("per_char")
    if not pc:
        return
    if "ch" not in pc[0]:
        txt = old_texts()
        assert len(txt) == len(pc), (len(txt), len(pc))
        for c, ch in zip(pc, txt):
            c["ch"] = ch
    pc = [c for c in pc if c["ch"] != " "]
    allv = [c["lat_ms"] for c in pc]
    seen = [x for x in allv if x is not None]
    r.update(chars=len(pc), never_visible=len(allv) - len(seen),
             lat_all=st(allv), lat_first_of_line=st([c["lat_ms"] for c in pc if c["k"] == 0]),
             lat_chars_2_4=st([c["lat_ms"] for c in pc if 1 <= c["k"] <= 3]),
             lat_chars_5plus=st([c["lat_ms"] for c in pc if c["k"] >= 4]),
             lat_first_of_word=st([c["lat_ms"] for c in pc if c["after_space"]]),
             within30_per100=round(100.0 * sum(1 for x in seen if x <= 30) / max(1, len(allv)), 1),
             within60_per100=round(100.0 * sum(1 for x in seen if x <= 60) / max(1, len(allv)), 1),
             underlined_share=round(sum(1 for c in pc if c["ul"]) / max(1, len(pc)), 3),
             ul_gone=st([c["ul_gone_ms"] for c in pc]))


for _r in R:
    if _r["kind"] == "latency" and "error" not in _r:
        fix_latency(_r)


def f(x, k="med"):
    return "-" if not x or x.get("n", 0) == 0 else str(x[k])


print("## latency (ms from key to visible; med/p90; n chars)\n")
lat = [r for r in R if r["kind"] == "latency"]
print("| cond | client | chars | first of line med/p90 | chars 2-4 med/p90 | chars 5+ med/p90 | all med/p90 | within 30 ms /100 | underlined | ul gone med/p90 | never seen | retract | residue |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
for c in COND_ORDER:
    for cl in ORDER:
        for r in lat:
            if r["tag"] == c and r["client"] == cl:
                if "error" in r:
                    print("| %s | %s | ERROR %s |" % (c, cl, r["error"][:80]))
                    continue
                print("| %s | %s | %d | %s/%s | %s/%s | %s/%s | %s/%s | %s | %d%% | %s/%s | %d | %d | %s |" % (
                    c, cl, r["chars"], f(r["lat_first_of_line"]), f(r["lat_first_of_line"], "p90"),
                    f(r["lat_chars_2_4"]), f(r["lat_chars_2_4"], "p90"), f(r["lat_chars_5plus"]), f(r["lat_chars_5plus"], "p90"),
                    f(r["lat_all"]), f(r["lat_all"], "p90"), r["within30_per100"], round(100 * r["underlined_share"]),
                    f(r["ul_gone"]), f(r["ul_gone"], "p90"), r["never_visible"], r["retractions"],
                    "YES" if r["final_underline_residue"] else "no"))

print("\n## scenarios (correctness at quiescence)\n")
sc = [r for r in R if r["kind"] == "scenario"]
print("| scenario | cond | client | equal to server truth | residue | worst converge ms | unsettled | extra |")
print("|---|---|---|---|---|---|---|---|")
for r in sorted(sc, key=lambda r: (r["name"], r["tag"], r["client"])):
    cond = re.sub(r"^%s_" % r["name"], "", r["tag"])
    if "error" in r:
        print("| %s | %s | %s | ERROR %s |" % (r["name"], cond, r["client"], r["error"][:100]))
        continue
    ex = {k: v for k, v in r.items() if k in ("secret_max_chars_visible", "secret_visible_seconds", "lowercase_visible_seconds",
          "typed_visible_seconds", "retractions", "retractions_total", "insert_lat_ms", "insert_underlined_share",
          "paste_first_visible_ms", "paste_max_underlined_cells", "chars_ever_lowercase_visible", "chars_ever_visible")}
    bad = [c["name"] for c in r["checks"] if not (c["text_equal"] and c["cursor_equal"])]
    print("| %s | %s | %s | %s | %s | %s | %s | %s |" % (r["name"], cond, r["client"], "YES" if r["all_equal"] else "NO " + ",".join(bad),
          "YES" if r["residue"] else "no", r["worst_converge_ms"], ",".join(r["unsettled"]) or "-", json.dumps(ex)))

print("\n## cost\n")
print("| cond | client | keys | secs | cpu ms (user+sys) | cpu ms/key | wakeups/s | RSS kB | HWM kB | mmap-family syscalls/key |")
print("|---|---|---|---|---|---|---|---|---|---|")
for r in sorted([r for r in R if r["kind"] == "cost"], key=lambda r: (r["tag"], ORDER.index(r["client"]))):
    if "error" in r:
        print("| %s | %s | ERROR %s |" % (r["tag"], r["client"], r["error"][:100]))
        continue
    print("| %s | %s | %d | %s | %d | %s | %s | %s | %s | %s |" % (r["tag"], r["client"], r["keys"], r["secs"], r["cpu_ms_total"],
          r["cpu_ms_per_key"], r["wakeups_per_s"], r["rss_kb"].get("VmRSS"), r["rss_kb"].get("VmHWM"), r["strace_mmap_family_per_key"]))

print("\n## wire (client -> server datagrams, same scripted typing)\n")
print("| cond | client | datagrams | bytes | size histogram |")
print("|---|---|---|---|---|")
for r in sorted([r for r in R if r["kind"] == "wire"], key=lambda r: (r["client"], r["tag"])):
    if "error" in r:
        print("| %s | %s | ERROR %s |" % (r["tag"], r["client"], r["error"][:100]))
        continue
    h = {}
    for s in r["sizes_sorted"]:
        h[s // 16 * 16] = h.get(s // 16 * 16, 0) + 1
    print("| %s | %s | %d | %d | %s |" % (r["tag"], r["client"], r["datagrams"], r["bytes"],
          " ".join("%d-%d:%d" % (k, k + 15, v) for k, v in sorted(h.items()))))
