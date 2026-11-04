/*
 * moche.h - C API of moche, a mosh client core (sans-IO).
 *
 * The host (sigillo, Swift) owns sockets, timers and the terminal view; this
 * library owns the crypto, the SSP transport and the screen model. Nothing
 * here blocks, spawns threads, opens sockets or calls back into the host.
 *
 * C99. Hand-written: keep it in step with crates/moche-ffi/src/lib.rs
 * (tools/ffi/check-header.sh compares the two symbol lists).
 *
 * CONVENTIONS
 *
 *  - Every function returns int32_t (except moche_version, moche_last_error):
 *      0   MOCHE_OK       done, out parameters are filled
 *      >0  status codes   not errors; see below
 *      <0  errors         see below; moche_last_error() explains the last one
 *  - All lengths are explicit (size_t); all integers fixed-width.
 *  - `now_ms` is a monotonic clock in milliseconds that YOU supply (for
 *    example DispatchTime.now().uptimeNanoseconds / 1_000_000). Use the same
 *    clock for every call of a session. Going backwards is tolerated (the
 *    library clamps it), but deadlines are only meaningful on that clock.
 *  - Out parameters are always written on success. Where a function can return
 *    MOCHE_NONE, MocheBytes out parameters are set to empty.
 *
 * THREADING
 *
 *  A MocheSession is NOT thread-safe: one owner at a time (a Swift actor or a
 *  serial queue). Different sessions are independent. moche_parse_connect,
 *  moche_find_connect, moche_version and moche_last_error need no session and
 *  may be called from any thread; the last error is per calling thread.
 *
 * OWNERSHIP
 *
 *  - MocheSession* comes from moche_session_new and goes to moche_session_free
 *    exactly once. Do not use it afterwards. NULL is accepted by free.
 *  - Every MocheBytes filled by the library must be released with
 *    moche_bytes_free (a non-empty buffer is heap memory owned by the library).
 *    The free function resets the struct to empty, so freeing the same
 *    variable twice is harmless. An empty MocheBytes (ptr NULL) needs no free.
 *    Out parameters are overwritten, never freed: free the old one first.
 *  - Input buffers are borrowed for the duration of the call only.
 *
 * KEY HYGIENE
 *
 *  The 22-character key from "MOSH CONNECT" is a secret. The library never logs
 *  it, never puts it in an error message, and wipes its own decoded copies when
 *  moche_session_new returns and when the session is freed. Wipe your copy
 *  (the key_out buffer of the parse functions) after moche_session_new.
 *
 * HOST LOOP (sketch)
 *
 *   // bootstrap, over your existing ssh connection:
 *   //   run `mosh-server new -s -c 256 -l LANG=en_US.UTF-8`, collect stdout
 *   uint16_t port; uint8_t key[MOCHE_KEY_BUF];
 *   moche_find_connect(stdout_buf, stdout_len, &port, key);
 *   MocheSession *s;
 *   moche_session_new(key, MOCHE_KEY_CHARS, cols, rows, now, &s);
 *   // optional: moche_session_set_predict_mode(s, MOCHE_PREDICT_ADAPTIVE);
 *   // wipe `key`; open a UDP socket to <server host>:port
 *
 *   // one "pump", run on the session's queue after EVERY event below:
 *   //   - a UDP datagram arrived        -> moche_session_handle_datagram
 *   //   - the user typed / pasted       -> moche_session_send_keys
 *   //   - the view changed size         -> moche_session_send_resize
 *   //   - the timer fired (it also fires for predictive-echo expiry: see
 *   //     PREDICTIVE ECHO; run the WHOLE pump, take_paint included)
 *   //
 *   // BACKPRESSURE for big pastes: do not hand a whole paste to
 *   // moche_session_send_keys at once. Feed it in slices (say 64 KiB) from your
 *   // pump while moche_session_stats(...).queued_bytes is below a few hundred
 *   // KB, and resume when it drains. Everything typed afterwards (a Ctrl-C!)
 *   // waits behind the queued bytes in the ordered stream.
 *   void pump(MocheSession *s, uint64_t now) {
 *       MocheBytes d;
 *       while (moche_session_poll_transmit(s, now, &d) == MOCHE_OK) {
 *           udp_send(d.ptr, d.len);              // errors: ignore, SSP resends
 *           moche_bytes_free(&d);
 *       }
 *       MocheBytes p;
 *       if (moche_session_take_paint(s, &p) == MOCHE_OK) {
 *           terminal_view_feed(p.ptr, p.len);    // SwiftTerm
 *           moche_bytes_free(&p);
 *       }
 *       uint32_t ev;  moche_session_take_events(s, &ev);   // bell, shutdown...
 *       MocheModes m; moche_session_modes(s, &m);          // for the key row
 *       MocheStats st; moche_session_stats(s, &st);        // paste pump: feed
 *       // more of a pending paste only while st.queued_bytes < ~256 KiB
 *       uint64_t dl;
 *       if (moche_session_next_deadline_ms(s, &dl) == MOCHE_OK)
 *           arm_timer_at(dl);                    // fire at max(dl - now, 0) ms
 *       else
 *           cancel_timer();
 *   }
 *
 *   // "connection lost" is YOUR UI decision:
 *   //   moche_session_ms_since_last_contact(s, now, &ms); if (ms > 5000) ...
 *   // The server heartbeats about every 3 s, so silence means trouble.
 *   // A roaming network change needs nothing: keep sending from the same
 *   // socket (or a new one); the server follows the latest authentic source.
 *
 * PAINT STREAM
 *
 *  moche_session_take_paint returns bytes for an xterm-like terminal view. It is
 *  made to be fed to a terminal in ANY state (host view just created, resized,
 *  or left with odd modes by earlier output):
 *   - every paint, full or partial, BEGINS with
 *       ESC[?25l  ESC[0m  ESC[r  ESC[?6l  ESC[?7h
 *     (hide cursor, reset SGR, reset scroll region, origin mode off, autowrap
 *     on), and places every row it writes with an absolute cursor position;
 *     it never relies on earlier regions or modes;
 *   - it never scrolls (no LF/CR/IND/RI/SU/SD) and never uses the alternate
 *     screen: the server's alternate screen is flattened into the one grid;
 *   - the only modes it ever toggles are DEC private
 *       ?25 cursor visibility
 *       ?1 application cursor keys
 *       ?1000 / ?1002 / ?1003 mouse reporting
 *       ?1005 / ?1006 / ?1015 mouse encodings
 *       ?2004 bracketed paste
 *     plus the ?6l and ?7h resets above, and OSC 0 / 1 / 2 titles (BEL-terminated);
 *   - a bell never appears as a BEL byte (BEL only terminates OSC titles): see
 *     MOCHE_EVENT_BELL;
 *   - a full repaint (first paint, after moche_session_force_full_paint, or
 *     when the server framebuffer size changed) starts with ESC[2J; a partial
 *     paint rewrites only changed rows (each ends with erase-to-end-of-line).
 *  The stream ends with the cursor placed and shown or hidden as the server has it.
 *
 *  Size: the paint is for the SERVER's framebuffer size (moche_session_screen_size).
 *  After moche_session_send_resize the server answers with a resize and a
 *  repaint; until it arrives the old size keeps being painted. When the size
 *  changes (MOCHE_EVENT_RESIZED) the next paint is already a full repaint.
 *  Size your view to match, or call moche_session_force_full_paint after you
 *  recreate or resize the view.
 *
 *  SGR underline (ESC[4m, reset by ESC[24m or ESC[0m) is part of the stream
 *  since predictive echo (below): a predicted cell is either ordinary text
 *  or, on a slow link, underlined. With predictive echo off (the default) the
 *  stream is exactly what it was before this feature existed, and the server's
 *  own underline attribute is carried as before. Every other rule above
 *  (preamble, absolute rows, no scrolling, the toggled modes) holds with
 *  overlays unchanged.
 *
 * PREDICTIVE ECHO
 *
 *  Like mosh's best-known feature: typed characters appear at once instead of
 *  one round trip later, and are then reconciled with the server's real state.
 *
 *   - OPT IN. A session starts in MOCHE_PREDICT_NEVER: nothing is predicted and
 *     every call behaves exactly as before. Call moche_session_set_predict_mode
 *     to enable it (ADAPTIVE is the sensible choice).
 *   - DISPLAY ONLY. Predictions are drawn into a copy of the screen that only
 *     moche_session_take_paint returns. They never change what is sent to the
 *     server, the screen model, the modes, the events or the deadlines of the
 *     transport; a wrong guess costs a repaint of one row, never a wrong
 *     byte on the wire.
 *   - MODES. NEVER: no prediction. ADAPTIVE: predict while the smoothed RTT
 *     (moche_session_srtt_ms) is above about 30 ms (stops below 20 ms), and
 *     only after the server's real echo has confirmed a prediction of the
 *     current epoch. ALWAYS: predict at any RTT and show the first prediction
 *     of an epoch before any confirmation. CAVEAT: with ALWAYS a secret typed
 *     where nothing echoes (a password prompt) is visible on screen for about
 *     one round trip plus 50 ms, until the server's echo ack names it, then it
 *     is erased. Do not offer ALWAYS to users who type passwords through it.
 *   - PASSWORD PROMPTS. In ADAPTIVE nothing typed at a prompt that does not
 *     echo is ever displayed: an epoch only starts showing after the server
 *     echoed one of its characters.
 *   - WHAT IS PREDICTED. Printable single-width characters typed at the end of
 *     a line, backspace and left/right arrows. Enter, Tab, control keys, other
 *     escapes, pastes, full-screen programs (alternate screen, mouse reporting
 *     or hidden cursor) end the epoch and are not predicted. Predictions are
 *     dropped on resize, on scrolling or screen redraws (anything the server
 *     does that contradicts them) and on the alternate screen.
 *   - THE HOST MUST KEEP PUMPING. A prediction that the server never confirms
 *     is removed after max(3 s, 4 x srtt), and the underline fades when the
 *     echo arrives. moche_session_next_deadline_ms already includes that time,
 *     so your single timer wakes in time; on that wake you must call
 *     moche_session_take_paint (the sketch's pump does): the removal is
 *     painted there. Call poll_transmit (or any call that takes now_ms) before
 *     take_paint in the same pump, as in the sketch, so the library sees the
 *     current time.
 *   - PAINT. take_paint paints "server state + overlay", changed rows only
 *     (a prediction appearing, changing or disappearing rewrites its row, with
 *     the usual preamble); after moche_session_force_full_paint a full repaint
 *     includes the overlay currently displayed.
 *   - moche_session_predict_stats reports counters, for a settings screen or
 *     debugging.
 */

