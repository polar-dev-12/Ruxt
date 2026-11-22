//! SFTP subsystem driver with recursive, progress-reporting transfers.
//!
//! Safety properties enforced here:
//! * Remote-supplied names never become local paths unless `safe_component` accepts them
//!   (no separators, no `..`, no drive/stream prefixes, no reserved device names).
//! * Transfers are atomic from the reader's point of view: downloads land in a `.ruxt-part`
//!   file and uploads in a dotted temporary name, both renamed into place only when complete.
//! * Symbolic links are never followed when deleting or recursing.
//! * "Edit remote" refuses to hand executable file types to the OS "open" verb.

use super::ssh::{ClientHandler, UiEvent};
use super::TabId;
use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use russh::client::Handle;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Semaphore};

/// Parallel transfer limit per session (each transfer holds a 256 KiB buffer and a remote handle).
const MAX_CONCURRENT_TRANSFERS: usize = 4;
/// Recursion limit for directory transfers (a hostile server can feed an endless tree).
const MAX_DEPTH: usize = 64;
/// Largest file the "edit remote" feature will pull into a local temp file.
const MAX_EDIT_BYTES: u64 = 64 * 1024 * 1024;
/// Largest file the built-in text editor loads into memory.
pub const MAX_TEXT_EDIT_BYTES: u64 = 8 * 1024 * 1024;
/// Concurrent `stat` calls while resolving symlinks in a listing.
const LIST_STAT_CONCURRENCY: usize = 16;
/// A folder transfer counts its files first so the progress is "n of m"; the count stops
/// here (the bar then shows bytes of the files counted so far).
const MAX_COUNT_FILES: u32 = 50_000;

