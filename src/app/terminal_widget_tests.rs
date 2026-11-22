use super::*;

#[test]
fn paste_strips_escapes_and_normalises_newlines() {
    let out = sanitize_paste("ls\x1b[31m -la\r\necho hi\n", false);
    assert_eq!(out, b"ls[31m -la\recho hi\r");
    let b = sanitize_paste("a\nb", true);
    assert!(b.starts_with(b"\x1b[200~") && b.ends_with(b"\x1b[201~"));
}

#[test]
fn key_mapping() {
    let none = Modifiers::NONE;
    assert_eq!(key_bytes(Key::Enter, none, false).unwrap(), b"\r");
    assert_eq!(key_bytes(Key::ArrowUp, none, false).unwrap(), b"\x1b[A");
    assert_eq!(key_bytes(Key::ArrowUp, none, true).unwrap(), b"\x1bOA");
    assert_eq!(key_bytes(Key::ArrowUp, Modifiers::SHIFT, false).unwrap(), b"\x1b[1;2A");
    assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false).unwrap(), vec![3]);
    assert_eq!(key_bytes(Key::D, Modifiers::ALT, false).unwrap(), vec![0x1b, b'd']);
    assert!(
        key_bytes(Key::A, none, false).is_none(),
        "plain letters come via Text events"
    );
    assert_eq!(key_bytes(Key::F5, none, false).unwrap(), b"\x1b[15~");
    assert_eq!(key_bytes(Key::Tab, Modifiers::SHIFT, false).unwrap(), b"\x1b[Z");
    // Alt+. (bash "last argument") and Ctrl+Shift+- (Ctrl+_)
    assert_eq!(key_bytes(Key::Period, Modifiers::ALT, false).unwrap(), vec![0x1b, b'.']);
    assert_eq!(
        key_bytes(Key::Minus, Modifiers::CTRL | Modifiers::SHIFT, false).unwrap(),
        vec![0x1f]
    );
    // AltGr on Windows arrives as Ctrl+Alt and must defer to the Text event.
    let ctrl_alt = Modifiers::CTRL | Modifiers::ALT;
    if cfg!(windows) {
        assert!(key_bytes(Key::Q, ctrl_alt, false).is_none());
    } else {
        assert_eq!(key_bytes(Key::Q, ctrl_alt, false).unwrap(), vec![0x1b, 0x11]);
    }
    assert!(key_bytes(Key::Num2, ctrl_alt, false).is_none());
}

#[test]
fn mouse_reports_follow_the_requested_encoding() {
    let none = Modifiers::NONE;
    assert_eq!(mouse_report(true, 0, 4, 9, false, none), b"\x1b[<0;5;10M");
    assert_eq!(mouse_report(true, 0, 4, 9, true, none), b"\x1b[<0;5;10m");
    assert_eq!(mouse_report(true, 64, 0, 0, false, Modifiers::SHIFT), b"\x1b[<68;1;1M");
    // Legacy X10 encoding: ESC [ M, then button+32 and 1-based coordinates+32.
    assert_eq!(
        mouse_report(false, 0, 4, 9, false, none),
        vec![0x1b, b'[', b'M', 32, 37, 42]
    );
    assert_eq!(
        mouse_report(false, 0, 4, 9, true, none),
        vec![0x1b, b'[', b'M', 35, 37, 42]
    );
    // Coordinates clamp instead of overflowing a byte.
    assert_eq!(mouse_report(false, 0, 500, 500, false, none)[4], 255);
}

#[test]
fn terminal_search_and_url_detection() {
    let ctx = egui::Context::default();
    let (term, _rx) = TermState::new(ctx, Size { cols: 80, rows: 10 }, 500, false);
    {
        let mut t = term.lock();
        t.advance(b"first line with Needle\r\n");
        t.advance(b"visit https://example.com/docs/page?x=1).\r\n");
        t.advance(b"another NEEDLE here\r\n");
        let matches = t.search("needle");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].col_start, 16);
        assert_eq!(matches[0].col_end, 22);
        assert_eq!(matches[1].col_start, 8);
        assert_eq!(matches[1].col_end, 14);

        let url = url_at_row(&t, 1, 10).expect("url should be detected");
        assert_eq!(url.2, "https://example.com/docs/page?x=1");
        assert!(url_at_row(&t, 1, 2).is_none());
    }
}

