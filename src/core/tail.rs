//! Background `tail` command parser and streaming runners (SSH exec channel + local file tailer).
//!
//! When the user types a `tail` command such as `tail -f -n 100 test.log` in the terminal,
//! Ruxt intercepts it, keeps the interactive shell prompt untouched, and streams the log
//! output into a dedicated TailViewer window.

use super::ssh::{ClientHandler, UiEvent};
use super::term::sanitize_title;
use super::TabId;
use anyhow::{Context, Result};
use russh::client::Handle;
use russh::ChannelMsg;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

/// Unique identifier for a running or closed TailViewer instance.
pub type TailId = u64;

/// Default number of initial lines when `-n` is not specified.
pub const DEFAULT_TAIL_LINES: usize = 100;

/// Maximum number of lines kept in a TailViewer buffer before old lines are dropped.
pub const MAX_BUFFER_LINES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailSpec {
    /// Original command string as typed by the user (e.g. `tail -f -n 100 test.log`).
    pub raw_cmd: String,
    /// Whether `-f` / `-F` / `--follow` was requested.
    pub follow: bool,
    /// Number of initial lines to show (`-n N` or `-N`).
    pub lines: usize,
    /// Target file paths extracted from the command.
    pub files: Vec<String>,
    /// Working directory of the terminal when the command was launched.
    pub cwd: Option<String>,
    /// Whether the command is prefixed with `sudo`.
    pub uses_sudo: bool,
}

