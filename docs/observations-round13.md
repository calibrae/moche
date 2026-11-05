# Observations, round 13: pipelined pastes (simulation only, lab run NOT done)

Date: 2026-11-05. Change: `ssp.rs` pipelines big states (up to 4 in flight), see `protocol.md` section 4.

## Status: lab measurements were not made

The x86 lab host (the x86 lab host) was unreachable for this work: the first an internal tool call (`host_status the x86 lab host`) was
refused with "this project has no an internal tool agent yet: approve its registration" and was not approved within
the 3-minute wait, twice. Not worked around. So NOT done: the 4 MiB paste at RTT 2/50/200 ms, lossless and
2 % / 10 % loss, against the pre-change binary and stock mosh-client; the B2 loop (>= 6 sessions, server abort
check); the B1 loop; and every question marked UNVERIFIED in `protocol.md` (does stock mosh-server 1.4.0 accept a
diff based on a received-but-unacked state, and tolerate out-of-order arrival).

## Simulation (virtual link, server model panics on B2 violations; not a substitute)

4 MiB lossless, client ack hold 100 ms, time to full delivery:

| one-way delay | before (1 state in flight) | after (<= 4) |
|---|---|---|
| 1 ms | 6834 ms | 1896 ms |
| 50 ms | 13400 ms | 4152 ms |
| 100 ms | 20100 ms | 7546 ms (max 3 in flight) |
| 0 ms, no hold | 1320 ms | 1320 ms |

Same bytes (1.03x). 1 MiB at 10 % loss, one-way 1/50/100 ms: cost 5.3-7.1x payload after against 5.7-7.9x
before (noisy: 1 ms seed 2 went from 5.65x to 7.05x, seed 1 6.08x to 6.11x); times within noise. Typing wire
schedule unchanged to the millisecond (golden fingerprint of 16 runs recorded on `a9b71c1`).

Regression risks to check on the lab: a lost state wastes the pipelined states behind it (mitigated: pipelining
stops after the first re-send and after a cap shrink); a key typed during a paste now goes out as a small state
in the pipeline instead of waiting for the ack.