#ifndef MOCHE_H
#define MOCHE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- codes -------------------------------------------------------------- */

/* Success. */
#define MOCHE_OK 0
/* Status, not an error: nothing to return (no datagram due, no deadline, no
 * paint needed, no new title). Out parameters of type MocheBytes are empty. */
#define MOCHE_NONE 1
/* Status, not an error: moche_session_handle_datagram dropped the datagram
 * (failed authentication, wrong direction, or too short). Normal on a hostile
 * or noisy network; keep going. It does not count as contact. */
#define MOCHE_AUTH_FAILED 2

/* A pointer argument was NULL, or NULL with a non-zero length. */
#define MOCHE_ERR_ARG (-1)
/* The key is not 22 characters of valid unpadded base64 (a 128-bit key). */
#define MOCHE_ERR_BAD_KEY (-2)
/* No valid "MOSH CONNECT <port> <key>" line (also: port 0, out of range or not
 * a number, wrong number of tokens). */
#define MOCHE_ERR_NO_CONNECT (-3)
/* An internal bug panicked inside the call. The session it happened in is dead. */
#define MOCHE_ERR_PANIC (-4)
/* The session is dead after an earlier MOCHE_ERR_PANIC. Free it and start over. */
#define MOCHE_ERR_DEAD (-5)
/* Input larger than allowed (connect buffer > 1 MiB, keys > 16 MiB per call). */
#define MOCHE_ERR_TOO_LARGE (-6)
/* cols or rows outside 1..1000. */
#define MOCHE_ERR_BAD_SIZE (-7)
/* The operating system random source failed (needed for padding bytes). */
#define MOCHE_ERR_RNG (-8)