impl TailSpec {
    /// Short title suitable for a window header (e.g. `test.log` or `a.log, b.log`).
    pub fn display_title(&self) -> String {
        if self.files.is_empty() {
            return sanitize_title(&self.raw_cmd);
        }
        let names: Vec<&str> = self
            .files
            .iter()
            .map(|f| {
                f.rsplit(['/', '\\'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(f.as_str())
            })
            .collect();
        sanitize_title(&names.join(", "))
    }

    /// Construct a `TailSpec` directly for a single file path (used by SFTP "TailViewer").
    pub fn for_file(path: impl Into<String>, cwd: Option<String>) -> Self {
        let p = path.into();
        let quoted = shell_quote(&p);
        Self {
            raw_cmd: format!("tail -f -n {DEFAULT_TAIL_LINES} {quoted}"),
            follow: true,
            lines: DEFAULT_TAIL_LINES,
            files: vec![p],
            cwd,
            uses_sudo: false,
        }
    }
}

/// Control messages sent from the UI to a running SSH `tail` channel.
pub enum TailControl {
    /// Send raw bytes (e.g. a `sudo` password followed by `\n`) to the remote PTY.
    Input(Zeroizing<Vec<u8>>),
    /// Stop the remote command cleanly (Ctrl+C + close).
    Stop,
}

#[derive(Debug, Clone)]
pub enum TailEvent {
    Data { id: TailId, bytes: Vec<u8> },
    Closed { id: TailId, reason: String },
}

/// Parse a command line typed at the terminal prompt. Returns `Some(TailSpec)` if it is a
/// standalone `tail` invocation targeting one or more files without shell pipelines or
/// redirections.
pub fn parse_tail_cmd(line: &str) -> Option<TailSpec> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if has_unquoted_shell_operators(trimmed) {
        return None;
    }
    let tokens = split_shell_words(trimmed)?;
    if tokens.is_empty() {
        return None;
    }

    // Skip leading VAR=val environment assignments if any.
    let mut idx = 0;
    while idx < tokens.len() && is_env_assignment(&tokens[idx]) {
        idx += 1;
    }
    if idx >= tokens.len() {
        return None;
    }

    // Optional `sudo` wrapper (e.g. `sudo tail -f /var/log/syslog`, `sudo -u root tail -f ...`).
    let mut uses_sudo = false;
    if matches!(tokens[idx].as_str(), "sudo" | "/usr/bin/sudo" | "/bin/sudo") {
        uses_sudo = true;
        idx = skip_sudo_options(&tokens, idx + 1)?;
        while idx < tokens.len() && is_env_assignment(&tokens[idx]) {
            idx += 1;
        }
        if idx >= tokens.len() {
            return None;
        }
    }

    let prog = &tokens[idx];
    if prog != "tail" && prog != "/usr/bin/tail" && prog != "/bin/tail" {
        return None;
    }
    idx += 1;

    let mut follow = false;
    let mut lines: Option<usize> = None;
    let mut files: Vec<String> = Vec::new();
    let mut double_dash = false;

    while idx < tokens.len() {
        let arg = &tokens[idx];
        if double_dash {
            files.push(arg.clone());
            idx += 1;
            continue;
        }
        if arg == "--" {
            double_dash = true;
            idx += 1;
            continue;
        }
        if arg == "--help" || arg == "--version" {
            return None;
        }
        if arg == "--follow" || arg.starts_with("--follow=") || arg == "--retry" {
            follow = true;
            idx += 1;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--lines=") {
            lines = parse_line_count(val);
            idx += 1;
            continue;
        }
        if arg == "--lines" {
            idx += 1;
            if idx < tokens.len() {
                lines = parse_line_count(&tokens[idx]);
                idx += 1;
            }
            continue;
        }
        if arg.starts_with("--sleep-interval=")
            || arg.starts_with("--pid=")
            || arg.starts_with("--bytes=")
            || arg.starts_with("--max-unchanged-stats=")
        {
            idx += 1;
            continue;
        }
        if arg == "--sleep-interval" || arg == "--pid" || arg == "--bytes" || arg == "--max-unchanged-stats" {
            idx += 2;
            continue;
        }
        if arg.starts_with("--") {
            idx += 1;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            let rest = &arg[1..];
            // Legacy numeric flag like `tail -100f` or `tail -100`
            let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            if digits_end > 0 {
                if let Ok(n) = rest[..digits_end].parse::<usize>() {
                    lines = Some(n);
                }
                let tail_flags = &rest[digits_end..];
                for ch in tail_flags.chars() {
                    if ch == 'f' || ch == 'F' {
                        follow = true;
                    }
                }
                idx += 1;
                continue;
            }

            // Combined short flags like `-f`, `-n 100`, `-n100`, `-fn 100`, `-fn100`
            let mut chars = rest.chars().peekable();
            let mut consumed_next = false;
            while let Some(ch) = chars.next() {
                match ch {
                    'f' | 'F' => follow = true,
                    'n' => {
                        let remaining: String = chars.collect();
                        if !remaining.is_empty() {
                            // e.g. `-n100` or `-n100f`
                            let d_end = remaining
                                .find(|c: char| !c.is_ascii_digit() && c != '+')
                                .unwrap_or(remaining.len());
                            if d_end > 0 {
                                lines = parse_line_count(&remaining[..d_end]);
                                for trailing in remaining[d_end..].chars() {
                                    if trailing == 'f' || trailing == 'F' {
                                        follow = true;
                                    }
                                }
                            }
                        } else if idx + 1 < tokens.len() {
                            lines = parse_line_count(&tokens[idx + 1]);
                            consumed_next = true;
                        }
                        break;
                    }
                    'c' | 's' | 'z' | 'q' | 'v' => {
                        if matches!(ch, 'c' | 's') {
                            let remaining: String = chars.collect();
                            if remaining.is_empty() && idx + 1 < tokens.len() {
                                consumed_next = true;
                            }
                            break;
                        }
                    }
                    _ => {}
                }
            }
            idx += if consumed_next { 2 } else { 1 };
            continue;
        }

        // Positional file argument
        files.push(arg.clone());
        idx += 1;
    }

    if files.is_empty() {
        return None;
    }

    let default_lines = if follow { DEFAULT_TAIL_LINES } else { 10 };
    Some(TailSpec {
        raw_cmd: trimmed.to_string(),
        follow,
        lines: lines.unwrap_or(default_lines),
        files,
        cwd: None,
        uses_sudo,
    })
}

/// Advance past `sudo` flags to the wrapped command token. Returns `None` for non-command or
/// unsupported `sudo` modes (interactive shell, edit, list, background, help/version, etc.).
fn skip_sudo_options(tokens: &[String], mut idx: usize) -> Option<usize> {
    while idx < tokens.len() {
        let arg = tokens[idx].as_str();
        if arg == "--" {
            return Some(idx + 1);
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        if arg.starts_with("--") {
            let name = arg.split_once('=').map_or(arg, |(k, _)| k);
            match name {
                "--login" | "--shell" | "--list" | "--validate" | "--reset-timestamp" | "--remove-timestamp"
                | "--version" | "--edit" | "--background" | "--help" => return None,
                "--user" | "--group" | "--close-from" | "--chdir" | "--host" | "--prompt" | "--role" | "--type"
                | "--command-timeout" | "--other-user" | "--chroot" => {
                    idx += if arg.contains('=') { 1 } else { 2 };
                }
                _ => {
                    idx += 1;
                }
            }
            continue;
        }

        // Short flags like `-n`, `-E`, `-u root`, `-uroot`, `-nE`.
        let mut chars = arg[1..].chars();
        let mut consumed_next = false;
        while let Some(ch) = chars.next() {
            match ch {
                'i' | 's' | 'l' | 'v' | 'k' | 'K' | 'V' | 'e' | 'b' => return None,
                'u' | 'g' | 'C' | 'D' | 'h' | 'p' | 'r' | 't' | 'T' | 'U' | 'R' => {
                    if chars.next().is_none() {
                        consumed_next = true;
                    }
                    break;
                }
                _ => {}
            }
        }
        idx += if consumed_next { 2 } else { 1 };
    }
    if idx < tokens.len() {
        Some(idx)
    } else {
        None
    }
}

fn parse_line_count(s: &str) -> Option<usize> {
    let s = s.strip_prefix('+').unwrap_or(s);
    s.parse::<usize>().ok()
}

fn is_env_assignment(s: &str) -> bool {
    let Some((k, _)) = s.split_once('=') else {
        return false;
    };
    !k.is_empty()
        && k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Check if the command line contains unquoted shell operators (`|`, `>`, `<`, `;`, `&`, `` ` ``, `$()`).
fn has_unquoted_shell_operators(s: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if c == '\\' && !in_single {
            escaped = true;
            i += 1;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if !in_single && !in_double {
            if matches!(c, '|' | '>' | '<' | ';' | '&' | '`') {
                return true;
            }
            if c == '$' && i + 1 < chars.len() && chars[i + 1] == '(' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Split a shell command line into words, respecting single and double quotes.
fn split_shell_words(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut has_token = false;

    for c in s.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            has_token = true;
            continue;
        }
        if c == '\\' && !in_single {
            escaped = true;
            has_token = true;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            has_token = true;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            has_token = true;
            continue;
        }
        if c.is_whitespace() && !in_single && !in_double {
            if has_token {
                words.push(std::mem::take(&mut current));
                has_token = false;
            }
            continue;
        }
        current.push(c);
        has_token = true;
    }
    if in_single || in_double || escaped {
        return None;
    }
    if has_token {
        words.push(current);
    }
    Some(words)
}

/// Quote a single POSIX shell argument safely.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+'))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build the remote shell command string, prepending a `cd` into `spec.cwd` if known.
pub fn build_remote_command(spec: &TailSpec) -> String {
    let mut prefix = String::new();
    if let Some(cwd) = spec.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if cwd == "~" {
            prefix = "cd ~ 2>/dev/null; ".to_string();
        } else if let Some(rest) = cwd.strip_prefix("~/") {
            let escaped = rest
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('`', "\\`");
            prefix = format!("cd \"$HOME/{escaped}\" 2>/dev/null; ");
        } else {
            prefix = format!("cd {} 2>/dev/null; ", shell_quote(cwd));
        }
    }
    format!("{prefix}exec {}", spec.raw_cmd)
}

