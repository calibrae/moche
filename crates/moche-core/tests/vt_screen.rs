//! Interpreter, state map and repaint tests on hand-written sequences.

use moche_core::screen::{repaint, ApplyError, Color, Framebuffer, Screen, ScreenEvent};

fn fb(cols: u16, rows: u16, s: &str) -> Framebuffer {
    let mut f = Framebuffer::new(cols, rows);
    f.feed(s.as_bytes());
    f
}

fn rt(f: &Framebuffer) {
    let mut g = Framebuffer::new(f.cols(), f.rows());
    g.feed(&repaint(f, None));
    assert!(
        g.display_eq(f),
        "round trip mismatch:\n{:?}\nvs\n{:?}",
        f.text(),
        g.text()
    );
}

#[test]
fn text_and_crlf() {
    let f = fb(10, 3, "ab\r\ncd");
    assert_eq!(f.row_text(0), "ab");
    assert_eq!(f.row_text(1), "cd");
    assert_eq!((f.cursor_row(), f.cursor_col()), (1, 2));
    rt(&f);
}

#[test]
fn wrap_is_deferred() {
    let mut f = fb(5, 3, "abcde");
    assert_eq!((f.cursor_row(), f.cursor_col()), (0, 4));
    assert!(f.pending_wrap());
    f.feed(b"f");
    assert_eq!(f.row_text(0), "abcde");
    assert_eq!(f.row_text(1), "f");
    assert!(!f.pending_wrap());
    // CR after pending wrap stays on the same row
    let f = fb(5, 3, "abcde\rX");
    assert_eq!(f.row_text(0), "Xbcde");
    // autowrap off overwrites the last column
    let f = fb(5, 3, "\x1b[?7labcdefg");
    assert_eq!(f.row_text(0), "abcdg");
    assert_eq!(f.row_text(1), "");
}

#[test]
fn scrolls_at_bottom() {
    let f = fb(4, 2, "a\r\nb\r\nc");
    assert_eq!(f.row_text(0), "b");
    assert_eq!(f.row_text(1), "c");
}

#[test]
fn cursor_movement() {
    let f = fb(
        10,
        5,
        "\x1b[3;4HX\x1b[2AY\x1b[3BZ\x1b[2DW\x1b[2CV\x1b[G1\x1b[5d2",
    );
    assert_eq!(f.row_text(2).chars().nth(3), Some('X'));
    assert_eq!(f.row_text(0).chars().nth(4), Some('Y'));
    assert_eq!(f.row_text(3).chars().nth(5), Some('Z'));
    assert_eq!(f.row_text(4), " 2");
    assert_eq!(f.row_text(3).chars().next(), Some('1'));
    assert_eq!(f.row_text(3).chars().nth(4), Some('W'));
    assert_eq!(f.row_text(3).chars().nth(7), Some('V'));
    let f = fb(10, 5, "\x1b[3;3H\x1b[E1\x1b[2F2");
    assert_eq!(f.row_text(3), "1");
    assert_eq!(f.row_text(1), "2");
    let f = fb(10, 5, "\x1b[99;99HX");
    assert_eq!(f.row_text(4), "         X");
    rt(&f);
}

#[test]
fn erase_modes() {
    let base = "\x1b[1;1Habcde\x1b[2;1Hfghij\x1b[3;1Hklmno\x1b[2;3H";
    let f = fb(5, 3, &format!("{base}\x1b[J"));
    assert_eq!(
        (f.row_text(0), f.row_text(1), f.row_text(2)),
        ("abcde".into(), "fg".into(), "".into())
    );
    let f = fb(5, 3, &format!("{base}\x1b[1J"));
    assert_eq!(
        (f.row_text(0), f.row_text(1), f.row_text(2)),
        ("".into(), "   ij".into(), "klmno".into())
    );
    let f = fb(5, 3, &format!("{base}\x1b[2J"));
    assert_eq!(f.text(), "\n\n");
    let f = fb(5, 3, &format!("{base}\x1b[3J"));
    assert_eq!(f.row_text(0), "abcde");
    let f = fb(5, 3, &format!("{base}\x1b[K"));
    assert_eq!(f.row_text(1), "fg");
    let f = fb(5, 3, &format!("{base}\x1b[1K"));
    assert_eq!(f.row_text(1), "   ij");
    let f = fb(5, 3, &format!("{base}\x1b[2K"));
    assert_eq!(f.row_text(1), "");
    let f = fb(5, 3, &format!("{base}\x1b[2X"));
    assert_eq!(f.row_text(1), "fg  j");
    rt(&f);
}