/* Key length in characters, and the buffer size for moche_*_connect (+ NUL). */
#define MOCHE_KEY_CHARS 22
#define MOCHE_KEY_BUF 23

/* Largest UDP payload moche_session_poll_transmit can return, in bytes:
 * 8 (nonce) + 16 (tag) + 14 (inner header) + 1214 (fragment payload). Below the
 * 1280-byte IPv6 minimum MTU and WireGuard's ~1420, so a datagram is never
 * fragmented at IP level. Size receive buffers to at least this too: a stock
 * server sends the same maximum. */
#define MOCHE_MAX_DATAGRAM 1252

/* Bits of moche_session_take_events. One-shot bits are cleared by the call;
 * level bits are reported on every call while the condition holds. */
/* one-shot: the server rang the bell (the paint stream carries no BEL). */
#define MOCHE_EVENT_BELL 0x01u
/* one-shot: the title changed; fetch it with moche_session_take_title. */
#define MOCHE_EVENT_TITLE 0x02u
/* one-shot: the server framebuffer changed size (moche_session_screen_size). */
#define MOCHE_EVENT_RESIZED 0x04u
/* level: a server update could not be applied (its base state is missing), so
 * the screen may be wrong until the server sends a state we can apply; the
 * transport keeps acking, so this normally heals by itself. Show a hint. */
