# Predictive local echo

Code: `crates/moche-core/src/predict.rs` (the state machine), `Transport::echoed_input_bytes`
and `input_bytes_total` (`ssp.rs`), the overlay painting in `crates/moche-cli/src/display.rs`.
Option: `moche --predict never|adaptive|always` (default `adaptive`).

Labels: **SOURCED** = the public mosh paper (Winstein and Balakrishnan, USENIX ATC 2012,
section 3.2, "paper"); **OBSERVED** = black-box runs of a stock `mosh-client` 1.4.0 against a
local stock `mosh-server`, through `tools/perf/delay_relay.py` (a UDP relay adding delay),
read with `tmux capture-pane -p -e`; **OUR CHOICE** = ours. No mosh source was read.

## What it does

When the user types, the typed characters are shown at once, before the server's echo
arrives; then the display is reconciled with the server's real state.

## Hard invariants and how they hold

1. **Display only.** The `Predictor` reads the latest server `Framebuffer` and two byte counts
   and writes only into a *copy* of the framebuffer given to `apply_overlay`. It never
   touches `Screen`'s numbered states or the `Transport`, so the wire is unchanged.
   Checked by: `tools/perf/golden-diff.sh` (IDENTICAL), `tests/predict_wire.rs` (datagrams,
   timing and deadlines identical with an active predictor next to the transport).
2. **Never lies for long.** Every prediction ends by the server's echo (cell and cursor
   match), by an echo ack that covers it without a match (cancelled), by timeout, by
   silence, or by a world change (resize, alternate screen, scroll). At quiescence the
   display equals the server framebuffer exactly (`assert_converged` in
   `tests/predict_sim.rs`, 150 random seeds with loss and reordering).
3. **No ghost secrets.** A prediction is shown only after one of its epoch was confirmed
   by the server's real echo (SOURCED, paper: "an epoch begins tentatively, making
   predictions only in the background. If any prediction from a certain epoch is
   confirmed by the server, the rest ... are immediately displayed"). A password prompt
   never confirms, so nothing is shown (Adaptive). Nothing means nothing: no character, no
   blank cell and no cursor move either (a predicted backspace or arrow is held back like a
   character). Checked by `property_no_ghost_when_echo_is_off_before_typing` (400 seeds, loss,
   jitter, srtt spikes): (i) no character the server never had is displayed, (ii) no overlay of
   any kind while a key typed after the prompt is pending. The one design exception is a prompt
   that stops echoing while the secret is already being typed (below).
4. **Cheap.** `Never` and an inactive Adaptive cost a few ns per call and no allocation
   (`predict_bench`, `docs/PERF.md`). Active: one grid copy per paint (the recycled
   `spare` buffer in `ScreenDisplay`).

## Modes

| mode | behaviour |
|---|---|
| `never` | nothing is predicted. |
| `adaptive` (default) | predicts while the smoothed RTT is high enough to notice (hysteresis, below), with epoch gating. |
| `always` | predicts at any RTT and shows the first prediction of an epoch **before** any confirmation. Risk: a secret typed where nothing echoes is visible until the echo ack names it (one RTT plus 50 ms), then hidden and the connection is tentative (measured in simulation: 249 ms visible of 600 ms of typing at 200 ms RTT). Documented, opt-in. |

## Constants

| constant | value | why |
|---|---|---|
| `ADAPTIVE_ON_MS` / `ADAPTIVE_OFF_MS` | 30 / 20 ms srtt | OBSERVED: the stock client showed predictions at a path RTT of 30 ms and above and not at 20 ms (typing latency 5-7 ms against 45 ms). The hysteresis band is OUR CHOICE. |
| `FLAG_ON_MS` / `FLAG_OFF_MS` | 100 / 80 ms (was 85 / 65 before the lab validation, see "Lab validation") | OBSERVED: stock underlines unconfirmed predictions (SGR 4) from a path RTT of 90 ms, not at 80 ms. SOURCED: "on high-delay connections, we underline unconfirmed predictions". Band is OUR CHOICE. |
| `CONFIRM_STREAK` | 2 | OUR CHOICE: confirmations needed to trust again after a contradiction. |
| `MAX_PENDING` | 64 | OUR CHOICE: bounds work per server state; the next key ends the epoch. |
| `PASTE_BYTES` | 1024 | OUR CHOICE (task): a single input call longer than this is a paste. |
| timeout | `max(3000 ms, 4 * srtt)` | OUR CHOICE. The paper rejects client-side timeouts as the *correctness* signal (jitter makes false negatives) and uses the server's echo ack instead; moche uses the echo ack for that and the timeout only as a bound when no ack comes. |
| `SILENT_MS` | 3000 | OUR CHOICE: no server datagram for 3 s drops everything (the status line appears at 5 s). |