/// Run a remote `tail` command over its own SSH session channel and stream its output to the UI.
pub async fn run_ssh_tail(
    handle: Arc<Handle<ClientHandler>>,
    tab: TabId,
    id: TailId,
    spec: TailSpec,
    ui: mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
    mut ctrl_rx: mpsc::UnboundedReceiver<TailControl>,
) {
    let send_ev = |ev: TailEvent| {
        let _ = ui.send(UiEvent::Tail { tab, ev });
        ctx.request_repaint();
    };

    let mut ch = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) => {
            send_ev(TailEvent::Closed {
                id,
                reason: format!("channel open failed: {e}"),
            });
            return;
        }
    };

    // Allocate a dumb PTY so that closing the channel immediately sends SIGHUP to `tail -f`
    // on the remote server (preventing orphan `tail -f` processes when logs are quiet).
    let _ = ch.request_pty(false, "dumb", 160, 40, 0, 0, &[]).await;

    let cmd = build_remote_command(&spec);
    if let Err(e) = ch.exec(true, cmd.as_str()).await {
        let _ = ch.close().await;
        send_ev(TailEvent::Closed {
            id,
            reason: format!("exec failed: {e}"),
        });
        return;
    }

    let reason = loop {
        tokio::select! {
            ctrl = ctrl_rx.recv() => {
                match ctrl {
                    Some(TailControl::Input(bytes)) => {
                        let _ = ch.data(&bytes[..]).await;
                    }
                    Some(TailControl::Stop) | None => {
                        // Send Ctrl+C (SIGINT) then close the PTY channel (SIGHUP).
                        let _ = ch.data(&b"\x03"[..]).await;
                        let _ = ch.eof().await;
                        let _ = ch.close().await;
                        break "stopped".to_string();
                    }
                }
            }
            msg = ch.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        send_ev(TailEvent::Data {
                            id,
                            bytes: data.to_vec(),
                        });
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        if exit_status == 0 {
                            break "completed".to_string();
                        } else {
                            break format!("exit status {exit_status}");
                        }
                    }
                    Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                        break format!("signal {signal_name:?}");
                    }
                    Some(ChannelMsg::Close) | None => {
                        break "closed".to_string();
                    }
                    _ => {}
                }
            }
        }
    };

    let _ = ch.close().await;
    send_ev(TailEvent::Closed { id, reason });
}