#define MOCHE_EVENT_OUT_OF_SYNC 0x08u
/* level: the server ended the session (the shell exited). Stop the loop after
 * painting the last screen. */
#define MOCHE_EVENT_SHUTDOWN 0x10u

/* ---- types -------------------------------------------------------------- */

/* Opaque session handle. Not thread-safe, one owner at a time. */
typedef struct MocheSession MocheSession;

/* A heap buffer owned by the library. ptr is NULL when empty.
 * Release with moche_bytes_free. cap is for the library; do not touch it. */
typedef struct MocheBytes {
    uint8_t *ptr;
    size_t len;
    size_t cap;
} MocheBytes;

/* Terminal modes of the latest server state; read them to encode keys and
 * mouse events (no need to parse the stream). Flags are 0 or 1. */
typedef struct MocheModes {
    uint16_t mouse_mode;      /* 0 off, else 1000, 1002 or 1003 */
    uint16_t mouse_encoding;  /* 0 default (X10), else 1005, 1006 or 1015 */
    uint8_t app_cursor_keys;  /* 1: arrows send ESC O A.. instead of ESC [ A.. */
    uint8_t bracketed_paste;  /* 1: wrap pastes in ESC[200~ ... ESC[201~ */
    uint8_t cursor_visible;
    uint8_t alt_screen;       /* 1: a full-screen program is running */
} MocheModes;

/* Input backlog and transport counters (moche_session_stats). 40 bytes, 8-byte
 * aligned, no padding; new fields are taken from `reserved`, which is always
 * zero for now. All counters are cumulative for the session. */
typedef struct MocheStats {
    uint64_t queued_bytes;  /* input bytes (keys/paste) not yet acked by the server */
    uint32_t send_errors;   /* frames that could not be built; should stay 0 */
    uint32_t rx_errors;     /* server messages dropped as malformed or over-limit */
    uint32_t rx_gaps;       /* server updates not applied, base state missing */
    uint32_t reserved[5];   /* zero */
} MocheStats;

/* Predictive echo modes, for moche_session_set_predict_mode. */
#define MOCHE_PREDICT_NEVER 0    /* default: nothing predicted */
#define MOCHE_PREDICT_ADAPTIVE 1 /* predict while the RTT is high, epoch-gated */
#define MOCHE_PREDICT_ALWAYS 2   /* predict at any RTT; may show a secret briefly */

/* Predictive echo counters and state (moche_session_predict_stats). 36 bytes,
 * 4-byte aligned, no hidden padding; new fields are taken from `reserved`,
 * which is always zero for now. Counters are cumulative since the session was
 * created (a mode change does not reset them) and saturate at UINT32_MAX. */
