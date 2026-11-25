//! Main application: tabs, panels, dialogs and event plumbing.

pub mod dialogs;
mod fonts;
mod glyphs;
mod kit;
mod sftp_panel;
mod tail_viewer;
mod terminal_widget;
mod theme;
#[cfg(test)]
mod ui_tests;

use crate::core::agent_guard::{self, AgentGuard};
use crate::core::applock;
use crate::core::audit;
use crate::core::config::{self, AppConfig, Paths, SessionProfile, Theme, ToolbarItems, TunnelSpec};
use crate::core::history::History;
use crate::core::hostkeys::HostKeyStore;
use crate::core::i18n::{self, tr, trf};
use crate::core::import;
use crate::core::local_pty::LocalShell;
use crate::core::monitor::{self, MonitorControl, MonitorEvent, Sample};
use crate::core::sftp::{self, DeleteItem, SftpCmd, SftpEvent};
use crate::core::ssh::{
    self, AppLockOutcome, AppLockThen, ConnInfo, Prompt, PromptKind, PromptReply, SecretKind, SessionArgs, SessionCmd,
    TunnelEvent, UiEvent, VaultOpOutcome,
};
use crate::core::tail::{self, TailEvent, TailId, TailSpec};
use crate::core::term::{self, Palette, ShellDirCmd, Size, TermState};
use crate::core::theme::{self as color_theme, ThemeSpec};
use crate::core::update::{self, UpdateState};
use crate::core::vault::{Opened, Vault};
use crate::core::xserver::{self, XServerManager};
use crate::core::TabId;

use alacritty_terminal::term::TermMode;
use dialogs::{
    AppLockDialog, AppLockMode, AppLockResult, EditorResult, KeygenDialog, MigrateDialog, MigrateResult, SessionEditor,
    SettingsAction, ThemeEditor, ThemeEditorResult, WelcomeDialog, WelcomeResult,
};
use egui::{Color32, RichText};
use fonts::FontEntry;
use parking_lot::Mutex;
use secrecy::SecretString;
use sftp_panel::{SftpAction, SftpPanelState};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tail_viewer::{TailViewer, TailViewerAction};
use terminal_widget::TermView;
use tokio::sync::mpsc;
use zeroize::Zeroize;

enum TabKind {
    Ssh { cmd_tx: mpsc::UnboundedSender<SessionCmd> },
    Local { shell: Arc<LocalShell> },
}

#[derive(Debug, Clone)]
enum TunnelStatus {
    Stopped,
    Running(String),
    Error(String),
}

struct Tab {
    id: TabId,
    title: String,
    kind: TabKind,
    term: Arc<Mutex<TermState>>,
    view: TermView,
    connected: bool,
    info: ConnInfo,
    status: String,
    profile: Option<SessionProfile>,
    sftp: SftpPanelState,
    tunnels: Vec<TunnelStatus>,
    /// Server status monitor (SSH tabs): last sample and its switches.
    monitor: MonitorState,
    monitor_ctl: Option<MonitorControl>,
    /// Where the input line being typed starts on the grid, and the completions for it.
    cmd: CmdLine,
    /// Last working directory reported by the terminal (OSC 7, window title, `cd`, or `pwd`).
    cwd: Option<String>,
    /// Previous working directory, used to resolve `cd -`.
    prev_cwd: Option<String>,
    /// Pending `pwd` command: `(cmd_abs_line, started_at)` waiting for its output line on the grid.
    pending_pwd: Option<(usize, Instant)>,
}

/// Samples kept for the CPU / memory sparklines.
const MONITOR_HISTORY: usize = 120;

#[derive(Default)]
struct MonitorState {
    last: Option<Sample>,
    at: Option<Instant>,
    error: Option<String>,
    /// Polling stopped for this session (exec refused, repeated failures).
    unavailable: Option<String>,
    cpu_hist: VecDeque<f32>,
    mem_hist: VecDeque<f32>,
}

impl MonitorState {
    fn apply(&mut self, ev: MonitorEvent) {
        match ev {
            MonitorEvent::Sample(s) => {
                if let Some(c) = s.cpu_pct {
                    push_capped(&mut self.cpu_hist, c);
                }
                if let Some(m) = s.mem_used_pct() {
                    push_capped(&mut self.mem_hist, m);
                }
                self.last = Some(*s);
                self.at = Some(Instant::now());
                self.error = None;
            }
            MonitorEvent::Error(e) => self.error = Some(e),
            MonitorEvent::Unavailable(e) => self.unavailable = Some(e),
        }
    }

    /// One-line summary for the status bar.
    fn summary(&self) -> Option<String> {
        let s = self.last.as_ref()?;
        let mut parts = Vec::new();
        if let Some(c) = s.cpu_pct {
            parts.push(format!("CPU {c:.0}%"));
        }
        if let Some(m) = s.mem_used_pct() {
            parts.push(format!("MEM {m:.0}%"));
        }
        if let Some(l) = s.load {
            parts.push(format!("load {:.2}", l[0]));
        }
        if let Some(d) = s.disks.first() {
            parts.push(format!("disk {:.0}%", d.used_pct()));
        }
        if parts.is_empty() {
            parts.push(s.hostname.clone());
        }
        Some(parts.join("  "))
    }
}

fn push_capped(v: &mut VecDeque<f32>, x: f32) {
    v.push_back(x);
    while v.len() > MONITOR_HISTORY {
        v.pop_front();
    }
}

/// Tracks the command line being typed so completions can be offered. The text itself is
/// always read back from the terminal grid (so only what the program echoed counts); this
/// only remembers where the line started.
#[derive(Default)]
struct CmdLine {
    /// (absolute grid row, column) of the first typed character of the current line.
    start: Option<(usize, usize)>,
    /// What is typed so far, leading spaces removed.
    prefix: String,
    candidates: Vec<String>,
    idx: usize,
    /// The prefix for which the user hid the completions (Esc).
    suppressed: Option<String>,
    /// Local shadow buffer of characters typed on the current line (fallback before echo arrives).
    typed: String,
}

impl CmdLine {
    fn reset(&mut self) {
        *self = Self::default();
    }
    fn visible(&self) -> bool {
        !self.candidates.is_empty() && self.suppressed.as_deref() != Some(self.prefix.as_str())
    }
    fn feed_bytes(&mut self, bytes: &[u8]) {
        let Ok(s) = std::str::from_utf8(bytes) else {
            return;
        };
        // Ignore escape sequences (arrow keys, function keys).
        if s.starts_with('\x1b') {
            return;
        }
        for ch in s.chars() {
            match ch {
                '\x7f' | '\x08' => {
                    self.typed.pop();
                }
                '\x15' => {
                    self.typed.clear();
                }
                '\x17' => {
                    let trimmed = self.typed.trim_end();
                    let keep = trimmed.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                    self.typed.truncate(keep);
                }
                '\r' | '\n' => {}
                c if !c.is_control() => {
                    self.typed.push(c);
                }
                _ => {}
            }
        }
    }
}

impl Tab {
    /// Update `cwd` (saving the old one in `prev_cwd`) and, when SFTP terminal-follow is active,
    /// ask the SFTP task to move to it. `force = true` bypasses the `follow_last` dedup (used for
    /// explicit `cd`/`pwd` commands so they still sync even if the user browsed elsewhere in SFTP).
    fn set_terminal_cwd(&mut self, cwd: String, force: bool) {
        let resolved = sftp::expand_tilde(&cwd, self.sftp.home.as_deref());
        if self.cwd.as_deref() != Some(resolved.as_str()) {
            if let Some(prev) = self.cwd.take() {
                self.prev_cwd = Some(prev);
            }
            self.cwd = Some(resolved.clone());
        }
        self.follow_sftp(&resolved, force);
    }

    fn follow_sftp(&mut self, cwd: &str, force: bool) {
        if !(self.connected && self.sftp.started && self.sftp.follow_terminal) {
            return;
        }
        let want = sftp::expand_tilde(cwd, self.sftp.home.as_deref());
        if want != self.sftp.cwd && (force || self.sftp.follow_last.as_deref() != Some(want.as_str())) {
            if let TabKind::Ssh { cmd_tx } = &self.kind {
                let _ = cmd_tx.send(SessionCmd::Sftp(SftpCmd::Follow(want.clone())));
            }
            self.sftp.follow_last = Some(want);
        }
    }

    fn effective_cwd(&self) -> Option<String> {
        if let Some(c) = &self.cwd {
            if !c.is_empty() {
                if let Some(home) = self.sftp.home.as_deref() {
                    return Some(sftp::expand_tilde(c, Some(home)));
                }
                return Some(c.clone());
            }
        }
        if !self.sftp.cwd.is_empty() {
            return Some(self.sftp.cwd.clone());
        }
        None
    }
    fn send_input(&self, bytes: Vec<u8>) {
        match &self.kind {
            TabKind::Ssh { cmd_tx } => {
                let _ = cmd_tx.send(SessionCmd::Input(bytes));
            }
            TabKind::Local { shell } => shell.write(&bytes),
        }
    }
    fn resize(&self, size: Size) {
        match &self.kind {
            TabKind::Ssh { cmd_tx } => {
                let _ = cmd_tx.send(SessionCmd::Resize {
                    cols: size.cols,
                    rows: size.rows,
                });
            }
            TabKind::Local { shell } => shell.resize(size),
        }
    }
    fn is_ssh(&self) -> bool {
        matches!(self.kind, TabKind::Ssh { .. })
    }
}

/// Saved passwords and key passphrases. They are encrypted with the screen-lock password and
/// usable only while Ruxt is unlocked (see `core::vault`).
enum VaultState {
    /// No screen-lock password: nothing is saved.
    Absent,
    /// Ruxt is locked, or the store could not be opened.
    Locked,
    Unlocked(Vault),
    /// The file is still under an old vault master password. The key derived from the
    /// screen-lock password waits for the migration dialog.
    Foreign {
        key: zeroize::Zeroizing<[u8; 32]>,
        salt: [u8; 32],
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightPanel {
    None,
    Sftp,
    Tunnels,
}

enum ConfirmAction {
    DeleteSession(String),
    SftpDelete {
        tab: TabId,
        items: Vec<DeleteItem>,
    },
    /// Downloads whose local target exists.
    SftpOverwriteDownload {
        tab: TabId,
        cmds: Vec<SftpCmd>,
    },
    /// An upload that would replace a remote file.
    SftpOverwriteUpload {
        tab: TabId,
        local: std::path::PathBuf,
        remote_dir: String,
    },
    Paste {
        tab: TabId,
        text: String,
    },
    /// Replace passwords saved under an old vault master password with an empty store.
    DiscardOldVault,
    CloseTab(TabId),
    Quit,
    /// A local edit is ready to upload. `No` tells the watcher to keep the local copy.
    SftpEditSave {
        tab: TabId,
        remote: String,
    },
}

/// Config writes are coalesced: a slider drag marks the config dirty and it is flushed once
/// nothing changed for this long (and always at exit).
const CFG_FLUSH_DELAY: Duration = Duration::from_millis(500);

struct Confirm {
    title: String,
    message: String,
    danger: bool,
    action: ConfirmAction,
    /// Offer "don't ask again" (close-tab / quit confirmations); the checkbox state.
    dont_ask: Option<bool>,
}

impl Confirm {
    fn new(title: impl Into<String>, message: impl Into<String>, danger: bool, action: ConfirmAction) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            danger,
            action,
            dont_ask: None,
        }
    }
}

#[derive(Default)]
struct PromptInput {
    secret: String,
    remember: bool,
    answers: Vec<String>,
    ack: bool,
}

impl Drop for PromptInput {
    fn drop(&mut self) {
        self.secret.zeroize();
        for a in &mut self.answers {
            a.zeroize();
        }
    }
}

/// Colours for the OS title bar. `dark` selects light minimize / maximize / close glyphs.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CaptionPaint {
    bg: [u8; 3],
    fg: [u8; 3],
    border: [u8; 3],
    dark: bool,
}

/// Title-bar fill from the theme, with glyphs that stay readable on it.
fn caption_paint(spec: &ThemeSpec) -> CaptionPaint {
    let bg = spec.panel.0;
    let lum = luminance(bg);
    let dark = lum < 140.0;
    let fg = spec.text.0;
    let fg = if (lum - luminance(fg)).abs() >= 90.0 {
        fg
    } else if dark {
        [0xf2, 0xf2, 0xf2]
    } else {
        [0x1a, 0x1a, 0x1a]
    };
    CaptionPaint {
        bg,
        fg,
        border: spec.stroke.0,
        dark,
    }
}

fn luminance(rgb: [u8; 3]) -> f32 {
    0.2126 * f32::from(rgb[0]) + 0.7152 * f32::from(rgb[1]) + 0.0722 * f32::from(rgb[2])
}

#[cfg(windows)]
fn window_hwnd(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    let handle = frame.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(window) => Some(window.hwnd.get()),
        _ => None,
    }
}

pub struct RuxtApp {
    rt: tokio::runtime::Handle,
    paths: Paths,
    cfg: AppConfig,
    vault: VaultState,
    hostkeys: Arc<HostKeyStore>,
    ui_tx: mpsc::UnboundedSender<UiEvent>,
    ui_rx: mpsc::UnboundedReceiver<UiEvent>,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: TabId,
    prompts: VecDeque<(TabId, Prompt)>,
    prompt_input: PromptInput,
    editor: Option<SessionEditor>,
    settings_open: bool,
    /// Moving passwords saved under an old vault master password.
    vault_dialog: Option<MigrateDialog>,
    /// The migration dialog opened by itself once this run; afterwards only from Settings.
    migrate_asked: bool,
    keygen: Option<KeygenDialog>,
    confirm: Option<Confirm>,
    /// An edit-upload question that arrived while another confirm dialog was open.
    pending_edit_save: Option<(TabId, String, String)>,
    show_sidebar: bool,
    right_panel: RightPanel,
    multi_exec: bool,
    last_activity: Instant,
    status_msg: Option<(String, Instant)>,
    quick_connect: String,
    session_filter: String,
    selected_session: Option<String>,
    about_open: bool,
    /// The welcome tour (first run, or reopened from Help & about).
    welcome: Option<WelcomeDialog>,
    quit_confirmed: bool,
    xserver: XServerManager,
    font_cache: Option<Vec<FontEntry>>,
    /// Receives the result of a background font scan (see `ensure_font_scan`).
    font_scan_rx: Option<std::sync::mpsc::Receiver<Vec<FontEntry>>>,
    /// When the config was last changed and not yet written.
    cfg_dirty: Option<Instant>,
    /// UI scale and UI font size currently installed in egui (see `apply_ui_metrics`).
    applied_ui: Option<(f32, f32)>,
    /// Windows title-bar colours still to paint (see `push_caption`).
    caption: Option<CaptionPaint>,
    /// Frames left to keep painting `caption`. egui sends its theme command after `ui`.
    caption_pushes: u8,
    /// Whole-window lock (see `core::applock`). While set, nothing but the lock screen is drawn.
    app_locked: bool,
    /// Capture exclusion and injected-input block, kept in line with `cfg.agent_guard`.
    agent_guard: AgentGuard,
    /// A screen-lock password exists on disk.
    app_lock_set: bool,
    /// Consecutive failed unlock attempts (throttles the next check).
    app_lock_failures: u32,
    applock_dialog: Option<AppLockDialog>,
    /// Command history for completions (SQLite, loaded into memory).
    history: History,
    /// The "Server info" window for the active tab.
    server_info_open: bool,
    /// Terminal palette of the theme in effect (new tabs start with it).
    palette: Palette,
    theme_editor: Option<ThemeEditor>,
    /// Text Ruxt last put on the clipboard and when (for the auto-clear timer).
    clipboard_marker: Option<(Instant, String)>,
    egui_ctx: egui::Context,
    tail_viewers: Vec<TailViewer>,
    next_tail_id: TailId,
    /// Latest update-check status and the channel receiving the background thread's result.
    update_state: UpdateState,
    update_rx: Option<std::sync::mpsc::Receiver<UpdateState>>,
}

