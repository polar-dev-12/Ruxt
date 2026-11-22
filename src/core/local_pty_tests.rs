use super::*;
use std::time::{Duration, Instant};

fn term_text(term: &Arc<Mutex<TermState>>) -> String {
    let t = term.lock();
    let content = t.term.renderable_content();
    let mut s = String::new();
    let mut last = i32::MIN;
    for c in content.display_iter {
        if c.point.line.0 != last {
            s.push('\n');
            last = c.point.line.0;
        }
        s.push(c.cell.c);
    }
    s
}

#[test]
fn local_shell_round_trip() {
    let ctx = egui::Context::default();
    let size = Size { cols: 100, rows: 30 };
    let (term, pty_rx) = TermState::new(ctx.clone(), size, 1000, false);
    let shell = LocalShell::spawn("", size, term.clone(), ctx, pty_rx).expect("spawn local shell");
    // The GUI resizes the pty on its very first frame; make sure that does not kill the child.
    shell.resize(Size { cols: 135, rows: 43 });
    std::thread::sleep(Duration::from_millis(1500));
    shell.resize(Size { cols: 120, rows: 40 });
    assert!(!term.lock().closed, "shell died after an early resize");
    shell.write(b"echo marker-12345\r");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let txt = term_text(&term);
        // the echoed command line plus the output line both contain the marker
        if txt.matches("marker-12345").count() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "no shell output:\n{txt}");
        std::thread::sleep(Duration::from_millis(100));
    }
    shell.resize(Size { cols: 80, rows: 24 });
    shell.write(b"exit\r");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !term.lock().closed {
        assert!(Instant::now() < deadline, "shell did not exit");
        std::thread::sleep(Duration::from_millis(100));
    }
    shell.shutdown();
    shell.shutdown(); // idempotent
}

#[test]
fn shutdown_kills_a_running_shell() {
    let ctx = egui::Context::default();
    let size = Size { cols: 80, rows: 24 };
    let (term, pty_rx) = TermState::new(ctx.clone(), size, 1000, false);
    let shell = LocalShell::spawn("", size, term.clone(), ctx, pty_rx).expect("spawn local shell");
    std::thread::sleep(Duration::from_millis(800));
    assert!(!term.lock().closed);
    shell.shutdown();
    // The waiter thread notices the killed child and closes the tab's terminal.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !term.lock().closed {
        assert!(Instant::now() < deadline, "shell was not terminated by shutdown()");
        std::thread::sleep(Duration::from_millis(100));
    }
    // Writes after shutdown are harmless no-ops.
    shell.write(b"echo still-alive\r");
    drop(shell);
}
