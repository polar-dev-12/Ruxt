//! Non-secret configuration: application settings and session profiles.
//! Secrets (passwords, passphrases) are NEVER stored here — see `vault.rs`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SecurityProfile {
    /// Post-quantum hybrid / Curve25519 KEX, AEAD ciphers only, no SHA-1, no RSA/SHA-1.
    #[default]
    Strict,
    /// russh safe defaults (adds DH-GEX / group14+ and CTR ciphers) for older servers; still
    /// no SHA-1 anywhere.
    Compatible,
    /// `Compatible` plus `ssh-rsa` (SHA-1) host-key signatures for servers older than
    /// OpenSSH 7.2. Only for the rare box that cannot be upgraded.
    Legacy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMethod {
    /// Agent → default key files → password → keyboard-interactive.
    #[default]
    Auto,
    Password,
    PublicKey {
        path: String,
    },
    Agent,
    KeyboardInteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TunnelKind {
    #[default]
    Local,
    Remote,
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelSpec {
    pub kind: TunnelKind,
    pub bind_addr: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub auto_start: bool,
}

impl Default for TunnelSpec {
    fn default() -> Self {
        Self {
            kind: TunnelKind::Local,
            bind_addr: "127.0.0.1".into(),
            bind_port: 8080,
            target_host: "127.0.0.1".into(),
            target_port: 80,
            auto_start: true,
        }
    }
}

impl TunnelSpec {
    pub fn describe(&self) -> String {
        match self.kind {
            TunnelKind::Local => format!(
                "L {}:{} -> {}:{}",
                self.bind_addr, self.bind_port, self.target_host, self.target_port
            ),
            TunnelKind::Remote => format!(
                "R {}:{} <- {}:{}",
                self.bind_addr, self.bind_port, self.target_host, self.target_port
            ),
            TunnelKind::Dynamic => format!("D SOCKS5 {}:{}", self.bind_addr, self.bind_port),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProfile {
    pub id: String,
    pub name: String,
    pub group: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub security: SecurityProfile,
    pub x11_forwarding: bool,
    /// Local X display, e.g. ":0" or "127.0.0.1:0". Empty = use $DISPLAY or :0.
    pub x11_display: String,
    /// Trusted forwarding (`ssh -Y`): remote X clients get full access to the local display.
    /// Off = untrusted (`ssh -X`): an untrusted cookie from the X SECURITY extension confines
    /// them (no reading other windows, no input snooping).
    #[serde(default)]
    pub x11_trusted: bool,
    pub agent_forwarding: bool,
    pub tunnels: Vec<TunnelSpec>,
    pub startup_command: String,
    pub keepalive_secs: u64,
    pub term_type: String,
    pub scrollback: usize,
    /// Allow legacy compression (off by default).
    pub compression: bool,
    /// Id of another saved session to connect through first (ProxyJump); "" = direct.
    #[serde(default)]
    pub jump: String,
    /// Free-form, comma-separated tags; matched by the sidebar filter.
    #[serde(default)]
    pub tags: String,
    /// Run the read-only status script on this server (per-session consent; the global
    /// `AppConfig::server_monitor` switch must be on as well).
    #[serde(default)]
    pub monitor: bool,
    /// Status poll interval for this session in seconds; 0 = `AppConfig::monitor_interval_secs`.
    #[serde(default)]
    pub monitor_interval_secs: u64,
    /// The SFTP panel shows dot files for this session.
    #[serde(default)]
    pub sftp_show_hidden: bool,
}

impl Default for SessionProfile {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            group: "Default".into(),
            host: String::new(),
            port: 22,
            username: String::new(),
            auth: AuthMethod::Auto,
            security: SecurityProfile::Strict,
            x11_forwarding: false,
            x11_display: String::new(),
            x11_trusted: false,
            agent_forwarding: false,
            tunnels: Vec::new(),
            startup_command: String::new(),
            keepalive_secs: 30,
            term_type: "xterm-256color".into(),
            scrollback: 10_000,
            compression: false,
            jump: String::new(),
            tags: String::new(),
            monitor: false,
            monitor_interval_secs: 0,
            sftp_show_hidden: false,
        }
    }
}

impl SessionProfile {
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            if self.username.is_empty() {
                format!("{}:{}", self.host, self.port)
            } else {
                format!("{}@{}:{}", self.username, self.host, self.port)
            }
        } else {
            self.name.clone()
        }
    }
}

pub fn new_id() -> String {
    let mut b = [0u8; 16];
    rand::fill(&mut b);
    hex::encode(b)
}

/// Which graphics backend draws the window. Applied when Ruxt starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Renderer {
    /// OpenGL through glow: smallest footprint, works on every desktop driver.
    #[default]
    OpenGl,
    /// wgpu: DirectX 12 / Vulkan / Metal. Faster on modern GPUs and avoids broken OpenGL
    /// drivers (remote desktop, some virtual machines).
    Wgpu,
}