#[derive(Debug, Clone)]
pub struct SftpEntry {
    /// Display name: control and bidi characters stripped, length-capped. Never use it as a path.
    pub name: String,
    /// The exact remote name; validated with `safe_component` before it becomes a local name.
    pub raw_name: String,
    pub path: String,
    /// Directory (after following a symlink).
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Regular file (after following a symlink). Pipes, sockets and devices are skipped.
    pub is_regular: bool,
    pub size: u64,
    pub mtime: Option<u32>,
    pub permissions: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct DeleteItem {
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// What a progress entry in the transfer list is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Upload,
    Download,
    /// Server-side copy (streamed through the client; SFTP has no copy primitive).
    Copy,
}

#[derive(Debug)]
pub enum SftpCmd {
    List(String),
    /// List only the sub-directories of `path` for the folder tree; does not change the
    /// current directory (the reply is `SftpEvent::Subdirs`).
    Peek(String),
    Home,
    Download {
        remote: String,
        local: PathBuf,
        is_dir: bool,
    },
    /// `overwrite = false` asks first when a remote file of the same name exists.
    Upload {
        local: PathBuf,
        remote_dir: String,
        overwrite: bool,
    },
    Mkdir(String),
    DeleteBatch(Vec<DeleteItem>),
    Rename {
        from: String,
        to: String,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    /// Copy a remote file or directory tree to another remote path.
    Copy {
        from: String,
        to: String,
        is_dir: bool,
    },
    /// Rename several items (cut & paste / drag-move); `(from, to)` pairs.
    Move(Vec<(String, String)>),
    CancelTransfer(u64),
    /// `editor` is the user-configured external editor command ("" = OS default handler).
    EditRemote {
        remote: String,
        editor: String,
    },
    /// Load a remote file for the built-in text editor (reply: `SftpEvent::TextLoaded`).
    ReadText(String),
    /// Save the built-in editor's buffer (reply: `SftpEvent::TextSaved`). `expected_mtime` is
    /// the server mtime seen when the file was loaded / last saved; a different one means
    /// someone else changed the file, and the save is refused unless `force` is set.
    WriteText {
        remote: String,
        data: Vec<u8>,
        expected_mtime: Option<u32>,
        force: bool,
    },
    /// Answer the "upload this edit?" prompt. `upload: None` drops a pending question so the
    /// watcher asks again.
    EditSaveAnswer {
        remote: String,
        upload: Option<bool>,
    },
}

#[derive(Debug, Clone)]
pub enum SftpEvent {
    /// The remote home directory (sent once after the subsystem opens).
    Home(String),
    Listing {
        path: String,
        entries: Vec<SftpEntry>,
    },
    /// Sub-directories of `path` (reply to `SftpCmd::Peek`); the current directory is unchanged.
    Subdirs {
        path: String,
        dirs: Vec<SftpEntry>,
    },
    Progress {
        id: u64,
        name: String,
        /// Bytes so far / in total over the whole transfer (all files of a folder).
        done: u64,
        total: u64,
        /// Files finished / planned (0 = still counting or unknown).
        files_done: u32,
        files_total: u32,
        kind: TransferKind,
    },
    Finished {
        id: u64,
        name: String,
        result: Result<(), String>,
    },
    /// An upload would replace an existing remote file; the UI confirms and re-sends with `overwrite: true`.
    UploadExists {
        local: PathBuf,
        remote_dir: String,
        name: String,
    },
    /// Reply to `SftpCmd::ReadText`: the raw bytes and the server mtime.
    TextLoaded {
        remote: String,
        result: Result<(Vec<u8>, Option<u32>), String>,
    },
    /// Reply to `SftpCmd::WriteText`. `Err(None)` = refused because the server copy changed.
    TextSaved {
        remote: String,
        result: Result<Option<u32>, Option<String>>,
    },
    /// The local edit copy changed. The UI asks before anything is uploaded.
    SaveRequested {
        remote: String,
        name: String,
    },
    Error(String),
    Info(String),
}

static TRANSFER_ID: AtomicU64 = AtomicU64::new(1);

/// Temporary directories created for "edit remote" by every live session, so the app can
/// remove them synchronously at exit even when the session tasks never get to run again.
static EDIT_ROOTS: parking_lot::Mutex<Vec<PathBuf>> = parking_lot::Mutex::new(Vec::new());

/// Where edit directories are created (set by the app; `%TEMP%` until then).
static EDIT_BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Use `dir` (inside the user's data directory) for edit copies instead of `%TEMP%`, and
/// keep the OS content indexer out of it.
pub fn set_edit_base(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    mark_not_indexed(&dir);
    let _ = EDIT_BASE.set(dir);
}

fn edit_base() -> PathBuf {
    EDIT_BASE.get().cloned().unwrap_or_else(std::env::temp_dir)
}

/// `attrib +I`: Windows Search must not index plaintext copies of remote files.
#[cfg(windows)]
fn mark_not_indexed(dir: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("attrib")
        .arg("+I")
        .arg(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn mark_not_indexed(_dir: &Path) {}

/// Remove every edit directory created by this process. Called from the app's exit hook.
pub fn cleanup_edit_dirs() {
    for dir in EDIT_ROOTS.lock().drain(..) {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Remove `ruxt-edit-*` directories left behind by crashed instances (older than a day).
pub fn sweep_stale_edit_dirs() {
    let Ok(rd) = std::fs::read_dir(edit_base()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(24 * 3600);
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("ruxt-edit-") {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if !md.is_dir() {
            continue;
        }
        if md.modified().map(|m| m < cutoff).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Shared handles every spawned SFTP task needs.
#[derive(Clone)]
struct Shared {
    sftp: Arc<SftpSession>,
    tab: TabId,
    ui: mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
    cancels: Arc<parking_lot::Mutex<std::collections::HashMap<u64, Arc<AtomicBool>>>>,
    gate: Arc<Semaphore>,
}

impl Shared {
    fn send(&self, ev: SftpEvent) {
        let _ = self.ui.send(UiEvent::Sftp { tab: self.tab, ev });
        self.ctx.request_repaint();
    }
    async fn list_and_send(&self, path: &str) {
        match list_dir(&self.sftp, path).await {
            Ok(entries) => self.send(SftpEvent::Listing {
                path: path.to_string(),
                entries,
            }),
            Err(e) => self.send(SftpEvent::Error(format!("list {path}: {e:#}"))),
        }
    }
    fn new_transfer(&self) -> (u64, Arc<AtomicBool>) {
        let id = TRANSFER_ID.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().insert(id, cancel.clone());
        (id, cancel)
    }
    fn end_transfer(&self, id: u64) {
        self.cancels.lock().remove(&id);
    }
}

pub async fn run_sftp(
    handle: Arc<Handle<ClientHandler>>,
    tab: TabId,
    ui: mpsc::UnboundedSender<UiEvent>,
    mut rx: mpsc::UnboundedReceiver<SftpCmd>,
    ctx: egui::Context,
) {
    let sftp = match open_sftp(&handle).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            let _ = ui.send(UiEvent::Sftp {
                tab,
                ev: SftpEvent::Error(format!("SFTP unavailable: {e:#}")),
            });
            ctx.request_repaint();
            return;
        }
    };
    let sh = Shared {
        sftp,
        tab,
        ui,
        ctx,
        cancels: Default::default(),
        gate: Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSFERS)),
    };
    let mut home_dir: Option<String> = sh.sftp.canonicalize(".").await.ok();
    if let Some(h) = &home_dir {
        sh.send(SftpEvent::Home(h.clone()));
    }
    let edit_watchers: Arc<parking_lot::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>> = Default::default();
    let edit_root: Arc<parking_lot::Mutex<Option<PathBuf>>> = Default::default();
    let edit_answers: Arc<parking_lot::Mutex<std::collections::HashMap<String, EditAnswer>>> = Default::default();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            SftpCmd::Home => {
                let sh = sh.clone();
                match sh.sftp.canonicalize(".").await {
                    Ok(p) => {
                        home_dir = Some(p.clone());
                        sh.send(SftpEvent::Home(p.clone()));
                        tokio::spawn(async move { sh.list_and_send(&p).await });
                    }
                    Err(e) => sh.send(SftpEvent::Error(format!("home: {e}"))),
                }
            }
            SftpCmd::List(p) => {
                let sh = sh.clone();
                let expanded = expand_tilde(&p, home_dir.as_deref());
                tokio::spawn(async move {
                    let p = match sh.sftp.canonicalize(expanded.clone()).await {
                        Ok(c) => c,
                        Err(_) => expanded,
                    };
                    sh.list_and_send(&p).await;
                });
            }
            SftpCmd::Peek(p) => {
                let sh = sh.clone();
                let expanded = expand_tilde(&p, home_dir.as_deref());
                tokio::spawn(async move {
                    let path = match sh.sftp.canonicalize(expanded.clone()).await {
                        Ok(c) => c,
                        Err(_) => expanded,
                    };
                    match list_dir(&sh.sftp, &path).await {
                        Ok(entries) => sh.send(SftpEvent::Subdirs {
                            path,
                            dirs: entries.into_iter().filter(|e| e.is_dir).collect(),
                        }),
                        // The tree simply shows the node as empty; the main list reports errors.
                        Err(_) => sh.send(SftpEvent::Subdirs { path, dirs: Vec::new() }),
                    }
                });
            }
            SftpCmd::Mkdir(p) => {
                let sh = sh.clone();
                tokio::spawn(async move {
                    match sh.sftp.create_dir(p.clone()).await {
                        Ok(()) => sh.send(SftpEvent::Info(format!("created {p}"))),
                        Err(e) => sh.send(SftpEvent::Error(format!("mkdir {p}: {e}"))),
                    }
                    sh.list_and_send(&parent_of(&p)).await;
                });
            }
            SftpCmd::DeleteBatch(items) => {
                if !items.is_empty() {
                    spawn_delete_batch(sh.clone(), items);
                }
            }
            SftpCmd::Rename { from, to } => {
                let sh = sh.clone();
                tokio::spawn(async move {
                    match sh.sftp.rename(from.clone(), to.clone()).await {
                        Ok(()) => sh.send(SftpEvent::Info(format!("renamed to {}", file_name_of(&to)))),
                        Err(e) => sh.send(SftpEvent::Error(format!("rename: {e}"))),
                    }
                    sh.list_and_send(&parent_of(&from)).await;
                });
            }
            SftpCmd::Chmod { path, mode } => {
                let sh = sh.clone();
                tokio::spawn(async move {
                    let md = FileAttributes {
                        permissions: Some(mode & 0o7777),
                        ..Default::default()
                    };
                    match sh.sftp.set_metadata(path.clone(), md).await {
                        Ok(()) => sh.send(SftpEvent::Info(format!(
                            "chmod {:o} {}",
                            mode & 0o7777,
                            file_name_of(&path)
                        ))),
                        Err(e) => sh.send(SftpEvent::Error(format!("chmod: {e}"))),
                    }
                    sh.list_and_send(&parent_of(&path)).await;
                });
            }
            SftpCmd::CancelTransfer(id) => {
                if let Some(flag) = sh.cancels.lock().get(&id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            SftpCmd::Copy { from, to, is_dir } => {
                let sh = sh.clone();
                let (id, cancel) = sh.new_transfer();
                tokio::spawn(async move {
                    let _permit = sh.gate.clone().acquire_owned().await;
                    let name = file_name_of(&from);
                    let reporter = Reporter::new(&sh, id, name.clone(), TransferKind::Copy);
                    let res = if is_dir {
                        copy_dir(&sh.sftp, &from, &to, &reporter, &cancel, 0).await
                    } else {
                        copy_file(&sh.sftp, &from, &to, &reporter, &cancel).await
                    };
                    super::audit::log(
                        "sftp.copy",
                        serde_json::json!({"tab": sh.tab, "from": from, "to": to, "ok": res.is_ok()}),
                    );
                    sh.end_transfer(id);
                    sh.send(SftpEvent::Finished {
                        id,
                        name,
                        result: res.map_err(|e| format!("{e:#}")),
                    });
                    sh.list_and_send(&parent_of(&to)).await;
                });
            }
            SftpCmd::Move(pairs) => {
                let sh = sh.clone();
                tokio::spawn(async move {
                    let mut errors: Vec<String> = Vec::new();
                    let mut moved = 0usize;
                    let mut dirs: Vec<String> = Vec::new();
                    for (from, to) in &pairs {
                        if from == to {
                            continue;
                        }
                        // Never replace an existing target silently.
                        if sh.sftp.symlink_metadata(to.clone()).await.is_ok() {
                            errors.push(format!("{}: target exists", file_name_of(from)));
                            continue;
                        }
                        match sh.sftp.rename(from.clone(), to.clone()).await {
                            Ok(()) => moved += 1,
                            Err(e) => errors.push(format!("{}: {e}", file_name_of(from))),
                        }
                        for d in [parent_of(from), parent_of(to)] {
                            if !dirs.contains(&d) {
                                dirs.push(d);
                            }
                        }
                    }
                    super::audit::log(
                        "sftp.move",
                        serde_json::json!({"tab": sh.tab, "count": moved, "errors": errors.len()}),
                    );
                    if errors.is_empty() {
                        sh.send(SftpEvent::Info(format!("moved {moved} item(s)")));
                    } else {
                        let shown: Vec<&str> = errors.iter().take(5).map(String::as_str).collect();
                        sh.send(SftpEvent::Error(format!(
                            "move: {} item(s) failed: {}",
                            errors.len(),
                            shown.join("; ")
                        )));
                    }
                    // The current directory is listed last so it is what the panel ends up showing.
                    for d in dirs {
                        sh.list_and_send(&d).await;
                    }
                });
            }
            SftpCmd::Download { remote, local, is_dir } => {
                let sh = sh.clone();
                let (id, cancel) = sh.new_transfer();
                tokio::spawn(async move {
                    let _permit = sh.gate.clone().acquire_owned().await;
                    let name = file_name_of(&remote);
                    let reporter = Reporter::new(&sh, id, name.clone(), TransferKind::Download);
                    let res = if is_dir {
                        download_dir(&sh.sftp, &remote, &local, &reporter, &cancel, 0).await
                    } else {
                        download_file(&sh.sftp, &remote, &local, &reporter, &cancel).await
                    };
                    super::audit::log(
                        "sftp.download",
                        serde_json::json!({"tab": sh.tab, "remote": remote, "local": local.display().to_string(), "ok": res.is_ok()}),
                    );
                    sh.end_transfer(id);
                    sh.send(SftpEvent::Finished {
                        id,
                        name,
                        result: res.map_err(|e| format!("{e:#}")),
                    });
                });
            }
            SftpCmd::Upload {
                local,
                remote_dir,
                overwrite,
            } => {
                let sh = sh.clone();
                tokio::spawn(async move {
                    let name = local
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "?".into());
                    if !overwrite {
                        let is_file = tokio::fs::metadata(&local).await.map(|m| m.is_file()).unwrap_or(false);
                        if is_file && sh.sftp.metadata(join_remote(&remote_dir, &name)).await.is_ok() {
                            sh.send(SftpEvent::UploadExists {
                                local,
                                remote_dir,
                                name,
                            });
                            return;
                        }
                    }
                    let (id, cancel) = sh.new_transfer();
                    let _permit = sh.gate.clone().acquire_owned().await;
                    let reporter = Reporter::new(&sh, id, name.clone(), TransferKind::Upload);
                    let res = upload_path(&sh.sftp, &local, &remote_dir, &reporter, &cancel, 0).await;
                    super::audit::log(
                        "sftp.upload",
                        serde_json::json!({"tab": sh.tab, "local": local.display().to_string(), "remote_dir": remote_dir, "ok": res.is_ok()}),
                    );
                    sh.end_transfer(id);
                    sh.send(SftpEvent::Finished {
                        id,
                        name,
                        result: res.map_err(|e| format!("{e:#}")),
                    });
                    sh.list_and_send(&remote_dir).await;
                });
            }
            SftpCmd::EditRemote { remote, editor } => {
                spawn_edit_remote(
                    sh.clone(),
                    remote,
                    editor,
                    edit_root.clone(),
                    edit_watchers.clone(),
                    edit_answers.clone(),
                );
            }
            SftpCmd::EditSaveAnswer { remote, upload } => {
                let mut answers = edit_answers.lock();
                match upload {
                    Some(true) => {
                        answers.insert(remote, EditAnswer::Upload);
                    }
                    Some(false) => {
                        answers.insert(remote, EditAnswer::Skip);
                    }
                    None => {
                        answers.remove(&remote);
                    }
                }
            }
            SftpCmd::ReadText(remote) => {
                let sh = sh.clone();
                tokio::spawn(async move {
                    let result = read_text(&sh.sftp, &remote).await.map_err(|e| format!("{e:#}"));
                    sh.send(SftpEvent::TextLoaded { remote, result });
                });
            }
            SftpCmd::WriteText {
                remote,
                data,
                expected_mtime,
                force,
            } => {
                let sh = sh.clone();
                tokio::spawn(async move {
                    let result = write_text(&sh.sftp, &remote, &data, expected_mtime, force).await;
                    let ok = result.is_ok();
                    super::audit::log(
                        "sftp.save_text",
                        serde_json::json!({"tab": sh.tab, "remote": remote, "bytes": data.len(), "ok": ok}),
                    );
                    sh.send(SftpEvent::TextSaved {
                        remote: remote.clone(),
                        result,
                    });
                    if ok {
                        sh.list_and_send(&parent_of(&remote)).await;
                    }
                });
            }
        }
    }

    // Stop any running auto-upload watchers and clean up temporary edit files.
    for (_, stop) in edit_watchers.lock().drain() {
        stop.store(true, Ordering::Relaxed);
    }
    let root = edit_root.lock().take();
    if let Some(root) = root {
        EDIT_ROOTS.lock().retain(|p| p != &root);
        let _ = std::fs::remove_dir_all(&root);
    }
}

pub fn expand_tilde(path: &str, home: Option<&str>) -> String {
    let p = path.trim();
    let Some(h) = home else {
        return p.to_string();
    };
    if p == "~" {
        h.to_string()
    } else if let Some(rest) = p.strip_prefix("~/") {
        join_remote(h, rest)
    } else {
        p.to_string()
    }
}

fn spawn_delete_batch(sh: Shared, items: Vec<DeleteItem>) {
    tokio::spawn(async move {
        let mut last_parent = None;
        let mut deleted = 0usize;
        for item in items {
            last_parent = Some(parent_of(&item.path));
            // A symlink is always unlinked, never descended into: `remove_dir_all` on a link to
            // a directory would otherwise empty the link target.
            let r = if item.is_dir && !item.is_symlink {
                remove_dir_all(&sh.sftp, &item.path, 0).await
            } else {
                sh.sftp.remove_file(item.path.clone()).await.map_err(|e| anyhow!(e))
            };
            match r {
                Ok(()) => {
                    deleted += 1;
                    super::audit::log("sftp.delete", serde_json::json!({"tab": sh.tab, "path": item.path}));
                }
                Err(e) => sh.send(SftpEvent::Error(format!("delete {}: {e:#}", file_name_of(&item.path)))),
            }
        }
        if deleted > 0 {
            sh.send(SftpEvent::Info(format!("deleted {deleted} item(s)")));
        }
        if let Some(parent) = last_parent {
            sh.list_and_send(&parent).await;
        }
    });
}

/// File types the OS "open" handler would *execute* rather than edit. Opening one of these
/// from a hostile server would run attacker-controlled code, so they need an explicit editor.
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe",
    "com",
    "bat",
    "cmd",
    "scr",
    "pif",
    "lnk",
    "url",
    "hta",
    "msi",
    "msp",
    "msc",
    "reg",
    "cpl",
    "inf",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "ws",
    "sct",
    "ps1",
    "psm1",
    "psd1",
    "jar",
    "app",
    "command",
    "desktop",
    "sh",
    "bash",
    "zsh",
    "run",
    "appimage",
    "scf",
    "gadget",
    "xbap",
    "website",
    "webloc",
    "library-ms",
    "search-ms",
    "py",
    "pyw",
    "pl",
    "rb",
    "php",
    "lua",
    "tcl",
    "vb",
    "mst",
    "dll",
    "sys",
    "drv",
    "ocx",
    "action",
    "workflow",
    "terminal",
    "pkg",
    "dmg",
];

fn is_executable_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let trimmed = lower.trim_end_matches(['.', ' ']);
    match trimmed.rsplit_once('.') {
        Some((_, ext)) => EXECUTABLE_EXTENSIONS.contains(&ext),
        None => false,
    }
}

/// Launch the editor for a downloaded copy of a remote file.
fn launch_editor(editor: &str, path: &Path, name: &str) -> Result<()> {
    let editor = editor.trim();
    if editor.is_empty() {
        if is_executable_name(name) {
            bail!("{name} is an executable/script type that the OS would run instead of edit; set an external editor in Settings to edit it");
        }
        open::that(path).with_context(|| format!("cannot launch editor for {name}"))?;
        return Ok(());
    }
    // "C:\Program Files\X\editor.exe" --wait   or   code -w
    let (prog, args) = split_command(editor);
    std::process::Command::new(&prog)
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("cannot start editor {prog:?}"))?;
    Ok(())
}

/// Split an editor command line: a leading double-quoted program path or the first word,
/// followed by whitespace-separated arguments.
fn split_command(s: &str) -> (String, Vec<String>) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            let prog = rest[..end].to_string();
            let args = rest[end + 1..].split_whitespace().map(String::from).collect();
            return (prog, args);
        }
    }
    let mut it = s.split_whitespace();
    let prog = it.next().unwrap_or_default().to_string();
    (prog, it.map(String::from).collect())
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::DirBuilder::new().create(path)
    }
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::fill(&mut b[..]);
    hex::encode(b)
}