#[test]
fn insert_delete_chars_and_lines() {
    let f = fb(6, 3, "abcdef\x1b[1;3H\x1b[2@");
    assert_eq!(f.row_text(0), "ab  cd");
    let f = fb(6, 3, "abcdef\x1b[1;3H\x1b[2P");
    assert_eq!(f.row_text(0), "abef");
    let f = fb(3, 4, "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
    assert_eq!(f.text(), "a\n\nb\nc");
    let f = fb(3, 4, "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[2M");
    assert_eq!(f.text(), "a\nd\n\n");
}

#[test]
fn scroll_region_with_il_dl_su_sd() {
    // rows 2..4 of 5 form the region
    let setup = "1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r";
    let f = fb(3, 5, setup);
    assert_eq!((f.cursor_row(), f.cursor_col()), (0, 0));
    let f = fb(3, 5, &format!("{setup}\x1b[4;1H\n"));
    assert_eq!(f.text(), "1\n3\n4\n\n5");
    let f = fb(3, 5, &format!("{setup}\x1b[2;1H\x1bM"));
    assert_eq!(f.text(), "1\n\n2\n3\n5");
    let f = fb(3, 5, &format!("{setup}\x1b[3;1H\x1b[L"));
    assert_eq!(f.text(), "1\n2\n\n3\n5");
    let f = fb(3, 5, &format!("{setup}\x1b[3;1H\x1b[M"));
    assert_eq!(f.text(), "1\n2\n4\n\n5");
    // IL outside the region does nothing
    let f = fb(3, 5, &format!("{setup}\x1b[1;1H\x1b[L"));
    assert_eq!(f.text(), "1\n2\n3\n4\n5");
    let f = fb(3, 5, &format!("{setup}\x1b[S"));
    assert_eq!(f.text(), "1\n3\n4\n\n5");
    let f = fb(3, 5, &format!("{setup}\x1b[T"));
    assert_eq!(f.text(), "1\n\n2\n3\n5");
    // invalid region ignored; reset with ESC[r
    let f = fb(3, 5, "\x1b[4;2r\x1b[3;3H");
    assert_eq!((f.cursor_row(), f.cursor_col()), (2, 2));
    // origin mode is relative to the region
    let f = fb(3, 5, &format!("{setup}\x1b[?6h\x1b[1;1HX"));
    assert_eq!(f.row_text(1), "X");
    let f = fb(3, 5, &format!("{setup}\x1b[?6h\x1b[9;1HX"));
    assert_eq!(f.row_text(3), "X");
    rt(&f);
}

#[test]
fn sgr_forms() {
    let f = fb(20, 2, "\x1b[0;1;4;7;38;5;196;48;5;21mA\x1b[0mB");
    let a = f.cell(0, 0).attrs;
    assert_eq!(a.fg, Color::Indexed(196));
    assert_eq!(a.bg, Color::Indexed(21));
    assert_eq!(a.flags, 1 | 8 | 32);
    assert_eq!(f.cell(0, 1).attrs, Default::default());
    let f = fb(
        20,
        2,
        "\x1b[38;2;10;200;30;48;2;1;2;3mA\x1b[39;49mB\x1b[31;42mC\x1b[91;104mD",
    );
    assert_eq!(f.cell(0, 0).attrs.fg, Color::Rgb(10, 200, 30));
    assert_eq!(f.cell(0, 0).attrs.bg, Color::Rgb(1, 2, 3));
    assert_eq!(f.cell(0, 1).attrs.fg, Color::Default);
    assert_eq!(f.cell(0, 2).attrs.fg, Color::Indexed(1));
    assert_eq!(f.cell(0, 2).attrs.bg, Color::Indexed(2));
    assert_eq!(f.cell(0, 3).attrs.fg, Color::Indexed(9));
    assert_eq!(f.cell(0, 3).attrs.bg, Color::Indexed(12));
    let f = fb(
        20,
        2,
        "\x1b[1;2;3;4;5;7;8;9mA\x1b[22;23;24;25;27;28;29mB\x1b[mC",
    );
    assert_eq!(f.cell(0, 0).attrs.flags, 255);
    assert_eq!(f.cell(0, 1).attrs.flags, 0);
    // truncated extended colours do not panic or eat later parameters wrongly
    let f = fb(5, 1, "\x1b[38;5mA\x1b[38;2;1mB\x1b[48;5;300mC");
    assert_eq!(f.row_text(0), "ABC");
    rt(&f);
    // erased cells take the current background
    let f = fb(5, 1, "\x1b[44m\x1b[K");
    assert_eq!(f.cell(0, 3).attrs.bg, Color::Indexed(4));
    rt(&f);
}

#[test]
fn wide_chars_and_edges() {
    let f = fb(6, 2, "日本a");
    assert_eq!(f.cell(0, 0).width, 2);
    assert_eq!(f.cell(0, 1).width, 0);
    assert_eq!(f.cell(0, 4).ch, 'a');
    assert_eq!(f.row_text(0), "日本a");
    assert_eq!(f.cursor_col(), 5);
    // wide char at the last column wraps, leaving the last cell blank
    let f = fb(5, 2, "abcd日");
    assert_eq!(f.row_text(0), "abcd");
    assert_eq!(f.row_text(1), "日");
    assert_eq!(f.cell(0, 4).ch, ' ');
    // overwriting half of a wide char blanks the other half
    let f = fb(6, 1, "日\x1b[1;2Hx");
    assert_eq!(f.row_text(0), " x");
    let f = fb(6, 1, "日\x1b[1;1Hx");
    assert_eq!(f.row_text(0), "x");
    assert_eq!(f.cell(0, 1).width, 1);
    // ECH through the middle
    let f = fb(6, 1, "a日b\x1b[1;3H\x1b[X");
    assert_eq!(f.row_text(0), "a  b");
    // wide char ending exactly at the last column
    let f = fb(4, 1, "ab日");
    assert_eq!(f.row_text(0), "ab日");
    assert!(f.pending_wrap());
    rt(&f);
    rt(&fb(5, 2, "abcd日"));
    // emoji and fullwidth
    let f = fb(10, 1, "😀 ＡＢ");
    assert_eq!(f.cell(0, 0).width, 2);
    assert_eq!(f.cell(0, 3).width, 2);
    rt(&f);
    // wide char wider than the screen is dropped
    let f = fb(1, 1, "日a");
    assert_eq!(f.row_text(0), "a");
}

#[test]
fn combining_marks_and_zwj() {
    let f = fb(10, 1, "e\u{301}x a\u{308}\u{302}");
    assert_eq!(f.cell(0, 0).ch, 'e');
    assert_eq!(f.cell(0, 0).comb.as_deref(), Some("\u{301}"));
    assert_eq!(f.cell(0, 1).ch, 'x');
    assert_eq!(f.cell(0, 3).comb.as_deref(), Some("\u{308}\u{302}"));
    assert_eq!(f.cursor_col(), 4);
    rt(&f);
    // mark on a wide char at the right edge (pending wrap)
    let f = fb(4, 1, "ab日\u{301}");
    assert_eq!(f.cell(0, 2).comb.as_deref(), Some("\u{301}"));
    // mark at column 0 with nothing before is dropped
    let f = fb(4, 1, "\u{301}a");
    assert_eq!(f.row_text(0), "a");
    // ZWJ sequence stays in one cell
    let f = fb(10, 1, "👩\u{200d}👩x");
    assert_eq!(f.cell(0, 0).comb.as_deref(), Some("\u{200d}👩"));
    assert_eq!(f.cell(0, 2).ch, 'x');
    rt(&f);
    // trailing ZWJ then a separate cell survives repaint
    let f = fb(10, 1, "a\u{200d}\x1b[1;2Hb");
    assert_eq!(f.cell(0, 1).ch, 'b');
    rt(&f);
}

#[test]
fn utf8_incremental_and_invalid() {
    let mut f = Framebuffer::new(10, 1);
    f.feed(&[0xe6, 0x97]);
    f.feed(&[0xa5, b'a']);
    assert_eq!(f.row_text(0), "日a");
    let f = fb(10, 1, "");
    assert_eq!(f.row_text(0), "");
    let mut f = Framebuffer::new(10, 1);
    f.feed(&[b'a', 0xe6, b'b', 0xff, 0xc0, 0x80, 0xed, 0xa0, 0x80, b'c']);
    assert!(f.row_text(0).starts_with("a\u{fffd}b"));
    assert!(f.row_text(0).ends_with('c'));
}

#[test]
fn save_restore_cursor() {
    let f = fb(10, 3, "\x1b[2;3H\x1b[1;31m\x1b7\x1b[1;1H\x1b[0mA\x1b8B");
    assert_eq!(f.row_text(1), "  B");
    assert_eq!(f.cell(1, 2).attrs.fg, Color::Indexed(1));
    let f = fb(10, 3, "\x1b[2;3H\x1b[s\x1b[1;1H\x1b[uX");
    assert_eq!(f.row_text(1), "  X");
    // restore with nothing saved goes home
    let f = fb(10, 3, "\x1b[2;3H\x1b8X");
    assert_eq!(f.row_text(0), "X");
}

#[test]
fn alt_screen_swap_and_restore() {
    let f = fb(10, 3, "main\x1b[?1049h");
    assert!(f.modes().alt_screen);
    assert_eq!(f.text(), "\n\n");
    let f = fb(10, 3, "main\x1b[?1049hALT\x1b[?1049lz");
    assert!(!f.modes().alt_screen);
    // main screen back, cursor restored to just after "main"
    assert_eq!(f.row_text(0), "mainz");
    assert_eq!(f.row_text(1), "");
}

#[test]
fn alt_screen_modes_47_1047() {
    let f = fb(10, 2, "main\x1b[?47h\x1b[HALT\x1b[?47l");
    assert_eq!(f.row_text(0), "main");
    let f = fb(10, 2, "main\x1b[?1047hxx\x1b[?1047l");
    assert_eq!(f.row_text(0), "main");
    // repaint paints whatever is visible
    let f = fb(10, 2, "main\x1b[?1049h\x1b[HALT");
    assert_eq!(f.row_text(0), "ALT");
    rt(&f);
}

#[test]
fn title_and_events() {
    let mut f = fb(10, 1, "\x1b]0;hello\x07\x1b]2;world\x1b\\\x1b]1;ic\x07\x07");
    assert_eq!(f.title(), "world");
    assert_eq!(f.icon_name(), "ic");
    let ev = f.take_events();
    assert!(ev.contains(&ScreenEvent::Bell));
    assert!(ev.contains(&ScreenEvent::TitleChanged("hello".into())));
    assert!(ev.contains(&ScreenEvent::TitleChanged("world".into())));
    assert!(ev.contains(&ScreenEvent::IconNameChanged("ic".into())));
    // UTF-8 title, control characters stripped, other OSCs ignored
    let f = fb(
        10,
        1,
        "\x1b]2;caf\u{e9}\x01\x1b\\\x1b]52;c;AAAA\x07\x1b]8;;http://x\x07ok",
    );
    assert_eq!(f.title(), "café");
    assert_eq!(f.row_text(0), "ok");
    let f = fb(10, 1, "\x1b]2;t\x07");
    rt(&f);
}

#[test]
fn modes_set_reset() {
    let f = fb(
        10,
        1,
        "\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?25l\x1b[?12h",
    );
    let m = f.modes();
    assert!(m.app_cursor && m.bracketed_paste && m.cursor_blink);
    assert_eq!((m.mouse_mode, m.mouse_enc), (1000, 1006));
    assert!(!f.cursor_visible());
    rt(&f);
    // the server's full mouse reset
    let f = fb(
        10,
        1,
        "\x1b[?1000h\x1b[?1006h\x1b[?1003l\x1b[?1002l\x1b[?1001l\x1b[?1000l\x1b[?1015l\x1b[?1006l",
    );
    assert_eq!((f.modes().mouse_mode, f.modes().mouse_enc), (0, 0));
    let f = fb(10, 1, "\x1b[?1002h\x1b[?1003h\x1b[?1005h\x1b[?1015h");
    assert_eq!((f.modes().mouse_mode, f.modes().mouse_enc), (1003, 1015));
    // multiple modes in one sequence
    let f = fb(10, 1, "\x1b[?25;2004;1h");
    assert!(f.modes().app_cursor && f.modes().bracketed_paste);
}

#[test]
fn tabs() {
    let f = fb(20, 1, "a\tb\t\tc");
    assert_eq!(f.cell(0, 8).ch, 'b');
    assert_eq!(f.cell(0, 19).ch, 'c');
    let f = fb(20, 1, "\x1b[3g\tX");
    assert_eq!(f.cell(0, 19).ch, 'X');
    let f = fb(20, 1, "\x1b[1;4H\x1bH\x1b[1;1H\tX");
    assert_eq!(f.cell(0, 3).ch, 'X');
    let f = fb(20, 1, "\x1b[1;12H\x1b[Z!");
    assert_eq!(f.cell(0, 8).ch, '!');
}

#[test]
fn rep_and_misc_controls() {
    let f = fb(10, 1, "ab\x1b[3b");
    assert_eq!(f.row_text(0), "abbbb");
    let f = fb(10, 1, "ab\x08\x08\x08c\x0e\x0fd\x1b(Bx\x1b)0y");
    assert_eq!(f.row_text(0), "cdxy");
    let f = fb(10, 3, "a\x1bEb\x1bDc");
    assert_eq!(f.row_text(0), "a");
    assert_eq!(f.row_text(1), "b");
    assert_eq!(f.row_text(2), "  c".replace("  ", " "));
}

#[test]
fn ris_and_unknown_sequences() {
    let f = fb(10, 2, "hello\x1b]2;t\x07\x1b[1m\x1bc");
    assert_eq!(f.text(), "\n");
    assert_eq!(f.title(), "t");
    // unknown / ignored sequences are consumed whole
    let f = fb(
        10,
        1,
        "\x1b[6n\x1b[>c\x1b[?1;2c\x1b[ q\x1b[!p\x1bP1$rsome\x1b\\\x1b_apc\x1b\\\x1b[99zA",
    );
    assert_eq!(f.row_text(0), "A");
    // a control character inside a CSI executes
    let f = fb(10, 2, "\x1b[2\n;3HX");
    assert_eq!(f.row_text(1), "  X");
    // CAN aborts a sequence
    let f = fb(10, 1, "\x1b[3\x18A");
    assert_eq!(f.row_text(0), "A");
}

#[test]
fn resize_keeps_top_left() {
    let mut f = fb(5, 3, "abcde\r\nfghij\r\nklmno\x1b[2;4r");
    f.resize(3, 2);
    assert_eq!(f.text(), "abc\nfgh");
    f.resize(6, 4);
    assert_eq!(f.row_text(0), "abc");
    assert_eq!((f.cols(), f.rows()), (6, 4));
    assert!(f.cursor_row() < 4 && f.cursor_col() < 6);
    // cutting a wide char in half blanks it
    let mut f = fb(4, 1, "ab日");
    f.resize(3, 1);
    assert_eq!(f.row_text(0), "ab");
}

#[test]
fn repaint_round_trips() {
    let f = fb(
        20,
        5,
        "\x1b[?1h\x1b]2;ti\x07\x1b[1;31mred\x1b[0m plain\r\n\x1b[44m   \x1b[0m 日本 e\u{301}\r\n\x1b[4mul\x1b[3;19Hxy\x1b[5;1H\x1b[38;2;1;2;3mrgb\x1b[2;7H\x1b[?25l",
    );
    rt(&f);
    // blank screen
    rt(&Framebuffer::new(80, 24));
    // repaint(None) starts with a clear and hides the cursor first
    let b = repaint(&f, None);
    let s = String::from_utf8(b).unwrap();
    assert!(s.starts_with("\x1b[?25l"));
    assert!(s.contains("\x1b[2J"));
    assert!(s.ends_with("\x1b[0m\x1b[2;7H"));
}

#[test]
fn repaint_incremental() {
    let a = fb(20, 4, "one\r\ntwo\r\nthree\r\nfour");
    let mut b = a.clone();
    b.feed(b"\x1b[2;1H\x1b[KTWO\x1b[?2004h\x1b[?1000h\x1b]2;new\x07");
    let diff = String::from_utf8(repaint(&b, Some(&a))).unwrap();
    assert!(!diff.contains("\x1b[2J"));
    assert!(!diff.contains("one") && !diff.contains("three") && diff.contains("TWO"));
    assert!(diff.contains("\x1b[?2004h") && diff.contains("\x1b[?1000h"));
    assert!(diff.contains("\x1b]2;new\x07"));
    assert!(!diff.contains("\x1b[?1h"));
    // applying it to a host that shows `a` reproduces `b`
    let mut host = Framebuffer::new(20, 4);
    host.feed(&repaint(&a, None));
    host.feed(&repaint(&b, Some(&a)));
    assert!(host.display_eq(&b));
    // and back, mouse off again
    let mut c = b.clone();
    c.feed(b"\x1b[?1000l\x1b[?2004l\x1b[1;1HXXXXXXXXXXXXXXXXXXXXXXXXX");
    host.feed(&repaint(&c, Some(&b)));
    assert!(host.display_eq(&c));
    // a row that gains a coloured tail then loses it
    let mut d = c.clone();
    d.feed(b"\x1b[3;1H\x1b[41m\x1b[K");
    host.feed(&repaint(&d, Some(&c)));
    assert!(host.display_eq(&d));
    // size change: full repaint
    let mut e = d.clone();
    e.resize(10, 3);
    e.feed(b"\x1b[H\x1b[2Jsmall");
    let s = String::from_utf8(repaint(&e, Some(&d))).unwrap();
    assert!(s.contains("\x1b[2J"));
    host.resize(10, 3);
    host.feed(s.as_bytes());
    assert!(host.display_eq(&e));
}

#[test]
fn state_map() {
    let mut s = Screen::new(10, 2);
    assert_eq!(s.latest_num(), 0);
    assert!(s.has_state(0));
    s.apply(0, 1, None, b"one").unwrap();
    s.apply(1, 2, None, b"\r\ntwo").unwrap();
    // any held base works
    s.apply(0, 3, None, b"zero").unwrap();
    s.apply(1, 4, None, b"X").unwrap();
    assert_eq!(s.latest_num(), 4);
    assert_eq!(s.latest().row_text(0), "oneX");
    assert_eq!(s.state(3).unwrap().row_text(0), "zero");
    assert_eq!(s.state(2).unwrap().row_text(1), "two");
    // missing base
    assert_eq!(s.apply(9, 10, None, b"x"), Err(ApplyError::BaseMissing(9)));
    assert!(!s.has_state(10));
    // already held target: no-op, held state kept
    s.apply(0, 3, None, b"other").unwrap();
    assert_eq!(s.state(3).unwrap().row_text(0), "zero");
    // discard keeps >= throwaway and the latest
    s.discard_below(3);
    assert_eq!(s.held(), vec![3, 4]);
    assert_eq!(s.apply(1, 5, None, b"x"), Err(ApplyError::BaseMissing(1)));
    s.discard_below(100);
    assert_eq!(s.held(), vec![4]);
    assert_eq!(s.latest().row_text(0), "oneX");
    // a diff below the latest does not move latest
    let mut s = Screen::new(5, 1);
    s.apply(0, 5, None, b"a").unwrap();
    s.apply(0, 2, None, b"b").unwrap();
    assert_eq!(s.latest_num(), 5);
}

#[test]
fn resize_applies_before_bytes() {
    let mut s = Screen::new(80, 24);
    s.apply(
        0,
        1,
        Some((100, 30)),
        b"\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[30;100HZ",
    )
    .unwrap();
    let f = s.latest();
    assert_eq!((f.cols(), f.rows()), (100, 30));
    assert_eq!(f.cell(29, 99).ch, 'Z');
    // the base is untouched
    assert_eq!(s.state(0).unwrap().cols(), 80);
    assert_eq!(
        s.apply(0, 2, Some((0, 5)), b""),
        Err(ApplyError::BadSize(0, 5))
    );
    assert_eq!(
        s.apply(0, 2, Some((5, 5000)), b""),
        Err(ApplyError::BadSize(5, 5000))
    );
}

#[test]
fn events_from_apply() {
    let mut s = Screen::new(10, 2);
    s.apply(0, 1, None, b"\x07\x1b]2;t\x07").unwrap();
    assert_eq!(
        s.take_events(),
        vec![ScreenEvent::Bell, ScreenEvent::TitleChanged("t".into())]
    );
    assert!(s.take_events().is_empty());
    // later states derived from this one do not re-emit
    s.apply(1, 2, None, b"x").unwrap();
    assert!(s.take_events().is_empty());
}

#[test]
fn repaint_via_screen() {
    let mut s = Screen::new(10, 2);
    s.apply(0, 1, None, b"hi").unwrap();
    let a = s.latest().clone();
    s.apply(1, 2, None, b"\r\nyo").unwrap();
    let mut host = Framebuffer::new(10, 2);
    host.feed(&s.repaint(None));
    assert!(host.display_eq(s.latest()));
    let mut host = Framebuffer::new(10, 2);
    host.feed(&repaint(&a, None));
    host.feed(&s.repaint(Some(&a)));
    assert!(host.display_eq(s.latest()));
}

// ---- fuzz-ish ----

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn check_invariants(f: &Framebuffer) {
    assert!(f.cursor_row() < f.rows() && f.cursor_col() < f.cols());
    for r in 0..f.rows() {
        let cells = f.row_cells(r);
        for (c, cell) in cells.iter().enumerate() {
            match cell.width {
                2 => assert!(c + 1 < cells.len() && cells[c + 1].width == 0),
                0 => assert!(c > 0 && cells[c - 1].width == 2),
                1 => {}
                w => panic!("bad width {w}"),
            }
        }
    }
}

#[test]
fn fuzz_random_bytes() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let specials: &[u8] =
        b"\x1b[]P\\;:?0123456789@ABCDEFGHIJKLMPSTXZ`bdfghlmnrsu \x07\x18\x1a\r\n\t";
    for i in 0..5000 {
        let len = rng.below(300);
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                if rng.below(3) == 0 {
                    rng.next() as u8
                } else {
                    specials[rng.below(specials.len())]
                }
            })
            .collect();
        let (c, r) = (1 + rng.below(30) as u16, 1 + rng.below(10) as u16);
        let mut f = Framebuffer::new(c, r);
        f.feed(&bytes);
        check_invariants(&f);
        // incremental feeding gives the same screen
        let mut g = Framebuffer::new(c, r);
        let cut = rng.below(bytes.len() + 1);
        g.feed(&bytes[..cut]);
        g.feed(&bytes[cut..]);
        assert!(g.display_eq(&f), "split feed differs, case {i}");
        rt(&f);
    }
}