/// GPU acceleration policy for the OpenGL backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HwAccel {
    /// Use the GPU when available, fall back to software rendering.
    #[default]
    Preferred,
    /// Refuse to start without a GPU context.
    Required,
    /// Software rendering only.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    /// Dark "matcha" theme: deep green-black surfaces with matcha-green accents.
    #[default]
    DarkMatcha,
    /// Dark purple surfaces with a green accent.
    PurpleGreen,
    /// Dark blue-slate.
    Harbor,
    /// Dark warm brown with an amber accent.
    Ember,
    /// Neutral charcoal with a cool blue accent.
    Ink,
    /// Deep teal forest.
    Pine,
    Light,
    /// Cool white paper with a blue accent.
    Porcelain,
    /// Warm cream paper with an amber accent.
    Sand,
    /// Pale mint paper with a deep green accent.
    Mint,
    /// Blush paper with a rose accent.
    Rose,
    /// Pale lavender paper with a purple accent.
    Lavender,
    /// One of `AppConfig::custom_themes`, chosen by `AppConfig::custom_theme`.
    Custom,
}

impl Theme {
    /// Dark built-ins, in menu order.
    pub fn dark_builtins() -> &'static [Theme] {
        &[
            Theme::DarkMatcha,
            Theme::PurpleGreen,
            Theme::Harbor,
            Theme::Ember,
            Theme::Ink,
            Theme::Pine,
        ]
    }

    /// Light built-ins, in menu order.
    pub fn light_builtins() -> &'static [Theme] {
        &[
            Theme::Light,
            Theme::Porcelain,
            Theme::Sand,
            Theme::Mint,
            Theme::Rose,
            Theme::Lavender,
        ]
    }

    /// Colours for a built-in. `None` for [`Theme::Custom`].
    pub fn builtin_spec(self) -> Option<super::theme::ThemeSpec> {
        use super::theme::ThemeSpec;
        let make: fn() -> ThemeSpec = match self {
            Theme::DarkMatcha => ThemeSpec::matcha,
            Theme::PurpleGreen => ThemeSpec::purple_green,
            Theme::Harbor => ThemeSpec::harbor,
            Theme::Ember => ThemeSpec::ember,
            Theme::Ink => ThemeSpec::ink,
            Theme::Pine => ThemeSpec::pine,
            Theme::Light => ThemeSpec::light,
            Theme::Porcelain => ThemeSpec::porcelain,
            Theme::Sand => ThemeSpec::sand,
            Theme::Mint => ThemeSpec::mint,
            Theme::Rose => ThemeSpec::rose,
            Theme::Lavender => ThemeSpec::lavender,
            Theme::Custom => return None,
        };
        Some(make())
    }

    /// `RUXT_THEME` value for this run. Unknown names are ignored by the caller.
    pub fn from_env(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "matcha" | "dark" => Some(Self::DarkMatcha),
            "purple" | "purple-green" | "purple_green" | "purplegreen" => Some(Self::PurpleGreen),
            "harbor" => Some(Self::Harbor),
            "ember" => Some(Self::Ember),
            "ink" => Some(Self::Ink),
            "pine" => Some(Self::Pine),
            "light" => Some(Self::Light),
            "porcelain" => Some(Self::Porcelain),
            "sand" => Some(Self::Sand),
            "mint" => Some(Self::Mint),
            "rose" => Some(Self::Rose),
            "lavender" => Some(Self::Lavender),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub theme: Theme,
    /// Name of the active custom theme (when `theme == Custom`).
    pub custom_theme: String,
    /// User-defined themes (see `core::theme`).
    pub custom_themes: Vec<super::theme::ThemeSpec>,
    /// UI language.
    pub language: super::i18n::Lang,
    /// Graphics backend (needs a restart).
    pub renderer: Renderer,
    /// Hardware acceleration policy for OpenGL (needs a restart).
    pub hw_accel: HwAccel,
    /// Vertical sync (needs a restart). Off lowers input latency on high-refresh displays.
    pub vsync: bool,
    /// Whole-UI zoom factor (1.0 = 100 %). Scales every widget, the terminal included.
    pub ui_scale: f32,
    /// Body text size of menus, panels and dialogs (points, before `ui_scale`). The other
    /// egui text styles keep their default ratios to it.
    pub ui_font_size: f32,
    /// Terminal font family name ("" = auto-detect a system monospace font).
    pub font_family: String,
    pub font_size: f32,
    /// Ask before pasting text that contains a newline (clipboard injection defence).
    pub confirm_multiline_paste: bool,
    /// Ask before closing a connected SSH tab or quitting with live sessions.
    pub confirm_close_session: bool,
    /// Lock the whole window after this many idle minutes (0 = off; 10-minute steps). Only
    /// effective once a screen-lock password exists (`applock.bin`).
    pub app_lock_minutes: u64,
    /// The first-run "set a screen lock password?" prompt has been answered (or skipped).
    pub app_lock_prompted: bool,
    /// Passwords saved by an older version under a separate vault master password were found
    /// and the user was asked once to set a screen lock so they can be moved.
    pub vault_merge_prompted: bool,
    /// Allow remote programs to write the local clipboard via OSC 52 (off by default).
    pub osc52_clipboard: bool,
    /// Agent guard (Windows): hide the windows from screen capture and drop injected
    /// keyboard / mouse input, locking the window when that happens.
    pub agent_guard: bool,
    /// Allow tunnels bound to non-loopback addresses (off by default).
    pub allow_public_bind: bool,
    /// Copy terminal selection to clipboard automatically.
    pub copy_on_select: bool,
    /// Intercept `tail` commands typed in the terminal and open them in a separate TailViewer window.
    pub tail_viewer: bool,
    /// Local shell program ("" = auto-detect).
    pub local_shell: String,
    /// Explicit path to an X server executable (XWin.exe / vcxsrv.exe / Xming.exe; "" = the
    /// bundled Cygwin/X, else an installed VcXsrv / Xming).
    pub x11_server_path: String,
    /// Start the X server automatically when a session with X11 forwarding connects.
    pub x11_auto_start: bool,
    /// Start the bundled X server when Ruxt starts (hidden console, random cookie).
    /// Off by default: the server listens on every interface, so it starts only when an
    /// X11 session connects or the user turns it on.
    pub x11_start_on_launch: bool,
    /// Stop an X server we started when Ruxt exits.
    pub x11_stop_on_exit: bool,
    /// Let remote X11 clients read/write the local clipboard via the X server's selection
    /// bridge (off by default: it is the same exposure OSC 52 is blocked for).
    pub x11_clipboard: bool,
    /// External editor command for "edit remote file" ("" = OS default handler, which refuses
    /// executable file types).
    pub external_editor: String,
    /// Record submitted command lines (SQLite `history.db`) and offer them as completions.
    pub cmd_history: bool,
    /// Poll connected servers for load/memory/disk with a small read-only script.
    pub server_monitor: bool,
    /// Seconds between server status polls.
    pub monitor_interval_secs: u64,
    /// Clear the clipboard this many seconds after Ruxt put text on it (0 = never).
    pub clipboard_clear_secs: u64,
    /// Disconnect every SSH session after this many idle minutes (0 = never).
    pub idle_disconnect_minutes: u64,
    /// Forget command-history entries unused for this many days (0 = keep forever).
    pub history_days: u64,
    /// Drop audit-log lines older than this many days at start-up (0 = keep forever).
    pub audit_days: u64,
    /// Write the audit log at all (off by default).
    pub audit_log: bool,
    /// The first-run welcome tour has been shown.
    pub welcome_shown: bool,
    /// Which buttons the toolbar shows. Right-click the bar to change it.
    pub toolbar: ToolbarItems,
    pub sessions: Vec<SessionProfile>,
    /// Ids of the most recently connected saved sessions, newest first.
    pub recent: Vec<String>,
}