/// Handle to stop a running local file tailer thread.
pub struct LocalTailHandle {
    stop: Arc<AtomicBool>,
}

impl LocalTailHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for LocalTailHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn a background thread that tails local file(s) natively in Rust (works on Windows and Unix
/// without requiring an external `tail` binary).
pub fn spawn_local_tail(
    tab: TabId,
    id: TailId,
    spec: TailSpec,
    ui: mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
) -> LocalTailHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let _ = std::thread::Builder::new()
        .name(format!("local-tail-{id}"))
        .spawn(move || {
            run_local_tail_blocking(tab, id, spec, ui, ctx, stop2);
        });

    LocalTailHandle { stop }
}

fn resolve_local_path(raw: &str, cwd: Option<&str>) -> PathBuf {
    if raw == "~" {
        if let Some(h) = super::hostkeys::home_dir() {
            return h;
        }
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(h) = super::hostkeys::home_dir() {
            return h.join(rest);
        }
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(c) = cwd.filter(|s| !s.is_empty()) {
        let base = if c == "~" {
            super::hostkeys::home_dir().unwrap_or_else(|| PathBuf::from("."))
        } else if let Some(rest) = c.strip_prefix("~/").or_else(|| c.strip_prefix("~\\")) {
            super::hostkeys::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(c))
        } else {
            PathBuf::from(c)
        };
        return base.join(p);
    }
    if let Some(h) = super::hostkeys::home_dir() {
        return h.join(p);
    }
    p.to_path_buf()
}

