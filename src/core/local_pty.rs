//! Local shell tab backed by a real pty (ConPTY on Windows).
//!
//! Lifecycle: `spawn` starts the child plus three helper threads (pty reader, child waiter,
//! pty writer). `shutdown` kills the child and closes the pty master, which unblocks the reader
//! and lets every thread exit; it is called when the tab closes and when the app exits.

use super::term::{Size, TermState};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct LocalShell {
    /// Bytes for the child's stdin; a dedicated thread performs the (possibly blocking) write
    /// so the UI thread never stalls on a child that stopped reading.
    input_tx: std::sync::mpsc::Sender<Vec<u8>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
}

impl LocalShell {
    pub fn spawn(
        program: &str,
        size: Size,
        term: Arc<Mutex<TermState>>,
        ctx: egui::Context,
        mut pty_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    ) -> Result<Arc<Self>> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;
        let prog = if program.trim().is_empty() {
            default_shell()
        } else {
            program.trim().to_string()
        };
        let mut cmd = CommandBuilder::new(&prog);
        cmd.env("TERM", "xterm-256color");
        if let Some(home) = super::hostkeys::home_dir() {
            cmd.cwd(home);
        }
        let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("spawn {prog}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("pty reader")?;
        let mut writer = pair.master.take_writer().context("pty writer")?;
        let killer = child.clone_killer();
        let (input_tx, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let shell = Arc::new(Self {
            input_tx: input_tx.clone(),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(killer),
        });

        // Reader thread: pty → emulator. (On ConPTY the read only returns EOF once the
        // master is dropped, so child exit is detected by a separate waiter thread.)
        {
            let term = term.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("local-pty-reader".into())
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut t = term.lock();
                                if t.closed {
                                    break;
                                }
                                t.advance(&buf[..n]);
                                drop(t);
                                ctx.request_repaint();
                            }
                        }
                    }
                })?;
        }
        // Waiter thread: child exit → mark the tab closed.
        {
            let term = term.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("local-pty-waiter".into())
                .spawn(move || {
                    let started = std::time::Instant::now();
                    let status = child.wait().ok();
                    super::audit::log(
                        "local.shell.exit",
                        serde_json::json!({
                            "code": status.as_ref().map(|s| s.exit_code()),
                            "after_ms": started.elapsed().as_millis() as u64,
                        }),
                    );
                    // Give the reader a moment to drain the final output.
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let mut t = term.lock();
                    if t.closed {
                        return;
                    }
                    t.closed = true;
                    let msg = match status {
                        Some(s) => format!("process exited with code {}", s.exit_code()),
                        None => "process exited".to_string(),
                    };
                    t.exit_message = Some(msg.clone());
                    t.print_local(&msg);
                    ctx.request_repaint();
                })?;
        }
        // Writer thread: everything destined for the child's stdin goes through one channel.
        std::thread::Builder::new()
            .name("local-pty-writer".into())
            .spawn(move || {
                while let Ok(bytes) = input_rx.recv() {
                    if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                        break;
                    }
                }
            })?;
        // Emulator replies (cursor reports etc.) → pty.
        std::thread::Builder::new()
            .name("local-pty-replies".into())
            .spawn(move || {
                while let Some(bytes) = pty_rx.blocking_recv() {
                    if input_tx.send(bytes).is_err() {
                        break;
                    }
                }
            })?;
        Ok(shell)
    }

    /// Queue bytes for the child's stdin (never blocks the caller).
    pub fn write(&self, bytes: &[u8]) {
        let _ = self.input_tx.send(bytes.to_vec());
    }

    pub fn resize(&self, size: Size) {
        if let Some(m) = self.master.lock().as_ref() {
            let _ = m.resize(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Kill the child and close the pty. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.killer.lock().kill();
        // Dropping the master closes the pseudo console, which unblocks the reader thread.
        drop(self.master.lock().take());
    }
}

impl Drop for LocalShell {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        for cand in ["pwsh.exe", "powershell.exe"] {
            if which(cand) {
                return cand.to_string();
            }
        }
        "cmd.exe".to_string()
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

#[cfg(windows)]
fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).exists()))
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "local_pty_tests.rs"]
mod tests;