## What is predicted

Typed at the cursor, and only when every cell from the cursor to the end of the row is
blank (so nothing shifts): printable ASCII and single-width, non-combining characters
(UTF-8, also split across reads); backspace/DEL (cursor left, cell blanked, like `\b \b`);
left and right arrows (`CSI D/C`, `SS3 D/C`; right only over a non-blank cell; left only
inside the row).

## What is not predicted (each ends the epoch)

Enter, Tab, control keys, Esc and every other escape sequence (including arrows with
modifiers, up/down), bracketed paste and its content, any input call over 1 KiB, wide and
combining characters (width is not 1), a character that would reach the last column
(pending wrap), insertion or deletion with text to the right of the cursor, input while
the server is in the alternate screen, has mouse reporting on, or hides the cursor (a TUI),
a resize in flight. Why: each of these either changes the echo mode or moves the cursor
unpredictably (paper: "control characters ... cause Mosh to lose confidence and increment
the epoch"; arrow keys up/down likewise).

After such a key, **no prediction is made until the echo ack covers it** (`barrier`), so a
chain is never started from a cursor the server has not reached.
The characters displayed when the key was typed stay on screen (`frozen`) until the ack
covers the key or the server cursor leaves their row, so a typed line does not vanish at
Enter and come back one RTT later.

## State machine

State: `pending` chain (one epoch), `gate_open`, `tentative`, `streak`, `barrier`, `echoed`,
`frozen`.

| event | effect |
|---|---|
| predictable key | push onto `pending` (position chained from the previous prediction); shown iff `show()` |
| unpredictable key | `frozen` = shown predictions; clear `pending`; `gate_open = false` (new epoch); `barrier = end of key` |
| server state, a pending prefix matches (cursor == predicted cursor, touched cells equal, state differs from the baseline) | confirm that prefix; `gate_open = true`; `streak += n`; `tentative` ends at `streak >= 2`; rebase the rest; if the rest is already covered by the echo ack, cancel it with penalty |
| server state, no match, echo ack covers a pending key | cancel; penalty (`tentative = true`, `streak = 0`) unless the chain was typed-then-erased (no penalty) |
| server state, no match, not covered, not the baseline either | cancel with penalty (something else happened: scroll, redraw) |
| server state, baseline, not covered | wait |
| resize, alt screen on, size change | drop everything, no penalty |
| timeout / silence | drop `pending` (penalty if timeout, none if silence) and `frozen` |
| srtt below `ADAPTIVE_OFF_MS` | drop everything, stop predicting |

`show() = !tentative && (gate_open || mode == always)`. Confirmation compares characters
only, not attributes, so colour echo and syntax highlighting confirm. The check uses
the cursor and the touched cells, so a coincidental equal cell with an unmoved cursor
(a space over a blank at a non-echo prompt) never confirms.

## Rendering

`display = server latest + overlay`. The CLI keeps two notions: `screen` (server states)
and `painted` (server state, or an owned overlay copy). A paint is `repaint(display,
painted)`, so only changed rows are written when a prediction appears, changes or
disappears. Predicted cells keep the attributes of the cell they cover, plus underline
above the flag threshold (an erased cell is an underlined blank, as OBSERVED in stock).

## Observed in the stock client (1.4.0, loopback, relay delay)

- 200 ms RTT: the first character of an epoch appears with the real echo (about 245 ms),
  the next ones 5-10 ms after the keystroke, underlined, the underline disappearing as the
  echoes arrive. After Enter the first character is again shown only with the real echo,
  the following ones about 100 ms later (after the echo ack), then at once.
- Password prompt (`stty -echo`, 200 ms RTT): no typed character shown at all.
- Backspace: predicted, the erased cell shown as an underlined blank.
- Typing inside a line: stock predicts it and shifts the tail right (underlined). moche
  does not (see limits).
- Stock confirms from the echo ack (about 100 ms after the first real echo); moche opens
  the epoch as soon as the cursor and cell match the prediction, which is earlier by up
  to that amount. Same-order behaviour; ours is more eager only after a real echo was seen.

## Results

- Simulation (`tests/predict_sim.rs`, 100 ms one way, keys every 70 ms): displayed latency
  per character `[200, 130, 60, 0, 0, ...]` ms, mean 14 ms against about 200 without; every
  character after the first confirmed one is shown with 0 ms.
- Real stock server through the relay (`tests/predict_e2e.py`, 100 ms one way): first
  character 234 ms, the next nine 5-8 ms; `--predict=never` 250-545 ms; the final screens
  are equal; a password prompt showed 0 changed samples of 295.
- Property run: 150 seeds with loss, reordering and program switches converge with no
  residue; the overlay never spans more than one row.

## Known limits and risks

- Insertion in the middle of a line is not predicted (stock does). zsh autosuggestions put
  text to the right of the cursor, so those lines are not predicted either.
- A server that echoes later than the 50 ms echo-ack window gives a false contradiction:
  the display is correct, only the connection goes tentative for two confirmations.
- `always` leaks for up to one echo window (see Modes).
- Adaptive, echo switched off in the MIDDLE of a secret (a host stall delayed `stty -echo` after
  the first keys were really echoed): the real echo of the first keys opened the epoch, so the
  next key is shown until the echo ack that covers it comes back, then hidden, and the connection
  is tentative (OUR CHOICE, the paper's design). Bound: one-way up + echo window (50 ms) +
  one-way down, i.e. 250 ms at 100 ms one way, and the keys typed inside that window (2 at one
  key per 150 ms). Measured in `echo_off_mid_secret_leaks_one_window_at_most`: 249 ms, 2 chars.
  The rest of the overlay in that test (500 ms in total) is the display leading the REAL echo of
  keys that did reach the server: legitimate, not a leak. Not observed in black-box runs of
  stock (not tried: it needs a host that stalls between the echo and `stty -echo`).
- The display can differ from the server for at most the timeout (`max(3 s, 4 * srtt)`)
  when no echo ack arrives, and for at most one RTT plus 50 ms otherwise. The host must
  call `tick` at `next_deadline_ms`; the CLI does.
- Thresholds were observed on loopback with a simulated delay, not on a real path with
  jitter.

## Round 11: ghost-secret forensics

`docs/observations-round11.md`: 650 password checks against a stock server, 0 ghost secrets;
the two new simulation tests that first failed were test-model errors (the old `visible_steps`
assertion counted any overlay step, including the display leading a real echo and the frozen
line of the Enter); `predict.rs` is unchanged. Debug trace: `MOCHE_PREDICT_TRACE=<file>` on
`moche` (typed keys are written in hex: debugging only), harness in `tests/predict_e2e.py` and
`tools/perf/predict_e2e_campaign.py`.

## API for hosts (and the planned FFI)

```
Predictor::new(PredictMode)            set_mode, mode
on_input(now_ms, bytes, &Framebuffer latest, srtt_ms, Transport::input_bytes_total()) -> bool changed
on_server_state(&Framebuffer latest, Transport::echoed_input_bytes(), now_ms) -> bool changed
on_resize()                            tick(now_ms, srtt_ms, silent_ms) -> bool changed
next_deadline_ms() -> Option<u64>      is_displaying()
apply_overlay(&mut Framebuffer)        // on a copy of the latest state
stats()
```

Host loop: after `send_keys` call `on_input`; after each datagram's events call
`on_server_state`; call `tick` on wake-ups; if anything changed or `is_displaying()`, copy
the latest state, `apply_overlay`, and paint it against what the terminal shows.

## Lab validation (phase 2): real network, 2026-11-04

**Setup.** Build `0cc7b75`, `aarch64-unknown-linux-musl` release from `tools/release/build.sh`, run on the client VM of the VM lab
(`tools/vm-lab/README.md`: Ubuntu 24.04 client, Debian 13 server, stock `mosh-server` and `mosh-client` 1.4.0, real UDP between the
two VMs). Netem sits on `eth0` of **both** VMs, so a per-side delay D gives RTT = 2 D (plus the lab's own base of 1 to 2 ms); every
condition below is named after the **netem RTT** in ms. `j20_N` = RTT N with jitter 20 % of D on each side (`delay D J`, packets
can reorder), `l10_N` = 10 % loss in each direction, `jl_200` = RTT 200 + 20 % jitter + 10 % loss. The VMs were driven over their
vzNAT control addresses, never over the shaped path. Stock was observed as a black box (screen attributes and timing only).
Raw results: one JSON per run (164 files) and `report.md` in `~/moche-pred/out/` on the lab host.

**Tools** (this repo): `tools/perf/predict_lab.py` (runs in the client VM), `predict_lab_run.sh` (netem and loops, on the lab
host), `predict_lab_report.py` (tables). The client runs in a dedicated tmux; a tmux control-mode pipe sends keys and takes
`capture-pane -p -e` snapshots every 10 ms (0.1 ms each), so a character is timed from the key to the first snapshot where it is on
screen, with its SGR attributes (underline). Typing: random 80-120 ms gaps (about 10 characters/s), six lines `# L<n> <5 words>`
plus Enter (186 non-space characters per run; same text and gaps for every client), after a three-line warm-up so the RTT estimate
has settled. **A typed space cannot be timed** (capture-pane trims trailing blanks), so spaces are left out of every statistic.
The first character of a line (right after Enter) is reported on its own, as the paper's epoch start. Server truth for the
correctness runs: the server side runs inside a tmux whose pane is captured over ssh and compared with the client's pane (text and
cursor) once the display has been still for 2 s. Alternate-screen programs run without that wrapper (a tmux in between would hide
the alternate screen): vim is checked by comparing the display before and after a Ctrl-L redraw, the nested tmux against its own pane.

### 1. Latency

Milliseconds from key to visible. "chars 2-4" = the 2nd to 4th character of a line. "within 30" = of 100 typed characters. One run
(186 characters) per cell unless stated; the cap on "within 30" is 96.8 (the first character of each of the six lines always waits
for the server). `m-X` = `moche --predict=X`; `stock` = `mosh-client`, default (adaptive).

| cond | client | first of line | chars 2-4 | chars 5+ med/p90 | within 30 | underlined | underline lasts (med) | never shown |
|---|---|---|---|---|---|---|---|---|
| rtt0 | m-adaptive | 25 | 24 | 24/36 | 84.9 | 0% | - | 0 |
| rtt0 | m-never | 24 | 25 | 24/38 | 80.1 | 0% | - | 0 |
| rtt0 | m-always | 7 | 1 | 1/12 | 100.0 | 0% | - | 0 |
| rtt0 | stock | 25 | 26 | 26/36 | 84.4 | 0% | - | 0 |
| rtt30 | m-adaptive | 64 | 1 | 1/6 | 96.8 | 0% | - | 0 |
| rtt30 | m-never | 65 | 63 | 63/67 | 0.0 | 0% | - | 0 |
| rtt30 | m-always | 1 | 1 | 1/12 | 100.0 | 0% | - | 0 |
| rtt30 | stock | 49 | 50 | 52/58 | 0.0 | 0% | - | 0 |
| rtt40 | m-adaptive | 69 | 1 | 1/3 | 96.8 | 0% | - | 0 |
| rtt40 | stock | 64 | 1 | 1/12 | 96.8 | 0% | - | 0 |
| rtt50 | m-adaptive | 81 | 1 | 1/2 | 96.8 | 0% | - | 0 |
| rtt50 | stock | 73 | 11 | 1/12 | 96.8 | 0% | - | 0 |
| rtt60 | m-adaptive | 91 | 1 | 1/2 | 96.8 | 0% | - | 0 |
| rtt60 | m-never | 85 | 95 | 99/112 | 0.0 | 0% | - | 2 |
| rtt60 | m-always | 2 | 1 | 1/5 | 100.0 | 0% | - | 0 |
| rtt60 | stock | 86 | 1 | 1/12 | 96.8 | 0% | - | 0 |
| rtt80 | m-adaptive | 111 | 1 | 1/2 | 96.8 | 94% | 129 | 0 |
| rtt80 | stock | 105 | 1 | 1/2 | 96.8 | 0% | - | 0 |
| rtt90 | m-adaptive | 121 | 1 | 1/14 | 96.8 | 94% | 138 | 0 |
| rtt90 | stock | 114 | 1 | 1/2 | 96.8 | 0% | - | 0 |
| rtt100 | m-adaptive | 131 | 1 | 1/12 | 96.8 | 94% | 155 | 0 |
| rtt100 | m-never | 130 | 147 | 157/180 | 0.0 | 0% | - | 6 |
| rtt100 | m-always | 1 | 1 | 1/14 | 100.0 | 94% | 146 | 0 |
| rtt100 | stock | 121 | 1 | 1/12 | 96.8 | 94% | 141 | 0 |
| rtt120 | m-adaptive | 150 | 1 | 1/13 | 96.8 | 94% | 174 | 0 |
| rtt120 | stock | 143 | 1 | 1/12 | 96.2 | 94% | 167 | 0 |
| rtt200 | m-adaptive | 226 | 12 | 1/14 | 95.7 | 94% | 269 | 0 |
| rtt200 | m-never | 236 | 232 | 260/312 | 1.1 | 0% | - | 15 |
| rtt200 | m-always | 1 | 1 | 1/5 | 100.0 | 94% | 260 | 0 |
| rtt200 | stock | 225 | 35 | 1/15 | 93.5 | 94% | 253 | 0 |
| rtt400 | m-adaptive | 470 | 234 | 1/16 | 89.2 | 96% | 603 | 0 |
| rtt400 | m-never | 453 | 534 | 622/703 | 0.0 | 0% | - | 34 |
| rtt400 | m-always | 1 | 12 | 1/16 | 100.0 | 94% | 614 | 0 |
| rtt400 | stock | 452 | 357 | 1/16 | 83.3 | 94% | 606 | 0 |
| j20_60 | m-adaptive | 99 | 1 | 1/12 | 96.8 | 0% | - | 0 |
| j20_60 | m-never | 89 | 98 | 97/114 | 0.0 | 0% | - | 2 |
| j20_60 | m-always | 1 | 1 | 1/2 | 100.0 | 0% | - | 0 |
| j20_60 | stock | 85 | 1 | 1/3 | 96.8 | 0% | - | 0 |
| j20_100 | m-adaptive | 138 | 1 | 1/13 | 96.8 | 94% | 171 | 0 |
| j20_100 | m-never | 132 | 153 | 168/196 | 0.0 | 0% | - | 7 |
| j20_100 | m-always | 1 | 2 | 1/2 | 100.0 | 94% | 162 | 0 |
| j20_100 | stock | 126 | 1 | 1/12 | 96.8 | **30%** | 140 | 0 |
| j20_200 | m-adaptive | 230 | 2 | 1/13 | 95.2 | 94% | 298 | 0 |
| j20_200 | m-never | 233 | 250 | 313/358 | 0.5 | 0% | - | 15 |
| j20_200 | m-always | 7 | 2 | 1/13 | 100.0 | 94% | 310 | 0 |
| j20_200 | stock | 234 | 41 | 1/14 | 92.5 | 94% | 290 | 0 |
| j20_400 | m-adaptive | 442 | 193 | 2/559 | 74.2 | 80% | 583 | 4 |
| j20_400 | m-never | 431 | 522 | 601/699 | 0.0 | 0% | - | 33 |
| j20_400 | m-always | 6 | 6 | 1/14 | 100.0 | 94% | 580 | 0 |
| j20_400 | stock | 456 | 358 | 7/16 | 82.8 | 94% | 586 | 0 |
| l10_100 | m-adaptive | 133 | 1 | 1/2 | 96.8 | 94% | 157 | 0 |
| l10_100 | m-never | 157 | 149 | 164/209 | 0.5 | 0% | - | 6 |
| l10_100 | m-always | 1 | 1 | 1/13 | 100.0 | 94% | 151 | 0 |
| l10_100 | stock | 129 | 1 | 1/13 | 96.8 | 94% | 153 | 0 |
| l10_200 | m-adaptive | 241 | 13 | 2/14 | 94.6 | 94% | 297 | 0 |
| l10_200 | m-never | 278 | 282 | 315/420 | 0.0 | 0% | - | 17 |
| l10_200 | m-always | 6 | 14 | 2/15 | 100.0 | 94% | 293 | 0 |
| l10_200 | stock | 226 | 57 | 1/2 | 92.5 | 94% | 275 | 0 |
| jl_200 | m-adaptive | 243 | 14 | 2/15 | 94.6 | 95% | 314 | 0 |
| jl_200 | m-never | 226 | 230 | 324/422 | 1.1 | 0% | - | 16 |
| jl_200 | m-always | 6 | 1 | 1/14 | 100.0 | 94% | 304 | 0 |
| jl_200 | stock | 236 | 102 | 1/13 | 91.4 | 95% | 317 | 0 |

("never shown" = typed characters that never became visible before Enter; only in `never`, where a long backlog can pass Enter, and
once in `j20_400` adaptive, see below. No run ended with an underline left on the screen.)

**Reading the table**

- **Where prediction switches on (netem RTT).** Neither client predicts at 0. At **30** moche already predicts (chars 2+ at 1 ms) and
  stock does **not** (52 ms, same as `never`); at **40** both do. So moche's `ADAPTIVE_ON_MS = 30` fires about 10 ms of RTT earlier
  than stock's trigger. Harmless: nothing shows before the first echo, there is no underline, and the gain at 30 is a real 50 ms
  per character.
- **Where the underline starts.** Moche underlines from netem **80** (94 % of characters, nothing at 60); stock only from **100**
  (nothing at 80 or 90), and with jitter at 100 stock underlines 30 % of the characters while moche underlines 94 %. The stock
  threshold is therefore about 20 ms above `FLAG_ON_MS = 85`. This is the clearest difference between the two.
- **How long the underline stays.** From the first sight of a predicted character to the confirmed redraw: about 1.35 to 1.55 times the RTT
  (155 at RTT 100, 269 at 200, 603 at 400), the same as stock within 15 ms (30 ms with jitter) at every RTT.
- **Chars 2-4.** Moche is faster than stock from RTT 200 up (12 against 35 ms at 200, 234 against 357 at 400; `j20_200` 2 against 41):
  it opens the epoch on the first matching echo, stock waits for the echo ack (documented above). From chars 5 on, both show
  100 % of the characters within about 15 ms at every RTT.
- **First character after Enter** waits for the real echo for every client (no prediction): about RTT + 25 ms. Moche's real echo
  is 5 to 18 ms later than stock's at the same RTT (131 against 121 at 100, 150 against 143 at 120, 470 against 452 at 400); `never`
  shows the same figure, so this is the receive path, not the prediction. Not investigated.
- **`always`** shows the first character at 1 to 7 ms at any RTT (the 2 to 3 chars of a secret it leaks are in section 2).
- **Without prediction** (`never`) a character takes RTT + 25 to 60 ms, a backlog builds from RTT 200 and up to 34 characters
  were still not shown at Enter at 400.
- **Jitter and loss.** Jitter 20 % and loss 10 % change nothing for adaptive up to RTT 200 (94.6 to 96.8 within 30, stock 91.4 to
  96.8). Single episode: `j20_400` seed 7, adaptive, 4 characters never shown and 35 of 198 later characters over 200 ms (a
  stretch where it fell back to the real echo, probably tentative after a reordering-induced contradiction). Three more seeds
  of the same condition: adaptive 90.0 / 89.8 / 88.8 within 30, none slow; stock 81.7 / 83.3 / 84.5 (and 82.8 in the first run). Not
  reproduced, cause not established (the display log is not kept; see "Open points").

**200 ms RTT, 100 typed characters, how many are on screen within 30 ms of the key** (three runs each, 186 characters each; cap 96.8):
`moche adaptive` **95.7 / 95.0 / 95.7** (mean 95.5), `stock` **93.5 / 93.9 / 93.0** (mean 93.5), `moche never` 1, `moche always` 100.
Moche shows about two more characters per hundred than stock, from the faster start of the epoch (chars 2-4).

### 2. Correctness and convergence

71 scenario runs (moche adaptive and stock, each at RTT 100 and at `jl_200`; `always` for the password and vim scenarios), 203
checkpoints. At each checkpoint the display (text and cursor) is compared with the server's own pane once it has been still for
2 s. **Result: every checkpoint equal, no underline left, nothing unsettled, for both clients, at both conditions**, except the 9
`vim_plain` checkpoints of the NORMAL-mode step, which are a test artefact (below). Time from the last key to the last display
change: 1.6 s at most, except the two programs that themselves echo late (`slow_echo` 7.1 s, `slow_echo2` 9.1 s, the program's own
pace); at no checkpoint did the display differ from the server beyond the `max(3 s, 4 x srtt)` bound.

| scenario | what was typed | moche adaptive | stock |
|---|---|---|---|
| `shell_edit` | line, 6 backspaces, 4 left arrows, 2 chars inside the line, 2 right arrows, 2 chars, Ctrl-U, Enter | equal, 2 runs | equal, 2 runs |
| `wrap` | 110 characters across the right margin, Ctrl-U, 90 more, Enter | equal | equal |
| `password` (`stty -echo; read x`) | 9 capital letters | **0 secret characters ever visible**, 2 runs | 0, 2 runs |
| `password`, `always` | same | 2 chars visible for 0.17 s (RTT 100), 3 chars for 0.27 s (`jl_200`): the documented leak | - |
| `vim_plain` (real alternate screen) | `i` + 30 chars + Esc, then `h j k l x d d u 0 x p`, then `i` + text + Esc, `:q!` | equal at every checkpoint; the NORMAL-mode window showed **0 underlined cells** (no ghost characters), for adaptive, always and stock | same |
| `cat_echo` | `cat`, two lines with Enter, Ctrl-D | equal | equal |
| `ctrl_c_enter` | `sleep 30`, 3 chars while it runs, Ctrl-C, command, 3 lines typed with no pause between Enters | equal | equal |
| `nested_plain` (inner tmux, alternate screen) | typing and 3 backspaces in the inner shell, Ctrl-U, command | equal to the inner pane | equal |
| `resize` | line typed while the window goes 80x24, 60x20, 80x24 | equal | equal |
| `paste200` | 200 characters in one write, then the same through bracketed paste | equal; **not predicted** (0 underlined cells; first visible 238 / 329 ms for adaptive at RTT 100 / `jl_200`, stock 279 / 449) | equal |
| `transform2` (echo as typed for 3 chars, then upper-cased) | 20 letters | 2 wrong lower-case characters shown for 0.13 s (RTT 100), 3 for 0.26 s (`jl_200`), then corrected; 1 contradiction | 2 for 0.18 s, 4 for 0.30 s; 1 contradiction |
| `slow_echo2` (3 chars at once, then 0.3 s each) | 24 letters | equal; 1 contradiction, then it waits for the real echo (no further guesses) | same |
| `mid_insert` | 10 characters inserted in the middle of a line of 25 | equal; **not predicted**: median 170 ms at RTT 100, 290 ms at `jl_200` | equal; predicted and underlined: median 11 ms and 1 ms |
| zsh autosuggestion | - | not run: no zsh in the lab VMs | - |

(`transform` and `slow_echo`, where the program gives no first echo to confirm an epoch, were also run: both clients show 0 s of
wrong characters in `transform` and no contradiction in `slow_echo`, and every checkpoint is equal.)

Notes on the vim row. The only mismatch the harness reported (9 of 9 vim runs, stock too) is row 24: vim's message "1 more line;
before #3 0 seconds ago" disappears when the Ctrl-L redraw is sent; it is the test, not the display. Against a stock
`mosh-server`, vim's alternate screen is **not visible to the client** (the server's terminal does not report it), so the alternate-
screen guard of `predict.rs` never fires in practice: moche predicts in vim insert mode exactly as stock does (median 1 ms,
93 % of the characters underlined at RTT 100; stock median 1 ms, 80 % underlined) and, like stock, shows nothing for NORMAL-mode
keys because the Esc ends the epoch and nothing confirms the next one.

### 3. Cost (RTT 100, about 10 keys/s for 32 s, three runs each)

CPU is user + sys of the client process over the typing window; wakeups are voluntary + involuntary context switches of all
threads per second.

| client | CPU ms per run (3 runs) | CPU ms per key | wakeups/s | RSS (kB) |
|---|---|---|---|---|
| moche adaptive | 770 / 620 / 640 | 2.15 to 2.60 | 115 / 117 / 116 | 5040 to 5068 |
| moche never | 720 / 610 / 640 | 2.11 to 2.43 | 106 / 108 / 108 | 5012 to 5040 |
| stock | 790 / 610 / 660 | 2.11 to 2.67 | 115 / 115 / 114 | 5032 to 5064 |

Adaptive costs 0 to 7 % more CPU than `never` (within the run-to-run spread of 20 %) and about 8 % more wakeups (the extra paint
per key); stock and adaptive are indistinguishable. RSS is 5 MB for all. **`strace -f -c -e trace=mmap,munmap,mremap,brk,madvise`
over 206 keys with adaptive active: 0 mmap/munmap/mremap, 14 brk (0.0 per key)**: the recycled `spare` grid works, no allocation
churn per key.

### 4. Wire neutrality (real server, same scripted typing, tcpdump of client to server datagrams, RTT 100, three runs per client)

| client | datagrams | bytes |
|---|---|---|
| moche never | 583 / 585 / 582 | 46565 / 46389 / 46099 |
| moche adaptive | 586 / 581 / 579 | 46200 / 45899 / 46314 |
| moche always | 585 / 582 / 581 | 46260 / 46185 / 46081 |
| stock | 351 / 350 / 346 | 27689 / 27381 / 27149 |

`never`, `adaptive` and `always` are indistinguishable (counts within 7 of each other, the same two datagram sizes, 64-79 and
80-95 bytes, in the same proportions): **prediction does not change the wire**. Separate finding: for the same typing moche sends
**about 1.67 times as many client datagrams as stock** (about 583 against 349) and 1.7 times the bytes, in all three modes. This is
not a prediction effect; it is probably that moche acknowledges server states with their own datagram where stock folds the ack into
the next keystroke or delays it. Relevant for battery on the phone; not investigated here.

### Conclusions

1. **No bug found.** No display differed from the server beyond the bound, no ghost secret in adaptive, no ghost keys in vim
   NORMAL mode, no underline residue, display equal to the server at 194 of 203 checkpoints (the 9 others: vim's own status line,
   see the vim notes), 10 % loss and jitter included.
2. **Better than stock** where it matters most: from RTT 200 up it shows about two more characters per hundred within 30 ms and starts
   the epoch earlier (chars 2-4); same cost; same wire.
3. **Worse than stock:** (a) typing inside a line is not predicted (170 to 290 ms per character against 1 to 11 ms at RTT 100 / 200
   with loss): the one functional gap, and the commonest editing gesture after a left arrow; (b) the underline starts about 20 ms of
   RTT earlier than stock's; (c) the real echo path is 5 to 18 ms slower (also in `never`).
4. **Constants.** Keep `ADAPTIVE_ON_MS`/`OFF_MS` 30/20: moche predicts at 30 where stock waits for 40, which costs nothing and
   gains 50 ms per character. Change `FLAG_ON_MS`/`FLAG_OFF_MS` from 85/65 to **100/80**: stock underlines from a netem RTT of 100
   (not 80 or 90, 30 % of characters with jitter at 100) and moche from 80; the lab shows no reason to flag earlier. The offset
   between the netem RTT and the client's own SRTT was not measured, so re-run the `rtt80`, `rtt90`, `rtt100`, `j20_100` conditions
   after changing. `CONFIRM_STREAK` and the epoch gating showed no problem (the secret never shows, one contradiction costs 2 to 3 wrong characters
   for 0.13 to 0.26 s). `MAX_PENDING`, `PASTE_BYTES` and the timeout were not exercised (the 200-character write came after an
   Enter, an unconfirmed epoch, so nothing would have been shown anyway).
5. **Default.** Yes: `adaptive` can be the CLI default as it is. It never differed from the server beyond the bound at any checkpoint, hides
   secrets, leaves the wire untouched and costs the same as stock. The constant change above is cosmetic. The mid-line insertion
   gap is the next feature, not a blocker.

### Open points (not done here)

- Mid-line insertion (and zsh autosuggestions, which put text to the right of the cursor) is not predicted; stock does.
- The `j20_400` seed 7 episode (about 35 characters at real-echo speed) was not reproduced in 3 more seeds; the display log was not
  kept, so the cause is open. Next time keep the per-sample log for the run where `never_visible` is not 0.
- The 5-18 ms slower real echo of moche against stock, and the 1.67 times client datagrams, are not prediction topics.
- The alternate-screen guard has no effect against a stock `mosh-server`; keep it for a server that reports it, or drop it from the
  "not predicted" list above, which still mentions it.
- Typed spaces cannot be timed with `capture-pane`; they are excluded from the statistics.
- Not run: zsh autosuggestions (no zsh in the VMs), a slow echo on a loaded server (emulated with `sleep` instead).