fn read_last_n_lines(file: &mut File, n: usize) -> Result<(Vec<u8>, u64)> {
    let len = file.metadata().context("metadata")?.len();
    if n == 0 || len == 0 {
        file.seek(SeekFrom::End(0))?;
        return Ok((Vec::new(), len));
    }
    // Read backwards in chunks to find `n` newlines.
    let chunk_size: u64 = 64 * 1024;
    let mut pos = len;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut newlines = 0usize;

    while pos > 0 {
        let read_len = pos.min(chunk_size);
        pos -= read_len;
        file.seek(SeekFrom::Start(pos))?;
        let mut buf = vec![0u8; read_len as usize];
        file.read_exact(&mut buf)?;
        for (i, &b) in buf.iter().enumerate().rev() {
            if b == b'\n' {
                // Ignore a trailing newline at the very end of the file.
                if pos + (i as u64) + 1 == len {
                    continue;
                }
                newlines += 1;
                if newlines >= n {
                    let start_in_buf = i + 1;
                    let mut out = Vec::from(&buf[start_in_buf..]);
                    for prev in chunks.into_iter().rev() {
                        out.extend_from_slice(&prev);
                    }
                    file.seek(SeekFrom::Start(len))?;
                    return Ok((out, len));
                }
            }
        }
        chunks.push(buf);
        // Cap initial read at 4 MiB so huge single-line files never stall.
        if len - pos >= 4 * 1024 * 1024 {
            break;
        }
    }

    let mut out = Vec::new();
    for prev in chunks.into_iter().rev() {
        out.extend_from_slice(&prev);
    }
    file.seek(SeekFrom::Start(len))?;
    Ok((out, len))
}

