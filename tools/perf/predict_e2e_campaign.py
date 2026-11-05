#!/usr/bin/env python3
"""Runs tests/predict_e2e.py many times, sequentially, with progress logging.

usage: predict_e2e_campaign.py OUTDIR MOCHE_BIN COUNT LABEL [WHICH [PW]]
  WHICH full|password|timing (default full), PW marker|early|both (default marker).
Writes OUTDIR/<LABEL>.log (one line per run: run, exit code, seconds, wall start/end, first
output lines) and keeps the artefacts of failing runs in OUTDIR/<LABEL>/run-N-*/.
Several PW variants can be mixed with `marker:200,early:100` (counts per variant, WHICH password).
"""
import os, subprocess, sys, time

here = os.path.dirname(os.path.abspath(__file__))
e2e = os.path.join(here, "..", "..", "crates", "moche-cli", "tests", "predict_e2e.py")
relay = os.path.join(here, "delay_relay.py")
out, binary, label = sys.argv[1], sys.argv[2], sys.argv[4]
count = sys.argv[3]
which = sys.argv[5] if len(sys.argv) > 5 else "full"
pw = sys.argv[6] if len(sys.argv) > 6 else "marker"
plan = []
if ":" in pw:
    for part in pw.split(","):
        v, n = part.split(":")
        plan += [v] * int(n)
else:
    plan = [pw] * int(count)
art = os.path.join(out, label)
os.makedirs(art, exist_ok=True)
log = open(os.path.join(out, label + ".log"), "a", buffering=1)
fails = 0
for n, v in enumerate(plan, 1):
    env = dict(os.environ, MOCHE_E2E_ART=art, MOCHE_E2E_RUN=str(n), MOCHE_E2E_WHICH=which,
               MOCHE_E2E_PW=v, MOCHE_E2E_RETRY="0")
    t0 = time.time()
    p = subprocess.run([sys.executable, "-I", e2e, binary, relay], env=env, capture_output=True,
                       text=True, timeout=600)
    t1 = time.time()
    fails += p.returncode != 0
    first = " | ".join((p.stdout + p.stderr).strip().split("\n")[-4:])[:600]
    log.write("run=%d pw=%s rc=%d secs=%.1f t0=%.3f t1=%.3f %s\n" % (n, v, p.returncode, t1 - t0, t0, t1, first))
log.write("DONE runs=%d fails=%d\n" % (len(plan), fails))