/// Create (once per session) an unpredictable, owner-only temp directory for edited files.
fn edit_root_dir(slot: &parking_lot::Mutex<Option<PathBuf>>) -> Result<PathBuf> {
    if let Some(p) = slot.lock().as_ref() {
        if p.is_dir() {
            return Ok(p.clone());
        }
    }
    let base = edit_base();
    for _ in 0..8 {
        let dir = base.join(format!("ruxt-edit-{}-{}", std::process::id(), rand_hex(8)));
        match create_private_dir(&dir) {
            Ok(()) => {
                *slot.lock() = Some(dir.clone());
                EDIT_ROOTS.lock().push(dir.clone());
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("creating edit temp directory"),
        }
    }
    bail!("cannot create a unique edit temp directory")
}

/// What the UI decided about a local edit that is waiting to be uploaded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditAnswer {
    /// The confirm dialog is up (or queued). Don't ask again.
    Waiting,
    Upload,
    Skip,
}

fn spawn_edit_remote(
    sh: Shared,
    remote: String,
    editor: String,
    edit_root: Arc<parking_lot::Mutex<Option<PathBuf>>>,
    watchers: Arc<parking_lot::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    edit_answers: Arc<parking_lot::Mutex<std::collections::HashMap<String, EditAnswer>>>,
) {
    // Stop any previous watcher on the same remote file before starting a fresh one.
    if let Some(prev) = watchers.lock().remove(&remote) {
        prev.store(true, Ordering::Relaxed);
    }
    edit_answers.lock().remove(&remote);
    let stop = Arc::new(AtomicBool::new(false));
    watchers.lock().insert(remote.clone(), stop.clone());

    tokio::spawn(async move {
        let name = file_name_of(&remote);
        let safe_name = match safe_component(&name) {
            Ok(s) => s.to_string(),
            Err(e) => {
                sh.send(SftpEvent::Error(format!("refusing to edit: {e}")));
                return;
            }
        };
        let display_name = super::term::sanitize_title(&safe_name);
        let remote_md = match sh.sftp.metadata(remote.clone()).await {
            Ok(m) => m,
            Err(e) => {
                sh.send(SftpEvent::Error(format!("stat {display_name}: {e}")));
                return;
            }
        };
        if !remote_md.is_regular() {
            sh.send(SftpEvent::Error(format!("{display_name} is not a regular file")));
            return;
        }
        if remote_md.size.unwrap_or(0) > MAX_EDIT_BYTES {
            sh.send(SftpEvent::Error(format!(
                "{display_name} is larger than {} MiB; download it instead of editing in place",
                MAX_EDIT_BYTES / (1024 * 1024)
            )));
            return;
        }
        let root = match edit_root_dir(&edit_root) {
            Ok(r) => r,
            Err(e) => {
                sh.send(SftpEvent::Error(format!("temp dir: {e:#}")));
                return;
            }
        };
        let mut hasher = Sha256::new();
        hasher.update(remote.as_bytes());
        let local_dir = root.join(hex::encode(&hasher.finalize()[..8]));
        if let Err(e) = create_private_dir(&local_dir) {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                sh.send(SftpEvent::Error(format!("temp dir: {e}")));
                return;
            }
        }
        let local_path = local_dir.join(&safe_name);
        let (dl_id, _dl_cancel) = sh.new_transfer();
        let rep = Reporter::new(&sh, dl_id, display_name.clone(), TransferKind::Download);
        let dl = download_file(&sh.sftp, &remote, &local_path, &rep, &stop).await;
        sh.end_transfer(dl_id);
        if let Err(e) = dl {
            sh.send(SftpEvent::Finished {
                id: dl_id,
                name: display_name,
                result: Err(format!("{e:#}")),
            });
            return;
        }
        sh.send(SftpEvent::Finished {
            id: dl_id,
            name: display_name.clone(),
            result: Ok(()),
        });
        let mut remote_mtime = remote_md.mtime;

        let Ok(mut last_hash) = file_sha256(&local_path).await else {
            return;
        };
        let mut last_mtime = tokio::fs::metadata(&local_path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        if let Err(e) = launch_editor(&editor, &local_path, &display_name) {
            sh.send(SftpEvent::Error(format!("{e:#}")));
            let _ = tokio::fs::remove_file(&local_path).await;
            return;
        }
        sh.send(SftpEvent::Info(format!(
            "editing {display_name}; Ruxt will ask before uploading"
        )));

        let parent = parent_of(&remote);
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(750)).await;
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let decided = edit_answers.lock().get(&remote).copied();
            let upload_now = decided == Some(EditAnswer::Upload);
            let skip_now = decided == Some(EditAnswer::Skip);
            if upload_now || skip_now {
                edit_answers.lock().remove(&remote);
            }
            let Ok(md) = tokio::fs::metadata(&local_path).await else {
                continue;
            };
            let mtime = md.modified().ok();
            if !upload_now && !skip_now && mtime == last_mtime {
                continue;
            }
            // Wait briefly in case the editor is still flushing or replacing the file.
            if !upload_now && !skip_now {
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
            let Ok(new_hash) = file_sha256(&local_path).await else {
                continue;
            };
            last_mtime = tokio::fs::metadata(&local_path)
                .await
                .ok()
                .and_then(|m| m.modified().ok());
            if skip_now || new_hash == last_hash {
                last_hash = new_hash;
                continue;
            }
            // Do not clobber changes somebody else made on the server since we fetched the file.
            let now_remote = sh.sftp.metadata(remote.clone()).await.ok().and_then(|m| m.mtime);
            if now_remote.is_some() && now_remote != remote_mtime {
                last_hash = new_hash;
                sh.send(SftpEvent::Error(format!(
                    "{display_name} changed on the server since it was opened; local save NOT uploaded. Re-open it to edit the current version."
                )));
                continue;
            }
            if !upload_now {
                if decided != Some(EditAnswer::Waiting) {
                    edit_answers.lock().insert(remote.clone(), EditAnswer::Waiting);
                    sh.send(SftpEvent::SaveRequested {
                        remote: remote.clone(),
                        name: display_name.clone(),
                    });
                }
                continue;
            }
            let len = tokio::fs::metadata(&local_path).await.map(|m| m.len()).unwrap_or(0);
            let (up_id, _c) = sh.new_transfer();
            let up_rep = Reporter::new(&sh, up_id, display_name.clone(), TransferKind::Upload);
            let r = upload_file(&sh.sftp, &local_path, &remote, len, &up_rep, &stop).await;
            sh.end_transfer(up_id);
            match r {
                Ok(()) => {
                    last_hash = new_hash;
                    remote_mtime = sh.sftp.metadata(remote.clone()).await.ok().and_then(|m| m.mtime);
                    super::audit::log(
                        "sftp.auto_upload",
                        serde_json::json!({"tab": sh.tab, "remote": remote, "bytes": len}),
                    );
                    sh.send(SftpEvent::Finished {
                        id: up_id,
                        name: display_name.clone(),
                        result: Ok(()),
                    });
                    sh.send(SftpEvent::Info(format!("auto-uploaded {display_name}")));
                    sh.list_and_send(&parent).await;
                }
                Err(e) => {
                    sh.send(SftpEvent::Finished {
                        id: up_id,
                        name: display_name.clone(),
                        result: Err(format!("{e:#}")),
                    });
                }
            }
        }
    });
}