typedef struct MochePredictStats {
    uint32_t predictions;  /* typed keys turned into predictions (shown or hidden) */
    uint32_t confirmed;    /* predictions the server's echo confirmed */
    uint32_t contradicted; /* chains cancelled because the server showed something else */
    uint32_t expired;      /* chains or frozen overlays dropped by timeout or silence */
    uint8_t displaying;    /* 1: the overlay currently changes what take_paint draws */
    uint8_t mode;          /* current MOCHE_PREDICT_* value */
    uint8_t pad[2];        /* zero */
    uint32_t reserved[4];  /* zero */
} MochePredictStats;

/* ---- general ------------------------------------------------------------ */

/* Library version, a NUL-terminated static string. */
const char *moche_version(void);

/* Copies this thread's last error message (UTF-8) into buf, NUL-terminated,
 * truncated to cap bytes (including the NUL) at a character boundary. Returns
 * the full message length without the NUL, like snprintf (call with (NULL, 0)
 * to size a buffer). The message never contains key material. */
size_t moche_last_error(uint8_t *buf, size_t cap);

/* Releases a buffer returned by this library and resets *b to empty.
 * Safe to call twice on the same variable. */
int32_t moche_bytes_free(MocheBytes *b);

/* ---- bootstrap ---------------------------------------------------------- */

/* Parses one "MOSH CONNECT <port> <key>" line (surrounding whitespace and
 * CR/LF are ignored; extra tokens are refused). Writes the UDP port and the 22
 * key characters followed by a NUL into key_out (MOCHE_KEY_BUF bytes).
 * Returns MOCHE_OK, MOCHE_ERR_NO_CONNECT, MOCHE_ERR_BAD_KEY, MOCHE_ERR_TOO_LARGE
 * or MOCHE_ERR_ARG. */
int32_t moche_parse_connect(const uint8_t *line, size_t len,
                            uint16_t *port_out, uint8_t *key_out);

/* Same, for a whole buffer of ssh output (banner, motd, mosh-server's own
 * chatter): the first valid connect line wins. Up to 1 MiB. */
int32_t moche_find_connect(const uint8_t *buf, size_t len,
                           uint16_t *port_out, uint8_t *key_out);

/* ---- session ------------------------------------------------------------ */

/* Creates a session from the 22-character key (key_len must be 22), for a
 * terminal of cols x rows (1..1000). Queues the initial resize message. On
 * failure *out is NULL. */
int32_t moche_session_new(const uint8_t *key, size_t key_len,
                          uint16_t cols, uint16_t rows, uint64_t now_ms,
                          MocheSession **out);

/* Destroys the session and wipes its key material. NULL is a no-op. Always
 * succeeds, also on a dead session. */
int32_t moche_session_free(MocheSession *s);

/* Feeds one received UDP datagram. MOCHE_OK: authentic, state updated (paint,
 * events and deadlines may have changed: run your pump). MOCHE_AUTH_FAILED:
 * dropped, nothing changed. Never fatal. */
int32_t moche_session_handle_datagram(MocheSession *s, uint64_t now_ms,
                                      const uint8_t *data, size_t len);

/* Queues keystrokes or pasted bytes (any length up to 16 MiB; len 0 is a
 * no-op). Run your pump afterwards: the datagram leaves at once when a frame
 * interval has passed since the last send, else when it has (SRTT/2 + 6 ms,
 * at least 20 ms), see moche_session_next_deadline_ms. */
int32_t moche_session_send_keys(MocheSession *s, uint64_t now_ms,
                                const uint8_t *data, size_t len);

/* Tells the server the terminal is now cols x rows (1..1000). */
int32_t moche_session_send_resize(MocheSession *s, uint64_t now_ms,
                                  uint16_t cols, uint16_t rows);

/* Returns MOCHE_OK with one datagram to send to the server (UDP payload), or
 * MOCHE_NONE when nothing is due at now_ms. Call in a loop until MOCHE_NONE. */
int32_t moche_session_poll_transmit(MocheSession *s, uint64_t now_ms,
                                    MocheBytes *out);

/* MOCHE_OK with the absolute time (same clock as now_ms) at which
 * moche_session_poll_transmit next has work; a value at or before now means
 * "now". MOCHE_NONE (*out = 0): nothing scheduled. Re-arm your single timer
 * from this after every pump. With predictive echo on, it also covers the
 * time a prediction expires; wake, then run the whole pump (take_paint). */