fn default_shown() -> bool {
    true
}

/// Toolbar buttons the user has chosen to show. Missing fields stay shown, so a new button
/// does not appear hidden in an older config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolbarItems {
    #[serde(default = "default_shown")]
    pub sessions: bool,
    #[serde(default = "default_shown")]
    pub new_session: bool,
    #[serde(default = "default_shown")]
    pub local_shell: bool,
    #[serde(default = "default_shown")]
    pub quick_connect: bool,
    #[serde(default = "default_shown")]
    pub find: bool,
    #[serde(default = "default_shown")]
    pub sftp: bool,
    #[serde(default = "default_shown")]
    pub tunnels: bool,
    #[serde(default = "default_shown")]
    pub broadcast: bool,
    #[serde(default = "default_shown")]
    pub agent_guard: bool,
    #[serde(default = "default_shown")]
    pub x11: bool,
    #[serde(default = "default_shown")]
    pub keygen: bool,
    #[serde(default = "default_shown")]
    pub lock: bool,
    #[serde(default = "default_shown")]
    pub settings: bool,
    #[serde(default = "default_shown")]
    pub help: bool,
}

impl Default for ToolbarItems {
    fn default() -> Self {
        Self {
            sessions: true,
            new_session: true,
            local_shell: true,
            quick_connect: true,
            find: true,
            sftp: true,
            tunnels: true,
            broadcast: true,
            agent_guard: true,
            x11: true,
            keygen: true,
            lock: true,
            settings: true,
            help: true,
        }
    }
}

