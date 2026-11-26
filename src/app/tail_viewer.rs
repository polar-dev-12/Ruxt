//! Dedicated Tail Viewer window: streams `tail` output in a separate OS window (or docked
//! floating window) while keeping the main terminal prompt free, with multi-keyword
//! monitoring, real-time `.txt` file logging, visual window flashing, and jump-to-hit alerts.

use crate::core::i18n::{tr, trf};
use crate::core::tail::{LocalTailHandle, TailId, TailSpec, MAX_BUFFER_LINES};
use crate::core::TabId;
use egui::{
    Color32, Context, CornerRadius, FontId, Id, Key, RichText, ScrollArea, Stroke, StrokeKind, TextEdit, Ui, Vec2,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

/// Duration of the visual window/toolbar blinking effect after a keyword hit.
const WATCH_FLASH_DURATION: Duration = Duration::from_secs(3);

/// Blink interval for the visual alert highlight.
const WATCH_BLINK_INTERVAL_MS: u128 = 250;

pub enum TailViewerAction {
    None,
    Stop(TailId),
    Restart(TailId),
    Close(TailId),
    Copy(String),
    SendInput(TailId, Zeroizing<Vec<u8>>),
}

/// Format used when recording matched log lines to a separate `.txt` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatchLogFormat {
    /// Prefix each line with the local timestamp (`[YYYY-MM-DD HH:MM:SS.mmm] line`).
    #[default]
    Timestamped,
    /// Write the raw log line as-is without a timestamp prefix.
    Raw,
}

/// Real-time keyword monitoring settings and state for a [`TailViewer`] instance.
pub struct WatchConfig {
    /// Whether the watch configuration bar is expanded in the window.
    pub show_bar: bool,
    /// Whether keyword monitoring is active on incoming lines.
    pub enabled: bool,
    /// User-entered keywords (comma/semicolon-separated, or space-separated).
    pub raw_keywords: String,
    /// `false` = match ANY keyword (OR), `true` = match ALL keywords (AND).
    pub match_all: bool,
    /// Show only lines that match the watch keywords in the log view.
    pub filter_watched_only: bool,
    /// Append matched log lines to `log_file_path`.
    pub save_to_file: bool,
    /// Line format for the output `.txt` file (timestamped vs. raw).
    pub log_format: WatchLogFormat,
    /// Destination `.txt` path for matched log lines.
    pub log_file_path: PathBuf,
    writer: Option<BufWriter<File>>,
    opened_path: Option<PathBuf>,
    pub last_write_error: Option<String>,
    /// Flash/blink the viewer window and highlight matched lines on hit.
    pub alert_visual: bool,
    /// Automatically scroll to the newest matched line even when `Follow` is off.
    pub jump_on_hit: bool,
    /// Total matched lines recorded during this session.
    pub hit_count: u64,
    /// Matched lines that arrived while `Follow` was off (or paused) and haven't been jumped to.
    pub unread_hits: u64,
    /// Timestamp of the last watch hit (drives the visual blink animation).
    pub last_hit_at: Option<Instant>,
    /// Requests OS taskbar/window attention on the next UI frame.
    pub pending_attention: bool,
    /// Requests a one-shot scroll to the bottom of the log view on the next UI frame.
    pub force_scroll_bottom: bool,
}

impl WatchConfig {
    pub fn new(spec: &TailSpec, session_title: &str) -> Self {
        Self {
            show_bar: true,
            enabled: true,
            raw_keywords: String::new(),
            match_all: false,
            filter_watched_only: false,
            save_to_file: false,
            log_format: WatchLogFormat::Timestamped,
            log_file_path: default_watch_log_path(spec, session_title),
            writer: None,
            opened_path: None,
            last_write_error: None,
            alert_visual: true,
            jump_on_hit: false,
            hit_count: 0,
            unread_hits: 0,
            last_hit_at: None,
            pending_attention: false,
            force_scroll_bottom: false,
        }
    }

    /// Split `raw_keywords` into individual non-empty tokens.
    /// If commas or semicolons are present, splits on `','` / `';'` (allowing multi-word phrases);
    /// otherwise splits on whitespace so space-separated words work out of the box.
    pub fn keywords(&self) -> Vec<&str> {
        if self.raw_keywords.contains([',', ';']) {
            self.raw_keywords
                .split([',', ';'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            self.raw_keywords.split_whitespace().filter(|s| !s.is_empty()).collect()
        }
    }

    pub fn is_active(&self) -> bool {
        self.enabled && !self.keywords().is_empty()
    }

    /// Check whether `line` satisfies the configured watch keywords.
    pub fn matches(&self, line: &str, case_sensitive: bool) -> bool {
        if !self.enabled {
            return false;
        }
        let kw = self.keywords();
        if kw.is_empty() {
            return false;
        }
        if case_sensitive {
            if self.match_all {
                kw.iter().all(|k| line.contains(k))
            } else {
                kw.iter().any(|k| line.contains(k))
            }
        } else {
            let lower = line.to_lowercase();
            if self.match_all {
                kw.iter().all(|k| lower.contains(&k.to_lowercase()))
            } else {
                kw.iter().any(|k| lower.contains(&k.to_lowercase()))
            }
        }
    }

    /// Record a matched line: increment counters, trigger visual alert, and append to the `.txt` file.
    pub fn record_hit(&mut self, line: &str) {
        self.hit_count = self.hit_count.saturating_add(1);
        if self.alert_visual {
            self.last_hit_at = Some(Instant::now());
            self.pending_attention = true;
        }
        if self.save_to_file {
            self.write_line_to_file(line);
        }
    }

    fn write_line_to_file(&mut self, line: &str) {
        if self.log_file_path.as_os_str().is_empty() {
            return;
        }
        if self.writer.is_none() || self.opened_path.as_ref() != Some(&self.log_file_path) {
            self.writer = None;
            if let Some(parent) = self.log_file_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            match OpenOptions::new().create(true).append(true).open(&self.log_file_path) {
                Ok(f) => {
                    self.writer = Some(BufWriter::new(f));
                    self.opened_path = Some(self.log_file_path.clone());
                    self.last_write_error = None;
                }
                Err(e) => {
                    self.last_write_error = Some(e.to_string());
                    return;
                }
            }
        }
        if let Some(w) = self.writer.as_mut() {
            let res = match self.log_format {
                WatchLogFormat::Timestamped => {
                    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                    writeln!(w, "[{ts}] {line}")
                }
                WatchLogFormat::Raw => writeln!(w, "{line}"),
            };
            if let Err(e) = res.and_then(|()| w.flush()) {
                self.last_write_error = Some(e.to_string());
                self.writer = None;
            } else {
                self.last_write_error = None;
            }
        }
    }

    pub fn flush_writer(&mut self) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.flush();
        }
    }