#[test]
fn fuzz_csi_parameters() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let pchars = b"0123456789;:;;?<=> !\"$'";
    let finals: Vec<u8> = (0x40..=0x7e).collect();
    for _ in 0..5000 {
        let mut s = vec![0x1b, b'['];
        for _ in 0..rng.below(12) {
            s.push(pchars[rng.below(pchars.len())]);
        }
        if rng.below(10) != 0 {
            s.push(finals[rng.below(finals.len())]);
        }
        let mut f = Framebuffer::new(1 + rng.below(40) as u16, 1 + rng.below(12) as u16);
        f.feed("abc\x1b[2;3Hdef日本".as_bytes());
        f.feed(&s);
        f.feed(b"zz");
        check_invariants(&f);
        rt(&f);
    }
    // huge parameters
    let mut f = Framebuffer::new(80, 24);
    f.feed(b"\x1b[99999999999999999999;99999999999H\x1b[99999999999L\x1b[99999999999@\x1b[99999999999b\x1b[99999999999S\x1b[99999999999T");
    f.feed(&b"\x1b[1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1m"[..]);
    check_invariants(&f);
    // an unterminated OSC of a megabyte stays bounded and does not hang
    let mut f = Framebuffer::new(10, 2);
    f.feed(b"\x1b]2;");
    f.feed(&vec![b'a'; 1 << 20]);
    f.feed(b"\x07ok");
    assert_eq!(f.row_text(0), "ok");
}