async fn read_text(sftp: &SftpSession, remote: &str) -> Result<(Vec<u8>, Option<u32>)> {
    let md = sftp.metadata(remote.to_string()).await.context("stat")?;
    if !md.is_regular() {
        bail!("not a regular file");
    }
    if md.size.unwrap_or(0) > MAX_TEXT_EDIT_BYTES {
        bail!("larger than {} MiB", MAX_TEXT_EDIT_BYTES / (1024 * 1024));
    }
    let mut f = sftp.open(remote.to_string()).await.context("open")?;
    let mut buf = Vec::new();
    // Cap the read too: the size from `stat` may be stale or a lie.
    (&mut f)
        .take(MAX_TEXT_EDIT_BYTES + 1)
        .read_to_end(&mut buf)
        .await
        .context("read")?;
    let _ = f.shutdown().await;
    if buf.len() as u64 > MAX_TEXT_EDIT_BYTES {
        bail!("larger than {} MiB", MAX_TEXT_EDIT_BYTES / (1024 * 1024));
    }
    Ok((buf, md.mtime))
}

/// Overwrite `remote` in place (keeps its owner and permissions). Returns the new mtime.
async fn write_text(
    sftp: &SftpSession,
    remote: &str,
    data: &[u8],
    expected_mtime: Option<u32>,
    force: bool,
) -> Result<Option<u32>, Option<String>> {
    if !force {
        let now = sftp.metadata(remote.to_string()).await.ok().and_then(|m| m.mtime);
        if now.is_some() && expected_mtime.is_some() && now != expected_mtime {
            return Err(None);
        }
    }
    let res: Result<()> = async {
        let mut f = sftp
            .open_with_flags(
                remote.to_string(),
                OpenFlags::WRITE | OpenFlags::TRUNCATE | OpenFlags::CREATE,
            )
            .await
            .context("open for writing")?;
        f.write_all(data).await.context("write")?;
        f.flush().await.context("flush")?;
        f.shutdown().await.context("close")?;
        Ok(())
    }
    .await;
    res.map_err(|e| Some(format!("{e:#}")))?;
    Ok(sftp.metadata(remote.to_string()).await.ok().and_then(|m| m.mtime))
}