    pub fn reset_hits(&mut self) {
        self.hit_count = 0;
        self.unread_hits = 0;
        self.last_hit_at = None;
        self.pending_attention = false;
    }

    /// Returns `(is_within_flash_window, blink_on_phase)`.
    pub fn flash_state(&self) -> (bool, bool) {
        if !self.alert_visual {
            return (false, false);
        }
        let Some(at) = self.last_hit_at else {
            return (false, false);
        };
        let elapsed = at.elapsed();
        if elapsed >= WATCH_FLASH_DURATION {
            return (false, false);
        }
        let blink_on = (elapsed.as_millis() / WATCH_BLINK_INTERVAL_MS).is_multiple_of(2);
        (true, blink_on)
    }
}

fn default_watch_log_path(spec: &TailSpec, session_title: &str) -> PathBuf {
    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let title = sanitize(&spec.display_title());
    let sess = sanitize(session_title);
    let file_name = if sess.is_empty() {
        format!("tail-watch-{title}.txt")
    } else {
        format!("tail-watch-{sess}-{title}.txt")
    };
    if let Some(dirs) = directories::UserDirs::new() {
        if let Some(dl) = dirs.download_dir() {
            return dl.join(&file_name);
        }
        if let Some(doc) = dirs.document_dir() {
            return doc.join(&file_name);
        }
        return dirs.home_dir().join(&file_name);
    }
    PathBuf::from(file_name)
}

pub struct TailViewer {
    pub id: TailId,
    pub tab_id: TabId,
    pub session_title: String,
    pub spec: TailSpec,
    pub lines: VecDeque<String>,
    partial_line: String,
    pub total_bytes: u64,
    pub total_lines: u64,
    pub running: bool,
    pub status: String,
    pub follow: bool,
    pub paused: bool,
    paused_lines: VecDeque<String>,
    paused_bytes: usize,
    pub wrap: bool,
    pub filter: String,
    pub filter_only_matching: bool,
    pub case_sensitive: bool,
    /// Keyword monitoring, `.txt` file logging, and alert configuration.
    pub watch: WatchConfig,
    /// `true` = separate native OS window (`show_viewport_immediate`);
    /// `false` = floating `egui::Window` inside the main Ruxt window.
    pub detached: bool,
    /// Present when this viewer is backed by a local file tailer thread.
    pub local_handle: Option<LocalTailHandle>,
    /// Active `sudo` password prompt detected on the remote PTY (e.g. `[sudo] password for user:`).
    sudo_prompt: Option<String>,
    /// Password input buffer for `sudo_prompt` (wiped immediately on submit/close/drop).
    sudo_password: Zeroizing<String>,
    /// Request keyboard focus on the `sudo` password field once when a prompt appears.
    sudo_focus_requested: bool,
}

impl TailViewer {
    pub fn new(
        id: TailId,
        tab_id: TabId,
        session_title: String,
        spec: TailSpec,
        local_handle: Option<LocalTailHandle>,
    ) -> Self {
        let watch = WatchConfig::new(&spec, &session_title);
        Self {
            id,
            tab_id,
            session_title,
            spec,
            lines: VecDeque::new(),
            partial_line: String::new(),
            total_bytes: 0,
            total_lines: 0,
            running: true,
            status: tr("streaming").into(),
            follow: true,
            paused: false,
            paused_lines: VecDeque::new(),
            paused_bytes: 0,
            wrap: true,
            filter: String::new(),
            filter_only_matching: true,
            case_sensitive: false,
            watch,
            detached: true,
            local_handle,
            sudo_prompt: None,
            sudo_password: Zeroizing::new(String::new()),
            sudo_focus_requested: false,
        }
    }

