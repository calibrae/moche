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
   never confirms, so nothing is shown (Adaptive).
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
| `FLAG_ON_MS` / `FLAG_OFF_MS` | 85 / 65 ms | OBSERVED: stock underlines unconfirmed predictions (SGR 4) from a path RTT of 90 ms, not at 80 ms. SOURCED: "on high-delay connections, we underline unconfirmed predictions". Band is OUR CHOICE. |
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
- The display can differ from the server for at most the timeout (`max(3 s, 4 * srtt)`)
  when no echo ack arrives, and for at most one RTT plus 50 ms otherwise. The host must
  call `tick` at `next_deadline_ms`; the CLI does.
- Thresholds were observed on loopback with a simulated delay, not on a real path with
  jitter.

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

## Validating in the lab (phase 2)

Run `moche --predict=adaptive` against a real `mosh-server` over `netem` at 50, 100, 200
and 400 ms with jitter and 5 % loss: check the thresholds, `ssh`-style password prompts
(`sudo`), vim and readline editing (insert in the middle, Ctrl-A/E), a slow echo (loaded
server), and compare with the stock client under the same netem.