async fn file_sha256(path: &Path) -> Result<[u8; 32]> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

async fn open_sftp(handle: &Handle<ClientHandler>) -> Result<SftpSession> {
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .context("SFTP handshake")?;
    Ok(sftp)
}

/// Parent of a remote path. `/` stays `/`; a bare name has parent `.`.
pub fn parent_of(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/".into();
    }
    match t.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => t[..i].to_string(),
        None => ".".into(),
    }
}

pub fn file_name_of(p: &str) -> String {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string()
}

pub fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(windows)]
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2",
    "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A remote-supplied file name must not escape (or alias anything outside) the destination
/// directory. Accepts exactly one normal path component on the local platform.
pub fn safe_component(name: &str) -> Result<&str> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.chars().any(|c| c.is_control())
        || name.len() > 255
    {
        bail!("unsafe file name {name:?}");
    }
    // Exactly one `Normal` component: rejects drive prefixes (`C:x`), roots and `..` on every OS.
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(c)), None) if c == name => {}
        _ => bail!("unsafe file name {name:?}"),
    }
    #[cfg(windows)]
    {
        // `a:b` is an NTFS alternate data stream; trailing dots/spaces are silently stripped by
        // Win32 (so `foo.exe.` becomes `foo.exe`); device names bypass the directory entirely.
        if name.contains(':') || name.ends_with('.') || name.ends_with(' ') {
            bail!("unsafe file name {name:?}");
        }
        let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
        if RESERVED_DEVICE_NAMES.contains(&stem.as_str()) {
            bail!("unsafe file name {name:?}");
        }
    }
    Ok(name)
}