    pub fn window_title(&self) -> String {
        if self.watch.hit_count > 0 {
            format!(
                "Tail Viewer [{}] — {} ({})",
                self.watch.hit_count,
                self.spec.display_title(),
                self.session_title
            )
        } else {
            format!("Tail Viewer — {} ({})", self.spec.display_title(), self.session_title)
        }
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len() as u64);
        self.ingest_bytes(bytes);
    }

    fn flush_paused(&mut self) {
        while let Some(line) = self.paused_lines.pop_front() {
            self.push_line_to_buffer(line);
        }
        self.paused_bytes = 0;
    }

    fn ingest_bytes(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let clean = strip_ansi(&text);
        let mut saw_newline = false;
        for ch in clean.chars() {
            if ch == '\r' {
                continue;
            }
            if ch == '\n' {
                saw_newline = true;
                let line = std::mem::take(&mut self.partial_line);
                self.on_incoming_line(line);
            } else {
                self.partial_line.push(ch);
            }
        }
        if self.spec.uses_sudo && self.running {
            let pending = self.partial_line.trim();
            if !pending.is_empty() && looks_like_sudo_prompt(pending, self.total_lines) {
                if self.sudo_prompt.as_deref() != Some(pending) {
                    self.sudo_prompt = Some(pending.to_string());
                    self.sudo_focus_requested = true;
                }
            } else if saw_newline && pending.is_empty() {
                self.sudo_prompt = None;
            }
        }
    }

    fn on_incoming_line(&mut self, line: String) {
        if self.watch.matches(&line, self.case_sensitive) {
            self.watch.record_hit(&line);
            if self.paused {
                self.watch.unread_hits = self.watch.unread_hits.saturating_add(1);
            } else if !self.follow {
                if self.watch.jump_on_hit {
                    self.watch.force_scroll_bottom = true;
                    self.watch.unread_hits = 0;
                } else {
                    self.watch.unread_hits = self.watch.unread_hits.saturating_add(1);
                }
            } else {
                self.watch.unread_hits = 0;
            }
        }

        if self.paused {
            // Cap paused backlog at 8 MiB / MAX_BUFFER_LINES so memory stays bounded.
            if self.paused_bytes < 8 * 1024 * 1024 {
                self.paused_bytes = self.paused_bytes.saturating_add(line.len() + 1);
                self.paused_lines.push_back(line);
                while self.paused_lines.len() > MAX_BUFFER_LINES {
                    if let Some(old) = self.paused_lines.pop_front() {
                        self.paused_bytes = self.paused_bytes.saturating_sub(old.len() + 1);
                    }
                }
            }
        } else {
            self.push_line_to_buffer(line);
        }
    }

    fn push_line_to_buffer(&mut self, line: String) {
        self.total_lines = self.total_lines.saturating_add(1);
        self.lines.push_back(line);
        while self.lines.len() > MAX_BUFFER_LINES {
            self.lines.pop_front();
        }
    }

    pub fn mark_closed(&mut self, reason: String) {
        self.flush_paused();
        if !self.partial_line.is_empty() {
            let rem = std::mem::take(&mut self.partial_line);
            self.on_incoming_line(rem);
            self.flush_paused();
        }
        self.watch.flush_writer();
        self.running = false;
        self.status = reason;
        self.local_handle = None;
        self.sudo_prompt = None;
        self.sudo_password.zeroize();
        self.sudo_focus_requested = false;
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.partial_line.clear();
        self.paused_lines.clear();
        self.paused_bytes = 0;
        self.total_bytes = 0;
        self.total_lines = 0;
        self.watch.unread_hits = 0;
        self.sudo_prompt = None;
        self.sudo_password.zeroize();
        self.sudo_focus_requested = false;
    }

    /// Render the Tail Viewer (either as a standalone native viewport or as an in-app window).
    pub fn show(&mut self, ctx: &Context, font_size: f32) -> TailViewerAction {
        if self.detached {
            let vp_id = ViewportId::from_hash_of(("ruxt-tail-viewer", self.id));
            let builder = ViewportBuilder::default()
                .with_title(self.window_title())
                .with_inner_size([920.0, 600.0])
                .with_min_inner_size([460.0, 280.0]);

            let mut action = TailViewerAction::None;
            ctx.show_viewport_immediate(vp_id, builder, |vp_ui, class| {
                let vp_ctx = vp_ui.ctx().clone();
                if class != ViewportClass::Immediate {
                    // Backend does not support multiple OS windows; fall back to in-app window.
                    self.detached = false;
                    action = self.show_docked(&vp_ctx, font_size);
                    return;
                }
                inject_viewport_clipboard(&vp_ctx);
                if vp_ctx.input(|i| i.viewport().close_requested()) {
                    action = TailViewerAction::Close(self.id);
                    return;
                }
                if self.watch.pending_attention {
                    self.watch.pending_attention = false;
                    vp_ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                }
                let (flashing, blink_on) = self.watch.flash_state();
                if flashing {
                    vp_ctx.request_repaint_after(Duration::from_millis(WATCH_BLINK_INTERVAL_MS as u64));
                }
                egui::Panel::top(Id::unique(("tv-top", self.id))).show(vp_ui, |ui| {
                    self.toolbar_ui(ui, &mut action);
                    if self.running && self.sudo_prompt.is_some() {
                        ui.separator();
                        self.sudo_prompt_bar_ui(ui, &mut action);
                    }
                    if self.watch.show_bar {
                        ui.separator();
                        self.watch_bar_ui(ui, blink_on);
                    }
                    if self.watch.unread_hits > 0 {
                        ui.separator();
                        self.unread_hit_banner_ui(ui);
                    }
                });
                egui::Panel::bottom(Id::unique(("tv-bottom", self.id))).show(vp_ui, |ui| {
                    self.status_bar_ui(ui);
                });
                egui::CentralPanel::default().show(vp_ui, |ui| {
                    self.log_body_ui(ui, font_size, &mut action);
                    if blink_on {
                        paint_alert_border(ui);
                    }
                });
            });
            action
        } else {
            self.show_docked(ctx, font_size)
        }
    }

    fn show_docked(&mut self, ctx: &Context, font_size: f32) -> TailViewerAction {
        if self.watch.pending_attention {
            self.watch.pending_attention = false;
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        let (flashing, blink_on) = self.watch.flash_state();
        if flashing {
            ctx.request_repaint_after(Duration::from_millis(WATCH_BLINK_INTERVAL_MS as u64));
        }

        let mut open = true;
        let mut action = TailViewerAction::None;
        let max = super::kit::max_window_size(ctx);
        egui::Window::new(self.window_title())
            .id(Id::unique(("ruxt-tail-window", self.id)))
            .open(&mut open)
            .resizable(true)
            .collapsible(true)
            .default_size(Vec2::new(840.0_f32.min(max.x), 520.0_f32.min(max.y)))
            .max_size(max)
            .show(ctx, |ui| {
                self.toolbar_ui(ui, &mut action);
                if self.running && self.sudo_prompt.is_some() {
                    ui.separator();
                    self.sudo_prompt_bar_ui(ui, &mut action);
                }
                if self.watch.show_bar {
                    ui.separator();
                    self.watch_bar_ui(ui, blink_on);
                }
                if self.watch.unread_hits > 0 {
                    ui.separator();
                    self.unread_hit_banner_ui(ui);
                }
                ui.separator();
                let avail_h = (ui.available_height() - 28.0).max(120.0);
                ui.allocate_ui(Vec2::new(ui.available_width(), avail_h), |ui| {
                    self.log_body_ui(ui, font_size, &mut action);
                    if blink_on {
                        paint_alert_border(ui);
                    }
                });
                ui.separator();
                self.status_bar_ui(ui);
            });
        if !open {
            action = TailViewerAction::Close(self.id);
        }
        action
    }

    fn toolbar_ui(&mut self, ui: &mut Ui, action: &mut TailViewerAction) {
        ui.horizontal_wrapped(|ui| {
            let (dot_color, state_text) = if !self.running {
                (Color32::GRAY, self.status.as_str())
            } else if self.paused {
                (Color32::from_rgb(0xe0, 0xa0, 0x40), tr("paused"))
            } else {
                (Color32::from_rgb(0x60, 0xc0, 0x60), tr("streaming"))
            };
            ui.colored_label(dot_color, "●").on_hover_text(state_text);
            ui.label(RichText::new(&self.spec.raw_cmd).monospace().strong());

            ui.separator();

            let watch_label = if self.watch.hit_count > 0 {
                trf("Watch ({n})", &[("{n}", &self.watch.hit_count)])
            } else {
                tr("Watch").to_string()
            };
            if super::kit::toggle(ui, self.watch.show_bar, watch_label)
                .on_hover_text(tr("Toggle the keyword watch & file logging bar"))
                .clicked()
            {
                self.watch.show_bar = !self.watch.show_bar;
            }

            ui.separator();

            ui.label(tr("Filter"));
            let r = ui.add(
                TextEdit::singleline(&mut self.filter)
                    .hint_text(tr("keyword…"))
                    .desired_width(130.0),
            );
            if !self.filter.is_empty() && ui.small_button("x").on_hover_text(tr("Clear filter")).clicked() {
                self.filter.clear();
            }
            if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.filter.clear();
            }
            ui.checkbox(&mut self.filter_only_matching, tr(" matching only"))
                .on_hover_text(tr("Show only lines containing the filter text"));
            ui.checkbox(&mut self.case_sensitive, "Aa")
                .on_hover_text(tr("Case-sensitive filter"));

            ui.separator();

            if ui
                .checkbox(&mut self.follow, tr("Follow"))
                .on_hover_text(tr("Automatically scroll to the newest log lines"))
                .changed()
                && self.follow
            {
                self.watch.unread_hits = 0;
            }

            let pause_label = if self.paused {
                format!("▶ {}", tr("Resume"))
            } else {
                format!("⏸ {}", tr("Pause"))
            };
            if super::kit::toggle(ui, self.paused, pause_label)
                .on_hover_text(tr("Freeze the view while incoming lines buffer in the background"))
                .clicked()
            {
                self.paused = !self.paused;
                if !self.paused {
                    self.flush_paused();
                    if self.follow {
                        self.watch.unread_hits = 0;
                    }
                }
            }

            ui.checkbox(&mut self.wrap, tr("Wrap"))
                .on_hover_text(tr("Wrap long lines to the window width"));

            if ui
                .button(tr("Copy all"))
                .on_hover_text(tr("Copy visible lines to the clipboard"))
                .clicked()
            {
                let text = self.collect_visible_text();
                if !text.is_empty() {
                    *action = TailViewerAction::Copy(text);
                }
            }

            if ui
                .button(tr("Clear"))
                .on_hover_text(tr("Clear buffered log lines"))
                .clicked()
            {
                self.clear();
            }

            if self.running {
                if ui
                    .button(tr("Stop"))
                    .on_hover_text(tr("Stop the background tail stream"))
                    .clicked()
                {
                    *action = TailViewerAction::Stop(self.id);
                }
            } else if ui
                .button(tr("Restart"))
                .on_hover_text(tr("Start the tail command again"))
                .clicked()
            {
                *action = TailViewerAction::Restart(self.id);
            }

            let dock_label = if self.detached { tr("Dock") } else { tr("Pop out") };
            let dock_tip = if self.detached {
                tr("Attach inside the main Ruxt window")
            } else {
                tr("Detach into a separate OS window")
            };
            if ui.button(dock_label).on_hover_text(dock_tip).clicked() {
                self.detached = !self.detached;
            }
        });
    }

    fn sudo_prompt_bar_ui(&mut self, ui: &mut Ui, action: &mut TailViewerAction) {
        let Some(prompt) = self.sudo_prompt.clone() else {
            return;
        };
        let dark = ui.visuals().dark_mode;
        let bg = if dark {
            Color32::from_rgb(0x3a, 0x2c, 0x16)
        } else {
            Color32::from_rgb(0xff, 0xf3, 0xd6)
        };
        let border = if dark {
            Color32::from_rgb(0xb8, 0x82, 0x2a)
        } else {
            Color32::from_rgb(0xd4, 0x9b, 0x35)
        };
        egui::Frame::NONE
            .fill(bg)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&prompt).monospace().strong());
                    let r = ui.add(
                        TextEdit::singleline(&mut *self.sudo_password)
                            .password(true)
                            .hint_text(tr("Enter sudo password…"))
                            .desired_width(200.0),
                    );
                    if self.sudo_focus_requested {
                        r.request_focus();
                        self.sudo_focus_requested = false;
                    }
                    let submitted = (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))
                        || ui.button(tr("Send")).clicked();
                    if submitted {
                        let mut bytes = Vec::with_capacity(self.sudo_password.len() + 1);
                        bytes.extend_from_slice(self.sudo_password.as_bytes());
                        bytes.push(b'\n');
                        *action = TailViewerAction::SendInput(self.id, Zeroizing::new(bytes));
                        self.sudo_password.zeroize();
                        self.sudo_prompt = None;
                        self.partial_line.clear();
                    }
                });
            });
    }

    fn watch_bar_ui(&mut self, ui: &mut Ui, blink_on: bool) {
        let dark = ui.visuals().dark_mode;
        let bg = if blink_on {
            if dark {
                Color32::from_rgb(0x52, 0x22, 0x1c)
            } else {
                Color32::from_rgb(0xff, 0xd8, 0xd0)
            }
        } else if self.watch.is_active() {
            if dark {
                Color32::from_rgb(0x22, 0x2c, 0x26)
            } else {
                Color32::from_rgb(0xee, 0xf5, 0xf0)
            }
        } else {
            ui.visuals().faint_bg_color
        };

        egui::Frame::NONE
            .fill(bg)
            .corner_radius(CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                // Row 1: Keywords, OR/AND mode, Watched-only filter, Alert & Jump toggles
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.watch.enabled, tr("Watch"))
                        .on_hover_text(tr("Enable keyword monitoring on incoming log lines"));

                    let kw_resp = ui.add(
                        TextEdit::singleline(&mut self.watch.raw_keywords)
                            .hint_text(tr("words (comma-separated)…"))
                            .desired_width(210.0),
                    );
                    if !self.watch.raw_keywords.is_empty()
                        && ui.small_button("x").on_hover_text(tr("Clear watch keywords")).clicked()
                    {
                        self.watch.raw_keywords.clear();
                    }
                    if kw_resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                        self.watch.raw_keywords.clear();
                    }

                    // Show parsed keyword chips
                    let parsed: Vec<String> = self.watch.keywords().into_iter().map(String::from).collect();
                    for kw in &parsed {
                        let chip_bg = if dark {
                            Color32::from_rgb(0x35, 0x48, 0x3c)
                        } else {
                            Color32::from_rgb(0xd8, 0xe8, 0xdc)
                        };
                        egui::Frame::NONE
                            .fill(chip_bg)
                            .corner_radius(CornerRadius::same(3))
                            .inner_margin(egui::Margin::symmetric(5, 1))
                            .show(ui, |ui| {
                                ui.label(RichText::new(kw).monospace().small());
                            });
                    }

                    let mode_label = if self.watch.match_all {
                        tr("Match ALL (AND)")
                    } else {
                        tr("Match ANY (OR)")
                    };
                    if ui
                        .button(mode_label)
                        .on_hover_text(tr(
                            "Trigger when any keyword is present vs. when all keywords are present in the same line",
                        ))
                        .clicked()
                    {
                        self.watch.match_all = !self.watch.match_all;
                    }

                    ui.checkbox(&mut self.watch.filter_watched_only, tr(" watched only"))
                        .on_hover_text(tr("Show only lines that match the watch keywords"));

                    ui.separator();

                    ui.checkbox(&mut self.watch.alert_visual, tr("Blink & highlight"))
                        .on_hover_text(tr(
                            "Flash the viewer window and highlight matched lines when a watch keyword is detected",
                        ));

                    ui.checkbox(&mut self.watch.jump_on_hit, tr("Jump on hit"))
                        .on_hover_text(tr(
                            "Automatically scroll to the newest matched line even when Follow is off",
                        ));

                    if self.watch.hit_count > 0 {
                        ui.separator();
                        let badge_color = if dark {
                            Color32::from_rgb(0xf2, 0x8b, 0x66)
                        } else {
                            Color32::from_rgb(0xc0, 0x30, 0x18)
                        };
                        ui.colored_label(
                            badge_color,
                            RichText::new(trf("hits: {n}", &[("{n}", &self.watch.hit_count)])).strong(),
                        );
                        if ui.small_button("↺").on_hover_text(tr("Reset hit counter")).clicked() {
                            self.watch.reset_hits();
                        }
                    }
                });

                // Row 2: Separate .txt file logging options (Format: Timestamped vs Raw, Path, Browse, Open)
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.watch.save_to_file, tr("Save to .txt"))
                        .on_hover_text(tr("Append matched log lines to a separate text file in real time"));

                    let fmt_label = match self.watch.log_format {
                        WatchLogFormat::Timestamped => tr("[Timestamp] + line"),
                        WatchLogFormat::Raw => tr("Raw line only"),
                    };
                    egui::ComboBox::from_id_salt(("watch-log-fmt", self.id))
                        .selected_text(fmt_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.watch.log_format,
                                WatchLogFormat::Timestamped,
                                tr("[Timestamp] + line"),
                            );
                            ui.selectable_value(&mut self.watch.log_format, WatchLogFormat::Raw, tr("Raw line only"));
                        })
                        .response
                        .on_hover_text(tr("Choose whether to prefix each saved line with the local timestamp"));

                    let mut path_str = self.watch.log_file_path.display().to_string();
                    let path_resp = ui.add(
                        TextEdit::singleline(&mut path_str)
                            .desired_width(280.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    if path_resp.changed() {
                        self.watch.log_file_path = PathBuf::from(path_str);
                        self.watch.writer = None;
                    }

                    if ui
                        .button(tr("Browse…"))
                        .on_hover_text(tr("Select watch log file (.txt)"))
                        .clicked()
                    {
                        let mut dlg = rfd::FileDialog::new().add_filter("Text / Log", &["txt", "log"]);
                        if let Some(parent) = self.watch.log_file_path.parent() {
                            if !parent.as_os_str().is_empty() {
                                dlg = dlg.set_directory(parent);
                            }
                        }
                        if let Some(name) = self.watch.log_file_path.file_name().and_then(|n| n.to_str()) {
                            dlg = dlg.set_file_name(name);
                        }
                        if let Some(picked) = dlg.save_file() {
                            self.watch.log_file_path = picked;
                            self.watch.writer = None;
                            self.watch.last_write_error = None;
                        }
                    }

                    let file_exists = self.watch.log_file_path.exists();
                    if ui
                        .add_enabled(file_exists, egui::Button::new(tr("Open")))
                        .on_hover_text(tr("Open the recorded watch log file"))
                        .clicked()
                    {
                        self.watch.flush_writer();
                        let _ = open::that(&self.watch.log_file_path);
                    }

                    if let Some(err) = &self.watch.last_write_error {
                        ui.colored_label(
                            Color32::from_rgb(0xe0, 0x60, 0x60),
                            RichText::new(trf("file err: {e}", &[("{e}", err)])).small(),
                        );
                    }
                });
            });
    }

    fn unread_hit_banner_ui(&mut self, ui: &mut Ui) {
        let dark = ui.visuals().dark_mode;
        let banner_bg = if dark {
            Color32::from_rgb(0x58, 0x32, 0x18)
        } else {
            Color32::from_rgb(0xff, 0xe4, 0xc4)
        };
        let text_col = if dark {
            Color32::from_rgb(0xff, 0xd8, 0xa8)
        } else {
            Color32::from_rgb(0x7a, 0x2e, 0x00)
        };
        egui::Frame::NONE
            .fill(banner_bg)
            .corner_radius(CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        text_col,
                        RichText::new(trf(
                            "🔔 New watched log: {n} line(s) matched",
                            &[("{n}", &self.watch.unread_hits)],
                        ))
                        .strong(),
                    );
                    if ui.button(tr("Jump to hit")).clicked() {
                        if self.paused {
                            self.paused = false;
                            self.flush_paused();
                        }
                        self.watch.force_scroll_bottom = true;
                        self.watch.unread_hits = 0;
                    }
                    if ui.small_button(tr("Dismiss")).clicked() {
                        self.watch.unread_hits = 0;
                    }
                });
            });
    }

    fn log_body_ui(&mut self, ui: &mut Ui, font_size: f32, action: &mut TailViewerAction) {
        let font_id = FontId::monospace(font_size);
        let has_filter = !self.filter.is_empty();
        let case_sensitive = self.case_sensitive;
        let filter_only_matching = self.filter_only_matching;
        let filter_watched_only = self.watch.filter_watched_only && self.watch.is_active();
        let alert_visual = self.watch.alert_visual;
        let wrap = self.wrap;
        let needle = if case_sensitive {
            self.filter.clone()
        } else {
            self.filter.to_lowercase()
        };

        let matches_filter = |line: &str| -> bool {
            if !has_filter {
                return true;
            }
            if case_sensitive {
                line.contains(&needle)
            } else {
                line.to_lowercase().contains(&needle)
            }
        };

        ui.style_mut().interaction.selectable_labels = true;

        let stick_bottom = (self.follow && !self.paused) || self.watch.force_scroll_bottom;
        let scroll = ScrollArea::both()
            .id_salt(("tv-scroll", self.id))
            .auto_shrink([false, false])
            .stick_to_bottom(stick_bottom);

        let mut clear_req = false;
        let mut copy_all_req = false;
        let dark = ui.visuals().dark_mode;

        scroll.show(ui, |ui| {
            if wrap {
                ui.set_width(ui.available_width());
            }
            let mut shown = 0usize;
            for line in &self.lines {
                let watch_matched = self.watch.matches(line, case_sensitive);
                if filter_watched_only && !watch_matched {
                    continue;
                }
                let filter_matched = matches_filter(line);
                if has_filter && filter_only_matching && !filter_matched {
                    continue;
                }
                shown += 1;
                let color = line_severity_color(line, dark);
                let display_str = if watch_matched && alert_visual {
                    format!("⚑ {line}")
                } else {
                    line.clone()
                };
                let mut rt = RichText::new(display_str).font(font_id.clone());
                if let Some(c) = color {
                    rt = rt.color(c);
                } else if watch_matched && alert_visual {
                    rt = rt.color(if dark {
                        Color32::from_rgb(0xff, 0xb8, 0x90)
                    } else {
                        Color32::from_rgb(0x9c, 0x24, 0x0c)
                    });
                }
                if watch_matched && alert_visual {
                    rt = rt.background_color(if dark {
                        Color32::from_rgb(0x4a, 0x22, 0x1a)
                    } else {
                        Color32::from_rgb(0xff, 0xe0, 0xd4)
                    });
                } else if has_filter && !filter_only_matching && filter_matched {
                    rt = rt.background_color(if dark {
                        Color32::from_rgb(0x55, 0x48, 0x18)
                    } else {
                        Color32::from_rgb(0xff, 0xeb, 0x80)
                    });
                }
                let label = if wrap {
                    egui::Label::new(rt).wrap().selectable(true)
                } else {
                    egui::Label::new(rt).extend().selectable(true)
                };
                let resp = ui.add(label);
                resp.context_menu(|ui| {
                    if ui.button(tr("Copy line")).clicked() {
                        *action = TailViewerAction::Copy(line.clone());
                        ui.close();
                    }
                    if ui.button(tr("Copy all")).clicked() {
                        copy_all_req = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(tr("Clear")).clicked() {
                        clear_req = true;
                        ui.close();
                    }
                });
            }
            if !self.partial_line.is_empty() {
                let watch_matched = self.watch.matches(&self.partial_line, case_sensitive);
                let filter_matched = matches_filter(&self.partial_line);
                if (!filter_watched_only || watch_matched) && (!has_filter || !filter_only_matching || filter_matched) {
                    shown += 1;
                    let rt = RichText::new(self.partial_line.as_str()).font(font_id);
                    ui.add(egui::Label::new(rt).selectable(true));
                }
            }
            if shown == 0 {
                if self.lines.is_empty() && self.partial_line.is_empty() {
                    ui.label(
                        RichText::new(if self.running {
                            tr("Waiting for log output…")
                        } else {
                            tr("No output.")
                        })
                        .weak(),
                    );
                } else if filter_watched_only {
                    ui.label(RichText::new(tr("No lines match the current watch keywords.")).weak());
                } else {
                    ui.label(RichText::new(tr("No lines match the current filter.")).weak());
                }
            }
            if self.watch.force_scroll_bottom {
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                self.watch.force_scroll_bottom = false;
            }
        });

        if copy_all_req {
            let all = self.collect_visible_text();
            if !all.is_empty() {
                *action = TailViewerAction::Copy(all);
            }
        }
        if clear_req {
            self.clear();
        }
    }

    fn status_bar_ui(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let state = if !self.running {
                format!("{}: {}", tr("stopped"), self.status)
            } else if self.paused {
                trf("paused ({n} B queued)", &[("{n}", &self.paused_bytes)])
            } else {
                tr("streaming").into()
            };
            ui.label(RichText::new(state).small());
            ui.separator();
            ui.label(
                RichText::new(trf(
                    "{shown} / {total} lines",
                    &[("{shown}", &self.lines.len()), ("{total}", &self.total_lines)],
                ))
                .small(),
            );
            ui.separator();
            ui.label(RichText::new(humansize::format_size(self.total_bytes, humansize::BINARY)).small());
            if self.watch.is_active() || self.watch.hit_count > 0 {
                ui.separator();
                ui.label(RichText::new(trf("hits: {n}", &[("{n}", &self.watch.hit_count)])).small());
            }
            if let Some(cwd) = &self.spec.cwd {
                ui.separator();
                ui.label(RichText::new(format!("cwd: {cwd}")).small().weak());
            }
        });
    }

    fn collect_visible_text(&self) -> String {
        let has_filter = !self.filter.is_empty() && self.filter_only_matching;
        let filter_watched_only = self.watch.filter_watched_only && self.watch.is_active();
        let needle = if self.case_sensitive {
            self.filter.clone()
        } else {
            self.filter.to_lowercase()
        };
        let mut out = String::new();
        for line in &self.lines {
            if filter_watched_only && !self.watch.matches(line, self.case_sensitive) {
                continue;
            }
            if has_filter {
                let ok = if self.case_sensitive {
                    line.contains(&needle)
                } else {
                    line.to_lowercase().contains(&needle)
                };
                if !ok {
                    continue;
                }
            }
            out.push_str(line);
            out.push('\n');
        }
        if !self.partial_line.is_empty() {
            out.push_str(&self.partial_line);
        }
        out
    }
}