#[test]
fn cwd_extraction_from_osc7_and_titles() {
    use crate::core::term::{extract_cwd_from_osc7, extract_cwd_from_title};
    assert_eq!(
        extract_cwd_from_osc7(b"\x1b]7;file://myhost/var/log/nginx\x07").as_deref(),
        Some("/var/log/nginx")
    );
    assert_eq!(
        extract_cwd_from_osc7(b"\x1b]7;file://localhost/home/user/my%20folder\x1b\\").as_deref(),
        Some("/home/user/my folder")
    );
    assert_eq!(
        extract_cwd_from_title("ubuntu@prod-web: /etc/systemd/system").as_deref(),
        Some("/etc/systemd/system")
    );
    assert_eq!(
        extract_cwd_from_title("ubuntu@prod-web:~/projects").as_deref(),
        Some("~/projects")
    );
}

#[test]
fn osc7_survives_packet_splits_and_titles_drop_bidi_controls() {
    use crate::core::term::sanitize_title;
    let ctx = egui::Context::default();
    let (term, _rx) = TermState::new(ctx, Size { cols: 80, rows: 10 }, 500, false);
    let mut t = term.lock();
    // The sequence arrives in three chunks, split inside the prefix and inside the path.
    t.advance(b"prompt$ \x1b]");
    t.advance(b"7;file://box/var/li");
    assert!(t.take_cwd().is_none(), "incomplete sequence must not yield a cwd");
    t.advance(b"b/app\x07 more");
    assert_eq!(t.take_cwd().as_deref(), Some("/var/lib/app"));
    // Garbage that never terminates is dropped once it exceeds the carry limit.
    t.advance(b"\x1b]7;");
    t.advance(&vec![b'x'; 5000]);
    assert!(t.take_cwd().is_none());
    t.advance(b"\x1b]7;file://h/ok\x1b\\");
    assert_eq!(t.take_cwd().as_deref(), Some("/ok"));
    assert!(t.generation > 0);

    assert_eq!(sanitize_title("evil\u{202E}txt.exe"), "eviltxt.exe");
    assert_eq!(sanitize_title("a\u{200B}b\u{2066}c\u{FEFF}"), "abc");
    assert_eq!(sanitize_title("plain 한글"), "plain 한글");
}

#[test]
fn input_line_is_read_back_from_the_grid() {
    use crate::core::term::{Size, TermState};
    let ctx = egui::Context::default();
    let (term, _rx) = TermState::new(ctx, Size { cols: 20, rows: 4 }, 50, false);
    let mut t = term.lock();
    t.advance(b"user@host:~$ ");
    let (row, col) = t.cursor_abs();
    assert_eq!((row, col), (0, 13));
    t.advance(b"ls -la");
    assert_eq!(t.line_text_from(row, col, true).as_deref(), Some("ls -la"));
    assert_eq!(t.line_text_from(row, col, false).unwrap().trim_end(), "ls -la");
    // A long line soft-wraps onto the next row and is still read as one line.
    t.advance(b" /var/log/nginx");
    assert_eq!(
        t.line_text_from(row, col, true).as_deref(),
        Some("ls -la /var/log/nginx")
    );
    // The cursor moving left keeps the full row when the caller asks for it.
    t.advance(b"\x1b[3D");
    assert_eq!(t.line_text_from(row, col, true).as_deref(), Some("ls -la /var/log/ng"));
    assert_eq!(
        t.line_text_from(row, col, false).unwrap().trim_end(),
        "ls -la /var/log/nginx"
    );
    // A new prompt on a fresh row ends the tracked line.
    t.advance(b"\r\nuser@host:~$ ");
    assert!(t.line_text_from(row, col, true).is_none());
    // Rows scrolled into history keep their absolute index; the cursor is far below.
    let (row2, col2) = t.cursor_abs();
    assert_eq!(row2, 2);
    for _ in 0..10 {
        t.advance(b"\r\nline");
    }
    let (row3, _) = t.cursor_abs();
    assert_eq!(row3, 12);
    assert!(t.line_text_from(row2, col2, true).is_none());
    // Password prompts echo nothing: the read-back is empty, so nothing gets recorded.
    t.advance(b"\r\nPassword: ");
    let (prow, pcol) = t.cursor_abs();
    assert_eq!(t.line_text_from(prow, pcol, false).unwrap().trim(), "");
}

#[test]
fn generated_passphrases_are_long_random_and_typeable() {
    use crate::app::dialogs::random_passphrase;
    let a = random_passphrase();
    let b = random_passphrase();
    assert_ne!(a, b);
    assert_eq!(a.len(), 27, "{a}");
    assert_eq!(a.matches('-').count(), 3);
    assert!(a.chars().all(|c| c == '-' || c.is_ascii_alphanumeric()));
    // No look-alike characters (0/O, 1/l/I) that are painful to type from a note.
    assert!(!a.chars().any(|c| matches!(c, '0' | 'O' | '1' | 'l' | 'I')), "{a}");
}