async fn list_dir(sftp: &SftpSession, path: &str) -> Result<Vec<SftpEntry>> {
    let rd = sftp.read_dir(path.to_string()).await?;
    let mut out = Vec::new();
    for e in rd {
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let md = e.metadata();
        let ft = e.file_type();
        out.push(SftpEntry {
            path: join_remote(path, &name),
            name: super::term::sanitize_title(&name),
            raw_name: name,
            is_dir: ft.is_dir(),
            is_symlink: ft.is_symlink(),
            is_regular: ft.is_file(),
            size: md.size.unwrap_or(0),
            mtime: md.mtime,
            permissions: md.permissions,
        });
    }
    // Resolve symlink targets concurrently so a directory full of links does not cost one
    // round trip per entry.
    let links: Vec<(usize, String)> = out
        .iter()
        .enumerate()
        .filter(|(_, e)| e.is_symlink)
        .map(|(i, e)| (i, e.path.clone()))
        .collect();
    if !links.is_empty() {
        let resolved: Vec<(usize, Option<FileAttributes>)> = futures::stream::iter(links)
            .map(|(i, p)| async move { (i, sftp.metadata(p).await.ok()) })
            .buffer_unordered(LIST_STAT_CONCURRENCY)
            .collect()
            .await;
        for (i, md) in resolved {
            if let Some(md) = md {
                let e = &mut out[i];
                e.is_dir = md.is_dir();
                e.is_regular = md.is_regular();
                if let Some(s) = md.size {
                    e.size = s;
                }
            }
        }
    }
    out.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(out)
}

async fn remove_dir_all(sftp: &SftpSession, path: &str, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("directory nesting too deep");
    }
    let entries = list_dir(sftp, path).await?;
    for e in entries {
        if e.is_dir && !e.is_symlink {
            Box::pin(remove_dir_all(sftp, &e.path, depth + 1)).await?;
        } else {
            sftp.remove_file(e.path.clone()).await?;
        }
    }
    sftp.remove_dir(path.to_string()).await?;
    Ok(())
}

/// What a transfer is going to move, and how far it is. Set once the tree has been counted;
/// progress is then reported over the whole plan rather than per file.
#[derive(Default, Clone, Copy)]
struct Plan {
    files_total: u32,
    bytes_total: u64,
    files_done: u32,
    /// Bytes of the files already completed.
    bytes_done: u64,
}

struct Reporter {
    tab: TabId,
    id: u64,
    name: String,
    kind: TransferKind,
    ui: mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
    last: parking_lot::Mutex<Instant>,
    plan: parking_lot::Mutex<Plan>,
}

impl Reporter {
    fn new(sh: &Shared, id: u64, name: String, kind: TransferKind) -> Self {
        Self {
            tab: sh.tab,
            id,
            name,
            kind,
            ui: sh.ui.clone(),
            ctx: sh.ctx.clone(),
            last: parking_lot::Mutex::new(Instant::now() - Duration::from_secs(1)),
            plan: parking_lot::Mutex::new(Plan::default()),
        }
    }

    /// Record how many files and bytes the transfer will move.
    fn set_plan(&self, files: u32, bytes: u64) {
        let mut p = self.plan.lock();
        p.files_total = files;
        p.bytes_total = bytes;
    }

    /// Single-file transfers: plan = this one file (unless a folder plan exists).
    fn ensure_plan(&self, bytes: u64) {
        let mut p = self.plan.lock();
        if p.files_total == 0 {
            p.files_total = 1;
            p.bytes_total = bytes;
        }
    }

    /// One file finished: fold its bytes into the completed total.
    fn file_done(&self, bytes: u64) {
        let mut p = self.plan.lock();
        p.files_done = p.files_done.saturating_add(1);
        p.bytes_done = p.bytes_done.saturating_add(bytes);
    }

    /// While the tree is being counted: "counting… n files".
    fn counting(&self, files: u32, bytes: u64) {
        let mut last = self.last.lock();
        if last.elapsed() < Duration::from_millis(150) {
            return;
        }
        *last = Instant::now();
        let _ = self.ui.send(UiEvent::Sftp {
            tab: self.tab,
            ev: SftpEvent::Progress {
                id: self.id,
                name: self.name.clone(),
                done: 0,
                total: bytes,
                files_done: 0,
                files_total: files,
                kind: self.kind,
            },
        });
        self.ctx.request_repaint();
    }

    /// Progress inside the current file; reported as progress over the whole plan.
    fn progress(&self, current: &str, done_in_file: u64, file_total: u64, force: bool) {
        let mut last = self.last.lock();
        if !force && last.elapsed() < Duration::from_millis(100) {
            return;
        }
        *last = Instant::now();
        let p = *self.plan.lock();
        let current = super::term::sanitize_title(current);
        let name = if current == self.name || p.files_total <= 1 {
            self.name.clone()
        } else {
            format!("{} / {}", self.name, current)
        };
        let done = p.bytes_done.saturating_add(done_in_file);
        let total = if p.bytes_total > 0 {
            p.bytes_total.max(done)
        } else {
            p.bytes_done.saturating_add(file_total).max(done)
        };
        let _ = self.ui.send(UiEvent::Sftp {
            tab: self.tab,
            ev: SftpEvent::Progress {
                id: self.id,
                name,
                done,
                total,
                files_done: p.files_done,
                files_total: p.files_total,
                kind: self.kind,
            },
        });
        self.ctx.request_repaint();
    }
}

/// Count regular files and bytes under a remote directory (directory symlinks skipped, like
/// the transfers themselves). Stops at `MAX_COUNT_FILES`.
async fn count_remote(
    sftp: &SftpSession,
    path: &str,
    depth: usize,
    acc: &mut (u32, u64),
    rep: &Reporter,
    cancel: &AtomicBool,
) -> Result<()> {
    if depth > MAX_DEPTH || acc.0 >= MAX_COUNT_FILES {
        return Ok(());
    }
    for e in list_dir(sftp, path).await? {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(Cancelled));
        }
        if e.is_dir {
            if !e.is_symlink {
                Box::pin(count_remote(sftp, &e.path, depth + 1, acc, rep, cancel)).await?;
            }
        } else if e.is_regular {
            acc.0 = acc.0.saturating_add(1);
            acc.1 = acc.1.saturating_add(e.size);
            rep.counting(acc.0, acc.1);
        }
        if acc.0 >= MAX_COUNT_FILES {
            break;
        }
    }
    Ok(())
}