int32_t moche_session_next_deadline_ms(MocheSession *s, uint64_t *out);

/* ---- screen ------------------------------------------------------------- */

/* Returns MOCHE_OK with the bytes that bring the terminal view from what was
 * last painted to the current screen (see PAINT STREAM), advancing the painted
 * state, or MOCHE_NONE if the view is already current. Feed the bytes to the
 * view in order, all of them, before the next take_paint. The first paint is
 * a full repaint. */
int32_t moche_session_take_paint(MocheSession *s, MocheBytes *out);

/* Makes the next take_paint a full repaint. Call it after the terminal view was
 * recreated, cleared or resized. */
int32_t moche_session_force_full_paint(MocheSession *s);

/* Terminal modes of the latest server state (see MocheModes). */
int32_t moche_session_modes(MocheSession *s, MocheModes *out);

/* Pending events as MOCHE_EVENT_* bits (see above). */
int32_t moche_session_take_events(MocheSession *s, uint32_t *out);

/* MOCHE_OK with the new window title (UTF-8, not NUL-terminated, may be empty
 * text) if it changed since the last call, else MOCHE_NONE. The paint stream
 * also carries it as OSC 2. */
int32_t moche_session_take_title(MocheSession *s, MocheBytes *out);

/* Size of the server framebuffer that moche_session_take_paint draws. */
int32_t moche_session_screen_size(MocheSession *s, uint16_t *cols, uint16_t *rows);

/* ---- link health -------------------------------------------------------- */

/* Milliseconds since the last authentic datagram from the server with a newer
 * sequence number than any before (replays do not count), or since the
 * session was created if there was none yet. */
int32_t moche_session_ms_since_last_contact(MocheSession *s, uint64_t now_ms,
                                            uint64_t *out);

/* Smoothed round-trip time in milliseconds (1000 until the first sample). */
int32_t moche_session_srtt_ms(MocheSession *s, uint32_t *out);

/* ---- backpressure and diagnostics --------------------------------------- */

/* Fills *out (see MocheStats).
 *  - queued_bytes: input bytes accepted by moche_session_send_keys that the
 *    server has not acknowledged yet. A host should stop feeding a big paste
 *    while this exceeds a few hundred KB and resume when it drains: everything
 *    typed afterwards (a Ctrl-C!) waits behind it in the ordered stream.
 *  - send_errors: frames that could not be built. Should stay 0; nonzero means
 *    input is stuck.
 *  - rx_errors: server messages dropped as malformed or over a limit.
 *  - rx_gaps: server updates not applied because their base state was missing;
 *    the transport re-acks what it holds. Benign, informational.
 * Counters saturate at UINT32_MAX. Returns MOCHE_OK, MOCHE_ERR_ARG (NULL s or
 * out) or MOCHE_ERR_DEAD. */
int32_t moche_session_stats(MocheSession *s, MocheStats *out);

/* ---- predictive echo ---------------------------------------------------- */

/* Chooses MOCHE_PREDICT_NEVER (0, the default of every new session, so an app
 * that never calls this behaves exactly as before), MOCHE_PREDICT_ADAPTIVE (1)
 * or MOCHE_PREDICT_ALWAYS (2); see PREDICTIVE ECHO. Changing the mode drops
 * the pending predictions (the next take_paint removes them from the view).
 * Display only: what is sent never changes. Returns MOCHE_OK, MOCHE_ERR_ARG
 * (NULL s, or a mode outside 0..2) or MOCHE_ERR_DEAD. */
int32_t moche_session_set_predict_mode(MocheSession *s, int32_t mode);

/* Fills *out (see MochePredictStats). All zero except `mode` on a session that
 * never enabled predictions. Returns MOCHE_OK, MOCHE_ERR_ARG (NULL s or out) or
 * MOCHE_ERR_DEAD. */
int32_t moche_session_predict_stats(MocheSession *s, MochePredictStats *out);

#ifdef __cplusplus
}
#endif

#endif /* MOCHE_H */
