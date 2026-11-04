/* C smoke test of moche.h against the static library. Exits non-zero on failure. */
#include "moche.h"

#include <stdio.h>
#include <string.h>

#define CHECK(c)                                                              \
    do {                                                                      \
        if (!(c)) {                                                           \
            fprintf(stderr, "%s:%d: check failed: %s\n", __FILE__, __LINE__,  \
                    #c);                                                      \
            return 1;                                                         \
        }                                                                     \
    } while (0)

/* C99 compile-time layout check: a negative array size fails the build. */
typedef char moche_stats_size_check[sizeof(MocheStats) == 40 ? 1 : -1];

typedef char moche_predict_stats_size_check[sizeof(MochePredictStats) == 36 ? 1 : -1];

#define KEY "AAECAwQFBgcICQoLDA0ODw"

int main(void) {
    CHECK(strlen(moche_version()) > 0);

    /* parse_connect / find_connect */
    uint16_t port = 0;
    uint8_t key[MOCHE_KEY_BUF];
    const char *line = "  MOSH CONNECT 60001 " KEY "\r\n";
    CHECK(moche_parse_connect((const uint8_t *)line, strlen(line), &port, key) == MOCHE_OK);
    CHECK(port == 60001);
    CHECK(strcmp((const char *)key, KEY) == 0);

    const char *noisy = "Welcome\r\nMOSH CONNECT 61234 " KEY "\r\n[detached]\r\n";
    port = 0;
    CHECK(moche_find_connect((const uint8_t *)noisy, strlen(noisy), &port, key) == MOCHE_OK);
    CHECK(port == 61234);

    CHECK(moche_find_connect((const uint8_t *)"nothing", 7, &port, key) == MOCHE_ERR_NO_CONNECT);
    uint8_t msg[128];
    size_t n = moche_last_error(msg, sizeof msg);
    CHECK(n > 0 && n < sizeof msg && strlen((const char *)msg) == n);
    CHECK(moche_find_connect(NULL, 3, &port, key) == MOCHE_ERR_ARG);

    /* session lifecycle */
    MocheSession *s = NULL;
    CHECK(moche_session_new((const uint8_t *)"short", 5, 80, 24, 1000, &s) == MOCHE_ERR_BAD_KEY);
    CHECK(s == NULL);
    CHECK(moche_session_new(key, MOCHE_KEY_CHARS, 0, 24, 1000, &s) == MOCHE_ERR_BAD_SIZE);
    CHECK(moche_session_new(key, MOCHE_KEY_CHARS, 80, 24, 1000, &s) == MOCHE_OK);
    CHECK(s != NULL);
    memset(key, 0, sizeof key);

    /* loopback: keys -> a client datagram with the direction bit clear */
    MocheStats st;
    memset(&st, 0xff, sizeof st);
    CHECK(moche_session_stats(s, &st) == MOCHE_OK);
    CHECK(st.queued_bytes == 0 && st.send_errors == 0 && st.rx_errors == 0 && st.rx_gaps == 0);
    for (int i = 0; i < 5; i++) CHECK(st.reserved[i] == 0);
    CHECK(moche_session_stats(s, NULL) == MOCHE_ERR_ARG);
    CHECK(moche_session_stats(NULL, &st) == MOCHE_ERR_ARG);
    CHECK(moche_session_send_keys(s, 1000, (const uint8_t *)"ls\r", 3) == MOCHE_OK);
    CHECK(moche_session_stats(s, &st) == MOCHE_OK && st.queued_bytes == 3);
    uint64_t dl = 0;
    CHECK(moche_session_next_deadline_ms(s, &dl) == MOCHE_OK);
    CHECK(dl >= 1000);
    int sent = 0;
    MocheBytes d = {0};
    while (moche_session_poll_transmit(s, dl + 1000, &d) == MOCHE_OK) {
        CHECK(d.ptr != NULL && d.len >= 8 + 16);
        CHECK((d.ptr[0] & 0x80) == 0);
        CHECK(moche_bytes_free(&d) == MOCHE_OK);
        CHECK(d.ptr == NULL && d.len == 0);
        CHECK(moche_bytes_free(&d) == MOCHE_OK); /* double free is harmless */
        sent++;
    }
    CHECK(sent >= 1);
    CHECK(moche_session_poll_transmit(s, dl + 1000, &d) == MOCHE_NONE);
    CHECK(d.ptr == NULL);

    /* garbage is a non-fatal status */
    uint8_t junk[64];
    memset(junk, 7, sizeof junk);
    CHECK(moche_session_handle_datagram(s, 1100, junk, sizeof junk) == MOCHE_AUTH_FAILED);

    /* first paint, then nothing */
    MocheBytes p = {0};
    CHECK(moche_session_take_paint(s, &p) == MOCHE_OK);
    CHECK(p.len > 20 && memcmp(p.ptr, "\x1b[?25l\x1b[0m\x1b[r\x1b[?6l\x1b[?7h", 20) == 0);
    CHECK(moche_bytes_free(&p) == MOCHE_OK);
    CHECK(moche_session_take_paint(s, &p) == MOCHE_NONE);

    uint16_t cols = 0, rows = 0;
    CHECK(moche_session_screen_size(s, &cols, &rows) == MOCHE_OK);
    CHECK(cols == 80 && rows == 24);
    MocheModes m;
    CHECK(moche_session_modes(s, &m) == MOCHE_OK);
    CHECK(m.cursor_visible == 1 && m.mouse_mode == 0);
    uint32_t ev = 1;
    CHECK(moche_session_take_events(s, &ev) == MOCHE_OK && ev == 0);
    uint64_t since = 0;
    CHECK(moche_session_ms_since_last_contact(s, 4000, &since) == MOCHE_OK);
    CHECK(since == 3000);
    uint32_t srtt = 0;
    CHECK(moche_session_srtt_ms(s, &srtt) == MOCHE_OK);

    /* predictive echo: default NEVER, validation, stats */
    MochePredictStats ps;
    memset(&ps, 0xff, sizeof ps);
    CHECK(moche_session_predict_stats(s, &ps) == MOCHE_OK);
    CHECK(ps.mode == MOCHE_PREDICT_NEVER && ps.displaying == 0);
    CHECK(ps.predictions == 0 && ps.confirmed == 0 && ps.contradicted == 0 && ps.expired == 0);
    for (int i = 0; i < 4; i++) CHECK(ps.reserved[i] == 0);
    CHECK(ps.pad[0] == 0 && ps.pad[1] == 0);
    CHECK(moche_session_set_predict_mode(s, 3) == MOCHE_ERR_ARG);
    CHECK(moche_session_set_predict_mode(s, -1) == MOCHE_ERR_ARG);
    CHECK(moche_session_set_predict_mode(NULL, MOCHE_PREDICT_ADAPTIVE) == MOCHE_ERR_ARG);
    CHECK(moche_session_predict_stats(s, NULL) == MOCHE_ERR_ARG);
    CHECK(moche_session_predict_stats(NULL, &ps) == MOCHE_ERR_ARG);
    CHECK(moche_session_set_predict_mode(s, MOCHE_PREDICT_ADAPTIVE) == MOCHE_OK);
    CHECK(moche_session_predict_stats(s, &ps) == MOCHE_OK && ps.mode == MOCHE_PREDICT_ADAPTIVE);
    CHECK(moche_session_set_predict_mode(s, MOCHE_PREDICT_ALWAYS) == MOCHE_OK);
    CHECK(moche_session_predict_stats(s, &ps) == MOCHE_OK && ps.mode == MOCHE_PREDICT_ALWAYS);
    CHECK(moche_session_set_predict_mode(s, MOCHE_PREDICT_NEVER) == MOCHE_OK);

    CHECK(moche_session_free(s) == MOCHE_OK);
    CHECK(moche_session_free(NULL) == MOCHE_OK);
    puts("test_moche: ok");
    return 0;
}