/// Count regular files and bytes under a local directory (directory symlinks skipped).
async fn count_local(
    path: &Path,
    depth: usize,
    acc: &mut (u32, u64),
    rep: &Reporter,
    cancel: &AtomicBool,
) -> Result<()> {
    if depth > MAX_DEPTH || acc.0 >= MAX_COUNT_FILES {
        return Ok(());
    }
    let mut rd = tokio::fs::read_dir(path).await?;
    while let Some(entry) = rd.next_entry().await? {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(Cancelled));
        }
        let p = entry.path();
        let lmd = tokio::fs::symlink_metadata(&p).await?;
        let md = if lmd.is_symlink() {
            match tokio::fs::metadata(&p).await {
                Ok(m) if m.is_dir() => continue,
                Ok(m) => m,
                Err(_) => continue,
            }
        } else {
            lmd
        };
        if md.is_dir() {
            Box::pin(count_local(&p, depth + 1, acc, rep, cancel)).await?;
        } else if md.is_file() {
            acc.0 = acc.0.saturating_add(1);
            acc.1 = acc.1.saturating_add(md.len());
            rep.counting(acc.0, acc.1);
        }
        if acc.0 >= MAX_COUNT_FILES {
            break;
        }
    }
    Ok(())
}

/// Marker error so recursive transfers can tell "user cancelled" from a per-file failure.
#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}
impl std::error::Error for Cancelled {}

const CHUNK: usize = 256 * 1024;

fn part_path(local: &Path) -> PathBuf {
    let mut os = local.as_os_str().to_os_string();
    os.push(".ruxt-part");
    PathBuf::from(os)
}

/// Download one file to `local`, writing to a `.ruxt-part` sibling and renaming on success.
async fn download_file(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    rep: &Reporter,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut f = sftp
        .open(remote.to_string())
        .await
        .with_context(|| format!("open {remote}"))?;
    let total = f.metadata().await.ok().and_then(|m| m.size).unwrap_or(0);
    rep.ensure_plan(total);
    if let Some(parent) = local.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = part_path(local);
    let name = file_name_of(remote);
    let result: Result<u64> = async {
        let mut out = tokio::fs::File::create(&part)
            .await
            .with_context(|| format!("create {}", part.display()))?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        rep.progress(&name, 0, total, true);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).await?;
            done += n as u64;
            rep.progress(&name, done, total, false);
        }
        out.flush().await?;
        out.sync_all().await?;
        drop(out);
        // Windows refuses to rename over an existing file; the caller already confirmed the overwrite.
        if tokio::fs::rename(&part, local).await.is_err() {
            let _ = tokio::fs::remove_file(local).await;
            tokio::fs::rename(&part, local)
                .await
                .with_context(|| format!("rename into {}", local.display()))?;
        }
        Ok(done)
    }
    .await;
    match result {
        Ok(done) => {
            rep.progress(&name, done, total.max(done), true);
            rep.file_done(done);
            Ok(())
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

async fn download_dir(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    rep: &Reporter,
    cancel: &AtomicBool,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("directory nesting too deep");
    }
    if depth == 0 {
        let mut acc = (0u32, 0u64);
        count_remote(sftp, remote, 0, &mut acc, rep, cancel).await?;
        rep.set_plan(acc.0, acc.1);
    }
    tokio::fs::create_dir_all(local).await?;
    let entries = list_dir(sftp, remote).await?;
    let mut errors: Vec<String> = Vec::new();
    for e in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(Cancelled));
        }
        let name = match safe_component(&e.raw_name) {
            Ok(n) => n,
            Err(err) => {
                errors.push(format!("{}: {err}", e.name));
                continue;
            }
        };
        let dst = local.join(name);
        let r = if e.is_dir {
            if e.is_symlink {
                continue; // never follow directory symlinks (loop / escape protection)
            }
            Box::pin(download_dir(sftp, &e.path, &dst, rep, cancel, depth + 1)).await
        } else if e.is_regular {
            download_file(sftp, &e.path, &dst, rep, cancel).await
        } else {
            continue; // sockets, pipes, devices
        };
        if let Err(err) = r {
            if err.is::<Cancelled>() {
                return Err(err);
            }
            errors.push(format!("{}: {err:#}", e.name));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        let shown: Vec<&str> = errors.iter().take(5).map(String::as_str).collect();
        let more = if errors.len() > 5 {
            format!(" (+{} more)", errors.len() - 5)
        } else {
            String::new()
        };
        bail!("{} item(s) failed: {}{more}", errors.len(), shown.join("; "))
    }
}

async fn upload_path(
    sftp: &SftpSession,
    local: &Path,
    remote_dir: &str,
    rep: &Reporter,
    cancel: &AtomicBool,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("directory nesting too deep");
    }
    let lmd = tokio::fs::symlink_metadata(local).await?;
    let name = local
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| anyhow!("bad local path"))?;
    let remote = join_remote(remote_dir, &name);
    let md = if lmd.is_symlink() {
        // Follow file links but never directory links (symlink loops, escaping the tree).
        let target = tokio::fs::metadata(local).await?;
        if target.is_dir() {
            return Ok(());
        }
        target
    } else {
        lmd
    };
    if md.is_dir() {
        if depth == 0 {
            let mut acc = (0u32, 0u64);
            count_local(local, 0, &mut acc, rep, cancel).await?;
            rep.set_plan(acc.0, acc.1);
        }
        let _ = sftp.create_dir(remote.clone()).await;
        let mut rd = tokio::fs::read_dir(local).await?;
        let mut errors: Vec<String> = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            if let Err(e) = Box::pin(upload_path(sftp, &entry.path(), &remote, rep, cancel, depth + 1)).await {
                if e.is::<Cancelled>() {
                    return Err(e);
                }
                errors.push(format!("{}: {e:#}", entry.file_name().to_string_lossy()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            let shown: Vec<&str> = errors.iter().take(5).map(String::as_str).collect();
            bail!("{} item(s) failed: {}", errors.len(), shown.join("; "))
        }
    } else if md.is_file() {
        upload_file(sftp, local, &remote, md.len(), rep, cancel).await
    } else {
        Ok(()) // sockets, pipes, devices
    }
}

/// Copy one remote file to another remote path by streaming it through the client (SFTP has
/// no server-side copy). Written to a temporary name and renamed on success.
async fn copy_file(sftp: &SftpSession, from: &str, to: &str, rep: &Reporter, cancel: &AtomicBool) -> Result<()> {
    if from == to {
        bail!("source and destination are the same file");
    }
    if sftp.symlink_metadata(to.to_string()).await.is_ok() {
        bail!("{} already exists", file_name_of(to));
    }
    let mut src = sftp
        .open(from.to_string())
        .await
        .with_context(|| format!("open {from}"))?;
    let md = src.metadata().await.ok();
    let total = md.as_ref().and_then(|m| m.size).unwrap_or(0);
    rep.ensure_plan(total);
    let name = file_name_of(from);
    let dir = parent_of(to);
    let tmp = join_remote(&dir, &format!(".{}.ruxt-{}", file_name_of(to), rand_hex(4)));
    let written: Result<()> = async {
        let mut dst = sftp
            .open_with_flags(tmp.clone(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
            .await
            .with_context(|| format!("create remote {tmp}"))?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        rep.progress(&name, 0, total, true);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            let n = src.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await?;
            done += n as u64;
            rep.progress(&name, done, total, false);
        }
        dst.flush().await?;
        dst.shutdown().await?;
        rep.progress(&name, done, total.max(done), true);
        rep.file_done(done);
        Ok(())
    }
    .await;
    if let Err(e) = written {
        let _ = sftp.remove_file(tmp).await;
        return Err(e);
    }
    if let Some(mode) = md.and_then(|m| m.permissions) {
        let attrs = FileAttributes {
            permissions: Some(mode & 0o7777),
            ..Default::default()
        };
        let _ = sftp.set_metadata(tmp.clone(), attrs).await;
    }
    if let Err(e) = sftp.rename(tmp.clone(), to.to_string()).await {
        let _ = sftp.remove_file(tmp).await;
        bail!("rename into place: {e}");
    }
    Ok(())
}

/// Copy a remote directory tree. Directory symlinks are not followed.
async fn copy_dir(
    sftp: &SftpSession,
    from: &str,
    to: &str,
    rep: &Reporter,
    cancel: &AtomicBool,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("directory nesting too deep");
    }
    if to == from || to.starts_with(&format!("{}/", from.trim_end_matches('/'))) {
        bail!("cannot copy a folder into itself");
    }
    if depth == 0 {
        let mut acc = (0u32, 0u64);
        count_remote(sftp, from, 0, &mut acc, rep, cancel).await?;
        rep.set_plan(acc.0, acc.1);
    }
    if sftp.symlink_metadata(to.to_string()).await.is_err() {
        sftp.create_dir(to.to_string())
            .await
            .with_context(|| format!("mkdir {to}"))?;
    }
    let entries = list_dir(sftp, from).await?;
    let mut errors: Vec<String> = Vec::new();
    for e in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(Cancelled));
        }
        let dst = join_remote(to, &e.raw_name);
        let r = if e.is_dir {
            if e.is_symlink {
                continue;
            }
            Box::pin(copy_dir(sftp, &e.path, &dst, rep, cancel, depth + 1)).await
        } else if e.is_regular {
            copy_file(sftp, &e.path, &dst, rep, cancel).await
        } else {
            continue;
        };
        if let Err(err) = r {
            if err.is::<Cancelled>() {
                return Err(err);
            }
            errors.push(format!("{}: {err:#}", e.name));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        let shown: Vec<&str> = errors.iter().take(5).map(String::as_str).collect();
        bail!("{} item(s) failed: {}", errors.len(), shown.join("; "))
    }
}