/// Draw a blinking alert border around the log area when a watch hit just occurred.
fn paint_alert_border(ui: &mut Ui) {
    let rect = ui.max_rect().shrink(1.5);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(3),
        Stroke::new(2.5, Color32::from_rgb(0xf2, 0x5c, 0x38)),
        StrokeKind::Inside,
    );
}

/// Colour-code log lines by severity keywords (ERROR/FATAL, WARN, DEBUG/TRACE).
fn line_severity_color(line: &str, dark: bool) -> Option<Color32> {
    // Quick case-insensitive check for common log severity markers.
    let upper = line.to_ascii_uppercase();
    if upper.contains("ERROR")
        || upper.contains("FATAL")
        || upper.contains("CRITICAL")
        || upper.contains("PANIC")
        || upper.contains("EXCEPTION")
    {
        return Some(if dark {
            Color32::from_rgb(0xf2, 0x78, 0x78)
        } else {
            Color32::from_rgb(0xb8, 0x1d, 0x1d)
        });
    }
    if upper.contains("WARN") {
        return Some(if dark {
            Color32::from_rgb(0xf0, 0xc0, 0x58)
        } else {
            Color32::from_rgb(0x9a, 0x67, 0x00)
        });
    }
    if upper.contains("DEBUG") || upper.contains("TRACE") {
        return Some(if dark {
            Color32::from_rgb(0x88, 0x96, 0x8e)
        } else {
            Color32::from_rgb(0x66, 0x70, 0x6b)
        });
    }
    None
}