#[cfg(test)]
impl ToolbarItems {
    pub fn any(self) -> bool {
        self.sessions
            || self.new_session
            || self.local_shell
            || self.quick_connect
            || self.find
            || self.sftp
            || self.tunnels
            || self.broadcast
            || self.agent_guard
            || self.x11
            || self.keygen
            || self.lock
            || self.settings
            || self.help
    }
}

/// Hard upper bound for per-session scrollback (lines); keeps worst-case memory per tab sane.
pub const MAX_SCROLLBACK: usize = 200_000;

/// Allowed range of `AppConfig::ui_scale`.
pub const UI_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.75..=2.0;
/// Allowed range and default of `AppConfig::ui_font_size` (egui's own body size is 12.5).
pub const UI_FONT_SIZE_RANGE: std::ops::RangeInclusive<f32> = 9.0..=24.0;
pub const UI_FONT_SIZE_DEFAULT: f32 = 12.5;
/// Allowed range of `AppConfig::font_size` (terminal).
pub const TERM_FONT_SIZE_RANGE: std::ops::RangeInclusive<f32> = 8.0..=32.0;

/// Clamp `v` into `range`; a non-finite value (hand-edited config) becomes `default`.
pub fn clamp_or(v: f32, range: &std::ops::RangeInclusive<f32>, default: f32) -> f32 {
    if v.is_finite() {
        v.clamp(*range.start(), *range.end())
    } else {
        default
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: Theme::DarkMatcha,
            custom_theme: String::new(),
            custom_themes: Vec::new(),
            language: super::i18n::Lang::Ko,
            renderer: Renderer::OpenGl,
            hw_accel: HwAccel::Preferred,
            vsync: true,
            ui_scale: 1.0,
            ui_font_size: UI_FONT_SIZE_DEFAULT,
            font_family: String::new(),
            font_size: 14.0,
            confirm_multiline_paste: true,
            confirm_close_session: true,
            app_lock_minutes: 10,
            app_lock_prompted: false,
            vault_merge_prompted: false,
            osc52_clipboard: false,
            agent_guard: false,
            allow_public_bind: false,
            copy_on_select: false,
            tail_viewer: true,
            local_shell: String::new(),
            x11_server_path: String::new(),
            x11_auto_start: true,
            x11_start_on_launch: false,
            x11_stop_on_exit: true,
            x11_clipboard: false,
            external_editor: String::new(),
            cmd_history: true,
            server_monitor: true,
            monitor_interval_secs: super::monitor::DEFAULT_INTERVAL,
            clipboard_clear_secs: 0,
            idle_disconnect_minutes: 0,
            history_days: 90,
            audit_days: 180,
            audit_log: false,
            welcome_shown: false,
            toolbar: ToolbarItems::default(),
            sessions: Vec::new(),
            recent: Vec::new(),
        }
    }
}