/// Upload one file atomically: write to a dotted temporary name in the target directory, then
/// rename over the destination so a failed transfer never leaves a truncated remote file.
async fn upload_file(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    total: u64,
    rep: &Reporter,
    cancel: &AtomicBool,
) -> Result<()> {
    let dir = parent_of(remote);
    let name = file_name_of(remote);
    let tmp = join_remote(&dir, &format!(".{name}.ruxt-{}", rand_hex(4)));
    let existing_mode = sftp.metadata(remote.to_string()).await.ok().and_then(|m| m.permissions);
    rep.ensure_plan(total);

    let written: Result<()> = async {
        let mut src = tokio::fs::File::open(local).await?;
        let mut dst = sftp
            .open_with_flags(tmp.clone(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
            .await
            .with_context(|| format!("create remote {tmp}"))?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        rep.progress(&name, 0, total, true);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            let n = src.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await?;
            done += n as u64;
            rep.progress(&name, done, total, false);
        }
        dst.flush().await?;
        dst.shutdown().await?;
        rep.progress(&name, done, total, true);
        rep.file_done(done);
        Ok(())
    }
    .await;
    if let Err(e) = written {
        let _ = sftp.remove_file(tmp).await;
        return Err(e);
    }
    if let Some(mode) = existing_mode {
        let md = FileAttributes {
            permissions: Some(mode & 0o7777),
            ..Default::default()
        };
        let _ = sftp.set_metadata(tmp.clone(), md).await;
    }
    // SFTP v3 `rename` refuses to replace an existing target on most servers. The new content is
    // already complete on the server at this point, so removing the old file first is safe.
    if sftp.rename(tmp.clone(), remote.to_string()).await.is_err() {
        let _ = sftp.remove_file(remote.to_string()).await;
        if let Err(e) = sftp.rename(tmp.clone(), remote.to_string()).await {
            let _ = sftp.remove_file(tmp).await;
            bail!("rename into place: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_component_rejects_escapes() {
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b", "a\x1bb", "../x"] {
            assert!(safe_component(bad).is_err(), "{bad:?} must be rejected");
        }
        #[cfg(windows)]
        for bad in ["C:evil", "a:stream", "NUL", "nul.txt", "COM1", "trailing.", "trailing "] {
            assert!(safe_component(bad).is_err(), "{bad:?} must be rejected on Windows");
        }
        for good in ["notes.txt", "한글 파일.log", "a.b.c", ".hidden", "x"] {
            assert_eq!(safe_component(good).unwrap(), good);
        }
        let long = "a".repeat(256);
        assert!(safe_component(&long).is_err());
    }

    #[test]
    fn executable_names_are_detected() {
        for n in [
            "a.exe",
            "A.BAT",
            "x.lnk",
            "s.ps1",
            "run.sh",
            "t.py",
            "evil.exe.",
            "evil.exe ",
        ] {
            assert!(is_executable_name(n), "{n}");
        }
        for n in ["notes.txt", "config.yaml", "Makefile", "a.c", "x.rs", "README"] {
            assert!(!is_executable_name(n), "{n}");
        }
    }

    #[test]
    fn remote_path_helpers() {
        assert_eq!(parent_of("/"), "/");
        assert_eq!(parent_of("/etc"), "/");
        assert_eq!(parent_of("/etc/ssh/"), "/etc");
        assert_eq!(parent_of("file"), ".");
        assert_eq!(file_name_of("/a/b/c.txt"), "c.txt");
        assert_eq!(join_remote("/a/", "b"), "/a/b");
        assert_eq!(join_remote("/a", "b"), "/a/b");
        assert_eq!(expand_tilde("~", Some("/home/u")), "/home/u");
        assert_eq!(expand_tilde("~/x", Some("/home/u")), "/home/u/x");
        assert_eq!(expand_tilde("~/x", None), "~/x");
        assert_eq!(expand_tilde("/abs", Some("/home/u")), "/abs");
    }

    #[test]
    fn split_command_handles_quotes() {
        let (p, a) = split_command(r#""C:\Program Files\Editor\ed.exe" --wait -n"#);
        assert_eq!(p, r"C:\Program Files\Editor\ed.exe");
        assert_eq!(a, vec!["--wait", "-n"]);
        let (p, a) = split_command("code -w");
        assert_eq!(p, "code");
        assert_eq!(a, vec!["-w"]);
    }

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(
            part_path(Path::new("dir/a.tar.gz")),
            PathBuf::from("dir/a.tar.gz.ruxt-part")
        );
    }
}