impl RuxtApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        // theme is applied after construction (see below)
        let paths = Paths::discover().expect("config directory");
        let cfg = AppConfig::load(&paths);
        audit::init(paths.audit_file(), cfg.audit_days, cfg.audit_log);
        sftp::set_edit_base(paths.edit_dir());
        // Off the UI thread: a `reg.exe` round trip.
        let _ = std::thread::Builder::new()
            .name("wer-exclude".into())
            .spawn(crate::core::platform::exclude_from_error_reporting);
        i18n::set_lang(cfg.language);
        fonts::install(&cc.egui_ctx, &cfg.font_family);
        // With a screen lock Ruxt starts locked: the same password opens the saved passwords.
        let app_lock_set = applock::exists(&paths.applock_file());
        let vault = if app_lock_set {
            VaultState::Locked
        } else {
            VaultState::Absent
        };
        let hostkeys = Arc::new(HostKeyStore::new(paths.known_hosts_file()));
        let (ui_tx, ui_rx) = mpsc::unbounded_channel();
        audit::log("app.start", serde_json::json!({"version": env!("CARGO_PKG_VERSION")}));
        let mut xserver = XServerManager::new(paths.config_dir.clone(), cfg.x11_server_path.clone(), cfg.x11_clipboard);
        if cfg.x11_stop_on_exit {
            // A crashed or killed Ruxt leaves its X server running; it would otherwise be taken
            // for an external server and a second one started beside it.
            xserver.refresh(true);
            let n = xserver.reap_orphans();
            if n > 0 {
                tracing::info!("stopped {n} X server(s) left behind by a previous instance");
            }
        }
        if cfg.x11_start_on_launch {
            match xserver.ensure_running() {
                Ok(_) => {}
                Err(e) => tracing::warn!("X server did not start at launch: {e:#}"),
            }
        }
        // First run: offer the screen lock; declining is remembered. Passwords saved by an
        // older version (separate vault master password) need a lock password before they can
        // be moved, so their owner is asked once more.
        let old_vault = !app_lock_set && Vault::exists(&paths.vault_file());
        let applock_dialog = if app_lock_set {
            Some(AppLockDialog::new(AppLockMode::Unlock))
        } else if !cfg.app_lock_prompted || (old_vault && !cfg.vault_merge_prompted) {
            Some(AppLockDialog::new(AppLockMode::Setup { first_run: true }))
        } else {
            None
        };
        // Temp copies of remotely edited files from a crashed instance must not linger.
        let _ = std::thread::Builder::new()
            .name("edit-dir-sweep".into())
            .spawn(sftp::sweep_stale_edit_dirs);
        // The command history is encrypted with a key from the saved-password store; it opens
        // when Ruxt is unlocked (see `open_history`).
        let history = History::locked();
        let mut app = Self {
            rt,
            paths,
            cfg,
            vault,
            hostkeys,
            ui_tx,
            ui_rx,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            prompts: VecDeque::new(),
            prompt_input: PromptInput::default(),
            editor: None,
            settings_open: false,
            vault_dialog: None,
            migrate_asked: false,
            keygen: None,
            confirm: None,
            pending_edit_save: None,
            show_sidebar: true,
            right_panel: RightPanel::None,
            multi_exec: false,
            last_activity: Instant::now(),
            status_msg: None,
            quick_connect: String::new(),
            session_filter: String::new(),
            selected_session: None,
            about_open: false,
            welcome: None,
            quit_confirmed: false,
            xserver,
            font_cache: None,
            font_scan_rx: None,
            cfg_dirty: None,
            applied_ui: None,
            caption: None,
            caption_pushes: 0,
            app_locked: app_lock_set,
            agent_guard: AgentGuard::default(),
            app_lock_set,
            app_lock_failures: 0,
            applock_dialog,
            history,
            server_info_open: false,
            palette: Palette::matcha(),
            theme_editor: None,
            clipboard_marker: None,
            egui_ctx: cc.egui_ctx.clone(),
            tail_viewers: Vec::new(),
            next_tail_id: 1,
            update_state: UpdateState::Idle,
            update_rx: None,
        };
        // Diagnostics / automation hooks (not persisted): force a theme, open a local shell.
        if let Ok(name) = std::env::var("RUXT_THEME").or_else(|_| std::env::var("RUSSHTERM_THEME")) {
            if let Some(theme) = Theme::from_env(&name) {
                app.cfg.theme = theme;
            }
        }
        app.apply_theme(&cc.egui_ctx);
        // Ctrl+wheel / Ctrl+plus/minus change the terminal font, not egui's global UI zoom.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        app.apply_ui_metrics(&cc.egui_ctx, true);
        if std::env::var_os("RUXT_OPEN_LOCAL")
            .or_else(|| std::env::var_os("RUSSHTERM_OPEN_LOCAL"))
            .is_some()
        {
            app.open_local_tab(&cc.egui_ctx);
        }
        if app.cfg.check_updates_on_startup && !cfg!(test) {
            app.start_update_check(&cc.egui_ctx);
        }
        app
    }

    fn start_update_check(&mut self, ctx: &egui::Context) {
        if matches!(self.update_state, UpdateState::Checking) {
            return;
        }
        self.update_state = UpdateState::Checking;
        self.update_rx = Some(update::spawn_check(ctx.clone()));
    }

    fn palette(&self) -> Palette {
        self.palette
    }

    /// Push the configured theme into egui and every open terminal.
    /// Install `cfg.ui_scale` (egui zoom factor) and `cfg.ui_font_size` (egui text styles,
    /// keeping their default ratios). Changes are held back while a mouse button is down so a
    /// settings slider does not jump away from the pointer mid-drag; `force` skips that wait.
    fn apply_ui_metrics(&mut self, ctx: &egui::Context, force: bool) {
        let scale = config::clamp_or(self.cfg.ui_scale, &config::UI_SCALE_RANGE, 1.0);
        let size = config::clamp_or(
            self.cfg.ui_font_size,
            &config::UI_FONT_SIZE_RANGE,
            config::UI_FONT_SIZE_DEFAULT,
        );
        if self.applied_ui == Some((scale, size)) || (!force && ctx.input(|i| i.pointer.any_down())) {
            return;
        }
        self.applied_ui = Some((scale, size));
        ctx.set_zoom_factor(scale);
        let ratio = size / config::UI_FONT_SIZE_DEFAULT;
        let defaults = egui::Style::default().text_styles;
        ctx.all_styles_mut(|style| {
            for (text_style, font) in style.text_styles.iter_mut() {
                if let Some(d) = defaults.get(text_style) {
                    font.size = (d.size * ratio * 2.0).round() / 2.0;
                }
            }
        });
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let spec = self.cfg.theme_spec();
        self.apply_spec(ctx, &spec);
    }

    /// Install a theme's colours into egui and every open terminal.
    fn apply_spec(&mut self, ctx: &egui::Context, spec: &ThemeSpec) {
        // egui keeps separate visuals per dark/light theme and picks one from the theme
        // preference, so pin the preference first and then install our visuals for it.
        let egui_theme = if spec.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(egui_theme);
        ctx.set_visuals_of(egui_theme, theme::chrome(spec.visuals()));
        let p = spec.palette();
        self.palette = p;
        for t in &self.tabs {
            t.term.lock().palette = p;
        }
        // `set_visuals_of` replaced the whole Visuals, radii and shadows included.
        kit::apply_style(ctx);
        self.note_caption(spec);
    }

    /// Remember title-bar colours. Painted for a few frames: the native theme command
    /// that hides the caption buttons is applied after this `ui` returns.
    fn note_caption(&mut self, spec: &ThemeSpec) {
        let next = caption_paint(spec);
        if self.caption != Some(next) {
            self.caption = Some(next);
            self.caption_pushes = 4;
        }
    }

    fn push_caption(&mut self, frame: &eframe::Frame) {
        if self.caption_pushes == 0 {
            return;
        }
        let Some(paint) = self.caption else { return };
        #[cfg(windows)]
        {
            let Some(hwnd) = window_hwnd(frame) else { return };
            ruxt_inputguard::set_caption_colors(hwnd, paint.dark, paint.bg, paint.fg, paint.border);
            self.caption_pushes -= 1;
        }
        #[cfg(not(windows))]
        {
            let _ = (frame, paint);
            self.caption_pushes = 0;
        }
    }

    /// The theme editor window with live preview.
    fn theme_editor_window(&mut self, ctx: &egui::Context) {
        let Some(ed) = self.theme_editor.as_mut() else { return };
        let res = ed.ui(ctx);
        if ed.changed {
            ed.changed = false;
            let spec = ed.spec.clone();
            self.apply_spec(ctx, &spec);
        }
        match res {
            ThemeEditorResult::Open => {}
            ThemeEditorResult::Cancel => {
                self.theme_editor = None;
                self.apply_theme(ctx);
            }
            ThemeEditorResult::Save => {
                let Some(ed) = self.theme_editor.as_mut() else { return };
                let mut spec = ed.spec.clone();
                spec.normalize_name();
                let clash = self
                    .cfg
                    .custom_themes
                    .iter()
                    .any(|t| t.name == spec.name && Some(&t.name) != ed.original.as_ref());
                if clash {
                    ed.error = Some(trf("A theme named '{name}' already exists", &[("{name}", &spec.name)]));
                    return;
                }
                match ed.original.clone() {
                    Some(orig) => {
                        if let Some(slot) = self.cfg.custom_themes.iter_mut().find(|t| t.name == orig) {
                            *slot = spec.clone();
                        } else {
                            self.cfg.custom_themes.push(spec.clone());
                        }
                    }
                    None => {
                        if self.cfg.custom_themes.len() >= color_theme::MAX_CUSTOM_THEMES {
                            ed.error = Some(trf(
                                "At most {n} custom themes; delete one first",
                                &[("{n}", &color_theme::MAX_CUSTOM_THEMES)],
                            ));
                            return;
                        }
                        self.cfg.custom_themes.push(spec.clone());
                    }
                }
                self.cfg.theme = Theme::Custom;
                self.cfg.custom_theme = spec.name.clone();
                self.theme_editor = None;
                self.save_cfg();
                self.apply_theme(ctx);
                audit::log("theme.saved", serde_json::json!({"name": spec.name}));
                self.flash(trf("Theme '{name}' saved", &[("{name}", &spec.name)]));
            }
        }
    }

    fn export_theme(&mut self, name: &str) {
        let Some(spec) = self.cfg.custom_themes.iter().find(|t| t.name == name).cloned() else {
            return;
        };
        if let Some(f) = rfd::FileDialog::new()
            .add_filter("Ruxt theme", &["json"])
            .set_file_name(spec.file_name())
            .save_file()
        {
            match std::fs::write(&f, spec.to_json()) {
                Ok(()) => self.flash(trf("Theme exported to {path}", &[("{path}", &f.display())])),
                Err(e) => self.flash(trf("Export failed: {e}", &[("{e}", &e)])),
            }
        }
    }

    fn import_theme(&mut self, ctx: &egui::Context) {
        let Some(f) = rfd::FileDialog::new().add_filter("Ruxt theme", &["json"]).pick_file() else {
            return;
        };
        let parsed = std::fs::read(&f)
            .map_err(anyhow::Error::from)
            .and_then(|b| ThemeSpec::from_json(&b));
        match parsed {
            Ok(mut spec) => {
                if self.cfg.custom_themes.len() >= color_theme::MAX_CUSTOM_THEMES {
                    self.flash(trf(
                        "At most {n} custom themes; delete one first",
                        &[("{n}", &color_theme::MAX_CUSTOM_THEMES)],
                    ));
                    return;
                }
                let taken: Vec<String> = self.cfg.custom_themes.iter().map(|t| t.name.clone()).collect();
                spec.name = color_theme::unique_name(&spec.name, &taken);
                self.cfg.theme = Theme::Custom;
                self.cfg.custom_theme = spec.name.clone();
                self.flash(trf("Theme '{name}' imported", &[("{name}", &spec.name)]));
                audit::log(
                    "theme.imported",
                    serde_json::json!({"name": spec.name, "path": f.display().to_string()}),
                );
                self.cfg.custom_themes.push(spec);
                self.save_cfg();
                self.apply_theme(ctx);
            }
            Err(e) => self.flash(trf("Import failed: {e}", &[("{e}", &format!("{e:#}"))])),
        }
    }

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    /// Status poll interval for a session: its own, else the global one.
    fn monitor_interval_for(&self, p: &SessionProfile) -> u64 {
        if p.monitor_interval_secs > 0 {
            p.monitor_interval_secs
        } else {
            self.cfg.monitor_interval_secs
        }
    }

    /// Change a per-connection preference on a tab's profile and, if it is a saved session,
    /// in the config too, so the next connection starts with it.
    fn update_session_pref(&mut self, idx: usize, f: impl Fn(&mut SessionProfile)) {
        let Some(p) = self.tabs.get_mut(idx).and_then(|t| t.profile.as_mut()) else {
            return;
        };
        f(p);
        let id = p.id.clone();
        if let Some(s) = self.cfg.sessions.iter_mut().find(|s| s.id == id) {
            f(s);
            self.save_cfg();
        }
    }

    /// Why passwords cannot be saved right now (`None`: they can).
    fn cred_note(&self) -> Option<&'static str> {
        match self.vault {
            VaultState::Unlocked(_) => None,
            VaultState::Absent => Some(tr(
                "Set a screen lock password (Settings → Security) to save passwords. They are encrypted with it.",
            )),
            VaultState::Foreign { .. } => Some(tr(
                "Passwords saved by an earlier version are not moved yet (Settings → Security).",
            )),
            VaultState::Locked => Some(tr("Saved passwords are not available.")),
        }
    }
    fn vault_mut(&mut self) -> Option<&mut Vault> {
        match &mut self.vault {
            VaultState::Unlocked(v) => Some(v),
            _ => None,
        }
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    /// Put `text` on the clipboard and arm the auto-clear timer (if configured).
    fn copy_to_clipboard(&mut self, ctx: &egui::Context, text: String) {
        ctx.copy_text(text.clone());
        self.clipboard_marker = Some((Instant::now(), text));
    }

    /// Clear the clipboard once the configured time has passed, but only if it still holds
    /// what Ruxt put there (never wipe something the user copied elsewhere).
    fn clipboard_tick(&mut self, ctx: &egui::Context) {
        let secs = self.cfg.clipboard_clear_secs;
        let Some((at, text)) = self.clipboard_marker.as_ref() else {
            return;
        };
        if secs == 0 {
            self.clipboard_marker = None;
            return;
        }
        let left = Duration::from_secs(secs).saturating_sub(at.elapsed());
        if !left.is_zero() {
            ctx.request_repaint_after(left);
            return;
        }
        let same = arboard::Clipboard::new()
            .and_then(|mut c| c.get_text())
            .map(|t| t.replace("\r\n", "\n") == text.replace("\r\n", "\n"))
            .unwrap_or(false);
        if same {
            if let Ok(mut c) = arboard::Clipboard::new() {
                let _ = c.clear();
            }
            self.flash(tr("clipboard cleared"));
        }
        self.clipboard_marker = None;
    }

    /// Mark the config dirty; it is written by `flush_cfg` once edits settle.
    fn save_cfg(&mut self) {
        if self.cfg_dirty.is_none() {
            self.cfg_dirty = Some(Instant::now());
        }
    }

    fn flush_cfg(&mut self, ctx: Option<&egui::Context>, force: bool) {
        let Some(since) = self.cfg_dirty else { return };
        if !force && since.elapsed() < CFG_FLUSH_DELAY {
            if let Some(ctx) = ctx {
                ctx.request_repaint_after(CFG_FLUSH_DELAY);
            }
            return;
        }
        self.cfg_dirty = None;
        if let Err(e) = self.cfg.save(&self.paths) {
            self.flash(format!("config save failed: {e}"));
        }
    }

    /// Kick off (or collect) the background system font scan used by the Settings window.
    fn ensure_font_scan(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.font_scan_rx {
            match rx.try_recv() {
                Ok(list) => {
                    self.font_cache = Some(list);
                    self.font_scan_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(200)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.font_scan_rx = None,
            }
            return;
        }
        if self.font_cache.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx2 = ctx.clone();
            let spawned = std::thread::Builder::new().name("font-scan".into()).spawn(move || {
                let _ = tx.send(fonts::scan_system_fonts());
                ctx2.request_repaint();
            });
            if spawned.is_ok() {
                self.font_scan_rx = Some(rx);
            }
        }
    }

    fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn open_ssh_tab(&mut self, profile: SessionProfile, ctx: &egui::Context, reuse: Option<usize>) {
        let size = reuse
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.term.lock().size)
            .unwrap_or(Size { cols: 100, rows: 30 });
        let (term, pty_rx) = TermState::with_palette(
            ctx.clone(),
            size,
            profile.scrollback,
            self.cfg.osc52_clipboard,
            self.palette(),
        );
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let secrets = match &self.vault {
            VaultState::Unlocked(v) => v.get(&profile.id).cloned(),
            _ => None,
        };
        // A reconnect gets a fresh id even when it reuses the tab slot, so late events from
        // the old session task (disconnect, SFTP listings) cannot land on the new session.
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        if let Some(i) = reuse {
            let old = self.tabs[i].id;
            self.prompts.retain(|(t, _)| *t != old);
        }
        // X11 forwarding needs a local X server: start the installed one if nothing listens.
        let mut profile = profile;
        if profile.x11_forwarding {
            if self.cfg.x11_auto_start {
                match self.xserver.ensure_running() {
                    Ok(Some(d)) => {
                        if profile.x11_display.trim().is_empty() {
                            profile.x11_display = format!(":{d}");
                        }
                        if self.xserver.firewall_rule() == Some(false) {
                            self.flash(tr("No Windows Firewall rule: the X server's TCP port (6000+n) is reachable from the network, protected by the cookie only."));
                        } else if self.xserver.is_managed_running() {
                            self.flash(trf("X server running on :{d}", &[("{d}", &d)]));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => self.flash(trf("X11: {e} — no X server (see the status bar)", &[("{e}", &e)])),
                }
            } else if !self.xserver.any_running() {
                self.flash(tr("X11 forwarding enabled but no local X server is running"));
            }
        }
        if self.cfg.sessions.iter().any(|s| s.id == profile.id) {
            self.cfg.touch_recent(&profile.id);
            self.save_cfg();
        }
        let jumps = self.jump_chain(&profile);
        let monitor_ctl = MonitorControl::new(
            self.cfg.server_monitor && profile.monitor,
            self.monitor_interval_for(&profile),
        );
        let args = SessionArgs {
            tab: id,
            profile: profile.clone(),
            secrets,
            jumps,
            term: term.clone(),
            pty_rx,
            ui: self.ui_tx.clone(),
            cmds: cmd_rx,
            ctx: ctx.clone(),
            hostkeys: self.hostkeys.clone(),
            allow_public_bind: self.cfg.allow_public_bind,
            monitor: monitor_ctl.clone(),
        };
        self.rt.spawn(ssh::run_session(args));
        let tunnels = profile.tunnels.iter().map(|_| TunnelStatus::Stopped).collect();
        let mut sftp = SftpPanelState::default();
        sftp.show_hidden = profile.sftp_show_hidden;
        let tab = Tab {
            id,
            title: profile.display_name(),
            kind: TabKind::Ssh { cmd_tx },
            term,
            view: TermView::new(id),
            connected: false,
            info: ConnInfo::default(),
            status: tr("connecting").into(),
            profile: Some(profile),
            sftp,
            tunnels,
            monitor: MonitorState::default(),
            monitor_ctl: Some(monitor_ctl),
            cmd: CmdLine::default(),
            cwd: None,
            prev_cwd: None,
            pending_pwd: None,
        };
        match reuse {
            Some(i) => {
                self.tabs[i] = tab;
                self.active = i;
            }
            None => {
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
            }
        }
    }

    fn open_local_tab(&mut self, ctx: &egui::Context) {
        let size = Size { cols: 100, rows: 30 };
        let (term, pty_rx) = TermState::with_palette(ctx.clone(), size, 10_000, false, self.palette());
        match LocalShell::spawn(&self.cfg.local_shell, size, term.clone(), ctx.clone(), pty_rx) {
            Ok(shell) => {
                let id = self.next_tab_id;
                self.next_tab_id += 1;
                audit::log("local.shell", serde_json::json!({"tab": id}));
                self.tabs.push(Tab {
                    id,
                    title: tr("Local").into(),
                    kind: TabKind::Local { shell },
                    term,
                    view: TermView::new(id),
                    connected: true,
                    info: ConnInfo::default(),
                    status: tr("local shell").into(),
                    profile: None,
                    sftp: SftpPanelState::default(),
                    tunnels: Vec::new(),
                    monitor: MonitorState::default(),
                    monitor_ctl: None,
                    cmd: CmdLine::default(),
                    cwd: crate::core::hostkeys::home_dir().map(|p| p.display().to_string()),
                    prev_cwd: None,
                    pending_pwd: None,
                });
                self.active = self.tabs.len() - 1;
            }
            Err(e) => self.flash(trf("cannot start local shell: {e}", &[("{e}", &format!("{e:#}"))])),
        }
    }

    fn close_tab(&mut self, id: TabId) {
        if let Some(i) = self.tab_index(id) {
            let tab = self.tabs.remove(i);
            match &tab.kind {
                TabKind::Ssh { cmd_tx } => {
                    let _ = cmd_tx.send(SessionCmd::Disconnect);
                }
                TabKind::Local { shell } => shell.shutdown(),
            }
            for tv in self.tail_viewers.iter_mut().filter(|v| v.tab_id == id && v.running) {
                tv.mark_closed(tr("tab closed").into());
            }
            self.prompts.retain(|(t, _)| *t != id);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
        }
    }

    /// The saved sessions to hop through before `profile`, first hop first (ProxyJump).
    /// Cycles and chains longer than four hops are cut.
    fn jump_chain(&self, profile: &SessionProfile) -> Vec<(SessionProfile, Option<crate::core::vault::SecretEntry>)> {
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(profile.id.clone());
        let mut cur = profile.jump.clone();
        while !cur.is_empty() && chain.len() < 4 && seen.insert(cur.clone()) {
            let Some(jp) = self.cfg.sessions.iter().find(|s| s.id == cur) else {
                break;
            };
            let secrets = match &self.vault {
                VaultState::Unlocked(v) => v.get(&jp.id).cloned(),
                _ => None,
            };
            cur = jp.jump.clone();
            chain.push((jp.clone(), secrets));
        }
        chain.reverse();
        chain
    }

    /// Import / export actions. Shown from the sidebar's overflow menu.
    fn import_export_bar(&mut self, ui: &mut egui::Ui) {
        let mut imported: Option<anyhow::Result<Vec<SessionProfile>>> = None;
        {
            ui.menu_button(tr("Import"), |ui| {
                if ui.button(tr("OpenSSH config (~/.ssh/config)…")).clicked() {
                    let mut d = rfd::FileDialog::new();
                    if let Some(p) = import::openssh_config_path() {
                        if let Some(dir) = p.parent() {
                            d = d.set_directory(dir);
                        }
                        d = d.set_file_name("config");
                    }
                    if let Some(f) = d.pick_file() {
                        imported = Some(import::import_openssh(&f, &self.cfg.sessions));
                    }
                    ui.close();
                }
                if ui.button(tr("PuTTY sessions")).clicked() {
                    imported = Some(import::import_putty());
                    ui.close();
                }
                if ui.button(tr("Ruxt JSON file…")).clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                        imported = Some(
                            std::fs::read(&f)
                                .map_err(Into::into)
                                .and_then(|b| import::import_json(&b)),
                        );
                    }
                    ui.close();
                }
            });
            if ui
                .button(tr("Export…"))
                .on_hover_text(tr("Export does not include passwords or passphrases."))
                .clicked()
            {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .set_file_name("ruxt-sessions.json")
                    .save_file()
                {
                    match import::export_json(&self.cfg.sessions).and_then(|b| Ok(std::fs::write(&f, b)?)) {
                        Ok(()) => {
                            audit::log(
                                "sessions.exported",
                                serde_json::json!({"path": f.display().to_string(), "count": self.cfg.sessions.len()}),
                            );
                            self.flash(trf(
                                "Exported {n} session(s) to {path}",
                                &[("{n}", &self.cfg.sessions.len()), ("{path}", &f.display())],
                            ));
                        }
                        Err(e) => self.flash(trf("Export failed: {e}", &[("{e}", &format!("{e:#}"))])),
                    }
                }
            }
        }
        if let Some(r) = imported {
            match r {
                Ok(list) => {
                    let new = import::merge_new(&self.cfg.sessions, list);
                    if new.is_empty() {
                        self.flash(tr("Nothing to import"));
                    } else {
                        let n = new.len();
                        self.cfg.sessions.extend(new);
                        self.save_cfg();
                        audit::log("sessions.imported", serde_json::json!({"count": n}));
                        self.flash(trf("Imported {n} session(s)", &[("{n}", &n)]));
                    }
                }
                Err(e) => self.flash(trf("Import failed: {e}", &[("{e}", &format!("{e:#}"))])),
            }
        }
    }

    fn connect_profile_by_id(&mut self, id: &str, ctx: &egui::Context) {
        if let Some(p) = self.cfg.sessions.iter().find(|s| s.id == id).cloned() {
            self.open_ssh_tab(p, ctx, None);
        }
    }

    fn quick_connect(&mut self, ctx: &egui::Context) {
        let s = self.quick_connect.trim().to_string();
        if s.is_empty() {
            return;
        }
        let (user, rest) = match s.split_once('@') {
            Some((u, r)) => (u.to_string(), r.to_string()),
            None => (String::new(), s.clone()),
        };
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() && !h.contains(':') => (h.to_string(), p.parse().unwrap()),
            _ => (rest.clone(), 22),
        };
        let profile = SessionProfile {
            host,
            port,
            username: user,
            ..Default::default()
        };
        self.open_ssh_tab(profile, ctx, None);
        self.quick_connect.clear();
    }

    // ------------------------------------------------------------------
    // events from sessions
    // ------------------------------------------------------------------

    fn drain_events(&mut self) {
        if let Some(rx) = &self.update_rx {
            if let Ok(state) = rx.try_recv() {
                self.update_rx = None;
                if let UpdateState::Available(ref info) = state {
                    self.flash(trf("Update available: v{ver}", &[("{ver}", &info.version)]));
                }
                self.update_state = state;
            }
        }
        while let Ok(ev) = self.ui_rx.try_recv() {
            match ev {
                UiEvent::Connected { tab, info } => {
                    if let Some(i) = self.tab_index(tab) {
                        self.tabs[i].connected = true;
                        self.tabs[i].info = info;
                        self.tabs[i].status = tr("connected").into();
                    }
                }
                UiEvent::Disconnected { tab, reason } => {
                    if let Some(i) = self.tab_index(tab) {
                        self.tabs[i].connected = false;
                        self.tabs[i].status = trf("closed: {reason}", &[("{reason}", &reason)]);
                        self.tabs[i].tunnels.fill(TunnelStatus::Stopped);
                        self.tabs[i].sftp.started = false;
                    }
                    for tv in self.tail_viewers.iter_mut().filter(|v| v.tab_id == tab && v.running) {
                        tv.mark_closed(reason.clone());
                    }
                    self.prompts.retain(|(t, _)| *t != tab);
                }
                UiEvent::Status { tab, msg } => {
                    if let Some(i) = self.tab_index(tab) {
                        self.tabs[i].status = msg.clone();
                        self.tabs[i].term.lock().print_local(&msg);
                    }
                }
                UiEvent::Prompt { tab, prompt } => {
                    if self.tab_index(tab).is_some() {
                        self.prompts.push_back((tab, prompt));
                        self.prompt_input = PromptInput::default();
                    }
                }
                UiEvent::Sftp { tab, ev } => {
                    if let Some(i) = self.tab_index(tab) {
                        match ev {
                            SftpEvent::UploadExists {
                                local,
                                remote_dir,
                                name,
                            } => {
                                if self.confirm.is_none() {
                                    self.confirm = Some(Confirm {
                                        title: tr("Replace remote file?").into(),
                                        message: trf(
                                            "'{name}' already exists in {remote_dir}.\nReplace it with {local}?",
                                            &[
                                                ("{name}", &name),
                                                ("{remote_dir}", &remote_dir),
                                                ("{local}", &local.display()),
                                            ],
                                        ),
                                        danger: true,
                                        dont_ask: None,
                                        action: ConfirmAction::SftpOverwriteUpload { tab, local, remote_dir },
                                    });
                                } else {
                                    self.tabs[i].sftp.apply(SftpEvent::Error(trf(
                                        "not uploaded: {name} exists (another dialog is open)",
                                        &[("{name}", &name)],
                                    )));
                                }
                            }
                            SftpEvent::SaveRequested { remote, name } => self.queue_edit_save(tab, remote, name),
                            SftpEvent::Home(h) => {
                                self.tabs[i].sftp.apply(SftpEvent::Home(h));
                                if let Some(cwd) = self.tabs[i].cwd.clone() {
                                    self.tabs[i].follow_sftp(&cwd, true);
                                }
                            }
                            SftpEvent::FollowFailed(path) => {
                                if self.tabs[i].cwd.as_deref() == Some(path.as_str()) {
                                    self.tabs[i].cwd = self.tabs[i].prev_cwd.take();
                                }
                                if self.tabs[i].sftp.follow_last.as_deref() == Some(path.as_str()) {
                                    self.tabs[i].sftp.follow_last = None;
                                }
                            }
                            other => self.tabs[i].sftp.apply(other),
                        }
                    }
                }
                UiEvent::Tunnel { tab, idx, ev } => {
                    if let Some(i) = self.tab_index(tab) {
                        if let Some(slot) = self.tabs[i].tunnels.get_mut(idx) {
                            *slot = match ev {
                                TunnelEvent::Started(d) => TunnelStatus::Running(d),
                                TunnelEvent::Stopped => TunnelStatus::Stopped,
                                TunnelEvent::Error(e) => TunnelStatus::Error(e),
                            };
                        }
                    }
                }
                UiEvent::SaveSecret {
                    session_id,
                    kind,
                    value,
                } => {
                    let exists = self.cfg.sessions.iter().any(|s| s.id == session_id);
                    if !exists {
                        self.flash(tr("Not saved: this is an unsaved quick-connect session"));
                        continue;
                    }
                    let r = match self.vault_mut() {
                        Some(v) => match kind {
                            SecretKind::Password => v.set_password(&session_id, Some(value)),
                            SecretKind::Passphrase => v.set_passphrase(&session_id, Some(value)),
                        },
                        None => Err(anyhow::anyhow!(tr("Saved passwords are not available."))),
                    };
                    match r {
                        Ok(()) => self.flash(tr("Password saved")),
                        Err(e) => self.flash(trf("Could not store credential: {e}", &[("{e}", &e)])),
                    }
                }
                UiEvent::AppLock(outcome) => self.on_applock_outcome(outcome),
                UiEvent::Monitor { tab, ev } => {
                    if let Some(i) = self.tab_index(tab) {
                        self.tabs[i].monitor.apply(ev);
                    }
                }
                UiEvent::Tail { tab, ev } => match ev {
                    TailEvent::Data { id, bytes } => {
                        if let Some(tv) = self.tail_viewers.iter_mut().find(|v| v.tab_id == tab && v.id == id) {
                            tv.push_bytes(&bytes);
                        }
                    }
                    TailEvent::Closed { id, reason } => {
                        if let Some(tv) = self.tail_viewers.iter_mut().find(|v| v.tab_id == tab && v.id == id) {
                            tv.mark_closed(reason);
                        }
                    }
                },
                UiEvent::VaultOp(outcome) => match outcome {
                    VaultOpOutcome::Migrated(vault) => {
                        if self.app_locked {
                            // Locked while the key was derived. The file is already re-encrypted,
                            // so the next unlock opens it; dropping `vault` wipes the key.
                            drop(vault);
                            continue;
                        }
                        self.vault = VaultState::Unlocked(vault);
                        self.vault_dialog = None;
                        self.open_history();
                        audit::log("vault.migrated", serde_json::json!({}));
                        self.flash(tr("Saved passwords moved to the screen lock password"));
                    }
                    VaultOpOutcome::Failed(e) => {
                        audit::log("vault.migrate_failed", serde_json::json!({}));
                        if let Some(d) = self.vault_dialog.as_mut() {
                            d.busy = false;
                            d.error = Some(e);
                            d.pw.zeroize();
                        }
                    }
                },
            }
        }
    }

    fn auto_lock(&mut self, ctx: &egui::Context) {
        let active = ctx.input(|i| !i.events.is_empty() || i.pointer.any_down());
        if active {
            self.last_activity = Instant::now();
        }
        let idle = self.cfg.idle_disconnect_minutes;
        if idle > 0 && self.last_activity.elapsed() > Duration::from_secs(idle * 60) {
            let mut n = 0;
            for t in self.tabs.iter().filter(|t| t.is_ssh() && t.connected) {
                if let TabKind::Ssh { cmd_tx } = &t.kind {
                    let _ = cmd_tx.send(SessionCmd::Disconnect);
                    n += 1;
                }
            }
            if n > 0 {
                audit::log(
                    "session.idle_disconnect",
                    serde_json::json!({"sessions": n, "minutes": idle}),
                );
                self.flash(trf(
                    "{n} session(s) disconnected after {m} idle minutes",
                    &[("{n}", &n), ("{m}", &idle)],
                ));
            }
        } else if idle > 0 {
            ctx.request_repaint_after(Duration::from_secs(30));
        }
        let app_minutes = applock::normalize_minutes(self.cfg.app_lock_minutes);
        if self.app_lock_set && app_minutes > 0 && !self.app_locked {
            if self.last_activity.elapsed() > Duration::from_secs(app_minutes * 60) {
                self.lock_app("idle");
            } else {
                ctx.request_repaint_after(Duration::from_secs(30));
            }
        }
    }

    /// Lock the window: hide everything, close the saved passwords, and show the unlock dialog.
    fn lock_app(&mut self, reason: &str) {
        if !self.app_lock_set || self.app_locked {
            return;
        }
        self.app_locked = true;
        // Dropping the store and the history wipes their keys and contents; unlocking opens
        // them again with the same password.
        if !matches!(self.vault, VaultState::Absent) {
            self.vault = VaultState::Locked;
        }
        self.history = History::locked();
        for t in &mut self.tabs {
            t.cmd.reset();
        }
        self.vault_dialog = None;
        self.settings_open = false;
        if let Some(ed) = self.editor.as_mut() {
            ed.wipe_secrets();
        }
        self.applock_dialog = Some(AppLockDialog::new(AppLockMode::Unlock));
        audit::log("app.lock", serde_json::json!({"reason": reason}));
    }

    /// Keep the agent guard in line with the config and lock the window when it dropped
    /// injected input.
    fn agent_guard_tick(&mut self, ctx: &egui::Context) {
        self.agent_guard.sync(self.cfg.agent_guard, ctx);
        if !agent_guard::take_tripped() {
            return;
        }
        audit::log("agent_guard.blocked", serde_json::json!({"locked": self.app_lock_set}));
        if self.app_lock_set {
            self.lock_app("agent_guard");
        } else if !self.app_locked {
            self.flash(tr(
                "Agent guard blocked automated input. Set a screen lock password to lock the window as well.",
            ));
        }
    }

    /// Flip the agent guard from the toolbar (or settings) and remember it.
    fn toggle_agent_guard(&mut self) {
        self.cfg.agent_guard = !self.cfg.agent_guard;
        self.save_cfg();
        audit::log("agent_guard.set", serde_json::json!({"on": self.cfg.agent_guard}));
        if self.cfg.agent_guard {
            self.flash(if self.app_lock_set {
                tr("Agent guard on: hidden from screen capture; automated input locks the window")
            } else {
                tr("Agent guard on: hidden from screen capture; automated input is blocked")
            });
        } else {
            self.flash(tr("Agent guard off"));
        }
    }

    /// Draw the app-lock dialog (lock screen or password management) and run its outcome.
    fn applock_modal(&mut self, ctx: &egui::Context) {
        let Some(d) = self.applock_dialog.as_mut() else { return };
        let mode = d.mode;
        match d.ui(ctx) {
            AppLockResult::Open => {}
            AppLockResult::Cancel => self.applock_dialog = None,
            AppLockResult::Skip => {
                self.applock_dialog = None;
                self.cfg.app_lock_prompted = true;
                self.cfg.vault_merge_prompted = true;
                self.cfg.app_lock_minutes = 0;
                self.save_cfg();
                audit::log("app.lock_declined", serde_json::json!({}));
            }
            AppLockResult::Submit { current, new } => {
                if let Some(d) = self.applock_dialog.as_mut() {
                    d.busy = true;
                    d.error = None;
                }
                let path = self.paths.applock_file();
                let vault_path = self.paths.vault_file();
                let ui_tx = self.ui_tx.clone();
                let ctx2 = ctx.clone();
                // Each failed attempt adds a second (capped) before the next check runs.
                let delay = Duration::from_secs(self.app_lock_failures.min(5) as u64);
                self.rt.spawn_blocking(move || {
                    let open = |pw: &SecretString| Vault::open_with(&vault_path, pw).map_err(|e| format!("{e:#}"));
                    let outcome = (|| -> anyhow::Result<AppLockOutcome> {
                        match mode {
                            AppLockMode::Setup { .. } => {
                                let new = new.as_ref().expect("new password");
                                applock::set_password(&path, new)?;
                                Ok(AppLockOutcome::Set { store: open(new) })
                            }
                            AppLockMode::Unlock => {
                                std::thread::sleep(delay);
                                let current = current.as_ref().expect("current password");
                                if !applock::verify(&path, current)? {
                                    return Ok(AppLockOutcome::WrongPassword {
                                        then: AppLockThen::Unlock,
                                    });
                                }
                                Ok(AppLockOutcome::Unlocked { store: open(current) })
                            }
                            AppLockMode::Change => {
                                std::thread::sleep(delay);
                                if !applock::verify(&path, current.as_ref().expect("current password"))? {
                                    return Ok(AppLockOutcome::WrongPassword {
                                        then: AppLockThen::Change,
                                    });
                                }
                                let new = new.as_ref().expect("new password");
                                applock::set_password(&path, new)?;
                                let (key, salt) = Vault::derive_new_master(new)?;
                                Ok(AppLockOutcome::Changed { key, salt })
                            }
                            AppLockMode::Remove => {
                                std::thread::sleep(delay);
                                if !applock::verify(&path, current.as_ref().expect("current password"))? {
                                    return Ok(AppLockOutcome::WrongPassword {
                                        then: AppLockThen::Remove,
                                    });
                                }
                                // Saved passwords first: nothing may stay saved without the lock.
                                Vault::delete(&vault_path)?;
                                applock::remove(&path)?;
                                Ok(AppLockOutcome::Removed)
                            }
                        }
                    })()
                    .unwrap_or_else(|e| AppLockOutcome::Failed(format!("{e:#}")));
                    let _ = ui_tx.send(UiEvent::AppLock(outcome));
                    ctx2.request_repaint();
                });
            }
        }
    }

    fn on_applock_outcome(&mut self, outcome: AppLockOutcome) {
        match outcome {
            AppLockOutcome::Set { store } => {
                self.applock_dialog = None;
                self.app_lock_set = true;
                self.app_lock_failures = 0;
                self.cfg.app_lock_prompted = true;
                self.cfg.vault_merge_prompted = true;
                if self.cfg.app_lock_minutes == 0 {
                    self.cfg.app_lock_minutes = applock::STEP_MINUTES;
                }
                self.save_cfg();
                audit::log("app.lock_password_set", serde_json::json!({}));
                self.flash(trf(
                    "Screen lock on: locks after {n} min of inactivity",
                    &[("{n}", &applock::normalize_minutes(self.cfg.app_lock_minutes))],
                ));
                self.take_store(store);
            }
            AppLockOutcome::Changed { key, salt } => {
                self.applock_dialog = None;
                self.app_lock_failures = 0;
                audit::log("app.lock_password_changed", serde_json::json!({}));
                let r = match &mut self.vault {
                    VaultState::Unlocked(v) => v.apply_new_master(key, salt),
                    VaultState::Foreign { .. } => {
                        // The old file is still to be moved; move it under the new password.
                        self.vault = VaultState::Foreign { key, salt };
                        Ok(())
                    }
                    _ => Ok(()),
                };
                match r {
                    Ok(()) => self.flash(tr("Screen lock password changed")),
                    Err(e) => self.flash(trf("Saved passwords: {e}", &[("{e}", &format!("{e:#}"))])),
                }
            }
            AppLockOutcome::Removed => {
                self.applock_dialog = None;
                self.app_lock_set = false;
                self.app_lock_failures = 0;
                self.vault = VaultState::Absent;
                // Its key went with the store: close the history (the writer is joined, so the
                // file is closed) and delete it.
                self.history = History::locked();
                if let Err(e) = crate::core::history::delete_file(&self.paths.history_file()) {
                    self.flash(trf("Command history: {e}", &[("{e}", &format!("{e:#}"))]));
                }
                self.vault_dialog = None;
                audit::log("app.lock_removed", serde_json::json!({}));
                self.flash(tr("Screen lock removed; saved passwords deleted"));
            }
            AppLockOutcome::Unlocked { store } => {
                self.app_lock_failures = 0;
                self.app_locked = false;
                self.applock_dialog = None;
                self.last_activity = Instant::now();
                audit::log("app.unlock", serde_json::json!({}));
                self.take_store(store);
            }
            AppLockOutcome::WrongPassword { then } => {
                self.app_lock_failures = self.app_lock_failures.saturating_add(1);
                audit::log(
                    "app.unlock_failed",
                    serde_json::json!({"for": format!("{then:?}"), "failures": self.app_lock_failures}),
                );
                if let Some(d) = self.applock_dialog.as_mut() {
                    d.busy = false;
                    d.error = Some(tr("Wrong password").into());
                    d.clear_fields();
                }
            }
            AppLockOutcome::Failed(e) => {
                if let Some(d) = self.applock_dialog.as_mut() {
                    d.busy = false;
                    d.error = Some(e);
                    d.clear_fields();
                }
            }
        }
    }

    /// Open the encrypted command history with the key kept in the saved-password store.
    fn open_history(&mut self) {
        let VaultState::Unlocked(v) = &mut self.vault else {
            return;
        };
        let r = v
            .history_key()
            .and_then(|key| History::open(&self.paths.history_file(), self.cfg.history_days, key));
        self.history = match r {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("command history unavailable: {e:#}");
                audit::log("history.unavailable", serde_json::json!({"error": format!("{e:#}")}));
                History::locked()
            }
        };
    }

    /// Install the saved-password store opened with the screen-lock password.
    fn take_store(&mut self, store: Result<Opened, String>) {
        match store {
            Ok(Opened::Open(v)) => {
                self.vault = VaultState::Unlocked(v);
                self.open_history();
            }
            Ok(Opened::Foreign { key, salt }) => {
                self.vault = VaultState::Foreign { key, salt };
                if !std::mem::replace(&mut self.migrate_asked, true) {
                    self.vault_dialog = Some(MigrateDialog::new());
                }
            }
            Err(e) => {
                self.vault = VaultState::Locked;
                audit::log("vault.open_failed", serde_json::json!({"error": e}));
                self.flash(trf("Saved passwords are not available: {e}", &[("{e}", &e)]));
            }
        }
    }

    // ------------------------------------------------------------------
    // UI pieces
    // ------------------------------------------------------------------

    /// Start or stop the X server Ruxt owns. An external display is left alone.
    fn toggle_xserver(&mut self) {
        self.xserver.refresh(false);
        if self.xserver.is_managed_running() {
            self.xserver.stop();
            self.flash(tr("X server stopped"));
        } else if self.xserver.any_running() {
            self.flash(x_state_label(self.xserver.state()));
        } else if self.xserver.installed.is_some() {
            match self.xserver.start() {
                Ok(d) => self.flash_xserver_started(d),
                Err(e) => self.flash(trf("X server: {e}", &[("{e}", &format!("{e:#}"))])),
            }
        } else {
            self.flash(tr(
                "No X server found: the bundled Cygwin/X tree (xserver\\bin\\XWin.exe) is missing.",
            ));
        }
    }

    /// Started, or started without the firewall block rule.
    fn flash_xserver_started(&mut self, display: u16) {
        if self.xserver.firewall_rule() == Some(false) {
            self.flash(tr("No Windows Firewall rule: the X server's TCP port (6000+n) is reachable from the network, protected by the cookie only."));
        } else {
            self.flash(trf("X server started on :{d}", &[("{d}", &display)]));
        }
    }

    /// The X server menu that used to live on the toolbar. Right-click the status item.
    fn xserver_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(300.0);
        match &self.xserver.installed {
            Some(i) => {
                ui.label(RichText::new(format!("{}: {}", xserver::SERVER_NAME, i.path.display())).small());
            }
            None => {
                ui.label(
                    RichText::new(tr(
                        "No X server found: the bundled Cygwin/X tree (xserver\\bin\\XWin.exe) is missing.",
                    ))
                    .small(),
                );
            }
        }
        if self.xserver.listening.is_empty() {
            ui.label(RichText::new(tr("No display is listening on 127.0.0.1:6000-6009.")).small());
        } else {
            let l: Vec<String> = self.xserver.listening.iter().map(|d| format!(":{d}")).collect();
            ui.label(RichText::new(trf("Listening displays: {list}", &[("{list}", &l.join(" "))])).small());
        }
        if let Some(e) = &self.xserver.last_error {
            ui.colored_label(Color32::from_rgb(0xe0, 0x60, 0x60), RichText::new(e).small());
        }
        if self.xserver.firewall_rule() == Some(false) {
            ui.colored_label(
                Color32::from_rgb(0xe0, 0xa0, 0x40),
                RichText::new(tr("No Windows Firewall rule: the X server's TCP port (6000+n) is reachable from the network, protected by the cookie only.")).small(),
            );
            if ui
                .button(tr("Add firewall block rule (administrator prompt)…"))
                .on_hover_text(tr("Adds an inbound block rule for the X server executable. Loopback traffic is never filtered, so X11 forwarding keeps working while the port is closed to the LAN."))
                .clicked()
            {
                if let Some(i) = &self.xserver.installed {
                    match xserver::add_firewall_rule(&i.path) {
                        Ok(()) => self.flash(tr("Firewall rule added")),
                        Err(e) => self.flash(trf("Firewall rule: {e}", &[("{e}", &format!("{e:#}"))])),
                    }
                    self.xserver.recheck_firewall();
                }
                ui.close();
            }
        }
        ui.separator();
        if self.xserver.is_managed_running() {
            if ui.button(tr("Stop X server")).clicked() {
                self.xserver.stop();
                self.flash(tr("X server stopped"));
                ui.close();
            }
        } else if ui
            .add_enabled(
                self.xserver.installed.is_some(),
                egui::Button::new(tr("Start X server (random cookie, multiwindow)")),
            )
            .on_hover_text(tr(
                "Launches the installed server with a freshly generated MIT-MAGIC-COOKIE-1 authority file so only Ruxt's X11 bridge can connect.",
            ))
            .clicked()
        {
            match self.xserver.start() {
                Ok(d) => self.flash_xserver_started(d),
                Err(e) => self.flash(trf("X server: {e}", &[("{e}", &format!("{e:#}"))])),
            }
            ui.close();
        }
        if ui.button(tr("Refresh")).clicked() {
            self.xserver.refresh(true);
        }
        if ui.button(tr("Get an X server…")).clicked() {
            let _ = open::that(xserver::DOWNLOAD_URL);
            ui.close();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let show = self.cfg.toolbar;
        let has_tab = !self.tabs.is_empty();
        let search_open = self.active_tab().map(|t| t.view.search_open).unwrap_or(false);
        let has_ssh = self.active_tab().map(|t| t.is_ssh()).unwrap_or(false);
        ui.spacing_mut().item_spacing.x = 2.0;
        // Right cluster is placed first; the nested left-to-right layout takes the rest.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if show.help
                && kit::toggle(ui, self.about_open, theme::icon::QUESTION)
                    .on_hover_text(tr("Help & about"))
                    .clicked()
            {
                self.about_open = !self.about_open;
            }
            if show.settings
                && kit::toggle(ui, self.settings_open, theme::label(theme::icon::GEAR, tr("Settings"))).clicked()
            {
                self.settings_open = !self.settings_open;
            }
            if show.lock
                && self.app_lock_set
                && theme::ghost(ui, theme::icon::LOCK, tr("Lock"))
                    .on_hover_text(tr("Lock Ruxt now (screen lock password required to continue)"))
                    .clicked()
            {
                self.lock_app("manual");
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if show.sessions
                    && theme::accent(ui, theme::icon::PLUS, tr("Sessions"))
                        .on_hover_text(tr("Sessions"))
                        .clicked()
                {
                    self.show_sidebar = !self.show_sidebar;
                }
                if show.new_session
                    && theme::ghost(ui, theme::icon::PLUS, theme::without_plus(tr("+ New")))
                        .on_hover_text(tr("New session profile"))
                        .clicked()
                {
                    self.editor = Some(SessionEditor::new(SessionProfile::default(), true, false, false));
                }
                if show.local_shell
                    && theme::ghost(ui, theme::icon::TERMINAL_WINDOW, tr("Local shell"))
                        .on_hover_text(tr("Local shell tab"))
                        .clicked()
                {
                    self.open_local_tab(ctx);
                }
                if show.quick_connect {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.quick_connect)
                            .hint_text("user@host:port")
                            .desired_width(180.0),
                    );
                    if (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.quick_connect(ctx);
                    }
                }
                if show.find
                    && ui
                        .add_enabled_ui(has_tab, |ui| {
                            kit::toggle(
                                ui,
                                search_open,
                                theme::label(theme::icon::MAGNIFYING_GLASS, tr("Find")),
                            )
                        })
                        .inner
                        .on_hover_text(tr("Find in terminal scrollback (Ctrl+Shift+F)"))
                        .clicked()
                {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.view.toggle_search();
                    }
                }
                if show.sftp
                    && ui
                        .add_enabled_ui(has_ssh, |ui| {
                            kit::toggle(
                                ui,
                                self.right_panel == RightPanel::Sftp,
                                theme::label(theme::icon::FOLDER, "SFTP"),
                            )
                        })
                        .inner
                        .clicked()
                {
                    self.right_panel = if self.right_panel == RightPanel::Sftp {
                        RightPanel::None
                    } else {
                        RightPanel::Sftp
                    };
                }
                if show.tunnels
                    && ui
                        .add_enabled_ui(has_ssh, |ui| {
                            kit::toggle(
                                ui,
                                self.right_panel == RightPanel::Tunnels,
                                theme::label(theme::icon::ARROWS_LEFT_RIGHT, tr("Tunnels")),
                            )
                        })
                        .inner
                        .clicked()
                {
                    self.right_panel = if self.right_panel == RightPanel::Tunnels {
                        RightPanel::None
                    } else {
                        RightPanel::Tunnels
                    };
                }
                if show.broadcast
                    && kit::toggle(
                        ui,
                        self.multi_exec,
                        theme::label(theme::icon::BROADCAST, tr("Broadcast input")),
                    )
                    .on_hover_text(tr("Type into all SSH tabs at once"))
                    .clicked()
                {
                    self.multi_exec = !self.multi_exec;
                }
                if show.agent_guard
                    && agent_guard::SUPPORTED
                    && kit::toggle(
                        ui,
                        self.cfg.agent_guard,
                        theme::label(theme::icon::SHIELD, tr("Agent guard")),
                    )
                    .on_hover_text(tr("Hide Ruxt from screenshots, recording and screen sharing, and block keyboard / mouse input sent by programs (AI agents, macros). Blocked input locks the window when a screen lock password is set.\n\nAlso blocks password-manager auto-type and on-screen keyboards. Not a hard boundary: programs running as your Windows user can still read Ruxt's files and memory."))
                    .clicked()
                {
                    self.toggle_agent_guard();
                }
                if show.keygen && theme::ghost(ui, theme::icon::KEY, tr("Keygen")).clicked() {
                    self.keygen = Some(KeygenDialog::new());
                }
                let drawn = show.sessions
                    || show.new_session
                    || show.local_shell
                    || show.quick_connect
                    || show.find
                    || show.sftp
                    || show.tunnels
                    || show.broadcast
                    || (show.agent_guard && agent_guard::SUPPORTED)
                    || show.keygen
                    || (show.lock && self.app_lock_set)
                    || show.settings
                    || show.help;
                if !drawn {
                    ui.label(RichText::new(tr("Right-click to choose toolbar buttons")).weak());
                }
            });
        });
        // The bar's own response covers buttons and the empty stretch, so a right-click
        // anywhere on it opens the menu (a button's own click does not swallow it).
        ui.response().container_context_menu(|ui| self.toolbar_menu(ui));
    }

    /// Right-click menu: show or hide each toolbar button. The choice is saved.
    fn toolbar_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(200.0);
        let mut changed = false;
        {
            let t = &mut self.cfg.toolbar;
            changed |= ui.checkbox(&mut t.sessions, tr("Sessions")).changed();
            changed |= ui.checkbox(&mut t.new_session, tr("+ New")).changed();
            changed |= ui.checkbox(&mut t.local_shell, tr("Local shell")).changed();
            changed |= ui.checkbox(&mut t.quick_connect, tr("Quick connect")).changed();
            changed |= ui.checkbox(&mut t.find, tr("Find")).changed();
            changed |= ui.checkbox(&mut t.sftp, tr("SFTP")).changed();
            changed |= ui.checkbox(&mut t.tunnels, tr("Tunnels")).changed();
            changed |= ui.checkbox(&mut t.broadcast, tr("Broadcast input")).changed();
            if agent_guard::SUPPORTED {
                changed |= ui.checkbox(&mut t.agent_guard, tr("Agent guard")).changed();
            }
            changed |= ui.checkbox(&mut t.x11, tr("X server")).changed();
            changed |= ui.checkbox(&mut t.keygen, tr("Keygen")).changed();
            changed |= ui.checkbox(&mut t.lock, tr("Lock")).changed();
            changed |= ui.checkbox(&mut t.settings, tr("Settings")).changed();
            changed |= ui.checkbox(&mut t.help, tr("Help & about")).changed();
            ui.separator();
            if ui.button(tr("Show all")).clicked() {
                *t = ToolbarItems::default();
                changed = true;
                ui.close();
            }
        }
        if changed {
            self.save_cfg();
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.session_filter)
                    .hint_text(tr("filter"))
                    .desired_width(f32::INFINITY),
            );
        });
        ui.separator();
        let filter = self.session_filter.to_lowercase();
        let mut groups: Vec<String> = self.cfg.sessions.iter().map(|s| s.group.clone()).collect();
        groups.sort();
        groups.dedup();
        let mut to_connect: Option<String> = None;
        let mut to_edit: Option<String> = None;
        let mut to_delete: Option<String> = None;
        let mut to_dup: Option<String> = None;
        let footer_h = ui.spacing().interact_size.y + 6.0;
        let list_h = (ui.available_height() - footer_h).max(32.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(list_h)
            .show(ui, |ui| {
                if filter.is_empty() && !self.cfg.recent.is_empty() {
                    egui::CollapsingHeader::new(tr("Recent"))
                        .id_salt("recent-sessions")
                        .default_open(true)
                        .show(ui, |ui| {
                            for id in &self.cfg.recent {
                                let Some(s) = self.cfg.sessions.iter().find(|s| &s.id == id) else {
                                    continue;
                                };
                                let r = theme::session_row(ui, false, &s.display_name()).on_hover_text(trf(
                                    "{user}@{host}:{port}\ndouble-click to connect",
                                    &[("{user}", &s.username), ("{host}", &s.host), ("{port}", &s.port)],
                                ));
                                if r.double_clicked() {
                                    to_connect = Some(s.id.clone());
                                } else if r.clicked() {
                                    self.selected_session = Some(s.id.clone());
                                }
                            }
                        });
                }
                for g in groups {
                    let header = if g.is_empty() {
                        tr("(ungrouped)").to_string()
                    } else {
                        g.clone()
                    };
                    egui::CollapsingHeader::new(header).default_open(true).show(ui, |ui| {
                        for s in self.cfg.sessions.iter().filter(|s| s.group == g) {
                            let name = s.display_name();
                            if !filter.is_empty()
                                && !name.to_lowercase().contains(&filter)
                                && !s.host.to_lowercase().contains(&filter)
                                && !s.tags.to_lowercase().contains(&filter)
                                && !s.group.to_lowercase().contains(&filter)
                            {
                                continue;
                            }
                            let selected = self.selected_session.as_deref() == Some(&s.id);
                            let r = theme::session_row(ui, selected, &name).on_hover_text(trf(
                                "{user}@{host}:{port}\ndouble-click to connect",
                                &[("{user}", &s.username), ("{host}", &s.host), ("{port}", &s.port)],
                            ));
                            if r.clicked() {
                                self.selected_session = Some(s.id.clone());
                            }
                            if r.double_clicked() {
                                to_connect = Some(s.id.clone());
                            }
                            r.context_menu(|ui| {
                                if ui.button(tr("Connect")).clicked() {
                                    to_connect = Some(s.id.clone());
                                    ui.close();
                                }
                                if ui.button(tr("Edit…")).clicked() {
                                    to_edit = Some(s.id.clone());
                                    ui.close();
                                }
                                if ui.button(tr("Duplicate")).clicked() {
                                    to_dup = Some(s.id.clone());
                                    ui.close();
                                }
                                if ui.button(tr("Delete…")).clicked() {
                                    to_delete = Some(s.id.clone());
                                    ui.close();
                                }
                            });
                        }
                    });
                }
                if self.cfg.sessions.is_empty() {
                    ui.label(RichText::new(tr("No sessions yet.\nUse + New or the quick-connect box.")).weak());
                }
            });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let _ = ui
                .menu_button(theme::icon::DOTS_THREE_VERTICAL, |ui| {
                    ui.set_min_width(240.0);
                    self.import_export_bar(ui);
                })
                .response
                .on_hover_text(tr("Import"));
        });
        if let Some(id) = to_connect {
            self.connect_profile_by_id(&id, ctx);
        }
        if let Some(id) = to_edit {
            if let Some(p) = self.cfg.sessions.iter().find(|s| s.id == id).cloned() {
                let (hpw, hpp) = match &self.vault {
                    VaultState::Unlocked(v) => v
                        .get(&id)
                        .map(|e| (e.password.is_some(), e.passphrase.is_some()))
                        .unwrap_or((false, false)),
                    _ => (false, false),
                };
                self.editor = Some(SessionEditor::new(p, false, hpw, hpp));
            }
        }
        if let Some(id) = to_dup {
            if let Some(mut p) = self.cfg.sessions.iter().find(|s| s.id == id).cloned() {
                p.id = crate::core::config::new_id();
                p.name = trf("{} (copy)", &[("{}", &p.display_name())]);
                self.cfg.sessions.push(p);
                self.save_cfg();
            }
        }
        if let Some(id) = to_delete {
            let name = self
                .cfg
                .sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.display_name())
                .unwrap_or_default();
            self.confirm = Some(Confirm {
                title: tr("Delete session").into(),
                message: trf(
                    "Delete the session profile '{name}' and any stored credentials for it?",
                    &[("{name}", &name)],
                ),
                danger: true,
                dont_ask: None,
                action: ConfirmAction::DeleteSession(id),
            });
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut close: Option<TabId> = None;
        let mut add_local = false;
        let plus = ui.spacing().interact_size.y;
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let scroll_w = (ui.available_width() - plus - 4.0).max(0.0);
            egui::ScrollArea::horizontal()
                .id_salt("tabbar")
                .max_width(scroll_w)
                .auto_shrink([true, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (i, tab) in self.tabs.iter_mut().enumerate() {
                            if let Some(t) = tab.term.lock().take_title() {
                                if !t.is_empty() {
                                    tab.title = t;
                                }
                            }
                            let taken_cwd = tab.term.lock().take_cwd();
                            if let Some(cwd) = taken_cwd {
                                tab.set_terminal_cwd(cwd, false);
                            }
                            if let Some((cmd_line, started_at)) = tab.pending_pwd {
                                if started_at.elapsed() > Duration::from_secs(3) {
                                    tab.pending_pwd = None;
                                } else {
                                    let outcome = {
                                        let t = tab.term.lock();
                                        if t.closed || t.term.mode().contains(TermMode::ALT_SCREEN) {
                                            Some(None)
                                        } else {
                                            let mut line_idx = cmd_line + 1;
                                            let mut found = None;
                                            let mut checked = 0usize;
                                            while checked < 4 {
                                                let Some((text, next_idx)) = t.logical_line_at(line_idx) else {
                                                    break;
                                                };
                                                line_idx = next_idx;
                                                checked += 1;
                                                if let Some(p) = term::extract_pwd_output(&text) {
                                                    found = Some(p);
                                                    break;
                                                }
                                            }
                                            if found.is_some() {
                                                Some(found)
                                            } else if checked >= 4 {
                                                Some(None)
                                            } else {
                                                None
                                            }
                                        }
                                    };
                                    if let Some(res) = outcome {
                                        tab.pending_pwd = None;
                                        if let Some(path) = res {
                                            tab.set_terminal_cwd(path, true);
                                        }
                                    }
                                }
                            }
                            let title = truncate(&tab.title, 28);
                            let (r, closed) = theme::tab(ui, &title, i == self.active, tab.connected);
                            if closed || r.middle_clicked() {
                                close = Some(tab.id);
                            } else if r.clicked() {
                                self.active = i;
                            }
                        }
                    });
                });
            if theme::icon_button(ui, theme::icon::PLUS)
                .on_hover_text(tr("Local shell tab"))
                .clicked()
            {
                add_local = true;
            }
        });
        if add_local {
            self.open_local_tab(ctx);
        }
        if let Some(id) = close {
            self.request_close_tab(id);
        }
    }

    fn request_close_tab(&mut self, id: TabId) {
        let connected = self
            .tabs
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.connected && t.is_ssh())
            .unwrap_or(false);
        if connected && self.cfg.confirm_close_session {
            self.confirm = Some(Confirm {
                dont_ask: Some(false),
                ..Confirm::new(
                    tr("Close tab"),
                    tr("This session is still connected. Disconnect and close it?"),
                    false,
                    ConfirmAction::CloseTab(id),
                )
            });
        } else {
            self.close_tab(id);
        }
    }

    fn terminal_area(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.tabs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(tr(
                        "Ruxt\n\nOpen a session from the sidebar, use quick-connect, or start a local shell.",
                    ))
                    .weak(),
                );
            });
            return;
        }
        let modal_open =
            !self.prompts.is_empty() || self.vault_dialog.is_some() || self.confirm.is_some() || self.welcome.is_some();
        let want_focus = !modal_open && self.editor.is_none() && !self.settings_open && self.keygen.is_none();
        let font_size = self.cfg.font_size;
        let copy_on_select = self.cfg.copy_on_select;
        let active = self.active;
        self.update_completions(active);
        let out = {
            let tab = &mut self.tabs[active];
            tab.view.show(ui, &tab.term, font_size, want_focus, copy_on_select)
        };
        if let Some(i) = out.accept {
            self.accept_completion(active, i);
        }
        if out.cycle != 0 {
            let c = &mut self.tabs[active].cmd;
            if !c.candidates.is_empty() {
                let n = c.candidates.len() as i32;
                c.idx = ((c.idx as i32 + out.cycle).rem_euclid(n)) as usize;
            }
        }
        if out.dismiss {
            let c = &mut self.tabs[active].cmd;
            c.suppressed = Some(c.prefix.clone());
        }
        if out.zoom != 0 {
            self.cfg.font_size = config::clamp_or(
                self.cfg.font_size + out.zoom as f32,
                &config::TERM_FONT_SIZE_RANGE,
                14.0,
            );
            self.save_cfg();
            self.flash(trf(
                "font size {size}",
                &[("{size}", &format!("{:.0}", self.cfg.font_size))],
            ));
        }
        if let Some(size) = out.resized {
            self.tabs[active].resize(size);
        }
        if out.bell {
            self.flash(tr("bell"));
        }
        if let Some(text) = out.copy {
            if !text.is_empty() {
                self.copy_to_clipboard(ctx, text);
                self.flash(tr("copied"));
            }
        }
        let osc52 = self.tabs[active].term.lock().take_clipboard_store();
        if let Some(text) = osc52 {
            self.copy_to_clipboard(ctx, text);
            self.flash(tr("remote program wrote the clipboard (OSC 52)"));
        }
        if !out.input.is_empty() {
            let closed = self.tabs[active].term.lock().closed;
            if closed {
                if out.input.contains(&b'\r') {
                    if let Some(p) = self.tabs[active].profile.clone() {
                        self.open_ssh_tab(p, ctx, Some(active));
                    } else {
                        self.close_tab(self.tabs[active].id);
                        self.open_local_tab(ctx);
                    }
                }
            } else {
                self.send_tracked(active, out.input);
            }
        }
        if out.paste {
            self.paste_into(active, ctx);
        }
        if let Some(text) = out.paste_text {
            self.paste_text_into(active, text);
        }
    }

    /// egui-winit's own Ctrl+C/X/V translation is switched off (`clipboard_shortcuts: false`)
    /// because inside a terminal Ctrl+C must reach the remote shell. That also silenced every
    /// other widget: text fields (session editor, quick-connect, SFTP address bar, prompts)
    /// never received a paste. Re-create the translation here for whatever has keyboard focus
    /// as long as it is not a terminal. The injected events are consumed by the focused
    /// `TextEdit` exactly as if egui-winit had produced them.
    fn inject_clipboard_shortcuts(&mut self, ctx: &egui::Context) {
        let Some(focused) = ctx.memory(|m| m.focused()) else {
            return;
        };
        if self.tabs.iter().any(|t| t.view.id() == focused) {
            return;
        }
        let (mut paste, mut copy, mut cut) = (false, false, false);
        ctx.input(|i| {
            for ev in &i.events {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers: m,
                    ..
                } = ev
                else {
                    continue;
                };
                let cmd = m.command && !m.alt;
                match key {
                    egui::Key::V if cmd => paste = true,
                    egui::Key::Insert if m.shift && !m.command => paste = true,
                    egui::Key::Paste => paste = true,
                    egui::Key::C if cmd => copy = true,
                    egui::Key::Insert if m.command && !m.shift => copy = true,
                    egui::Key::Copy => copy = true,
                    egui::Key::X if cmd => cut = true,
                    egui::Key::Delete if m.shift && !m.command => cut = true,
                    egui::Key::Cut => cut = true,
                    _ => {}
                }
            }
        });
        if !(paste || copy || cut) {
            return;
        }
        let text = if paste {
            match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                Ok(t) => {
                    let t = t.replace("\r\n", "\n");
                    if t.is_empty() {
                        None
                    } else {
                        Some(t)
                    }
                }
                Err(e) => {
                    self.flash(trf("clipboard: {e}", &[("{e}", &e)]));
                    None
                }
            }
        } else {
            None
        };
        ctx.input_mut(|i| {
            if cut {
                i.events.push(egui::Event::Cut);
            }
            if copy {
                i.events.push(egui::Event::Copy);
            }
            if let Some(t) = text {
                i.events.push(egui::Event::Paste(t));
            }
        });
    }

    /// Host name used to rank history entries for a tab.
    fn history_host(tab: &Tab) -> String {
        tab.profile
            .as_ref()
            .map(|p| p.host.trim().to_lowercase())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "local".into())
    }

    /// Refresh the completion candidates of tab `idx` from what is typed on its grid.
    fn update_completions(&mut self, idx: usize) {
        let tab = &mut self.tabs[idx];
        let track_dir = tab.is_ssh() && tab.connected;
        let typed: Option<String> = if !self.cfg.cmd_history && !self.cfg.tail_viewer && !track_dir {
            None
        } else {
            let t = tab.term.lock();
            let alt = t.term.mode().contains(TermMode::ALT_SCREEN);
            if t.closed || alt {
                None
            } else {
                tab.cmd.start.and_then(|(l, c)| t.line_text_from(l, c, true))
            }
        };
        match typed {
            None => {
                // The line being tracked is gone (new prompt, screen cleared, alt screen).
                if tab.cmd.start.is_some() {
                    tab.cmd.reset();
                }
            }
            Some(text) => {
                let prefix = text.trim_start().to_string();
                if prefix != tab.cmd.prefix {
                    let host = Self::history_host(tab);
                    tab.cmd.prefix = prefix.clone();
                    tab.cmd.idx = 0;
                    tab.cmd.candidates = if prefix.is_empty() || !self.cfg.cmd_history {
                        Vec::new()
                    } else {
                        self.history.suggest(&prefix, &host, terminal_widget::MAX_CANDIDATES)
                    };
                    if tab.cmd.suppressed.as_deref() != Some(prefix.as_str()) {
                        tab.cmd.suppressed = None;
                    }
                }
            }
        }
        let tab = &mut self.tabs[idx];
        if tab.cmd.visible() {
            let plen = tab.cmd.prefix.len();
            tab.view.candidates = tab.cmd.candidates.clone();
            tab.view.cand_idx = tab.cmd.idx.min(tab.cmd.candidates.len().saturating_sub(1));
            tab.view.ghost = tab
                .cmd
                .candidates
                .get(tab.view.cand_idx)
                .and_then(|c| c.get(plen..))
                .filter(|g| !g.is_empty())
                .map(str::to_string);
        } else {
            tab.view.candidates.clear();
            tab.view.cand_idx = 0;
            tab.view.ghost = None;
        }
    }

    /// Send the remainder of candidate `i` to the shell as if it had been typed.
    fn accept_completion(&mut self, idx: usize, i: usize) {
        let suffix = {
            let c = &self.tabs[idx].cmd;
            c.candidates
                .get(i)
                .and_then(|cand| cand.get(c.prefix.len()..))
                .map(str::to_string)
        };
        if let Some(suffix) = suffix {
            if !suffix.is_empty() {
                self.send_tracked(idx, suffix.into_bytes());
            }
        }
    }

    /// Send typed bytes to the tab and keep the input-line tracker in step: a line starts at
    /// the first key, Enter records what the grid shows for it, Ctrl+C / Ctrl+D abandon it.
    fn send_tracked(&mut self, idx: usize, bytes: Vec<u8>) {
        let track_dir = self.tabs[idx].is_ssh() && self.tabs[idx].connected;
        if self.cfg.cmd_history || self.cfg.tail_viewer || track_dir {
            let tab = &mut self.tabs[idx];
            let (closed, alt) = {
                let t = tab.term.lock();
                (t.closed, t.term.mode().contains(TermMode::ALT_SCREEN))
            };
            if !closed && !alt {
                if tab.cmd.start.is_none() {
                    tab.cmd.start = Some(tab.term.lock().cursor_abs());
                }
                tab.cmd.feed_bytes(&bytes);
                if bytes.ends_with(b"\r") {
                    // Characters sent in the same frame as Enter have not been echoed yet. A
                    // multi-line paste (control characters before the final Enter) submits
                    // several lines at once; those are not recorded.
                    let unechoed = std::str::from_utf8(&bytes[..bytes.len() - 1])
                        .ok()
                        .filter(|s| !s.chars().any(char::is_control));
                    let grid_line = if let Some(unechoed) = unechoed {
                        tab.cmd
                            .start
                            .and_then(|(l, c)| tab.term.lock().line_text_from(l, c, true))
                            .map(|t| format!("{}{unechoed}", t.trim_end()))
                    } else {
                        None
                    };
                    let typed_line = tab.cmd.typed.clone();
                    let host = Self::history_host(tab);

                    if self.cfg.tail_viewer && tab.connected {
                        let tail_spec = grid_line
                            .as_deref()
                            .and_then(tail::parse_tail_cmd)
                            .or_else(|| tail::parse_tail_cmd(&typed_line));
                        if let Some(spec) = tail_spec {
                            if self.cfg.cmd_history {
                                self.history.record(&spec.raw_cmd, &host);
                            }
                            self.tabs[idx].cmd.reset();
                            // Erase the typed command from the shell prompt so the prompt stays
                            // clean and ready for the next command.
                            let clear_seq: Vec<u8> = if self.tabs[idx].is_ssh() || !cfg!(windows) {
                                b"\x05\x15".to_vec() // Ctrl+E + Ctrl+U
                            } else {
                                b"\x1b".to_vec() // Escape in PowerShell / cmd.exe
                            };
                            self.broadcast_input(idx, clear_seq);
                            self.open_tail_viewer(idx, spec);
                            return;
                        }
                    }

                    if track_dir {
                        let raw_cmd = grid_line
                            .as_deref()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or(&typed_line);
                        let dir_cmds = term::parse_shell_dir_cmds(raw_cmd);
                        if !dir_cmds.is_empty() {
                            let tab = &mut self.tabs[idx];
                            let mut cur = tab.effective_cwd();
                            let mut prev = tab.prev_cwd.clone();
                            let home = tab.sftp.home.clone();
                            let mut cd_target = None;
                            let mut has_pwd = false;
                            for c in dir_cmds {
                                match c {
                                    ShellDirCmd::Cd(arg) => {
                                        if let Some(next) = term::resolve_cd_path(
                                            cur.as_deref(),
                                            prev.as_deref(),
                                            home.as_deref(),
                                            arg.as_deref(),
                                        ) {
                                            prev = cur;
                                            cur = Some(next.clone());
                                            cd_target = Some(next);
                                        }
                                    }
                                    ShellDirCmd::Pwd => {
                                        has_pwd = true;
                                    }
                                }
                            }
                            if let Some(next) = cd_target {
                                tab.set_terminal_cwd(next, true);
                            }
                            if has_pwd {
                                let (cmd_line, _) = tab.term.lock().cursor_abs();
                                tab.pending_pwd = Some((cmd_line, Instant::now()));
                            }
                        }
                    }

                    if self.cfg.cmd_history {
                        if let Some(line) = grid_line {
                            self.history.record(&line, &host);
                        }
                    }
                    self.tabs[idx].cmd.reset();
                } else if bytes.contains(&0x03) || bytes.contains(&0x04) {
                    tab.cmd.reset();
                    tab.pending_pwd = None;
                }
            }
        }
        self.broadcast_input(idx, bytes);
    }

    fn open_tail_viewer(&mut self, tab_idx: usize, mut spec: TailSpec) {
        let Some(tab) = self.tabs.get(tab_idx) else { return };
        if !tab.connected {
            return;
        }
        if spec.cwd.is_none() {
            spec.cwd = tab.effective_cwd();
        }
        let id = self.next_tail_id;
        self.next_tail_id += 1;
        let tab_id = tab.id;
        let session_title = tab.title.clone();
        let raw_cmd = spec.raw_cmd.clone();

        let local_handle = match &tab.kind {
            TabKind::Ssh { cmd_tx } => {
                let _ = cmd_tx.send(SessionCmd::StartTail { id, spec: spec.clone() });
                None
            }
            TabKind::Local { .. } => Some(tail::spawn_local_tail(
                tab_id,
                id,
                spec.clone(),
                self.ui_tx.clone(),
                self.egui_ctx.clone(),
            )),
        };

        audit::log(
            "tailviewer.start",
            serde_json::json!({"tab": tab_id, "id": id, "cmd": raw_cmd}),
        );
        self.tail_viewers
            .push(TailViewer::new(id, tab_id, session_title, spec, local_handle));
        self.flash(trf("Tail Viewer: {cmd}", &[("{cmd}", &raw_cmd)]));
    }

    fn stop_tail_viewer(&mut self, id: TailId) {
        let Some(tv) = self.tail_viewers.iter_mut().find(|v| v.id == id) else {
            return;
        };
        let tab_id = tv.tab_id;
        tv.local_handle = None;
        if tv.running {
            tv.mark_closed(tr("stopped").into());
        }
        if let Some(tab_idx) = self.tab_index(tab_id) {
            if let TabKind::Ssh { cmd_tx } = &self.tabs[tab_idx].kind {
                let _ = cmd_tx.send(SessionCmd::StopTail { id });
            }
        }
    }

    fn restart_tail_viewer(&mut self, id: TailId) {
        let Some(pos) = self.tail_viewers.iter().position(|v| v.id == id) else {
            return;
        };
        let tab_id = self.tail_viewers[pos].tab_id;
        let spec = self.tail_viewers[pos].spec.clone();
        let Some(tab_idx) = self.tab_index(tab_id) else {
            self.tail_viewers[pos].mark_closed(tr("session closed").into());
            return;
        };
        if !self.tabs[tab_idx].connected {
            self.tail_viewers[pos].mark_closed(tr("disconnected").into());
            return;
        }
        let local_handle = match &self.tabs[tab_idx].kind {
            TabKind::Ssh { cmd_tx } => {
                let _ = cmd_tx.send(SessionCmd::StartTail { id, spec: spec.clone() });
                None
            }
            TabKind::Local { .. } => Some(tail::spawn_local_tail(
                tab_id,
                id,
                spec,
                self.ui_tx.clone(),
                self.egui_ctx.clone(),
            )),
        };
        let tv = &mut self.tail_viewers[pos];
        tv.clear();
        tv.running = true;
        tv.paused = false;
        tv.status = tr("streaming").into();
        tv.local_handle = local_handle;
    }

    fn close_tail_viewer(&mut self, id: TailId) {
        self.stop_tail_viewer(id);
        self.tail_viewers.retain(|v| v.id != id);
    }

    fn tail_viewers_ui(&mut self, ctx: &egui::Context) {
        if self.tail_viewers.is_empty() {
            return;
        }
        let font_size = self.cfg.font_size;
        let mut actions = Vec::new();
        for tv in &mut self.tail_viewers {
            let a = tv.show(ctx, font_size);
            if !matches!(a, TailViewerAction::None) {
                actions.push(a);
            }
        }
        for a in actions {
            match a {
                TailViewerAction::None => {}
                TailViewerAction::Stop(id) => self.stop_tail_viewer(id),
                TailViewerAction::Restart(id) => self.restart_tail_viewer(id),
                TailViewerAction::Close(id) => self.close_tail_viewer(id),
                TailViewerAction::Copy(text) => {
                    self.copy_to_clipboard(ctx, text);
                    self.flash(tr("copied"));
                }
            }
        }
    }

    fn broadcast_input(&mut self, origin: usize, bytes: Vec<u8>) {
        if self.multi_exec && self.tabs[origin].is_ssh() {
            for t in self.tabs.iter().filter(|t| t.is_ssh() && t.connected) {
                t.send_input(bytes.clone());
            }
        } else {
            self.tabs[origin].send_input(bytes);
        }
    }

    fn paste_into(&mut self, idx: usize, _ctx: &egui::Context) {
        let text = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => t,
            Err(e) => {
                self.flash(trf("clipboard: {e}", &[("{e}", &e)]));
                return;
            }
        };
        self.paste_text_into(idx, text);
    }

    fn paste_text_into(&mut self, idx: usize, text: String) {
        if text.is_empty() {
            return;
        }
        let multiline = text.contains('\n') || text.contains('\r');
        if multiline && self.cfg.confirm_multiline_paste {
            let preview: String = text.chars().take(400).collect();
            self.confirm = Some(Confirm {
                title: tr("Paste multi-line text?").into(),
                message: trf(
                    "The clipboard contains {n} line(s). Each newline will be sent as Enter.\n\n{preview}{more}",
                    &[
                        ("{n}", &text.lines().count()),
                        ("{preview}", &preview),
                        ("{more}", &if text.chars().count() > 400 { "\n…" } else { "" }),
                    ],
                ),
                danger: true,
                dont_ask: None,
                action: ConfirmAction::Paste {
                    tab: self.tabs[idx].id,
                    text,
                },
            });
        } else {
            self.do_paste(self.tabs[idx].id, &text);
        }
    }

    fn do_paste(&mut self, tab: TabId, text: &str) {
        if let Some(i) = self.tab_index(tab) {
            let bracketed = self.tabs[i].term.lock().term.mode().contains(TermMode::BRACKETED_PASTE);
            let bytes = terminal_widget::sanitize_paste(text, bracketed);
            self.send_tracked(i, bytes);
        }
    }

    fn right_panel_ui(&mut self, ui: &mut egui::Ui) {
        let active = self.active;
        // The "hidden items" view choice belongs to the session: keep it with the profile.
        let hidden = self
            .tabs
            .get(active)
            .and_then(|t| t.profile.as_ref().map(|p| (p.sftp_show_hidden, t.sftp.show_hidden)));
        if let Some((saved, now)) = hidden {
            if saved != now {
                self.update_session_pref(active, |p| p.sftp_show_hidden = now);
            }
        }
        let Some(tab) = self.tabs.get_mut(active) else { return };
        if !tab.is_ssh() {
            ui.label(tr("Not an SSH tab."));
            return;
        }
        match self.right_panel {
            RightPanel::Sftp => {
                let actions = tab.sftp.ui(ui, tab.connected);
                let tab_id = tab.id;
                let editor = self.cfg.external_editor.clone();
                let mut confirm = None;
                let mut flash = None;
                let mut tail_file = None;
                for a in actions {
                    match a {
                        SftpAction::Cmd(cmd) => {
                            if let TabKind::Ssh { cmd_tx } = &tab.kind {
                                let _ = cmd_tx.send(SessionCmd::Sftp(cmd));
                            }
                        }
                        SftpAction::ConfirmDelete(items) => {
                            let message = if items.len() == 1 {
                                let (item, name) = &items[0];
                                let what = if item.is_symlink {
                                    tr(" (symbolic link: only the link is removed, the target stays)")
                                } else if item.is_dir {
                                    tr(" and everything inside it")
                                } else {
                                    ""
                                };
                                trf(
                                    "'{name}'{what}: delete permanently?",
                                    &[("{name}", &name), ("{what}", &what)],
                                )
                            } else {
                                let names: Vec<&str> = items.iter().take(15).map(|(_, n)| n.as_str()).collect();
                                let extra = if items.len() > 15 {
                                    trf("\n… and {n} more", &[("{n}", &(items.len() - 15))])
                                } else {
                                    String::new()
                                };
                                trf(
                                    "Permanently delete {n} selected items?\n\n{names}{extra}",
                                    &[
                                        ("{n}", &items.len()),
                                        ("{names}", &names.join("\n")),
                                        ("{extra}", &extra),
                                    ],
                                )
                            };
                            let batch: Vec<DeleteItem> = items.into_iter().map(|(item, _)| item).collect();
                            confirm = Some(Confirm {
                                title: tr("Delete remote item(s)").into(),
                                message,
                                danger: true,
                                dont_ask: None,
                                action: ConfirmAction::SftpDelete {
                                    tab: tab_id,
                                    items: batch,
                                },
                            });
                        }
                        SftpAction::ConfirmOverwrite { cmds, names } => {
                            let shown: Vec<&str> = names.iter().take(15).map(String::as_str).collect();
                            let extra = if names.len() > 15 {
                                trf("\n… and {n} more", &[("{n}", &(names.len() - 15))])
                            } else {
                                String::new()
                            };
                            confirm = Some(Confirm {
                                title: tr("Replace local file(s)?").into(),
                                message: trf(
                                    "{n} item(s) already exist in the destination folder and will be replaced:\n\n{names}{extra}",
                                    &[("{n}", &names.len()), ("{names}", &shown.join("\n")), ("{extra}", &extra)],
                                ),
                                danger: true,
                                dont_ask: None,
                    action: ConfirmAction::SftpOverwriteDownload { tab: tab_id, cmds },
                            });
                        }
                        SftpAction::OpenInEditor(path) => {
                            if let TabKind::Ssh { cmd_tx } = &tab.kind {
                                let _ = cmd_tx.send(SessionCmd::Sftp(SftpCmd::EditRemote {
                                    remote: path.clone(),
                                    editor: editor.clone(),
                                }));
                                flash = Some(trf(
                                    "Opening '{path}' in the local editor. Ruxt asks before uploading it.",
                                    &[("{path}", &crate::core::term::sanitize_title(&path))],
                                ));
                            }
                        }
                        SftpAction::OpenInTailViewer(path) => {
                            tail_file = Some(path);
                        }
                    }
                }
                if confirm.is_some() {
                    self.confirm = confirm;
                }
                if let Some(f) = flash {
                    self.flash(f);
                }
                if let Some(path) = tail_file {
                    self.open_tail_viewer(active, TailSpec::for_file(path, None));
                }
            }
            RightPanel::Tunnels => {
                ui.label(RichText::new(tr("Tunnels")).size(15.0).strong());
                kit::divider(ui);
                let Some(profile) = tab.profile.as_mut() else { return };
                let mut start: Option<(usize, TunnelSpec)> = None;
                let mut stop: Option<usize> = None;
                let mut changed = false;
                egui::Grid::new("tunnels-grid")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        for (i, spec) in profile.tunnels.iter().enumerate() {
                            ui.label(spec.describe());
                            match tab.tunnels.get(i) {
                                Some(TunnelStatus::Running(d)) => {
                                    ui.colored_label(Color32::from_rgb(0x60, 0xc0, 0x60), tr("running"))
                                        .on_hover_text(d);
                                    if ui.button(tr("Stop")).clicked() {
                                        stop = Some(i);
                                    }
                                }
                                Some(TunnelStatus::Error(e)) => {
                                    ui.colored_label(Color32::from_rgb(0xe0, 0x60, 0x60), tr("error"))
                                        .on_hover_text(e);
                                    if ui.add_enabled(tab.connected, egui::Button::new(tr("Retry"))).clicked() {
                                        start = Some((i, spec.clone()));
                                    }
                                }
                                _ => {
                                    ui.label(tr("stopped"));
                                    if ui.add_enabled(tab.connected, egui::Button::new(tr("Start"))).clicked() {
                                        start = Some((i, spec.clone()));
                                    }
                                }
                            }
                            ui.end_row();
                        }
                    });
                if profile.tunnels.is_empty() {
                    ui.label(RichText::new(tr("No tunnels. Add some below or in the session editor.")).weak());
                }
                ui.separator();
                if ui.button(tr("+ Add tunnel")).clicked() {
                    profile.tunnels.push(TunnelSpec::default());
                    tab.tunnels.push(TunnelStatus::Stopped);
                    changed = true;
                }
                if let Some(last) = profile.tunnels.last_mut() {
                    ui.label(RichText::new(tr("Edit last tunnel:")).small());
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::from_id_salt("tp-kind")
                            .selected_text(format!("{:?}", last.kind))
                            .show_ui(ui, |ui| {
                                changed |= ui
                                    .selectable_value(
                                        &mut last.kind,
                                        crate::core::config::TunnelKind::Local,
                                        tr("Local"),
                                    )
                                    .changed();
                                changed |= ui
                                    .selectable_value(
                                        &mut last.kind,
                                        crate::core::config::TunnelKind::Remote,
                                        tr("Remote"),
                                    )
                                    .changed();
                                changed |= ui
                                    .selectable_value(
                                        &mut last.kind,
                                        crate::core::config::TunnelKind::Dynamic,
                                        tr("Dynamic"),
                                    )
                                    .changed();
                            });
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut last.bind_addr).desired_width(90.0))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut last.bind_port).range(1..=65535))
                            .changed();
                        ui.label("->");
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut last.target_host).desired_width(110.0))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut last.target_port).range(1..=65535))
                            .changed();
                        changed |= ui.checkbox(&mut last.auto_start, tr("auto")).changed();
                    });
                }
                let profile_snapshot = profile.clone();
                if let TabKind::Ssh { cmd_tx } = &tab.kind {
                    if let Some((idx, spec)) = start {
                        let _ = cmd_tx.send(SessionCmd::StartTunnel { idx, spec });
                    }
                    if let Some(idx) = stop {
                        let _ = cmd_tx.send(SessionCmd::StopTunnel { idx });
                    }
                }
                if changed {
                    if let Some(s) = self.cfg.sessions.iter_mut().find(|s| s.id == profile_snapshot.id) {
                        s.tunnels = profile_snapshot.tunnels.clone();
                        self.save_cfg();
                    }
                }
            }
            RightPanel::None => {}
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        self.xserver.refresh(false);
        let mut open_info = false;
        let mut toggle_x = false;
        let show_x = self.cfg.toolbar.x11;
        let x_label = x_state_label(self.xserver.state());
        let x_tip = if self.xserver.is_managed_running() {
            tr("Stop X server")
        } else {
            tr("Start X server (random cookie, multiwindow)")
        };
        let text_color = ui.visuals().text_color();
        let warn = ui.visuals().warn_fg_color;
        ui.spacing_mut().item_spacing.x = 10.0;
        // X server is the rightmost item. The flash sits just to its left.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if show_x {
                let r = theme::status_button(ui, &x_label).on_hover_text(x_tip);
                if r.clicked() {
                    toggle_x = true;
                }
                r.context_menu(|ui| self.xserver_menu(ui));
            }
            if let UpdateState::Available(info) = &self.update_state {
                let label = trf("Update v{ver}", &[("{ver}", &info.version)]);
                if theme::status_button(ui, &label)
                    .on_hover_text(tr("New version available — click to open the release page"))
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(info.url.clone()));
                }
            }
            if let Some((msg, at)) = &self.status_msg {
                if at.elapsed() < Duration::from_secs(6) {
                    theme::status_text(ui, msg, text_color);
                }
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if let Some(tab) = self.tabs.get(self.active) {
                    let connected = tab.connected;
                    let status = tab.status.clone();
                    let (cols, rows) = {
                        let t = tab.term.lock();
                        (t.size.cols, t.size.rows)
                    };
                    let ssh = tab.is_ssh() && connected;
                    let monitoring = tab.monitor.unavailable.is_none();
                    let enabled = tab.monitor_ctl.as_ref().map(|c| c.is_enabled()).unwrap_or(false);
                    let usages: Vec<(&str, f32)> = if ssh && monitoring {
                        tab.monitor
                            .last
                            .as_ref()
                            .map(|s| {
                                let mut v = Vec::new();
                                if let Some(c) = s.cpu_pct {
                                    v.push(("CPU", c));
                                }
                                if let Some(m) = s.mem_used_pct() {
                                    v.push(("MEM", m));
                                }
                                if let Some(d) = s.disks.first() {
                                    v.push(("disk", d.used_pct()));
                                }
                                v
                            })
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    // Meters replace the one-line summary. Load-only or not-yet samples still
                    // use the text button so the details click stays available.
                    let monitor_text = if ssh && usages.is_empty() {
                        Some(match tab.monitor.summary().filter(|_| monitoring) {
                            Some(s) => (s, tr("Server status (click for details)")),
                            None if monitoring && enabled => {
                                (tr("server: …").to_string(), tr("Waiting for the first status sample"))
                            }
                            None => (tr("server info").to_string(), tr("Click for connection details")),
                        })
                    } else {
                        None
                    };
                    let broadcast = self.multi_exec;
                    theme::status_dot(ui, if connected { theme::CONNECTED } else { theme::IDLE });
                    theme::status_text(ui, &status, text_color);
                    theme::status_text(ui, &format!("{cols}×{rows}"), text_color);
                    if !usages.is_empty()
                        && theme::status_usages(ui, &usages)
                            .on_hover_text(tr("Server status (click for details)"))
                            .clicked()
                    {
                        open_info = true;
                    } else if let Some((text, tip)) = monitor_text {
                        if theme::status_button(ui, &text).on_hover_text(tip).clicked() {
                            open_info = true;
                        }
                    }
                    if broadcast {
                        theme::status_text(ui, tr("BROADCAST INPUT"), warn);
                    }
                } else if self.multi_exec {
                    theme::status_text(ui, tr("BROADCAST INPUT"), warn);
                }
            });
        });
        if toggle_x {
            self.toggle_xserver();
        }
        if open_info {
            self.server_info_open = true;
        }
    }

    /// Connection details plus the live server status of the active tab.
    fn server_info_window(&mut self, ctx: &egui::Context) {
        if !self.server_info_open {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            self.server_info_open = false;
            return;
        };
        let mut open = true;
        let mut set_enabled: Option<bool> = None;
        let mut interval = tab
            .monitor_ctl
            .as_ref()
            .map(|c| c.interval_secs())
            .unwrap_or(self.cfg.monitor_interval_secs);
        let interval_before = interval;
        let mut retry = false;
        let ctl_enabled = tab.monitor_ctl.as_ref().map(|c| c.is_enabled()).unwrap_or(false);
        let jump_name = tab.profile.as_ref().and_then(|p| {
            if p.jump.is_empty() {
                None
            } else {
                self.cfg
                    .sessions
                    .iter()
                    .find(|s| s.id == p.jump)
                    .map(|s| s.display_name())
            }
        });
        kit::window(ctx, tr("Server info"), egui::Id::unique("server-info"), 600.0)
            .open(&mut open)
            .show(ctx, |ui| {
                if !tab.is_ssh() {
                    ui.label(tr("Not an SSH tab."));
                    return;
                }
                kit::body(ui, ctx, |ui| {
                kit::section(ui, tr("Connection"), |ui| {
                egui::Grid::new("server-conn-grid").num_columns(2).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                    ui.label(tr("Session"));
                    ui.label(&tab.title);
                    ui.end_row();
                    if let Some(p) = &tab.profile {
                        ui.label(tr("Address"));
                        ui.label(RichText::new(format!("{}@{}:{}", p.username, p.host, p.port)).monospace());
                        ui.end_row();
                        ui.label(tr("Security profile"));
                        ui.label(format!("{:?}", p.security));
                        ui.end_row();
                        if let Some(j) = &jump_name {
                            ui.label(tr("Jump host"));
                            ui.label(j);
                            ui.end_row();
                        }
                        ui.label(tr("Forwarding"));
                        let mut f: Vec<&str> = Vec::new();
                        if p.x11_forwarding {
                            f.push(if p.x11_trusted { "X11 (trusted, -Y)" } else { "X11 (untrusted, -X)" });
                        }
                        if p.agent_forwarding {
                            f.push("agent");
                        }
                        let running = tab.tunnels.iter().filter(|t| matches!(t, TunnelStatus::Running(_))).count();
                        let tunnels = trf("{running}/{total} tunnels running", &[("{running}", &running), ("{total}", &p.tunnels.len())]);
                        f.push(&tunnels);
                        ui.label(f.join(" · "));
                        ui.end_row();
                    }
                    ui.label(tr("Status"));
                    ui.label(&tab.status);
                    ui.end_row();
                });
                });
                kit::section(ui, tr("Server status"), |ui| {
                ui.horizontal_wrapped(|ui| {
                    let mut on = ctl_enabled;
                    if ui.checkbox(&mut on, tr("monitor")).on_hover_text(tr("Runs a small read-only script on the server (hostname, uptime, /proc/stat, /proc/meminfo, df, /proc/net/dev) over the existing SSH session.")).changed() {
                        set_enabled = Some(on);
                    }
                    ui.label(tr("every"));
                    ui.add(egui::DragValue::new(&mut interval).range(monitor::INTERVAL_RANGE).suffix(" s"));
                    if let Some(at) = tab.monitor.at {
                        ui.label(RichText::new(trf("updated {n} s ago", &[("{n}", &at.elapsed().as_secs())])).weak().small());
                    }
                });
                if let Some(e) = &tab.monitor.unavailable {
                    ui.colored_label(Color32::from_rgb(0xe0, 0xa0, 0x40), trf("Monitoring stopped: {e}", &[("{e}", e)]));
                    if ui.button(tr("Retry")).clicked() {
                        retry = true;
                    }
                } else if let Some(e) = &tab.monitor.error {
                    ui.colored_label(Color32::from_rgb(0xe0, 0xa0, 0x40), trf("Last poll failed: {e}", &[("{e}", e)]));
                }
                match &tab.monitor.last {
                    None => {
                        ui.label(RichText::new(tr("No sample yet.")).weak());
                    }
                    Some(s) => server_status_ui(ui, s, &tab.monitor),
                }
                });
                });
            });
        self.server_info_open = open;
        // Both choices are per connection: they apply to this tab and are kept with its session.
        if interval != interval_before {
            if let Some(c) = self.tabs.get(self.active).and_then(|t| t.monitor_ctl.as_ref()) {
                c.set_interval(interval);
            }
            self.update_session_pref(self.active, |p| p.monitor_interval_secs = interval);
        }
        if let Some(on) = set_enabled {
            if let Some(t) = self.tabs.get_mut(self.active) {
                if let Some(c) = &t.monitor_ctl {
                    c.set_enabled(on);
                }
                if !on {
                    t.monitor.error = None;
                }
            }
            self.update_session_pref(self.active, |p| p.monitor = on);
        }
        if retry {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.monitor.unavailable = None;
                t.monitor.error = None;
                if let Some(c) = &t.monitor_ctl {
                    c.set_enabled(true);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // dialogs
    // ------------------------------------------------------------------

    fn prompt_modal(&mut self, ctx: &egui::Context) {
        let Some((tab_id, _)) = self.prompts.front() else {
            return;
        };
        let tab_id = *tab_id;
        let kind = self.prompts.front().map(|(_, p)| p.kind.clone()).unwrap();
        let tab_title = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        let cred_note = self.cred_note();
        let can_remember = cred_note.is_none()
            && self
                .tabs
                .iter()
                .find(|t| t.id == tab_id)
                .and_then(|t| t.profile.as_ref())
                .map(|p| self.cfg.sessions.iter().any(|s| s.id == p.id))
                .unwrap_or(false);
        let mut reply: Option<PromptReply> = None;
        let mut copied: Option<String> = None;
        let input = &mut self.prompt_input;
        let modal = egui::Modal::new(egui::Id::unique(("prompt", tab_id))).show(ctx, |ui| {
            kit::modal_body(ui, ctx, 540.0, |ui| {
            ui.label(RichText::new(trf("Session: {title}", &[("{title}", &tab_title)])).weak().small());
            match &kind {
                PromptKind::HostKeyUnknown { host, port, algorithm, fingerprint, randomart } => {
                    ui.heading(tr("Unknown host key"));
                    ui.label(trf("The authenticity of host {host}:{port} can't be established.", &[("{host}", host), ("{port}", port)]));
                    ui.label(trf("{algorithm} key fingerprint:", &[("{algorithm}", algorithm)]));
                    ui.label(RichText::new(fingerprint).monospace().strong());
                    ui.label(RichText::new(randomart).monospace());
                    ui.label(tr("Verify this fingerprint out-of-band (e.g. ssh-keygen -lf on the server). It will be pinned for future connections."));
                    kit::actions(ui, |ui| {
                        if kit::primary(ui, tr("Trust and connect")).clicked() {
                            reply = Some(PromptReply::Accept);
                        }
                        if kit::text_button(ui, tr("Cancel")).clicked() {
                            reply = Some(PromptReply::Reject);
                        }
                        if kit::outlined(ui, tr("copy fingerprint")).clicked() {
                            copied = Some(fingerprint.clone());
                        }
                    });
                }
                PromptKind::HostKeyChanged { host, port, algorithm, fingerprint, randomart, file, line } => {
                    ui.heading(RichText::new(tr("⚠ HOST KEY HAS CHANGED")).color(Color32::from_rgb(0xe0, 0x50, 0x50)));
                    ui.label(RichText::new(tr("Someone could be eavesdropping on you right now (man-in-the-middle attack), or the host key was legitimately replaced.")).strong());
                    ui.label(trf("Host: {host}:{port}\nNew {algorithm} fingerprint:", &[("{host}", host), ("{port}", port), ("{algorithm}", algorithm)]));
                    ui.label(RichText::new(fingerprint).monospace().strong());
                    ui.label(RichText::new(randomart).monospace());
                    ui.label(trf("Offending entry: {file} line {line}", &[("{file}", &file.display()), ("{line}", line)]));
                    ui.checkbox(&mut input.ack, tr("I have verified the new fingerprint out-of-band and understand the risk"));
                    kit::actions(ui, |ui| {
                        if ui.add_enabled_ui(input.ack, |ui| kit::danger(ui, tr("Replace pinned key and connect"))).inner.clicked() {
                            reply = Some(PromptReply::Accept);
                        }
                        if kit::primary(ui, tr("Cancel (recommended)")).clicked() {
                            reply = Some(PromptReply::Reject);
                        }
                    });
                }
                PromptKind::Password { user, host, attempt } => {
                    ui.heading(tr("Password"));
                    let attempt_txt = if *attempt > 1 { trf(" (attempt {n}/3)", &[("{n}", attempt)]) } else { String::new() };
                    ui.label(trf("{user}@{host}'s password{attempt}:", &[("{user}", user), ("{host}", host), ("{attempt}", &attempt_txt)]));
                    let r = ui.add(egui::TextEdit::singleline(&mut input.secret).password(true).desired_width(f32::INFINITY));
                    if !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                        r.request_focus();
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_enabled(can_remember, egui::Checkbox::new(&mut input.remember, tr("Remember (encrypted with the screen lock password)")))
                        .on_disabled_hover_text(cred_note.unwrap_or(tr("Only saved sessions can remember passwords")));
                    kit::actions(ui, |ui| {
                        if kit::primary(ui, tr("OK")).clicked() || enter {
                            reply = Some(PromptReply::Secret(SecretString::from(std::mem::take(&mut input.secret)), input.remember && can_remember));
                        }
                        if kit::text_button(ui, tr("Cancel")).clicked() {
                            reply = Some(PromptReply::Cancel);
                        }
                    });
                }
                PromptKind::Passphrase { key_path } => {
                    ui.heading(tr("Key passphrase"));
                    ui.label(trf("Enter passphrase for {key_path}:", &[("{key_path}", key_path)]));
                    let r = ui.add(egui::TextEdit::singleline(&mut input.secret).password(true).desired_width(f32::INFINITY));
                    if !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                        r.request_focus();
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_enabled(can_remember, egui::Checkbox::new(&mut input.remember, tr("Remember (encrypted with the screen lock password)")))
                        .on_disabled_hover_text(cred_note.unwrap_or(tr("Only saved sessions can remember passwords")));
                    kit::actions(ui, |ui| {
                        if kit::primary(ui, tr("OK")).clicked() || enter {
                            reply = Some(PromptReply::Secret(SecretString::from(std::mem::take(&mut input.secret)), input.remember && can_remember));
                        }
                        if kit::text_button(ui, tr("Cancel")).clicked() {
                            reply = Some(PromptReply::Cancel);
                        }
                    });
                }
                PromptKind::KeyboardInteractive { name, instructions, prompts } => {
                    ui.heading(if name.is_empty() { tr("Authentication") } else { name.as_str() });
                    if !instructions.is_empty() {
                        ui.label(instructions);
                    }
                    if input.answers.len() != prompts.len() {
                        input.answers = vec![String::new(); prompts.len()];
                    }
                    let mut enter = false;
                    for (i, (p, echo)) in prompts.iter().enumerate() {
                        ui.label(p);
                        let r = ui.add(egui::TextEdit::singleline(&mut input.answers[i]).password(!*echo).desired_width(f32::INFINITY));
                        if i == 0 && !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            enter = true;
                        }
                    }
                    kit::actions(ui, |ui| {
                        if kit::primary(ui, tr("OK")).clicked() || enter {
                            let answers = input.answers.drain(..).map(SecretString::from).collect();
                            reply = Some(PromptReply::Answers(answers));
                        }
                        if kit::text_button(ui, tr("Cancel")).clicked() {
                            reply = Some(PromptReply::Cancel);
                        }
                    });
                }
            }
            });
        });
        if let Some(t) = copied {
            self.copy_to_clipboard(ctx, t);
        }
        if reply.is_none() && modal.should_close() {
            reply = Some(PromptReply::Cancel);
        }
        if let Some(r) = reply {
            if let Some((_, mut p)) = self.prompts.pop_front() {
                p.answer(r);
            }
            self.prompt_input = PromptInput::default();
        }
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else { return };
        let (title, message, danger) = (c.title.clone(), c.message.clone(), c.danger);
        // The multi-line paste confirmation can be accepted from the keyboard with Ctrl+Enter.
        let is_paste = matches!(c.action, ConfirmAction::Paste { .. });
        let mut dont_ask = c.dont_ask;
        let mut decision: Option<bool> = None;
        if is_paste && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) {
            decision = Some(true);
        }
        let modal = egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
            let max = kit::max_window_size(ctx);
            ui.set_width(480.0_f32.min(max.x));
            ui.heading(&title);
            kit::divider(ui);
            egui::ScrollArea::vertical()
                .max_height((max.y - 160.0).max(60.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(RichText::new(&message).monospace());
                });
            if let Some(v) = dont_ask.as_mut() {
                ui.checkbox(v, tr("Don't ask again"))
                    .on_hover_text(tr("Can be turned back on in Settings → Security."));
            }
            kit::actions(ui, |ui| {
                let yes_label = if is_paste { tr("Paste (Ctrl+Enter)") } else { tr("Yes") };
                let yes = if danger {
                    kit::danger(ui, yes_label)
                } else {
                    kit::primary(ui, yes_label)
                };
                if yes.clicked() {
                    decision = Some(true);
                }
                if kit::text_button(ui, tr("No")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        if decision.is_none() && modal.should_close() {
            decision = Some(false);
        }
        if let Some(c) = self.confirm.as_mut() {
            c.dont_ask = dont_ask;
        }
        if let Some(yes) = decision {
            let c = self.confirm.take().unwrap();
            if yes && c.dont_ask == Some(true) {
                self.cfg.confirm_close_session = false;
                self.save_cfg();
                audit::log("confirm.close_session_disabled", serde_json::json!({}));
            }
            match c.action {
                ConfirmAction::SftpEditSave { tab, remote } => self.answer_edit_save(tab, remote, yes),
                action if yes => match action {
                    ConfirmAction::SftpEditSave { .. } => {}
                    ConfirmAction::DeleteSession(id) => {
                        self.cfg.sessions.retain(|s| s.id != id);
                        if let Some(v) = self.vault_mut() {
                            let _ = v.remove(&id);
                        }
                        self.save_cfg();
                        audit::log("session.deleted", serde_json::json!({"id": id}));
                    }
                    ConfirmAction::SftpDelete { tab, items } => {
                        if let Some(i) = self.tab_index(tab) {
                            if let TabKind::Ssh { cmd_tx } = &self.tabs[i].kind {
                                let _ = cmd_tx.send(SessionCmd::Sftp(SftpCmd::DeleteBatch(items)));
                            }
                        }
                    }
                    ConfirmAction::SftpOverwriteDownload { tab, cmds } => {
                        if let Some(i) = self.tab_index(tab) {
                            if let TabKind::Ssh { cmd_tx } = &self.tabs[i].kind {
                                for cmd in cmds {
                                    let _ = cmd_tx.send(SessionCmd::Sftp(cmd));
                                }
                            }
                        }
                    }
                    ConfirmAction::SftpOverwriteUpload { tab, local, remote_dir } => {
                        if let Some(i) = self.tab_index(tab) {
                            if let TabKind::Ssh { cmd_tx } = &self.tabs[i].kind {
                                let _ = cmd_tx.send(SessionCmd::Sftp(SftpCmd::Upload {
                                    local,
                                    remote_dir,
                                    overwrite: true,
                                }));
                            }
                        }
                    }
                    ConfirmAction::Paste { tab, text } => self.do_paste(tab, &text),
                    ConfirmAction::DiscardOldVault => match std::mem::replace(&mut self.vault, VaultState::Locked) {
                        VaultState::Foreign { key, salt } => {
                            match Vault::create_with_key(&self.paths.vault_file(), key, salt) {
                                Ok(v) => {
                                    self.vault = VaultState::Unlocked(v);
                                    self.open_history();
                                    audit::log("vault.old_discarded", serde_json::json!({}));
                                    self.flash(tr("Old saved passwords deleted"));
                                }
                                Err(e) => self.flash(trf("Saved passwords: {e}", &[("{e}", &format!("{e:#}"))])),
                            }
                        }
                        other => self.vault = other,
                    },
                    ConfirmAction::CloseTab(id) => self.close_tab(id),
                    ConfirmAction::Quit => {
                        self.quit_confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
                _ => {}
            }
            if let Some((tab, remote, name)) = self.pending_edit_save.take() {
                self.queue_edit_save(tab, remote, name);
            }
        }
    }

    fn queue_edit_save(&mut self, tab: TabId, remote: String, name: String) {
        if self.confirm.is_some() {
            self.pending_edit_save = Some((tab, remote, name));
            return;
        }
        self.confirm = Some(Confirm {
            title: tr("Save to the server?").into(),
            message: trf(
                "'{name}' changed in the editor. Upload it to the server?",
                &[("{name}", &name)],
            ),
            danger: false,
            dont_ask: None,
            action: ConfirmAction::SftpEditSave { tab, remote },
        });
    }

    fn answer_edit_save(&mut self, tab: TabId, remote: String, upload: bool) {
        if let Some(i) = self.tab_index(tab) {
            if let TabKind::Ssh { cmd_tx } = &self.tabs[i].kind {
                let _ = cmd_tx.send(SessionCmd::Sftp(SftpCmd::EditSaveAnswer {
                    remote,
                    upload: Some(upload),
                }));
            }
        }
    }

    fn editor_window(&mut self, ctx: &egui::Context) {
        let cred_note = self.cred_note();
        let session_id = self.editor.as_ref().map(|e| e.profile.id.clone());
        let changing = self.editor.as_ref().is_some_and(|e| e.change_password);
        // Borrowed only for this frame. The editor does not keep a copy, so locking
        // still wipes the saved password from memory.
        let mut stored_pw = None;
        if changing {
            if let Some(id) = session_id.as_deref() {
                if let VaultState::Unlocked(v) = &self.vault {
                    stored_pw = v.get(id).and_then(|e| e.password.clone());
                }
            }
        }
        let Some(ed) = self.editor.as_mut() else {
            if let Some(s) = stored_pw.as_mut() {
                s.zeroize();
            }
            return;
        };
        let others: Vec<(String, String)> = self
            .cfg
            .sessions
            .iter()
            .map(|s| (s.id.clone(), s.display_name()))
            .collect();
        let res = ed.ui(ctx, cred_note, &others, stored_pw.as_deref());
        if let Some(s) = stored_pw.as_mut() {
            s.zeroize();
        }
        match res {
            EditorResult::Open => {}
            EditorResult::Cancel => self.editor = None,
            EditorResult::Save | EditorResult::SaveAndConnect => {
                let ed = self.editor.take().unwrap();
                let profile = ed.profile.clone();
                if let Some(slot) = self.cfg.sessions.iter_mut().find(|s| s.id == profile.id) {
                    *slot = profile.clone();
                } else {
                    self.cfg.sessions.push(profile.clone());
                }
                self.save_cfg();
                // credentials
                let (set_pw, change_pw, pw, clear_pw) = (
                    ed.set_password,
                    ed.change_password,
                    ed.password.clone(),
                    ed.clear_password,
                );
                let (set_pp, pp, clear_pp) = (ed.set_passphrase, ed.passphrase.clone(), ed.clear_passphrase);
                drop(ed);
                let mut err = None;
                if let Some(v) = self.vault_mut() {
                    if set_pw || change_pw {
                        err = v.set_password(&profile.id, Some(SecretString::from(pw))).err();
                    } else if clear_pw {
                        err = v.set_password(&profile.id, None).err();
                    }
                    if set_pp {
                        err = v
                            .set_passphrase(&profile.id, Some(SecretString::from(pp)))
                            .err()
                            .or(err);
                    } else if clear_pp {
                        err = v.set_passphrase(&profile.id, None).err().or(err);
                    }
                }
                if let Some(e) = err {
                    self.flash(trf("Saved passwords: {e}", &[("{e}", &format!("{e:#}"))]));
                }
                audit::log(
                    "session.saved",
                    serde_json::json!({"id": profile.id, "name": profile.display_name()}),
                );
                if res == EditorResult::SaveAndConnect {
                    self.open_ssh_tab(profile, ctx, None);
                }
            }
        }
    }

    /// Move passwords saved under an old vault master password to the screen-lock password.
    fn migrate_modal(&mut self, ctx: &egui::Context) {
        // One password dialog at a time: the screen-lock dialog goes first, otherwise both
        // would fight over keyboard focus.
        if self.applock_dialog.is_some() {
            return;
        }
        let Some(d) = self.vault_dialog.as_mut() else { return };
        match d.ui(ctx) {
            MigrateResult::Open => {}
            MigrateResult::Cancel => self.vault_dialog = None,
            MigrateResult::Discard => {
                self.vault_dialog = None;
                self.confirm = Some(Confirm::new(
                    tr("Delete old passwords"),
                    tr("Every password and key passphrase saved by the earlier version is deleted. This cannot be undone."),
                    true,
                    ConfirmAction::DiscardOldVault,
                ));
            }
            MigrateResult::Submit(old) => {
                let VaultState::Foreign { key, salt } = &self.vault else {
                    self.vault_dialog = None;
                    return;
                };
                let (key, salt) = (key.clone(), *salt);
                if let Some(d) = self.vault_dialog.as_mut() {
                    d.busy = true;
                    d.error = None;
                }
                let path = self.paths.vault_file();
                let ui_tx = self.ui_tx.clone();
                let ctx2 = ctx.clone();
                self.rt.spawn_blocking(move || {
                    let outcome = match Vault::unlock(&path, &old).and_then(|mut v| {
                        v.apply_new_master(key, salt)?;
                        Ok(v)
                    }) {
                        Ok(v) => VaultOpOutcome::Migrated(v),
                        Err(e) => VaultOpOutcome::Failed(format!("{e:#}")),
                    };
                    let _ = ui_tx.send(UiEvent::VaultOp(outcome));
                    ctx2.request_repaint();
                });
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut action = SettingsAction::None;
        let mut rescan = false;
        let legacy_vault = matches!(self.vault, VaultState::Foreign { .. });
        let before = self.cfg.clone();
        self.ensure_font_scan(ctx);
        kit::window(ctx, tr("Settings"), egui::Id::unique("settings-window"), 520.0)
            .open(&mut open)
            .show(ctx, |ui| {
                kit::body(ui, ctx, |ui| {
                    action = dialogs::settings_ui(
                        ui,
                        &mut self.cfg,
                        legacy_vault,
                        self.app_lock_set,
                        self.font_cache.as_deref(),
                        &mut rescan,
                        self.history.len(),
                    );
                    kit::hint(
                        ui,
                        trf(
                            "config: {config}\naudit: {audit}",
                            &[
                                ("{config}", &self.paths.config_dir.display()),
                                ("{audit}", &self.paths.audit_file().display()),
                            ],
                        ),
                    );
                });
            });
        self.settings_open = open;
        if rescan {
            self.font_cache = None;
            self.ensure_font_scan(ctx);
        }
        if self.cfg != before {
            self.save_cfg();
            if self.cfg.language != before.language {
                i18n::set_lang(self.cfg.language);
            }
            if self.cfg.theme != before.theme || self.cfg.custom_theme != before.custom_theme {
                self.apply_theme(ctx);
            }
            if self.cfg.font_family != before.font_family {
                fonts::install_with_cache(ctx, &self.cfg.font_family, self.font_cache.as_deref());
                self.flash(if self.cfg.font_family.is_empty() {
                    tr("font: auto").to_string()
                } else {
                    trf("font: {family}", &[("{family}", &self.cfg.font_family)])
                });
            }
            if self.cfg.x11_server_path != before.x11_server_path {
                let p = self.cfg.x11_server_path.clone();
                self.xserver.set_override_path(&p);
            }
            if self.cfg.x11_clipboard != before.x11_clipboard {
                self.xserver.set_clipboard(self.cfg.x11_clipboard);
            }
            if self.cfg.audit_log != before.audit_log {
                audit::set_enabled(self.cfg.audit_log);
            }
            if self.cfg.server_monitor != before.server_monitor
                || self.cfg.monitor_interval_secs != before.monitor_interval_secs
            {
                for t in &self.tabs {
                    if let (Some(c), Some(p)) = (&t.monitor_ctl, &t.profile) {
                        c.set_interval(self.monitor_interval_for(p));
                        c.set_enabled(self.cfg.server_monitor && p.monitor && t.monitor.unavailable.is_none());
                    }
                }
            }
            if !self.cfg.cmd_history && before.cmd_history {
                for t in &mut self.tabs {
                    t.cmd.reset();
                }
            }
        }
        match action {
            SettingsAction::None => {}
            SettingsAction::MigrateVault => self.vault_dialog = Some(MigrateDialog::new()),
            SettingsAction::SetAppLock => {
                self.applock_dialog = Some(AppLockDialog::new(AppLockMode::Setup { first_run: false }))
            }
            SettingsAction::ChangeAppLock => self.applock_dialog = Some(AppLockDialog::new(AppLockMode::Change)),
            SettingsAction::RemoveAppLock => self.applock_dialog = Some(AppLockDialog::new(AppLockMode::Remove)),
            SettingsAction::LockApp => self.lock_app("manual"),
            SettingsAction::OpenAuditLog => {
                let _ = open::that(self.paths.audit_file());
            }
            SettingsAction::OpenConfigDir => {
                let _ = open::that(&self.paths.config_dir);
            }
            SettingsAction::ClearHistory => {
                if self.history.is_open() {
                    self.history.clear();
                } else if let Err(e) = crate::core::history::delete_file(&self.paths.history_file()) {
                    self.flash(trf("Command history: {e}", &[("{e}", &format!("{e:#}"))]));
                }
                for t in &mut self.tabs {
                    t.cmd.reset();
                }
                audit::log("history.cleared", serde_json::json!({}));
                self.flash(tr("Command history cleared"));
            }
            SettingsAction::NewTheme => {
                let mut spec = self.cfg.theme_spec();
                let taken: Vec<String> = self.cfg.custom_themes.iter().map(|t| t.name.clone()).collect();
                spec.name = color_theme::unique_name(&trf("{name} copy", &[("{name}", &spec.name)]), &taken);
                self.theme_editor = Some(ThemeEditor::new(spec, None));
            }
            SettingsAction::EditTheme(name) => {
                if let Some(spec) = self.cfg.custom_themes.iter().find(|t| t.name == name).cloned() {
                    self.theme_editor = Some(ThemeEditor::new(spec, Some(name)));
                }
            }
            SettingsAction::DeleteTheme(name) => {
                self.cfg.custom_themes.retain(|t| t.name != name);
                if self.cfg.custom_theme == name {
                    self.cfg.custom_theme = self
                        .cfg
                        .custom_themes
                        .first()
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    if self.cfg.custom_themes.is_empty() {
                        self.cfg.theme = Theme::DarkMatcha;
                    }
                }
                self.save_cfg();
                self.apply_theme(ctx);
            }
            SettingsAction::ImportTheme => self.import_theme(ctx),
            SettingsAction::ExportTheme(name) => self.export_theme(&name),
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        let mut tour = false;
        let mut check_updates = false;
        let update_state = self.update_state.clone();
        kit::window(ctx, tr("Help & about"), egui::Id::unique("about-window"), 520.0)
            .default_height(680.0)
            .open(&mut open)
            .show(ctx, |ui| {
                kit::body(ui, ctx, |ui| {
                    check_updates = dialogs::about_ui(ui, &update_state);
                    ui.add_space(kit::GRID);
                    tour = kit::outlined(ui, tr("Show the tour")).clicked();
                })
            });
        self.about_open = open;
        if check_updates {
            self.start_update_check(ctx);
        }
        if tour {
            self.about_open = false;
            self.welcome = Some(WelcomeDialog::new());
        }
    }

    /// The welcome tour. On first run it waits until the screen-lock prompt is answered, so
    /// the two never compete for the keyboard.
    fn welcome_modal(&mut self, ctx: &egui::Context) {
        if self.welcome.is_none() && !self.cfg.welcome_shown && self.applock_dialog.is_none() {
            self.welcome = Some(WelcomeDialog::new());
        }
        if self.applock_dialog.is_some() || self.vault_dialog.is_some() {
            return;
        }
        let Some(w) = self.welcome.as_mut() else { return };
        if let WelcomeResult::Close = w.ui(ctx) {
            self.welcome = None;
            if !self.cfg.welcome_shown {
                self.cfg.welcome_shown = true;
                self.save_cfg();
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let files: Vec<std::path::PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if files.is_empty() {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else { return };
        if let (TabKind::Ssh { cmd_tx }, true) = (&tab.kind, tab.connected) {
            let dir = if tab.sftp.cwd.is_empty() {
                ".".to_string()
            } else {
                tab.sftp.cwd.clone()
            };
            for f in files {
                let _ = cmd_tx.send(SessionCmd::Sftp(SftpCmd::Upload {
                    local: f,
                    remote_dir: dir.clone(),
                    overwrite: false,
                }));
            }
            self.right_panel = RightPanel::Sftp;
            self.flash(tr("uploading dropped files to current SFTP folder"));
        }
    }
}

/// Toolbar / status-bar text for the X server state.
fn x_state_label(state: xserver::XState) -> String {
    match state {
        xserver::XState::Running(d) => trf("X server: running on :{d}", &[("{d}", &d)]),
        xserver::XState::External(d) => trf("X server: external on :{d}", &[("{d}", &d)]),
        xserver::XState::Stopped => tr("X server: stopped").to_string(),
        xserver::XState::NotInstalled => tr("X server: not installed").to_string(),
    }
}

fn fmt_kb(kb: u64) -> String {
    humansize::format_size(kb.saturating_mul(1024), humansize::BINARY)
}

/// A labelled percentage bar.
fn pct_bar(ui: &mut egui::Ui, pct: f32, text: String) {
    let pct = pct.clamp(0.0, 100.0);
    let color = if pct >= 90.0 {
        Color32::from_rgb(0xe0, 0x60, 0x60)
    } else if pct >= 75.0 {
        Color32::from_rgb(0xe0, 0xa0, 0x40)
    } else {
        Color32::from_rgb(0x8f, 0xbf, 0x6a)
    };
    ui.add(
        egui::ProgressBar::new(pct / 100.0)
            .desired_width(220.0)
            .fill(color)
            .text(RichText::new(text).small()),
    );
}

/// Tiny line chart of the last samples (0–100 %).
fn sparkline(ui: &mut egui::Ui, data: &VecDeque<f32>, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(220.0, 36.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    if data.len() < 2 {
        return;
    }
    let n = MONITOR_HISTORY.max(2) as f32;
    let pts: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = rect.min.x + rect.width() * ((n - data.len() as f32 + i as f32) / (n - 1.0));
            let y = rect.max.y - rect.height() * (v.clamp(0.0, 100.0) / 100.0);
            egui::Pos2::new(x, y)
        })
        .collect();
    p.add(egui::Shape::line(pts, egui::Stroke::new(1.5, color)));
}

/// The body of the "Server status" section.
fn server_status_ui(ui: &mut egui::Ui, s: &Sample, st: &MonitorState) {
    egui::Grid::new("server-status-grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            if !s.hostname.is_empty() {
                ui.label(tr("Host name"));
                ui.label(RichText::new(&s.hostname).monospace());
                ui.end_row();
            }
            if !s.os.is_empty() || !s.kernel.is_empty() {
                ui.label(tr("System"));
                ui.label(if s.os.is_empty() {
                    s.kernel.clone()
                } else {
                    format!("{} · {}", s.os, s.kernel)
                });
                ui.end_row();
            }
            if let Some(u) = s.uptime_secs {
                ui.label(tr("Uptime"));
                ui.label(monitor::format_uptime(u));
                ui.end_row();
            }
            if let Some(l) = s.load {
                ui.label(tr("Load (1/5/15 min)"));
                let cores = s.ncpu.map(|n| format!("  ·  {n} CPU")).unwrap_or_default();
                ui.label(RichText::new(format!("{:.2}  {:.2}  {:.2}{cores}", l[0], l[1], l[2])).monospace());
                ui.end_row();
            }
            ui.label("CPU");
            ui.horizontal(|ui| {
                match s.cpu_pct {
                    Some(c) => pct_bar(ui, c, format!("{c:.0}%")),
                    None => {
                        ui.label(RichText::new(tr("n/a")).weak());
                    }
                }
                sparkline(ui, &st.cpu_hist, Color32::from_rgb(0x8c, 0xc1, 0xdd));
            });
            ui.end_row();
            ui.label(tr("Memory"));
            ui.horizontal(|ui| {
                match (s.mem_used_pct(), s.mem_total_kb, s.mem_avail_kb) {
                    (Some(p), Some(t), Some(a)) => {
                        pct_bar(
                            ui,
                            p,
                            format!("{p:.0}%  {} / {}", fmt_kb(t.saturating_sub(a)), fmt_kb(t)),
                        );
                    }
                    _ => {
                        ui.label(RichText::new(tr("n/a")).weak());
                    }
                }
                sparkline(ui, &st.mem_hist, Color32::from_rgb(0xc9, 0xa3, 0xcd));
            });
            ui.end_row();
            if let (Some(p), Some(t), Some(f)) = (s.swap_used_pct(), s.swap_total_kb, s.swap_free_kb) {
                ui.label(tr("Swap"));
                pct_bar(
                    ui,
                    p,
                    format!("{p:.0}%  {} / {}", fmt_kb(t.saturating_sub(f)), fmt_kb(t)),
                );
                ui.end_row();
            }
            if let Some(n) = s.net {
                ui.label(tr("Network"));
                let rate = |r: Option<f64>| match r {
                    Some(b) => format!("{}/s", humansize::format_size(b as u64, humansize::BINARY)),
                    None => "…".into(),
                };
                ui.label(RichText::new(format!("↓ {}   ↑ {}", rate(s.net_rx_rate), rate(s.net_tx_rate))).monospace())
                    .on_hover_text(trf(
                        "Download / upload since boot: {rx} / {tx}",
                        &[
                            ("{rx}", &humansize::format_size(n.rx_bytes, humansize::BINARY)),
                            ("{tx}", &humansize::format_size(n.tx_bytes, humansize::BINARY)),
                        ],
                    ));
                ui.end_row();
            }
            for d in &s.disks {
                ui.label(RichText::new(&d.mount).monospace());
                pct_bar(
                    ui,
                    d.used_pct(),
                    format!("{:.0}%  {} / {}", d.used_pct(), fmt_kb(d.used_kb), fmt_kb(d.total_kb)),
                );
                ui.end_row();
            }
            if s.users.is_some() || s.procs.is_some() {
                ui.label(tr("Users / processes"));
                let u = s.users.map(|n| n.to_string()).unwrap_or_else(|| "–".into());
                let p = s.procs.map(|n| n.to_string()).unwrap_or_else(|| "–".into());
                ui.label(format!("{u} / {p}"));
                ui.end_row();
            }
        });
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n - 1).collect();
        format!("{t}…")
    }
}

impl eframe::App for RuxtApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.push_caption(frame);
        let ctx = ui.ctx().clone();
        if !self.app_locked {
            self.inject_clipboard_shortcuts(&ctx);
        }
        self.drain_events();
        self.auto_lock(&ctx);
        self.agent_guard_tick(&ctx);
        self.clipboard_tick(&ctx);
        if !self.app_locked {
            self.handle_dropped_files(&ctx);
        }
        self.flush_cfg(Some(&ctx), false);
        self.apply_ui_metrics(&ctx, false);

        if self.app_locked {
            // Lock screen: an opaque panel with nothing from the sessions, plus the unlock
            // dialog. Sessions keep running; their output is simply not drawn. Quitting is
            // still possible (the close-confirmation below is drawn as usual).
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Ruxt").heading().weak());
                });
            });
            self.applock_modal(&ctx);
            self.confirm_modal(&ctx);
            return;
        }

        // Intercept window close while sessions are live.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            let live = self.tabs.iter().filter(|t| t.is_ssh() && t.connected).count();
            if live > 0 && self.cfg.confirm_close_session {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm = Some(Confirm {
                    dont_ask: Some(false),
                    ..Confirm::new(
                        tr("Quit"),
                        trf(
                            "{live} SSH session(s) are still connected. Quit anyway?",
                            &[("{live}", &live)],
                        ),
                        false,
                        ConfirmAction::Quit,
                    )
                });
            }
        }

        let chrome = theme::chrome_frame(ui.visuals());
        let term_bg = {
            let c = self.palette().bg;
            Color32::from_rgb(c.r, c.g, c.b)
        };
        egui::Panel::top("toolbar")
            .frame(chrome)
            .show(ui, |ui| self.toolbar(ui, &ctx));
        egui::Panel::bottom("status")
            .frame(theme::status_frame(ui.visuals()))
            .show(ui, |ui| self.status_bar(ui));
        if self.show_sidebar {
            egui::Panel::left("sessions")
                .resizable(true)
                .default_size(230.0)
                .frame(chrome)
                .show(ui, |ui| self.sidebar(ui, &ctx));
        }
        if self.right_panel != RightPanel::None && self.active_tab().map(|t| t.is_ssh()).unwrap_or(false) {
            egui::Panel::right("right")
                .resizable(true)
                .default_size(600.0)
                .frame(chrome)
                .show(ui, |ui| self.right_panel_ui(ui));
        }
        egui::CentralPanel::default()
            .frame(theme::terminal_frame(term_bg))
            .show(ui, |ui| {
                // No gap under the active tab: its square bottom should meet the terminal.
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin {
                        left: 4,
                        right: 4,
                        top: 3,
                        bottom: 0,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        self.tab_bar(ui, &ctx);
                    });
                self.terminal_area(ui, &ctx);
            });

        self.prompt_modal(&ctx);
        self.confirm_modal(&ctx);
        self.migrate_modal(&ctx);
        self.applock_modal(&ctx);
        self.welcome_modal(&ctx);
        self.editor_window(&ctx);
        self.settings_window(&ctx);
        self.theme_editor_window(&ctx);
        self.server_info_window(&ctx);
        self.tail_viewers_ui(&ctx);
        if let Some(k) = self.keygen.as_mut() {
            if !k.ui(&ctx) {
                self.keygen = None;
            }
        }
        self.about_window(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_cfg(None, true);
        self.tail_viewers.clear();
        for t in &self.tabs {
            match &t.kind {
                TabKind::Ssh { cmd_tx } => {
                    let _ = cmd_tx.send(SessionCmd::Disconnect);
                }
                TabKind::Local { shell } => shell.shutdown(),
            }
        }
        // Session tasks may never run again after this point; remove the plaintext copies of
        // remotely edited files synchronously.
        sftp::cleanup_edit_dirs();
        if self.cfg.x11_stop_on_exit {
            self.xserver.stop();
        } else {
            self.xserver.detach();
        }
        audit::log("app.exit", serde_json::json!({}));
    }
}

#[cfg(test)]
mod caption_tests {
    use super::caption_paint;
    use crate::core::theme::ThemeSpec;

    #[test]
    fn matcha_caption_glyphs_are_light() {
        let paint = caption_paint(&ThemeSpec::matcha());
        assert!(paint.dark);
        assert!(super::luminance(paint.fg) > super::luminance(paint.bg));
    }

    #[test]
    fn light_caption_glyphs_are_dark() {
        let paint = caption_paint(&ThemeSpec::light());
        assert!(!paint.dark);
        assert!(super::luminance(paint.fg) < super::luminance(paint.bg));
    }
}