/// Strip ANSI CSI / OSC escape sequences so raw terminal colour codes don't clutter the viewer.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    // CSI sequence: skip until 0x40..=0x7E
                    for ch in chars.by_ref() {
                        if ('@'..='~').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC sequence: skip until BEL (0x07) or ST (ESC \)
                    let mut prev_esc = false;
                    for ch in chars.by_ref() {
                        if ch == '\x07' || (prev_esc && ch == '\\') {
                            break;
                        }
                        prev_esc = ch == '\x1b';
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Inject Ctrl+C / Ctrl+V / Ctrl+X events into a secondary viewport (since `clipboard_shortcuts`
/// is disabled globally on `eframe::NativeOptions`).
fn inject_viewport_clipboard(ctx: &Context) {
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
            let cmd = (m.command || m.ctrl) && !m.alt;
            match key {
                Key::V if cmd => paste = true,
                Key::Insert if m.shift && !m.command => paste = true,
                Key::Paste => paste = true,
                Key::C if cmd => copy = true,
                Key::Insert if cmd && !m.shift => copy = true,
                Key::Copy => copy = true,
                Key::X if cmd => cut = true,
                Key::Delete if m.shift && !m.command => cut = true,
                Key::Cut => cut = true,
                _ => {}
            }
        }
    });
    if !(paste || copy || cut) {
        return;
    }
    let text = if paste {
        arboard::Clipboard::new()
            .and_then(|mut c| c.get_text())
            .ok()
            .map(|t| t.replace("\r\n", "\n"))
            .filter(|t| !t.is_empty())
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

/// Heuristic to detect an interactive `sudo` password prompt sitting in `partial_line` (no trailing
/// newline). Covers standard English, localized (Korean, Japanese, Chinese, European), and custom
/// `sudo -p` prompts while avoiding false positives once log lines are streaming.
fn looks_like_sudo_prompt(trimmed: &str, total_lines: u64) -> bool {
    if !trimmed.ends_with(':') || trimmed.len() > 160 {
        return false;
    }
    if total_lines == 0 {
        return true;
    }
    let lower = trimmed.to_lowercase();
    lower.contains("sudo")
        || lower.contains("password")
        || lower.contains("passcode")
        || lower.contains("passphrase")
        || lower.contains("암호")
        || lower.contains("비밀번호")
        || lower.contains("パスワード")
        || lower.contains("密码")
        || lower.contains("mot de passe")
        || lower.contains("passwort")
        || lower.contains("contraseña")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_keywords_split_and_match() {
        let spec = TailSpec::for_file("/var/log/app.log", None);
        let mut cfg = WatchConfig::new(&spec, "prod");
        cfg.raw_keywords = "ERROR, Timeout, Deadlock".into();
        assert_eq!(cfg.keywords(), vec!["ERROR", "Timeout", "Deadlock"]);

        // Case-insensitive OR matching
        assert!(cfg.matches("2026-10-30 [warn] connection timeout after 5s", false));
        assert!(!cfg.matches("2026-10-30 [info] healthy request", false));

        // Case-sensitive OR matching
        assert!(!cfg.matches("2026-10-30 [warn] connection timeout after 5s", true));
        assert!(cfg.matches("2026-10-30 [warn] connection Timeout after 5s", true));

        // AND matching (`match_all = true`)
        cfg.match_all = true;
        assert!(!cfg.matches("ERROR in worker thread", false));
        assert!(cfg.matches("ERROR: Timeout waiting for Deadlock detector", false));

        // Space-separated keywords when no comma is used
        cfg.raw_keywords = "FATAL PANIC".into();
        cfg.match_all = false;
        assert_eq!(cfg.keywords(), vec!["FATAL", "PANIC"]);
        assert!(cfg.matches("kernel panic: out of memory", false));
    }

    #[test]
    fn tail_viewer_records_to_file_and_tracks_unread_hits() {
        let spec = TailSpec::for_file("test.log", None);
        let mut viewer = TailViewer::new(1, 1, "srv".into(), spec, None);
        let tmp = std::env::temp_dir().join(format!("ruxt-watch-test-{}.txt", rand::random::<u64>()));
        viewer.watch.raw_keywords = "ERROR, critical".into();
        viewer.watch.save_to_file = true;
        viewer.watch.log_format = WatchLogFormat::Raw;
        viewer.watch.log_file_path = tmp.clone();
        viewer.follow = false;
        viewer.watch.jump_on_hit = false;

        viewer.push_bytes(b"normal line 1\nERROR database unreachable\nnormal line 2\nCRITICAL disk full\n");

        assert_eq!(viewer.total_lines, 4);
        assert_eq!(viewer.watch.hit_count, 2);
        assert_eq!(viewer.watch.unread_hits, 2);
        assert!(viewer.watch.pending_attention);

        let saved = std::fs::read_to_string(&tmp).expect("watch log file written");
        assert_eq!(saved, "ERROR database unreachable\nCRITICAL disk full\n");

        // Now test Jump on hit when Follow is off
        viewer.watch.jump_on_hit = true;
        viewer.push_bytes(b"ERROR second hit\n");
        assert_eq!(viewer.watch.hit_count, 3);
        assert!(viewer.watch.force_scroll_bottom);
        assert_eq!(viewer.watch.unread_hits, 0);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn detects_sudo_password_prompt_and_clears_after_output() {
        let spec = crate::core::tail::parse_tail_cmd("sudo tail -f /var/log/syslog").unwrap();
        let mut viewer = TailViewer::new(2, 1, "srv".into(), spec, None);

        viewer.push_bytes(b"[sudo] password for alice: ");
        assert_eq!(viewer.sudo_prompt.as_deref(), Some("[sudo] password for alice:"));

        // Localized prompt after a failed attempt
        viewer.push_bytes(b"\r\nSorry, try again.\r\n[sudo] alice \xec\x95\x94\xed\x98\xb8: ");
        assert_eq!(viewer.sudo_prompt.as_deref(), Some("[sudo] alice 암호:"));

        // Once log output streams with a trailing newline, the prompt disappears.
        viewer.push_bytes(b"\r\nNov 26 21:00:00 host kernel: ready\r\n");
        assert!(viewer.sudo_prompt.is_none());
    }
}