/// Well-known file locations.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "ruxt")
            .context("cannot determine a per-user configuration directory")?;
        let config_dir = dirs.config_dir().to_path_buf();
        let data_dir = dirs.data_local_dir().to_path_buf();
        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&data_dir)?;
        restrict_dir_permissions(&config_dir);
        restrict_dir_permissions(&data_dir);
        Ok(Self { config_dir, data_dir })
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }
    pub fn vault_file(&self) -> PathBuf {
        self.config_dir.join("vault.bin")
    }
    pub fn known_hosts_file(&self) -> PathBuf {
        self.config_dir.join("known_hosts")
    }
    pub fn applock_file(&self) -> PathBuf {
        self.config_dir.join("applock.bin")
    }
    pub fn audit_file(&self) -> PathBuf {
        self.data_dir.join("audit.log")
    }
    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("history.db")
    }
    /// Plaintext copies of remotely edited files live here (not in `%TEMP%`, which sample
    /// submission and clean-up tools sweep); marked not-content-indexed on Windows.
    pub fn edit_dir(&self) -> PathBuf {
        self.data_dir.join("edit")
    }
}

#[cfg(unix)]
fn restrict_dir_permissions(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn restrict_dir_permissions(_p: &Path) {}

/// Open options that create a file readable by its owner only, from the very first byte
/// (no umask window where the file is briefly world-readable).
pub fn private_open_options(create_new: bool) -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    o.write(true);
    if create_new {
        o.create_new(true);
    } else {
        o.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Write a file atomically (unique temp + rename) with owner-only permissions where supported.
/// Concurrent writers never collide because every call uses its own temporary name.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut rnd = [0u8; 6];
    rand::fill(&mut rnd);
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.tmp-{}", hex::encode(rnd)));
    let res = (|| -> Result<()> {
        let mut f = private_open_options(true).open(&tmp)?;
        use std::io::Write;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Make sure `path` exists with owner-only permissions (no-op when it already exists).
pub fn ensure_private_file(path: &Path) -> std::io::Result<()> {
    match private_open_options(true).open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// How many sessions the "Recent" list keeps.
pub const MAX_RECENT: usize = 8;

impl AppConfig {
    /// The colours in effect: a built-in or the selected custom theme (matcha when the
    /// selected name no longer exists).
    pub fn theme_spec(&self) -> super::theme::ThemeSpec {
        match self.theme.builtin_spec() {
            Some(spec) => spec,
            None => self
                .custom_themes
                .iter()
                .find(|t| t.name == self.custom_theme)
                .cloned()
                .unwrap_or_else(super::theme::ThemeSpec::matcha),
        }
    }

    /// Move `id` to the front of the recent list.
    pub fn touch_recent(&mut self, id: &str) {
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn load(paths: &Paths) -> Self {
        let file = paths.config_file();
        match std::fs::read(&file) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(mut cfg) => {
                    for s in &mut cfg.sessions {
                        s.scrollback = s.scrollback.clamp(100, MAX_SCROLLBACK);
                    }
                    let ids: std::collections::HashSet<String> = cfg.sessions.iter().map(|s| s.id.clone()).collect();
                    cfg.recent.retain(|r| ids.contains(r));
                    cfg
                }
                Err(e) => {
                    // Keep the unreadable file instead of overwriting it with defaults on the
                    // next save: the user's session list is in there.
                    let backup = file.with_file_name(format!(
                        "config.json.bad-{}",
                        chrono::Local::now().format("%Y%m%d-%H%M%S")
                    ));
                    let moved = std::fs::rename(&file, &backup).is_ok();
                    tracing::warn!(
                        "config.json unreadable ({e}); starting with defaults{}",
                        if moved {
                            format!(", original kept as {}", backup.display())
                        } else {
                            String::new()
                        }
                    );
                    super::audit::log(
                        "config.unreadable",
                        serde_json::json!({"error": e.to_string(), "backup": moved.then(|| backup.display().to_string())}),
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        write_private_atomic(&paths.config_file(), &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_or_handles_out_of_range_and_non_finite() {
        assert_eq!(clamp_or(1.25, &UI_SCALE_RANGE, 1.0), 1.25);
        assert_eq!(clamp_or(10.0, &UI_SCALE_RANGE, 1.0), 2.0);
        assert_eq!(clamp_or(0.1, &UI_SCALE_RANGE, 1.0), 0.75);
        assert_eq!(
            clamp_or(f32::NAN, &UI_FONT_SIZE_RANGE, UI_FONT_SIZE_DEFAULT),
            UI_FONT_SIZE_DEFAULT
        );
        assert_eq!(clamp_or(f32::INFINITY, &TERM_FONT_SIZE_RANGE, 14.0), 14.0);
    }

    #[test]
    fn old_config_without_ui_sizes_gets_defaults() {
        let cfg: AppConfig = serde_json::from_str(r#"{"font_size": 18.0}"#).unwrap();
        assert_eq!(cfg.font_size, 18.0);
        assert_eq!(cfg.ui_scale, 1.0);
        assert_eq!(cfg.ui_font_size, UI_FONT_SIZE_DEFAULT);
        assert!(cfg.toolbar.any());
    }

    #[test]
    fn toolbar_buttons_stay_shown_when_a_field_is_missing() {
        let cfg: AppConfig = serde_json::from_str(r#"{"toolbar":{"sessions":false}}"#).unwrap();
        assert!(!cfg.toolbar.sessions);
        assert!(cfg.toolbar.settings);
        assert!(cfg.toolbar.help);
    }
}

#[cfg(all(test, windows))]
mod dir_tests {
    /// No organisation folder: `%APPDATA%\ruxt\config` and `%LOCALAPPDATA%\ruxt\data`.
    #[test]
    fn config_dirs_sit_directly_under_appdata() {
        let d = directories::ProjectDirs::from("", "", "ruxt").unwrap();
        let appdata = std::path::PathBuf::from(std::env::var("APPDATA").unwrap());
        let local = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap());
        assert_eq!(d.config_dir(), appdata.join("ruxt").join("config"));
        assert_eq!(d.data_local_dir(), local.join("ruxt").join("data"));
    }
}