fn run_local_tail_blocking(
    tab: TabId,
    id: TailId,
    spec: TailSpec,
    ui: mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
) {
    let send_ev = |ev: TailEvent| {
        let _ = ui.send(UiEvent::Tail { tab, ev });
        ctx.request_repaint();
    };

    let multiple = spec.files.len() > 1;
    let mut tracked: Vec<(PathBuf, String, Option<File>, u64)> = Vec::new();

    for raw in &spec.files {
        let path = resolve_local_path(raw, spec.cwd.as_deref());
        match File::open(&path) {
            Ok(mut f) => {
                if multiple {
                    let header = format!("==> {} <==\n", path.display());
                    send_ev(TailEvent::Data {
                        id,
                        bytes: header.into_bytes(),
                    });
                }
                match read_last_n_lines(&mut f, spec.lines) {
                    Ok((initial, offset)) => {
                        if !initial.is_empty() {
                            send_ev(TailEvent::Data { id, bytes: initial });
                        }
                        tracked.push((path, raw.clone(), Some(f), offset));
                    }
                    Err(e) => {
                        let msg = format!("tail: {}: {e}\n", path.display());
                        send_ev(TailEvent::Data {
                            id,
                            bytes: msg.into_bytes(),
                        });
                        tracked.push((path, raw.clone(), None, 0));
                    }
                }
            }
            Err(e) => {
                let msg = format!("tail: cannot open '{}' for reading: {e}\n", path.display());
                send_ev(TailEvent::Data {
                    id,
                    bytes: msg.into_bytes(),
                });
                if spec.follow {
                    tracked.push((path, raw.clone(), None, 0));
                }
            }
        }
    }

    if !spec.follow {
        send_ev(TailEvent::Closed {
            id,
            reason: "completed".into(),
        });
        return;
    }

    let mut buf = vec![0u8; 32 * 1024];
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(150));
        if stop.load(Ordering::Relaxed) {
            break;
        }
        for (path, _label, file_opt, offset) in &mut tracked {
            if file_opt.is_none() {
                if let Ok(f) = File::open(&*path) {
                    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                    *offset = 0;
                    *file_opt = Some(f);
                    if len > 0 {
                        // Will be read below
                    }
                }
            }
            let Some(f) = file_opt.as_mut() else {
                continue;
            };
            let len = match f.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if len < *offset {
                // File was truncated / rotated.
                *offset = 0;
            }
            if len > *offset && f.seek(SeekFrom::Start(*offset)).is_ok() {
                while *offset < len && !stop.load(Ordering::Relaxed) {
                    match f.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            *offset += n as u64;
                            send_ev(TailEvent::Data {
                                id,
                                bytes: buf[..n].to_vec(),
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    }

    send_ev(TailEvent::Closed {
        id,
        reason: "stopped".into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_tail_commands() {
        let s = parse_tail_cmd("tail -f -n 100 test.log").unwrap();
        assert!(s.follow);
        assert_eq!(s.lines, 100);
        assert_eq!(s.files, vec!["test.log"]);

        let s2 = parse_tail_cmd("tail -100f /var/log/syslog").unwrap();
        assert!(s2.follow);
        assert_eq!(s2.lines, 100);
        assert_eq!(s2.files, vec!["/var/log/syslog"]);

        let s3 = parse_tail_cmd("tail -fn250 'my app.log'").unwrap();
        assert!(s3.follow);
        assert_eq!(s3.lines, 250);
        assert_eq!(s3.files, vec!["my app.log"]);

        let s4 = parse_tail_cmd("tail -n 50 app.log").unwrap();
        assert!(!s4.follow);
        assert_eq!(s4.lines, 50);
        assert_eq!(s4.files, vec!["app.log"]);
        assert!(!s4.uses_sudo);
    }

    #[test]
    fn parses_sudo_tail_commands() {
        let s1 = parse_tail_cmd("sudo tail -f /var/log/syslog").unwrap();
        assert!(s1.uses_sudo);
        assert!(s1.follow);
        assert_eq!(s1.files, vec!["/var/log/syslog"]);
        assert_eq!(s1.raw_cmd, "sudo tail -f /var/log/syslog");

        let s2 = parse_tail_cmd("sudo -u root -n tail -f -n 200 /var/log/auth.log").unwrap();
        assert!(s2.uses_sudo);
        assert!(s2.follow);
        assert_eq!(s2.lines, 200);
        assert_eq!(s2.files, vec!["/var/log/auth.log"]);

        let s3 = parse_tail_cmd("sudo --user=www-data -- tail -100f app.log").unwrap();
        assert!(s3.uses_sudo);
        assert!(s3.follow);
        assert_eq!(s3.lines, 100);
        assert_eq!(s3.files, vec!["app.log"]);

        let s4 = parse_tail_cmd("/usr/bin/sudo -E FOO=1 /usr/bin/tail -f error.log").unwrap();
        assert!(s4.uses_sudo);
        assert!(s4.follow);
        assert_eq!(s4.files, vec!["error.log"]);

        // Non-tail or unsupported sudo modes must be ignored.
        assert!(parse_tail_cmd("sudo -i").is_none());
        assert!(parse_tail_cmd("sudo -s").is_none());
        assert!(parse_tail_cmd("sudo -b tail -f /var/log/syslog").is_none());
        assert!(parse_tail_cmd("sudo -l tail").is_none());
        assert!(parse_tail_cmd("sudo tail -f /var/log/syslog | grep err").is_none());
    }

    #[test]
    fn ignores_pipelines_and_non_tail_commands() {
        assert!(parse_tail_cmd("tail -f test.log | grep ERROR").is_none());
        assert!(parse_tail_cmd("tail -n 100 test.log > out.txt").is_none());
        assert!(parse_tail_cmd("tail -f").is_none());
        assert!(parse_tail_cmd("tail --help").is_none());
        assert!(parse_tail_cmd("ls -la").is_none());
    }

    #[test]
    fn builds_remote_command_with_cwd() {
        let mut spec = parse_tail_cmd("tail -f -n 100 test.log").unwrap();
        spec.cwd = Some("/var/log/nginx".into());
        assert_eq!(
            build_remote_command(&spec),
            "cd /var/log/nginx 2>/dev/null; exec tail -f -n 100 test.log"
        );

        spec.cwd = Some("~/myapp/logs".into());
        assert_eq!(
            build_remote_command(&spec),
            "cd \"$HOME/myapp/logs\" 2>/dev/null; exec tail -f -n 100 test.log"
        );
    }
}
