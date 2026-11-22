//! SFTP file browser panel, laid out like Windows Explorer.
//!
//! * Navigation bar: back / forward / up / refresh, a breadcrumb address bar that turns into
//!   a text field when clicked, and a home button.
//! * Command bar: upload, download, new folder, rename, permissions, delete, search, view.
//! * Left: folder tree (populated from every listing; unknown branches are fetched lazily
//!   with `SftpCmd::Peek`, which never changes the current directory).
//! * Centre: optional local pane (this PC) and the remote details view, both the same
//!   sortable list (icon, name, date, type, size, permissions) with full-row selection,
//!   Ctrl/Shift multi-select and keyboard navigation.
//! * Drag and drop: rows dragged onto a folder (list row or tree node) are moved; local rows
//!   dropped on the remote pane are uploaded, remote rows dropped on the local pane are
//!   downloaded. Cut / copy / paste work within the remote side.
//! * Bottom: a one-line transfer summary (`done/total`). Click it, or drag its top edge,
//!   to open the progress list upward.
//!
//! Every user-facing string goes through `tr` (see `core::i18n`).

use crate::core::i18n::{tr, trf};
use crate::core::sftp::{self, DeleteItem, SftpCmd, SftpEntry, SftpEvent, TransferKind};
use egui::{
    Align2, Color32, CornerRadius, EventFilter, FontId, Key, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2,
};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// One remote file open in the built-in text editor window.
pub struct TextEditor {
    pub remote: String,
    name: String,
    text: String,
    /// Edited since the last load / successful save.
    dirty: bool,
    /// The file used CRLF line endings; they are restored on save.
    crlf: bool,
    /// Server mtime when loaded / last saved (detects someone else changing the file).
    mtime: Option<u32>,
    loading: bool,
    saving: bool,
    /// Status line: (message, is_error).
    status: Option<(String, bool)>,
    /// The last save was refused because the server copy changed.
    conflict: bool,
    /// The window's close button was pressed with unsaved changes.
    confirm_close: bool,
    /// Close the window once the pending save succeeds.
    close_after_save: bool,
    /// Give the text area the keyboard once the content arrives.
    focus_text: bool,
}

impl TextEditor {
    fn new(remote: String) -> Self {
        let name = crate::core::term::sanitize_title(&sftp::file_name_of(&remote));
        Self {
            remote,
            name,
            text: String::new(),
            dirty: false,
            crlf: false,
            mtime: None,
            loading: true,
            saving: false,
            status: None,
            conflict: false,
            confirm_close: false,
            close_after_save: false,
            focus_text: false,
        }
    }

    fn save_cmd(&mut self, force: bool) -> SftpCmd {
        self.saving = true;
        self.conflict = false;
        self.status = Some((tr("Saving…").into(), false));
        let data = if self.crlf {
            self.text.replace('\n', "\r\n").into_bytes()
        } else {
            self.text.clone().into_bytes()
        };
        SftpCmd::WriteText {
            remote: self.remote.clone(),
            data,
            expected_mtime: self.mtime,
            force,
        }
    }
}

/// Text the built-in editor can show: valid UTF-8 (a leading BOM is kept) without NUL bytes.
fn decode_text(bytes: Vec<u8>) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: u64,
    pub name: String,
    pub done: u64,
    pub total: u64,
    pub files_done: u32,
    pub files_total: u32,
    pub kind: TransferKind,
    pub result: Option<Result<(), String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Modified,
    Kind,
    Size,
}

/// Selection / sort state of one details view.
#[derive(Debug, Default)]
pub struct ListState {
    /// Indices into the entry list.
    pub selected: Vec<usize>,
    /// Entry index that anchors a Shift+click range selection.
    pub anchor: Option<usize>,
    /// Entry index to scroll into view (keyboard navigation).
    pub scroll_to: Option<usize>,
    pub sort: SortKey,
    pub sort_desc: bool,
    /// Cached `visible_sorted` result. Rebuilt when the listing or the view settings change.
    order: Vec<usize>,
    order_stamp: u64,
    order_hidden: bool,
    order_filter: String,
    order_sort: SortKey,
    order_desc: bool,
    order_lang: u8,
}

impl ListState {
    fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    /// Display order of `entries`. The `stamp` changes whenever that slice is replaced.
    fn ensure_order(&mut self, entries: &[SftpEntry], show_hidden: bool, filter: &str, stamp: u64) {
        let lang = crate::core::i18n::lang() as u8;
        let fresh = self.order_stamp == stamp
            && self.order_hidden == show_hidden
            && self.order_filter == filter
            && self.order_sort == self.sort
            && self.order_desc == self.sort_desc
            && self.order_lang == lang;
        if fresh {
            return;
        }
        self.order = visible_sorted(entries, self.sort, self.sort_desc, show_hidden, filter);
        self.order_stamp = stamp;
        self.order_hidden = show_hidden;
        self.order_filter = filter.to_string();
        self.order_sort = self.sort;
        self.order_desc = self.sort_desc;
        self.order_lang = lang;
    }
}

/// One cached sub-directory of a tree node.
#[derive(Debug, Clone)]
struct TreeChild {
    name: String,
    path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Remote,
    Local,
}

/// Items being dragged with the mouse.
#[derive(Debug, Clone)]
struct Drag {
    from: Pane,
    items: Vec<SftpEntry>,
}

/// The local ("this PC") pane.
pub struct LocalPane {
    pub cwd: PathBuf,
    pub entries: Vec<SftpEntry>,
    /// Bumped whenever `entries` is replaced, so the sort cache can see the change.
    entries_stamp: u64,
    pub list: ListState,
    pub error: Option<String>,
    path_input: String,
    address_editing: bool,
    address_focus: bool,
    loaded: bool,
}

impl Default for LocalPane {
    fn default() -> Self {
        let cwd = crate::core::hostkeys::home_dir()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            path_input: cwd.display().to_string(),
            cwd,
            entries: Vec::new(),
            entries_stamp: 0,
            list: ListState::default(),
            error: None,
            address_editing: false,
            address_focus: false,
            loaded: false,
        }
    }
}

impl LocalPane {
    fn go(&mut self, dir: PathBuf) {
        match read_local_dir(&dir) {
            Ok(entries) => {
                self.cwd = dir;
                self.entries = entries;
                self.entries_stamp = self.entries_stamp.wrapping_add(1);
                self.error = None;
            }
            Err(e) => {
                self.error = Some(trf(
                    "Cannot read {path}: {e}",
                    &[("{path}", &dir.display()), ("{e}", &e)],
                ));
            }
        }
        self.path_input = self.cwd.display().to_string();
        self.list.clear();
        self.loaded = true;
    }

    fn refresh(&mut self) {
        let d = self.cwd.clone();
        self.go(d);
    }
}

const ERR_COLOR: Color32 = Color32::from_rgb(0xe0, 0x60, 0x60);
const OK_COLOR: Color32 = Color32::from_rgb(0x60, 0xc0, 0x60);
const ICON: f32 = 16.0;

pub struct SftpPanelState {
    pub started: bool,
    pub cwd: String,
    /// Remote home directory (for `~` expansion when following the terminal).
    pub home: Option<String>,
    /// Last directory the terminal-follow logic asked for (avoids re-listing on every prompt).
    pub follow_last: Option<String>,
    pub path_input: String,
    pub entries: Vec<SftpEntry>,
    /// Bumped whenever `entries` is replaced, so the sort cache can see the change.
    entries_stamp: u64,
    pub list: ListState,
    pub filter: String,
    pub transfers: Vec<Transfer>,
    /// The bottom transfer list is open. Collapsed, only the done/total summary shows.
    transfers_expanded: bool,
    pub last_error: Option<String>,
    pub info: Option<String>,
    pub rename: Option<(String, String)>, // (path, new name)
    pub new_dir: Option<String>,
    pub chmod: Option<(String, String)>, // (path, octal text)
    pub show_hidden: bool,
    pub follow_terminal: bool,
    pub show_tree: bool,
    pub show_local: bool,
    pub local: LocalPane,

    // ---- explorer state ----
    /// The breadcrumb bar has been clicked and is a text field until Enter/Escape.
    address_editing: bool,
    address_focus: bool,
    history: Vec<String>,
    hist_pos: usize,
    /// History slot a back/forward navigation is heading for (set until its listing arrives).
    hist_pending: Option<usize>,
    /// Directory path → its sub-directories (sorted by name).
    tree: BTreeMap<String, Vec<TreeChild>>,
    tree_expanded: HashSet<String>,
    /// Flat list of expanded tree rows. Rebuilt when the tree or the expansion changes.
    tree_rows: Vec<TreeRow>,
    tree_stamp: u64,
    /// `u64::MAX` until the first build, so an empty tree still draws the root.
    tree_rows_stamp: u64,
    tree_rows_hidden: bool,
    /// Peeks in flight.
    tree_requested: HashSet<String>,
    /// Directories whose children the tree needs; filled by `apply`, drained into commands in `ui`.
    tree_wanted: Vec<String>,
    /// An inline editor was just opened and should take the keyboard.
    editor_focus: bool,
    /// Cut (`true`) or copied (`false`) remote items waiting for a paste.
    clip: Option<(Vec<SftpEntry>, bool)>,
    drag: Option<Drag>,
    /// Files open in the built-in text editor.
    pub text_editors: Vec<TextEditor>,
    /// Files the built-in editor could not show (binary / not UTF-8): hand them to the
    /// external editor on the next frame.
    external_wanted: Vec<String>,
}

impl Default for SftpPanelState {
    fn default() -> Self {
        Self {
            started: false,
            cwd: String::new(),
            home: None,
            follow_last: None,
            path_input: String::new(),
            entries: Vec::new(),
            entries_stamp: 0,
            list: ListState::default(),
            filter: String::new(),
            transfers: Vec::new(),
            transfers_expanded: false,
            last_error: None,
            info: None,
            rename: None,
            new_dir: None,
            chmod: None,
            show_hidden: false,
            follow_terminal: true,
            show_tree: true,
            show_local: false,
            local: LocalPane::default(),
            address_editing: false,
            address_focus: false,
            history: Vec::new(),
            hist_pos: 0,
            hist_pending: None,
            tree: BTreeMap::new(),
            tree_expanded: HashSet::new(),
            tree_rows: Vec::new(),
            tree_stamp: 0,
            tree_rows_stamp: u64::MAX,
            tree_rows_hidden: false,
            tree_requested: HashSet::new(),
            tree_wanted: Vec::new(),
            editor_focus: false,
            clip: None,
            drag: None,
            text_editors: Vec::new(),
            external_wanted: Vec::new(),
        }
    }
}

/// Actions the panel wants the app to perform (the app owns the command channel).
pub enum SftpAction {
    Cmd(SftpCmd),
    /// Items to confirm deleting, plus display names for the dialog.
    ConfirmDelete(Vec<(DeleteItem, String)>),
    /// Downloads whose local target already exists; the app confirms before sending them.
    ConfirmOverwrite {
        cmds: Vec<SftpCmd>,
        names: Vec<String>,
    },
    OpenInEditor(String),
    OpenInTailViewer(String),
}

/// Every proper ancestor of an absolute path, root first: `/a/b/c` → `/`, `/a`, `/a/b`.
fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !path.starts_with('/') {
        return out;
    }
    out.push("/".to_string());
    let mut cur = String::new();
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for p in parts.iter().take(parts.len().saturating_sub(1)) {
        cur.push('/');
        cur.push_str(p);
        out.push(cur.clone());
    }
    out
}

fn editor_window_id(remote: &str) -> egui::Id {
    egui::Id::unique(("sftp-edit-window", remote))
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("/")
}

/// Read a local directory into the same entry type the remote list uses.
pub fn read_local_dir(dir: &Path) -> Result<Vec<SftpEntry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        let raw_name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let ft = match e.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let is_symlink = ft.is_symlink();
        // Follow links for the type/size shown, like the remote list does.
        let md = if is_symlink {
            std::fs::metadata(&path).ok()
        } else {
            e.metadata().ok()
        };
        let (is_dir, is_regular, size, mtime, permissions) = match &md {
            Some(m) => (
                m.is_dir(),
                m.is_file(),
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as u32),
                local_mode(m),
            ),
            None => (false, false, 0, None, None),
        };
        out.push(SftpEntry {
            name: crate::core::term::sanitize_title(&raw_name),
            raw_name,
            path: path.to_string_lossy().into_owned(),
            is_dir,
            is_symlink,
            is_regular,
            size,
            mtime,
            permissions,
        });
    }
    out.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(out)
}

#[cfg(unix)]
fn local_mode(m: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(m.permissions().mode())
}
#[cfg(not(unix))]
fn local_mode(_m: &std::fs::Metadata) -> Option<u32> {
    None
}

impl SftpPanelState {
    pub fn apply(&mut self, ev: SftpEvent) {
        match ev {
            SftpEvent::Home(h) => {
                self.expand_to(&h);
                self.home = Some(h);
            }
            SftpEvent::Listing { path, entries } => {
                self.tree_set(&path, entries.iter());
                self.expand_to(&path);
                self.push_history(&path);
                self.cwd = path.clone();
                self.path_input = path;
                self.entries = entries;
                self.entries_stamp = self.entries_stamp.wrapping_add(1);
                self.list.clear();
                self.last_error = None;
                self.address_editing = false;
            }
            SftpEvent::Subdirs { path, dirs } => {
                self.tree_set(&path, dirs.iter());
            }
            SftpEvent::Progress {
                id,
                name,
                done,
                total,
                files_done,
                files_total,
                kind,
            } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.name = name;
                    t.done = done;
                    t.total = total;
                    t.files_done = files_done;
                    t.files_total = files_total;
                } else {
                    self.transfers.push(Transfer {
                        id,
                        name,
                        done,
                        total,
                        files_done,
                        files_total,
                        kind,
                        result: None,
                    });
                }
            }
            SftpEvent::Finished { id, name, result } => {
                let ok = result.is_ok();
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.result = Some(result);
                } else {
                    self.transfers.push(Transfer {
                        id,
                        name,
                        done: 0,
                        total: 0,
                        files_done: 0,
                        files_total: 0,
                        kind: TransferKind::Download,
                        result: Some(result),
                    });
                }
                if self.transfers.len() > 50 {
                    self.transfers.remove(0);
                }
                // A finished download may have landed in the local pane.
                if ok && self.show_local {
                    self.local.refresh();
                }
            }
            // Handled by the app (it owns the confirmation dialog) before reaching the panel.
            SftpEvent::UploadExists { .. } => {}
            SftpEvent::TextLoaded { remote, result } => {
                let Some(pos) = self.text_editors.iter().position(|ed| ed.remote == remote) else {
                    return;
                };
                let ed = &mut self.text_editors[pos];
                ed.loading = false;
                match result {
                    Ok((bytes, mtime)) => match decode_text(bytes) {
                        Some(text) => {
                            ed.crlf = text.contains("\r\n");
                            ed.text = if ed.crlf { text.replace("\r\n", "\n") } else { text };
                            ed.mtime = mtime;
                            ed.dirty = false;
                            ed.conflict = false;
                            ed.status = None;
                            ed.focus_text = true;
                        }
                        None => {
                            // Binary or another encoding: the in-app editor would corrupt it.
                            self.text_editors.remove(pos);
                            self.external_wanted.push(remote);
                        }
                    },
                    Err(e) => ed.status = Some((e, true)),
                }
            }
            SftpEvent::TextSaved { remote, result } => {
                let Some(pos) = self.text_editors.iter().position(|ed| ed.remote == remote) else {
                    return;
                };
                let ed = &mut self.text_editors[pos];
                ed.saving = false;
                match result {
                    Ok(mtime) => {
                        ed.mtime = mtime;
                        ed.dirty = false;
                        ed.conflict = false;
                        ed.status = Some((tr("Saved to the server.").into(), false));
                        if ed.close_after_save {
                            self.text_editors.remove(pos);
                        }
                    }
                    Err(None) => {
                        ed.close_after_save = false;
                        ed.conflict = true;
                        ed.status = Some((
                            tr("The file changed on the server since it was opened. Not saved.").into(),
                            true,
                        ));
                    }
                    Err(Some(e)) => {
                        ed.close_after_save = false;
                        ed.status = Some((trf("Save failed: {e}", &[("{e}", &e)]), true));
                    }
                }
            }
            SftpEvent::Error(e) => self.last_error = Some(e),
            SftpEvent::Info(i) => self.info = Some(i),
            // The app shows the confirm dialog; the panel has nothing to draw for it.
            SftpEvent::SaveRequested { .. } => {}
        }
    }

    /// Open a remote file in the built-in editor (or bring its window forward).
    /// Double-click uses the external editor; this stays for a future in-app path.
    #[allow(dead_code)]
    fn open_text_editor(&mut self, path: String, ctx: &egui::Context, actions: &mut Vec<SftpAction>) {
        if self.text_editors.iter().any(|ed| ed.remote == path) {
            ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, editor_window_id(&path)));
            return;
        }
        actions.push(SftpAction::Cmd(SftpCmd::ReadText(path.clone())));
        self.text_editors.push(TextEditor::new(path));
    }

    /// Draw every built-in editor window.
    fn text_editor_windows(&mut self, ctx: &egui::Context, actions: &mut Vec<SftpAction>) {
        for path in self.external_wanted.drain(..) {
            actions.push(SftpAction::OpenInEditor(path));
        }
        let mut close: Vec<String> = Vec::new();
        let mut reload: Vec<String> = Vec::new();
        for ed in &mut self.text_editors {
            let mut open = true;
            let title = if ed.dirty {
                format!("● {}", ed.name)
            } else {
                ed.name.clone()
            };
            let text_id = egui::Id::unique(("sftp-edit-text", &ed.remote));
            egui::Window::new(title)
                .id(editor_window_id(&ed.remote))
                .open(&mut open)
                .default_size([760.0, 540.0])
                .min_width(320.0)
                .min_height(200.0)
                .resizable(true)
                .collapsible(false)
                .show(ctx, |ui| {
                    let save_key = ui.input(|i| i.modifiers.command && i.key_pressed(Key::S))
                        && ui.memory(|m| m.has_focus(text_id));
                    ui.horizontal(|ui| {
                        let can_save = ed.dirty && !ed.saving && !ed.loading;
                        if ui
                            .add_enabled(can_save, egui::Button::new(tr("Save")))
                            .on_hover_text("Ctrl+S")
                            .clicked()
                            || (save_key && can_save)
                        {
                            actions.push(SftpAction::Cmd(ed.save_cmd(false)));
                        }
                        if ui
                            .add_enabled(!ed.saving && !ed.loading, egui::Button::new(tr("Reload")))
                            .clicked()
                        {
                            reload.push(ed.remote.clone());
                        }
                        if ed.conflict
                            && ui
                                .add_enabled(!ed.saving, egui::Button::new(tr("Overwrite anyway")))
                                .clicked()
                        {
                            actions.push(SftpAction::Cmd(ed.save_cmd(true)));
                        }
                        ui.label(
                            RichText::new(crate::core::term::sanitize_title(&ed.remote))
                                .small()
                                .weak(),
                        );
                    });
                    if let Some((msg, err)) = &ed.status {
                        if *err {
                            ui.colored_label(ERR_COLOR, RichText::new(msg).small());
                        } else {
                            ui.label(RichText::new(msg).small().weak());
                        }
                    }
                    if ed.confirm_close {
                        ui.horizontal(|ui| {
                            ui.colored_label(ERR_COLOR, tr("Discard unsaved changes?"));
                            if ui.add_enabled(!ed.saving, egui::Button::new(tr("Save"))).clicked() {
                                actions.push(SftpAction::Cmd(ed.save_cmd(false)));
                                ed.close_after_save = true;
                                ed.confirm_close = false;
                            }
                            if ui.button(tr("Discard")).clicked() {
                                close.push(ed.remote.clone());
                            }
                            if ui.button(tr("Cancel")).clicked() {
                                ed.confirm_close = false;
                            }
                        });
                    }
                    ui.separator();
                    if ed.loading {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr("Loading…"));
                        });
                        return;
                    }
                    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                        let resp = ui.add(
                            egui::TextEdit::multiline(&mut ed.text)
                                .id(text_id)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(24)
                                .interactive(!ed.saving),
                        );
                        if resp.changed() {
                            ed.dirty = true;
                        }
                        if ed.focus_text {
                            ed.focus_text = false;
                            resp.request_focus();
                        }
                    });
                });
            if !open {
                if ed.dirty {
                    ed.confirm_close = true;
                } else {
                    close.push(ed.remote.clone());
                }
            }
        }
        self.text_editors.retain(|ed| !close.contains(&ed.remote));
        for path in reload {
            if let Some(ed) = self.text_editors.iter_mut().find(|ed| ed.remote == path) {
                ed.loading = true;
                ed.status = None;
                ed.conflict = false;
                ed.confirm_close = false;
            }
            actions.push(SftpAction::Cmd(SftpCmd::ReadText(path)));
        }
    }

    fn tree_set<'a>(&mut self, path: &str, entries: impl Iterator<Item = &'a SftpEntry>) {
        let mut v: Vec<TreeChild> = entries
            .filter(|e| e.is_dir)
            .map(|e| TreeChild {
                name: e.name.clone(),
                path: e.path.clone(),
            })
            .collect();
        v.sort_by_cached_key(|c| c.name.to_lowercase());
        self.tree.insert(path.to_string(), v);
        self.tree_requested.remove(path);
        self.tree_stamp = self.tree_stamp.wrapping_add(1);
    }

    /// Expand every ancestor of `path` in the tree, fetching the ones not cached yet.
    fn expand_to(&mut self, path: &str) {
        let mut changed = false;
        for a in ancestors(path) {
            if self.tree_expanded.insert(a.clone()) {
                changed = true;
            }
            self.tree_want(&a);
        }
        if changed {
            self.tree_stamp = self.tree_stamp.wrapping_add(1);
        }
    }

    fn tree_want(&mut self, path: &str) {
        if !self.tree.contains_key(path)
            && !self.tree_requested.contains(path)
            && !self.tree_wanted.iter().any(|p| p == path)
        {
            self.tree_wanted.push(path.to_string());
        }
    }

    fn push_history(&mut self, path: &str) {
        if let Some(target) = self.hist_pending.take() {
            if self.history.get(target).map(String::as_str) == Some(path) {
                self.hist_pos = target;
                return;
            }
        }
        if self.history.get(self.hist_pos).map(String::as_str) == Some(path) {
            return;
        }
        if !self.history.is_empty() {
            self.history.truncate(self.hist_pos + 1);
        }
        self.history.push(path.to_string());
        if self.history.len() > 100 {
            self.history.remove(0);
        }
        self.hist_pos = self.history.len() - 1;
    }

    fn go(&mut self, path: String, actions: &mut Vec<SftpAction>) {
        self.hist_pending = None;
        actions.push(SftpAction::Cmd(SftpCmd::List(path)));
    }

    fn go_history(&mut self, pos: usize, actions: &mut Vec<SftpAction>) {
        if let Some(p) = self.history.get(pos).cloned() {
            self.hist_pending = Some(pos);
            actions.push(SftpAction::Cmd(SftpCmd::List(p)));
        }
    }

    fn selected_entries(&self) -> Vec<SftpEntry> {
        self.list
            .selected
            .iter()
            .filter_map(|&i| self.entries.get(i).cloned())
            .collect()
    }

    fn request_delete(&self, items: &[SftpEntry], actions: &mut Vec<SftpAction>) {
        let items: Vec<(DeleteItem, String)> = items
            .iter()
            .map(|e| {
                (
                    DeleteItem {
                        path: e.path.clone(),
                        is_dir: e.is_dir,
                        is_symlink: e.is_symlink,
                    },
                    e.name.clone(),
                )
            })
            .collect();
        if !items.is_empty() {
            actions.push(SftpAction::ConfirmDelete(items));
        }
    }

    /// Download `items`: into the local pane's folder when it is shown, otherwise via a dialog.
    fn request_download(&self, items: &[SftpEntry], actions: &mut Vec<SftpAction>, unsafe_names: &mut Vec<String>) {
        if items.is_empty() {
            return;
        }
        if self.show_local {
            downloads_into(&self.local.cwd, items, actions, unsafe_names);
            return;
        }
        if items.len() == 1 && !items[0].is_dir {
            let suggested = sftp::safe_component(&items[0].raw_name)
                .map(str::to_string)
                .unwrap_or_else(|_| "download".into());
            if let Some(dst) = rfd::FileDialog::new()
                .set_title(tr("Save as"))
                .set_file_name(suggested)
                .save_file()
            {
                // The OS save dialog already asked about replacing an existing file.
                actions.push(SftpAction::Cmd(SftpCmd::Download {
                    remote: items[0].path.clone(),
                    local: dst,
                    is_dir: false,
                }));
            }
        } else if let Some(dir) = rfd::FileDialog::new()
            .set_title(tr("Select the download folder"))
            .pick_folder()
        {
            downloads_into(&dir, items, actions, unsafe_names);
        }
    }

    fn upload_paths(&self, paths: &[PathBuf], remote_dir: &str, actions: &mut Vec<SftpAction>) {
        for p in paths {
            actions.push(SftpAction::Cmd(SftpCmd::Upload {
                local: p.clone(),
                remote_dir: remote_dir.to_string(),
                overwrite: false,
            }));
        }
    }

    /// Move remote `items` into `dest` (a remote directory).
    fn move_into(&mut self, items: &[SftpEntry], dest: &str, actions: &mut Vec<SftpAction>) {
        let pairs: Vec<(String, String)> = items
            .iter()
            .filter(|e| {
                sftp::parent_of(&e.path) != dest && e.path != dest && !dest.starts_with(&format!("{}/", e.path))
            })
            .map(|e| (e.path.clone(), sftp::join_remote(dest, &e.raw_name)))
            .collect();
        if pairs.is_empty() {
            return;
        }
        self.info = Some(trf(
            "Moving {n} item(s) to {dest}",
            &[
                ("{n}", &pairs.len()),
                ("{dest}", &crate::core::term::sanitize_title(dest)),
            ],
        ));
        actions.push(SftpAction::Cmd(SftpCmd::Move(pairs)));
    }

    /// Number of items on the internal cut/copy clipboard.
    #[cfg(test)]
    pub fn clip_len(&self) -> usize {
        self.clip.as_ref().map(|(v, _)| v.len()).unwrap_or(0)
    }

    fn paste(&mut self, actions: &mut Vec<SftpAction>) {
        let Some((items, cut)) = self.clip.take() else { return };
        if cut {
            let dest = self.cwd.clone();
            self.move_into(&items, &dest, actions);
        } else {
            for e in &items {
                let mut to = sftp::join_remote(&self.cwd, &e.raw_name);
                if sftp::parent_of(&e.path) == self.cwd {
                    // Pasting into the same folder: "name - copy" like Explorer.
                    let (stem, ext) = match e.raw_name.rsplit_once('.') {
                        Some((s, x)) if !s.is_empty() && !e.is_dir => (s.to_string(), format!(".{x}")),
                        _ => (e.raw_name.clone(), String::new()),
                    };
                    to = sftp::join_remote(&self.cwd, &format!("{stem} - copy{ext}"));
                }
                actions.push(SftpAction::Cmd(SftpCmd::Copy {
                    from: e.path.clone(),
                    to,
                    is_dir: e.is_dir,
                }));
            }
            // Copied items can be pasted again.
            self.clip = Some((items, false));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, connected: bool) -> Vec<SftpAction> {
        let mut actions = Vec::new();
        let mut unsafe_names: Vec<String> = Vec::new();
        if !connected {
            ui.heading(tr("File explorer"));
            ui.label(RichText::new(tr("Not connected.")).weak());
            return actions;
        }
        if !self.started {
            self.started = true;
            self.follow_last = None;
            actions.push(SftpAction::Cmd(SftpCmd::Home));
        }
        if self.show_local && !self.local.loaded {
            self.local.refresh();
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            self.drag = None;
        }

        self.text_editor_windows(&ui.ctx().clone(), &mut actions);
        self.nav_bar(ui, &mut actions);
        self.command_bar(ui, &mut actions, &mut unsafe_names);
        self.inline_editors(ui, &mut actions);
        if let Some(e) = &self.last_error {
            ui.colored_label(ERR_COLOR, RichText::new(e).small());
        }
        if let Some(info) = &self.info {
            ui.colored_label(OK_COLOR, RichText::new(info).small());
        }
        ui.add_space(2.0);

        if !self.transfers.is_empty() {
            let mut expanded = self.transfers_expanded;
            let mut toggle = false;
            // One text row plus the panel's own vertical margin, so the summary is not clipped.
            let bar_h = ui.spacing().interact_size.y + 12.0;
            let collapsed = egui::Panel::bottom("sftp-transfers-bar")
                .resizable(true)
                .exact_size(bar_h);
            let open = egui::Panel::bottom("sftp-transfers")
                .resizable(true)
                .default_size(140.0)
                .min_size(bar_h);
            egui::Panel::show_switched(ui, &mut expanded, collapsed, open, |ui, show_list| {
                if self.transfer_bar(ui, show_list) {
                    toggle = true;
                }
                if show_list {
                    self.transfer_rows(ui, &mut actions);
                }
            });
            if toggle {
                expanded = !expanded;
            }
            self.transfers_expanded = expanded;
        }
        let mut tree_drop: Option<String> = None;
        if self.show_tree {
            egui::Panel::left("sftp-tree")
                .resizable(true)
                .default_size(150.0)
                .min_size(80.0)
                .show(ui, |ui| tree_drop = self.tree_ui(ui, &mut actions));
        }
        let mut local_out: Option<ViewOut> = None;
        if self.show_local {
            let half = (ui.available_width() * 0.45).max(180.0);
            egui::Panel::left("sftp-local")
                .resizable(true)
                .default_size(half)
                .min_size(140.0)
                .show(ui, |ui| local_out = Some(self.local_ui(ui, &mut actions)));
        }
        let remote_out = self.list_ui(ui, &mut actions, &mut unsafe_names);

        self.drag_ui(
            ui,
            &remote_out,
            local_out.as_ref(),
            tree_drop,
            &mut actions,
            &mut unsafe_names,
        );

        // Tree branches discovered by `apply` that still need their children.
        for p in std::mem::take(&mut self.tree_wanted) {
            if !self.tree.contains_key(&p) && self.tree_requested.insert(p.clone()) {
                actions.push(SftpAction::Cmd(SftpCmd::Peek(p)));
            }
        }
        if !unsafe_names.is_empty() {
            self.last_error = Some(trf(
                "Skipped {n} item(s) with unsafe names: {names}",
                &[("{n}", &unsafe_names.len()), ("{names}", &unsafe_names.join(", "))],
            ));
        }
        actions
    }

    // ------------------------------------------------------------------
    // drag and drop
    // ------------------------------------------------------------------

    fn drag_ui(
        &mut self,
        ui: &mut Ui,
        remote: &ViewOut,
        local: Option<&ViewOut>,
        tree_drop: Option<String>,
        actions: &mut Vec<SftpAction>,
        unsafe_names: &mut Vec<String>,
    ) {
        // Start a drag from whichever list reported one.
        if self.drag.is_none() {
            if let Some(i) = remote.drag_started {
                if !self.list.selected.contains(&i) {
                    self.list.selected = vec![i];
                    self.list.anchor = Some(i);
                }
                self.drag = Some(Drag {
                    from: Pane::Remote,
                    items: self.selected_entries(),
                });
            } else if let Some(i) = local.and_then(|l| l.drag_started) {
                if !self.local.list.selected.contains(&i) {
                    self.local.list.selected = vec![i];
                    self.local.list.anchor = Some(i);
                }
                let items: Vec<SftpEntry> = self
                    .local
                    .list
                    .selected
                    .iter()
                    .filter_map(|&i| self.local.entries.get(i).cloned())
                    .collect();
                self.drag = Some(Drag {
                    from: Pane::Local,
                    items,
                });
            }
        }
        let Some(drag) = self.drag.clone() else { return };
        if drag.items.is_empty() {
            self.drag = None;
            return;
        }
        // Ghost label next to the pointer.
        if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
            let label = if drag.items.len() == 1 {
                drag.items[0].name.clone()
            } else {
                trf("{n} items", &[("{n}", &drag.items.len())])
            };
            egui::Area::new(ui.make_persistent_id("sftp-drag-ghost"))
                .order(egui::Order::Tooltip)
                .interactable(false)
                .fixed_pos(pos + Vec2::new(14.0, 10.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(label).small());
                    });
                });
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if !ui.input(|i| i.pointer.any_released()) {
            return;
        }
        // Dropped: resolve the target.
        self.drag = None;
        let remote_dir = remote
            .hovered_dir
            .and_then(|i| self.entries.get(i))
            .filter(|e| e.is_dir)
            .map(|e| e.path.clone())
            .or(tree_drop)
            .or_else(|| remote.over_list.then(|| self.cwd.clone()));
        let local_dir = local.and_then(|l| {
            l.hovered_dir
                .and_then(|i| self.local.entries.get(i))
                .filter(|e| e.is_dir)
                .map(|e| PathBuf::from(&e.path))
                .or_else(|| l.over_list.then(|| self.local.cwd.clone()))
        });
        match (drag.from, remote_dir, local_dir) {
            (Pane::Remote, Some(dir), _) => self.move_into(&drag.items, &dir, actions),
            (Pane::Remote, None, Some(dir)) => downloads_into(&dir, &drag.items, actions, unsafe_names),
            (Pane::Local, Some(dir), _) => {
                let paths: Vec<PathBuf> = drag.items.iter().map(|e| PathBuf::from(&e.path)).collect();
                self.upload_paths(&paths, &dir, actions);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // navigation bar
    // ------------------------------------------------------------------

    fn nav_bar(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>) {
        ui.horizontal(|ui| {
            let can_back = self.hist_pos > 0 && !self.history.is_empty();
            let can_fwd = self.hist_pos + 1 < self.history.len();
            if icon_button(ui, Icon::Back, can_back, tr("Back (Alt+Left)")).clicked() {
                self.go_history(self.hist_pos - 1, actions);
            }
            if icon_button(ui, Icon::Forward, can_fwd, tr("Forward (Alt+Right)")).clicked() {
                self.go_history(self.hist_pos + 1, actions);
            }
            if icon_button(ui, Icon::Up, self.cwd != "/", tr("Parent folder (Backspace)")).clicked() {
                let parent = sftp::parent_of(&self.cwd);
                self.go(parent, actions);
            }
            if icon_button(ui, Icon::Refresh, true, tr("Refresh (F5)")).clicked() {
                self.go(self.cwd.clone(), actions);
            }
            if ui
                .button(tr("Home"))
                .on_hover_text(tr("Go to the home directory"))
                .clicked()
            {
                actions.push(SftpAction::Cmd(SftpCmd::Home));
            }
            self.address_bar(ui, actions);
        });
    }

    fn address_bar(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>) {
        let h = 24.0;
        if self.address_editing {
            let r = ui.add_sized(
                [ui.available_width(), h],
                egui::TextEdit::singleline(&mut self.path_input)
                    .hint_text(tr("/remote/path"))
                    .return_key(None),
            );
            if self.address_focus {
                // Like Explorer: the whole path is selected, so typing replaces it.
                focus_select(ui, &r, self.path_input.chars().count());
                self.address_focus = false;
            }
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            let escape = ui.input(|i| i.key_pressed(Key::Escape));
            if r.has_focus() && enter {
                let p = self.path_input.trim().to_string();
                if !p.is_empty() {
                    self.go(p, actions);
                }
                self.address_editing = false;
            } else if escape || r.lost_focus() {
                self.address_editing = false;
                self.path_input = self.cwd.clone();
            }
            return;
        }
        let parts: Vec<String> = self
            .cwd
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let mut crumbs: Vec<(String, String)> = vec![("/".to_string(), "/".to_string())];
        let mut acc = String::new();
        for p in &parts {
            acc.push('/');
            acc.push_str(p);
            crumbs.push((crate::core::term::sanitize_title(p), acc.clone()));
        }
        let (target, edit) = breadcrumb_bar(ui, "sftp-remote-addr", &crumbs, h);
        if edit {
            self.address_editing = true;
            self.address_focus = true;
            self.path_input = self.cwd.clone();
        }
        if let Some(t) = target {
            self.go(t, actions);
        }
    }

    // ------------------------------------------------------------------
    // command bar
    // ------------------------------------------------------------------

    fn command_bar(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>, unsafe_names: &mut Vec<String>) {
        ui.horizontal_wrapped(|ui| {
            let m = ui.menu_button(format!("{}   ", tr("Upload")), |ui| {
                if ui.button(tr("Upload files…")).clicked() {
                    if let Some(files) = rfd::FileDialog::new()
                        .set_title(tr("Select files to upload"))
                        .pick_files()
                    {
                        let cwd = self.cwd.clone();
                        self.upload_paths(&files, &cwd, actions);
                    }
                    ui.close();
                }
                if ui.button(tr("Upload folder…")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title(tr("Select a folder to upload"))
                        .pick_folder()
                    {
                        let cwd = self.cwd.clone();
                        self.upload_paths(&[dir], &cwd, actions);
                    }
                    ui.close();
                }
                ui.label(
                    RichText::new(tr("Files dropped from Explorer are uploaded too."))
                        .small()
                        .weak(),
                );
            });
            paint_menu_arrow(ui, &m.response);
            let has_sel = !self.list.selected.is_empty();
            let one = self.list.selected.len() == 1;
            if ui
                .add_enabled(has_sel, egui::Button::new(tr("Download…")))
                .on_hover_text(tr("Download the selection to this PC"))
                .clicked()
            {
                let items = self.selected_entries();
                self.request_download(&items, actions, unsafe_names);
            }
            if ui
                .button(tr("New folder"))
                .on_hover_text(tr("Create a folder here"))
                .clicked()
            {
                self.begin_new_dir();
            }
            if ui
                .add_enabled(one, egui::Button::new(tr("Rename")))
                .on_hover_text("F2")
                .clicked()
            {
                self.begin_rename();
            }
            if ui
                .add_enabled(one, egui::Button::new(tr("Permissions")))
                .on_hover_text(tr("chmod (octal)"))
                .clicked()
            {
                self.begin_chmod();
            }
            if ui
                .add_enabled(has_sel, egui::Button::new(RichText::new(tr("Delete")).color(ERR_COLOR)))
                .on_hover_text("Delete")
                .clicked()
            {
                let items = self.selected_entries();
                self.request_delete(&items, actions);
            }
            ui.separator();
            let m = ui.menu_button(format!("{}   ", tr("View")), |ui| {
                ui.checkbox(&mut self.show_tree, tr("Folder pane"));
                if ui.checkbox(&mut self.show_local, tr("Local pane")).changed() && self.show_local {
                    self.local.refresh();
                }
                ui.checkbox(&mut self.show_hidden, tr("Hidden items (starting with .)"));
                ui.checkbox(&mut self.follow_terminal, tr("Follow the terminal's working directory"))
                    .on_hover_text(tr(
                        "Moves to the terminal's current directory whenever it changes (OSC 7 / prompt title)",
                    ));
                ui.separator();
                ui.label(RichText::new(tr("Sort by")).small().weak());
                for (k, label) in [
                    (SortKey::Name, tr("Name")),
                    (SortKey::Modified, tr("Date modified")),
                    (SortKey::Kind, tr("Type")),
                    (SortKey::Size, tr("Size")),
                ] {
                    if ui.radio(self.list.sort == k, label).clicked() {
                        self.list.sort = k;
                    }
                }
                ui.checkbox(&mut self.list.sort_desc, tr("Descending"));
            });
            paint_menu_arrow(ui, &m.response);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(tr("Search"))
                    .desired_width(110.0),
            );
            if !self.filter.is_empty()
                && ui
                    .small_button(tr("Clear"))
                    .on_hover_text(tr("Clear the search"))
                    .clicked()
            {
                self.filter.clear();
                r.request_focus();
            }
        });
    }

    fn begin_new_dir(&mut self) {
        self.new_dir = Some(String::new());
        self.rename = None;
        self.chmod = None;
        self.editor_focus = true;
    }

    fn begin_rename(&mut self) {
        if let Some(e) = self.list.selected.first().and_then(|&i| self.entries.get(i)) {
            self.rename = Some((e.path.clone(), e.raw_name.clone()));
            self.new_dir = None;
            self.chmod = None;
            self.editor_focus = true;
        }
    }

    fn begin_chmod(&mut self) {
        if let Some(e) = self.list.selected.first().and_then(|&i| self.entries.get(i)) {
            let cur = e.permissions.map(|p| format!("{:o}", p & 0o7777)).unwrap_or_default();
            self.chmod = Some((e.path.clone(), cur));
            self.new_dir = None;
            self.rename = None;
            self.editor_focus = true;
        }
    }

    fn inline_editors(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>) {
        let take_focus = std::mem::take(&mut self.editor_focus);
        if let Some(name) = self.new_dir.as_mut() {
            let mut done = false;
            ui.horizontal(|ui| {
                ui.label(tr("New folder name:"));
                let r = ui.add(
                    egui::TextEdit::singleline(name)
                        .desired_width(180.0)
                        .hint_text(tr("Folder name"))
                        .return_key(None),
                );
                if take_focus {
                    focus_select(ui, &r, 0);
                }
                let enter = r.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui.button(tr("Create")).clicked() || enter) && !name.trim().is_empty() {
                    let p = sftp::join_remote(&self.cwd, name.trim());
                    actions.push(SftpAction::Cmd(SftpCmd::Mkdir(p)));
                    actions.push(SftpAction::Cmd(SftpCmd::List(self.cwd.clone())));
                    done = true;
                }
                if ui.button(tr("Cancel")).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = true;
                }
            });
            if done {
                self.new_dir = None;
            }
        }
        if let Some((path, newname)) = self.rename.as_mut() {
            let mut done = false;
            ui.horizontal(|ui| {
                ui.label(trf(
                    "Rename '{name}':",
                    &[("{name}", &crate::core::term::sanitize_title(base_name(path)))],
                ));
                let r = ui.add(
                    egui::TextEdit::singleline(newname)
                        .desired_width(180.0)
                        .hint_text(tr("New name"))
                        .return_key(None),
                );
                if take_focus {
                    // Explorer selects the name without its extension.
                    let stem = match newname.rsplit_once('.') {
                        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => stem.chars().count(),
                        _ => newname.chars().count(),
                    };
                    focus_select(ui, &r, stem);
                }
                let enter = r.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui.button(tr("OK")).clicked() || enter) && !newname.trim().is_empty() {
                    let to = sftp::join_remote(&sftp::parent_of(path), newname.trim());
                    actions.push(SftpAction::Cmd(SftpCmd::Rename { from: path.clone(), to }));
                    actions.push(SftpAction::Cmd(SftpCmd::List(self.cwd.clone())));
                    done = true;
                }
                if ui.button(tr("Cancel")).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = true;
                }
            });
            if done {
                self.rename = None;
            }
        }
        if let Some((path, mode)) = self.chmod.as_mut() {
            let mut done = false;
            ui.horizontal(|ui| {
                ui.label(trf(
                    "Permissions of '{name}' (octal):",
                    &[("{name}", &crate::core::term::sanitize_title(base_name(path)))],
                ));
                let r = ui.add(
                    egui::TextEdit::singleline(mode)
                        .desired_width(60.0)
                        .hint_text("755")
                        .return_key(None),
                );
                if take_focus {
                    focus_select(ui, &r, mode.chars().count());
                }
                let enter = r.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let parsed = u32::from_str_radix(mode.trim(), 8).ok().filter(|m| *m <= 0o7777);
                if let Some(m) = parsed {
                    ui.label(RichText::new(fmt_mode(m)).monospace().weak());
                } else if !mode.trim().is_empty() {
                    ui.colored_label(ERR_COLOR, RichText::new(tr("3–4 octal digits")).small());
                }
                if ui
                    .add_enabled(parsed.is_some(), egui::Button::new(tr("Apply")))
                    .clicked()
                    || (enter && parsed.is_some())
                {
                    actions.push(SftpAction::Cmd(SftpCmd::Chmod {
                        path: path.clone(),
                        mode: parsed.unwrap_or(0),
                    }));
                    actions.push(SftpAction::Cmd(SftpCmd::List(self.cwd.clone())));
                    done = true;
                }
                if ui.button(tr("Cancel")).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = true;
                }
            });
            if done {
                self.chmod = None;
            }
        }
    }

    // ------------------------------------------------------------------
    // folder tree
    // ------------------------------------------------------------------

    /// Returns the tree node a drag is hovering (drop target), if any.
    fn tree_ui(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>) -> Option<String> {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let visuals = ui.visuals().clone();
        let dragging = self.drag.is_some();
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let mut navigate: Option<String> = None;
        let mut toggle: Option<String> = None;
        let mut drop_target: Option<String> = None;
        if self.tree_rows_stamp != self.tree_stamp || self.tree_rows_hidden != self.show_hidden {
            self.tree_rows = flatten_tree(&self.tree, &self.tree_expanded, self.show_hidden);
            self.tree_rows_stamp = self.tree_stamp;
            self.tree_rows_hidden = self.show_hidden;
        }
        let home = self.home.clone();
        let home_rows = usize::from(home.is_some());
        let total = home_rows + self.tree_rows.len();
        let cwd = self.cwd.clone();
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .id_salt("sftp-tree-scroll")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, total, |ui, range| {
                let width = ui.available_width().max(120.0);
                for i in range {
                    if i < home_rows {
                        let Some(home) = home.as_ref() else { continue };
                        let (rect, r) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TreeItem, true, tr("Home")));
                        let p = ui.painter();
                        if cwd == *home {
                            p.rect_filled(rect, CornerRadius::same(2), visuals.selection.bg_fill);
                        } else if r.hovered() {
                            p.rect_filled(rect, CornerRadius::same(2), visuals.widgets.hovered.weak_bg_fill);
                        }
                        let icon = Rect::from_min_size(
                            Pos2::new(rect.min.x + 6.0, rect.center().y - ICON / 2.0),
                            Vec2::splat(ICON),
                        );
                        paint_home_icon(p, icon);
                        p.text(
                            Pos2::new(icon.max.x + 6.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            tr("Home"),
                            font.clone(),
                            visuals.text_color(),
                        );
                        if r.clicked() {
                            navigate = Some(home.clone());
                        }
                        if dragging && pointer.map(|p| rect.contains(p)).unwrap_or(false) {
                            drop_target = Some(home.clone());
                            p.rect_stroke(
                                rect,
                                CornerRadius::same(2),
                                visuals.selection.stroke,
                                StrokeKind::Inside,
                            );
                        }
                        r.on_hover_text(home);
                        continue;
                    }
                    let row = &self.tree_rows[i - home_rows];
                    let (rect, r) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TreeItem, true, &row.name));
                    let is_cwd = cwd == row.path;
                    let p = ui.painter();
                    if is_cwd {
                        p.rect_filled(rect, CornerRadius::same(2), visuals.selection.bg_fill);
                    } else if r.hovered() {
                        p.rect_filled(rect, CornerRadius::same(2), visuals.widgets.hovered.weak_bg_fill);
                    }
                    let x0 = rect.min.x + 4.0 + row.depth as f32 * 14.0;
                    let arrow = Rect::from_center_size(Pos2::new(x0 + 6.0, rect.center().y), Vec2::splat(12.0));
                    if row.has_children {
                        let col = if row.known {
                            visuals.text_color()
                        } else {
                            visuals.weak_text_color()
                        };
                        paint_expander(p, arrow, row.expanded, col);
                    }
                    let icon =
                        Rect::from_min_size(Pos2::new(x0 + 14.0, rect.center().y - ICON / 2.0), Vec2::splat(ICON));
                    paint_folder_icon(p, icon, row.expanded && row.depth > 0);
                    let text_col = if is_cwd {
                        visuals.selection.stroke.color
                    } else {
                        visuals.text_color()
                    };
                    let clip = Rect::from_min_max(Pos2::new(icon.max.x + 4.0, rect.min.y), rect.max);
                    p.with_clip_rect(clip).text(
                        Pos2::new(icon.max.x + 5.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &row.name,
                        font.clone(),
                        text_col,
                    );
                    if dragging && pointer.map(|p| rect.contains(p)).unwrap_or(false) {
                        drop_target = Some(row.path.clone());
                        p.rect_stroke(
                            rect,
                            CornerRadius::same(2),
                            visuals.selection.stroke,
                            StrokeKind::Inside,
                        );
                    }
                    let on_arrow = r
                        .interact_pointer_pos()
                        .map(|pos| arrow.expand(2.0).contains(pos))
                        .unwrap_or(false);
                    if r.clicked() {
                        if on_arrow && row.has_children {
                            toggle = Some(row.path.clone());
                        } else {
                            navigate = Some(row.path.clone());
                        }
                    } else if r.double_clicked() && row.has_children {
                        toggle = Some(row.path.clone());
                    }
                    r.on_hover_text(&row.path);
                }
            });
        if let Some(p) = toggle {
            if self.tree_expanded.contains(&p) {
                self.tree_expanded.remove(&p);
            } else {
                self.tree_expanded.insert(p.clone());
                self.tree_want(&p);
            }
            self.tree_stamp = self.tree_stamp.wrapping_add(1);
        }
        if let Some(p) = navigate {
            self.go(p, actions);
        }
        drop_target
    }

    // ------------------------------------------------------------------
    // local pane
    // ------------------------------------------------------------------

    fn local_ui(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>) -> ViewOut {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Local")).strong());
            let parent = self.local.cwd.parent().map(Path::to_path_buf);
            if icon_button(ui, Icon::Up, parent.is_some(), tr("Parent folder (Backspace)")).clicked() {
                if let Some(p) = parent {
                    self.local.go(p);
                }
            }
            if icon_button(ui, Icon::Refresh, true, tr("Refresh (F5)")).clicked() {
                self.local.refresh();
            }
            let m = ui.menu_button(format!("{}   ", tr("Drives")), |ui| {
                let mut go: Option<PathBuf> = None;
                if let Some(ud) = directories::UserDirs::new() {
                    for (label, dir) in [
                        (tr("Home"), Some(ud.home_dir().to_path_buf())),
                        (tr("Desktop"), ud.desktop_dir().map(Path::to_path_buf)),
                        (tr("Documents"), ud.document_dir().map(Path::to_path_buf)),
                        (tr("Downloads"), ud.download_dir().map(Path::to_path_buf)),
                    ] {
                        if let Some(d) = dir {
                            if ui.button(label).clicked() {
                                go = Some(d);
                                ui.close();
                            }
                        }
                    }
                    ui.separator();
                }
                for d in local_roots() {
                    if ui.button(d.display().to_string()).clicked() {
                        go = Some(d);
                        ui.close();
                    }
                }
                if let Some(d) = go {
                    self.local.go(d);
                }
            });
            paint_menu_arrow(ui, &m.response);
            let has_sel = !self.local.list.selected.is_empty();
            if ui
                .add_enabled(has_sel, egui::Button::new(format!("{} →", tr("Upload"))))
                .on_hover_text(tr("Upload to the current remote folder"))
                .clicked()
            {
                let paths: Vec<PathBuf> = self
                    .local
                    .list
                    .selected
                    .iter()
                    .filter_map(|&i| self.local.entries.get(i))
                    .map(|e| PathBuf::from(&e.path))
                    .collect();
                let cwd = self.cwd.clone();
                self.upload_paths(&paths, &cwd, actions);
            }
        });
        // Address: breadcrumb of the local path, editable on click.
        let h = 22.0;
        if self.local.address_editing {
            let r = ui.add_sized(
                [ui.available_width(), h],
                egui::TextEdit::singleline(&mut self.local.path_input).return_key(None),
            );
            if self.local.address_focus {
                focus_select(ui, &r, self.local.path_input.chars().count());
                self.local.address_focus = false;
            }
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            let escape = ui.input(|i| i.key_pressed(Key::Escape));
            if r.has_focus() && enter {
                let p = PathBuf::from(self.local.path_input.trim());
                self.local.address_editing = false;
                if p.is_dir() {
                    self.local.go(p);
                } else {
                    self.local.error = Some(trf(
                        "Cannot read {path}: {e}",
                        &[("{path}", &p.display()), ("{e}", &"not a directory")],
                    ));
                }
            } else if escape || r.lost_focus() {
                self.local.address_editing = false;
                self.local.path_input = self.local.cwd.display().to_string();
            }
        } else {
            let mut crumbs: Vec<(String, String)> = Vec::new();
            let mut acc = PathBuf::new();
            for c in self.local.cwd.components() {
                acc.push(c.as_os_str());
                let label = match c {
                    std::path::Component::RootDir => continue,
                    other => other.as_os_str().to_string_lossy().into_owned(),
                };
                let mut shown = acc.clone();
                if matches!(c, std::path::Component::Prefix(_)) {
                    shown.push(std::path::MAIN_SEPARATOR_STR);
                }
                crumbs.push((label, shown.display().to_string()));
            }
            if crumbs.is_empty() {
                crumbs.push((
                    self.local.cwd.display().to_string(),
                    self.local.cwd.display().to_string(),
                ));
            }
            let (target, edit) = breadcrumb_bar(ui, "sftp-local-addr", &crumbs, h);
            if edit {
                self.local.address_editing = true;
                self.local.address_focus = true;
                self.local.path_input = self.local.cwd.display().to_string();
            }
            if let Some(t) = target {
                self.local.go(PathBuf::from(t));
            }
        }
        if let Some(e) = &self.local.error {
            ui.colored_label(ERR_COLOR, RichText::new(e).small());
        }

        let dragging = self.drag.is_some();
        let (mut ctx_open, mut ctx_upload, mut ctx_reveal) = (None::<usize>, None::<Vec<usize>>, false);
        let local_parent = self.local.cwd.parent().map(|p| p.to_path_buf());
        let out = details_view(
            ui,
            ui.make_persistent_id("sftp-local-list"),
            &self.local.entries,
            &mut self.local.list,
            self.show_hidden,
            "",
            self.local.entries_stamp,
            dragging,
            local_parent.is_some(),
            |ui, i, st, _entry| {
                if i == usize::MAX {
                    if ui.button(tr("Open in the system file manager")).clicked() {
                        ctx_reveal = true;
                        ui.close();
                    }
                    return;
                }
                let items: Vec<usize> = if st.selected.contains(&i) {
                    st.selected.clone()
                } else {
                    vec![i]
                };
                if ui.button(tr("Open")).clicked() {
                    ctx_open = Some(i);
                    ui.close();
                }
                if ui.button(format!("{} →", tr("Upload"))).clicked() {
                    ctx_upload = Some(items);
                    ui.close();
                }
                if ui.button(tr("Open in the system file manager")).clicked() {
                    ctx_reveal = true;
                    ui.close();
                }
            },
        );
        if out.open_parent {
            if let Some(p) = local_parent {
                self.local.go(p);
            }
        }
        if let Some(i) = out.open.or(ctx_open) {
            if let Some(e) = self.local.entries.get(i).cloned() {
                if e.is_dir {
                    self.local.go(PathBuf::from(&e.path));
                } else {
                    let _ = open::that(&e.path);
                }
            }
        }
        if let Some(items) = ctx_upload {
            let paths: Vec<PathBuf> = items
                .iter()
                .filter_map(|&i| self.local.entries.get(i))
                .map(|e| PathBuf::from(&e.path))
                .collect();
            let cwd = self.cwd.clone();
            self.upload_paths(&paths, &cwd, actions);
        }
        if ctx_reveal {
            let _ = open::that(&self.local.cwd);
        }
        if out.focused && !self.local.address_editing {
            let (backspace, f5) = ui.input(|i| (i.key_pressed(Key::Backspace), i.key_pressed(Key::F5)));
            if backspace {
                if let Some(p) = self.local.cwd.parent().map(Path::to_path_buf) {
                    self.local.go(p);
                }
            } else if f5 {
                self.local.refresh();
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // remote details view
    // ------------------------------------------------------------------

    fn list_ui(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>, unsafe_names: &mut Vec<String>) -> ViewOut {
        let list_id = ui.make_persistent_id("sftp-list-focus");
        if self.show_local {
            ui.label(RichText::new(tr("Remote")).strong());
        }
        // ---- keyboard ----
        if ui.memory(|m| m.has_focus(list_id)) {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    list_id,
                    EventFilter {
                        tab: false,
                        horizontal_arrows: false,
                        vertical_arrows: true,
                        escape: false,
                    },
                )
            });
            self.handle_keys(ui, actions);
        }

        let dragging = self.drag.is_some();
        let mut ctx_open: Option<usize> = None;
        let mut ctx_edit: Option<String> = None;
        let mut ctx_tail: Option<String> = None;
        let mut ctx_download: Option<Vec<usize>> = None;
        let mut ctx_delete: Option<Vec<usize>> = None;
        let mut ctx_clip: Option<(Vec<usize>, bool)> = None;
        let mut ctx_paste = false;
        let mut ctx_rename = false;
        let mut ctx_chmod = false;
        let mut ctx_copy_path: Option<String> = None;
        let mut ctx_newdir = false;
        let mut ctx_refresh = false;
        let has_clip = self.clip.as_ref().map(|(v, _)| v.len()).unwrap_or(0);
        let filter = self.filter.clone();
        let cwd = self.cwd.clone();
        let out = details_view(
            ui,
            list_id,
            &self.entries,
            &mut self.list,
            self.show_hidden,
            &filter,
            self.entries_stamp,
            dragging,
            self.cwd != "/" && !self.cwd.is_empty(),
            |ui, i, st, entry| {
                // `i == usize::MAX` means the empty area was right-clicked.
                if i == usize::MAX {
                    if has_clip > 0
                        && ui
                            .button(trf("Paste {n} item(s) here", &[("{n}", &has_clip)]))
                            .clicked()
                    {
                        ctx_paste = true;
                        ui.close();
                    }
                    if ui.button(tr("New folder")).clicked() {
                        ctx_newdir = true;
                        ui.close();
                    }
                    if ui.button(tr("Refresh")).clicked() {
                        ctx_refresh = true;
                        ui.close();
                    }
                    if ui.button(tr("Copy path")).clicked() {
                        ctx_copy_path = Some(cwd.clone());
                        ui.close();
                    }
                    return;
                }
                let items: Vec<usize> = if st.selected.contains(&i) {
                    st.selected.clone()
                } else {
                    vec![i]
                };
                let single = items.len() == 1;
                let Some(e) = entry else { return };
                if single && e.is_dir && ui.button(tr("Open")).clicked() {
                    ctx_open = Some(i);
                    ui.close();
                }
                if single
                    && !e.is_dir
                    && e.is_regular
                    && ui
                        .button(tr("Edit file"))
                        .on_hover_text(tr("Open in your editor. Ruxt asks before uploading the saved file."))
                        .clicked()
                {
                    ctx_edit = Some(e.path.clone());
                    ui.close();
                }
                if single && !e.is_dir && e.is_regular && ui.button(tr("Open in Tail Viewer (tail -f)")).clicked() {
                    ctx_tail = Some(e.path.clone());
                    ui.close();
                }
                if ui.button(trf("Download… ({n})", &[("{n}", &items.len())])).clicked() {
                    ctx_download = Some(items.clone());
                    ui.close();
                }
                ui.separator();
                if ui.button(tr("Cut")).clicked() {
                    ctx_clip = Some((items.clone(), true));
                    ui.close();
                }
                if ui.button(tr("Copy")).clicked() {
                    ctx_clip = Some((items.clone(), false));
                    ui.close();
                }
                if has_clip > 0
                    && ui
                        .button(trf("Paste {n} item(s) here", &[("{n}", &has_clip)]))
                        .clicked()
                {
                    ctx_paste = true;
                    ui.close();
                }
                ui.separator();
                if single && ui.button(tr("Rename")).clicked() {
                    ctx_rename = true;
                    ui.close();
                }
                if single && ui.button(tr("Permissions…")).clicked() {
                    ctx_chmod = true;
                    ui.close();
                }
                if ui.button(tr("Copy path")).clicked() {
                    ctx_copy_path = Some(e.path.clone());
                    ui.close();
                }
                ui.separator();
                if ui
                    .button(RichText::new(trf("Delete ({n})", &[("{n}", &items.len())])).color(ERR_COLOR))
                    .clicked()
                {
                    ctx_delete = Some(items.clone());
                    ui.close();
                }
            },
        );
        // Status line.
        let n_items = out.visible_count;
        let n_sel = self.list.selected.len();
        let sel_bytes: u64 = self
            .list
            .selected
            .iter()
            .filter_map(|&i| self.entries.get(i))
            .filter(|e| !e.is_dir)
            .map(|e| e.size)
            .sum();
        let mut status = trf("{n} items", &[("{n}", &n_items)]);
        if n_sel > 0 {
            status.push_str("  |  ");
            status.push_str(&trf("{n} selected", &[("{n}", &n_sel)]));
            if sel_bytes > 0 {
                status.push_str("  ");
                status.push_str(&fmt_size(sel_bytes));
            }
        }
        if n_items == 0 && !self.entries.is_empty() {
            status.push_str("  |  ");
            status.push_str(tr("No items match the search."));
        } else if self.entries.is_empty() && !self.cwd.is_empty() {
            status.push_str("  |  ");
            status.push_str(tr("This folder is empty."));
        }
        ui.label(RichText::new(status).small().weak());

        // ---- apply deferred interactions ----
        if out.open_parent && self.cwd != "/" {
            let parent = sftp::parent_of(&self.cwd);
            self.go(parent, actions);
        }
        if let Some(i) = out.open.or(ctx_open) {
            if let Some(e) = self.entries.get(i).cloned() {
                self.open_entry(&e, actions);
            }
        }
        if let Some(items) = ctx_download {
            let items: Vec<SftpEntry> = items.iter().filter_map(|&i| self.entries.get(i).cloned()).collect();
            self.request_download(&items, actions, unsafe_names);
        }
        if let Some(items) = ctx_delete {
            let items: Vec<SftpEntry> = items.iter().filter_map(|&i| self.entries.get(i).cloned()).collect();
            self.request_delete(&items, actions);
        }
        if let Some((items, cut)) = ctx_clip {
            let items: Vec<SftpEntry> = items.iter().filter_map(|&i| self.entries.get(i).cloned()).collect();
            self.clip = Some((items, cut));
        }
        if ctx_paste {
            self.paste(actions);
        }
        if ctx_rename {
            self.begin_rename();
        }
        if ctx_chmod {
            self.begin_chmod();
        }
        if ctx_newdir {
            self.begin_new_dir();
        }
        if ctx_refresh {
            let c = self.cwd.clone();
            self.go(c, actions);
        }
        if let Some(p) = ctx_edit {
            actions.push(SftpAction::OpenInEditor(p));
        }
        if let Some(p) = ctx_tail {
            actions.push(SftpAction::OpenInTailViewer(p));
        }
        if let Some(t) = ctx_copy_path {
            ui.ctx().copy_text(t);
            self.info = Some(tr("Path copied to the clipboard.").into());
        }
        out
    }

    /// Double-click / Enter: folders open, files open in the external editor — the same
    /// path as "Edit", which uploads the file when that editor saves it.
    fn open_entry(&mut self, e: &SftpEntry, actions: &mut Vec<SftpAction>) {
        if e.is_dir {
            self.go(e.path.clone(), actions);
        } else if e.is_regular {
            actions.push(SftpAction::OpenInEditor(e.path.clone()));
        }
    }

    fn handle_keys(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>) {
        // Inline editors own the keyboard while open.
        if self.new_dir.is_some() || self.rename.is_some() || self.chmod.is_some() || self.address_editing {
            return;
        }
        let (enter, backspace, delete, f2, f5, up, down, home, end, ctrl_a, alt_left, alt_right, escape) =
            ui.input(|i| {
                (
                    i.key_pressed(Key::Enter),
                    i.key_pressed(Key::Backspace),
                    i.key_pressed(Key::Delete) && !i.modifiers.shift,
                    i.key_pressed(Key::F2),
                    i.key_pressed(Key::F5),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::Home),
                    i.key_pressed(Key::End),
                    i.modifiers.command && i.key_pressed(Key::A),
                    i.modifiers.alt && i.key_pressed(Key::ArrowLeft),
                    i.modifiers.alt && i.key_pressed(Key::ArrowRight),
                    i.key_pressed(Key::Escape),
                )
            });
        // Cut / copy / paste arrive as events (the app translates Ctrl+X/C/V for non-terminal
        // widgets).
        let (cut, copy, paste) = ui.input(|i| {
            let mut f = (false, false, false);
            for ev in &i.events {
                match ev {
                    egui::Event::Cut => f.0 = true,
                    egui::Event::Copy => f.1 = true,
                    egui::Event::Paste(_) => f.2 = true,
                    _ => {}
                }
            }
            f
        });
        let shift = ui.input(|i| i.modifiers.shift);
        if cut || copy {
            let items = self.selected_entries();
            if !items.is_empty() {
                self.clip = Some((items, cut));
            }
            return;
        }
        if paste {
            self.paste(actions);
            return;
        }
        if alt_left {
            if self.hist_pos > 0 {
                self.go_history(self.hist_pos - 1, actions);
            }
            return;
        }
        if alt_right {
            if self.hist_pos + 1 < self.history.len() {
                self.go_history(self.hist_pos + 1, actions);
            }
            return;
        }
        if backspace {
            if self.cwd != "/" {
                let parent = sftp::parent_of(&self.cwd);
                self.go(parent, actions);
            }
            return;
        }
        if f5 {
            let c = self.cwd.clone();
            self.go(c, actions);
            return;
        }
        if escape {
            self.list.clear();
            return;
        }
        if enter {
            if self.list.selected.len() == 1 {
                if let Some(e) = self.entries.get(self.list.selected[0]).cloned() {
                    self.open_entry(&e, actions);
                }
            }
            return;
        }
        if delete && !self.list.selected.is_empty() {
            let items = self.selected_entries();
            self.request_delete(&items, actions);
            return;
        }
        if f2 && self.list.selected.len() == 1 {
            self.begin_rename();
            return;
        }
        if !(ctrl_a || up || down || home || end) {
            return;
        }
        self.list
            .ensure_order(&self.entries, self.show_hidden, &self.filter, self.entries_stamp);
        let visible = self.list.order.clone();
        if ctrl_a {
            self.list.selected = visible;
            return;
        }
        if visible.is_empty() {
            return;
        }
        let cur_pos = self
            .list
            .selected
            .last()
            .and_then(|&s| visible.iter().position(|&v| v == s));
        let next_pos = if up {
            Some(cur_pos.map(|p| p.saturating_sub(1)).unwrap_or(0))
        } else if down {
            Some(cur_pos.map(|p| (p + 1).min(visible.len() - 1)).unwrap_or(0))
        } else if home {
            Some(0)
        } else if end {
            Some(visible.len() - 1)
        } else {
            None
        };
        if let Some(np) = next_pos {
            let target = visible[np];
            if shift {
                let a = self.list.anchor.unwrap_or(target);
                let pa = visible.iter().position(|&v| v == a).unwrap_or(np);
                let (lo, hi) = if pa <= np { (pa, np) } else { (np, pa) };
                self.list.selected = visible[lo..=hi].to_vec();
                // Keep the moving end last so the next arrow continues from it.
                if let Some(pos) = self.list.selected.iter().position(|&x| x == target) {
                    let t = self.list.selected.remove(pos);
                    self.list.selected.push(t);
                }
            } else {
                self.list.selected = vec![target];
                self.list.anchor = Some(target);
            }
            self.list.scroll_to = Some(target);
        }
    }

    // ------------------------------------------------------------------
    // transfers
    // ------------------------------------------------------------------

    /// Summary row. Returns true when the row itself was clicked (not the clear button).
    fn transfer_bar(&mut self, ui: &mut Ui, show_list: bool) -> bool {
        let total = self.transfers.len();
        let done = self
            .transfers
            .iter()
            .filter(|t| t.result.as_ref().is_some_and(|r| r.is_ok()))
            .count();
        let failed = self
            .transfers
            .iter()
            .filter(|t| t.result.as_ref().is_some_and(|r| r.is_err()))
            .count();
        let summary = if failed > 0 {
            trf(
                "Transfers {done}/{total} done, {failed} failed",
                &[("{done}", &done), ("{total}", &total), ("{failed}", &failed)],
            )
        } else {
            trf(
                "Transfers {done}/{total} done",
                &[("{done}", &done), ("{total}", &total)],
            )
        };
        let color = if failed > 0 {
            ERR_COLOR
        } else if done == total {
            OK_COLOR
        } else {
            ui.visuals().text_color()
        };
        let mark = if show_list { "▾" } else { "▸" };
        let mut toggle = false;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.transfers.iter().any(|t| t.result.is_some()) && ui.small_button(tr("Clear finished")).clicked()
                {
                    self.transfers.retain(|t| t.result.is_none());
                }
                let width = ui.available_width();
                let height = ui.spacing().interact_size.y;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(3), ui.visuals().widgets.hovered.weak_bg_fill);
                }
                ui.painter().text(
                    rect.left_center() + Vec2::new(4.0, 0.0),
                    Align2::LEFT_CENTER,
                    format!("{mark}  {summary}"),
                    egui::TextStyle::Body.resolve(ui.style()),
                    color,
                );
                if resp.clicked() {
                    toggle = true;
                }
                resp.on_hover_text(tr(
                    "Click to show or hide the transfer list. Drag the top edge to resize.",
                ));
            });
        });
        toggle
    }

    fn transfer_rows(&mut self, ui: &mut Ui, actions: &mut Vec<SftpAction>) {
        egui::ScrollArea::vertical()
            .id_salt("sftp-transfers-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Fixed columns: [icon][name, truncated][bar 150px][Cancel]. The name never
                // pushes the bar or the button around.
                let bar_w = 150.0;
                let btn_w = 64.0;
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
                for t in self.transfers.iter().rev() {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(ICON), Sense::hover());
                        paint_transfer_icon(ui.painter(), rect, t.kind);
                        let dir = match t.kind {
                            TransferKind::Upload => tr("Uploading"),
                            TransferKind::Download => tr("Downloading"),
                            TransferKind::Copy => tr("Copying"),
                        };
                        let spacing = ui.spacing().item_spacing.x;
                        let name_w = (ui.available_width() - bar_w - btn_w - 2.0 * spacing).max(60.0);
                        match &t.result {
                            Some(Ok(())) => {
                                let text = trf("{dir} done · {name}", &[("{dir}", &dir), ("{name}", &t.name)]);
                                left_label(
                                    ui,
                                    [name_w + bar_w + btn_w + 2.0 * spacing, row_h],
                                    RichText::new(text).color(OK_COLOR),
                                )
                                .on_hover_text(&t.name);
                            }
                            Some(Err(e)) => {
                                let text = trf(
                                    "{dir} failed · {name}: {e}",
                                    &[("{dir}", &dir), ("{name}", &t.name), ("{e}", &e)],
                                );
                                left_label(
                                    ui,
                                    [name_w + bar_w + btn_w + 2.0 * spacing, row_h],
                                    RichText::new(text.clone()).color(ERR_COLOR),
                                )
                                .on_hover_text(text);
                            }
                            None => {
                                let frac = if t.total > 0 {
                                    (t.done as f64 / t.total as f64) as f32
                                } else {
                                    0.0
                                };
                                let text = trf("{dir} · {name}", &[("{dir}", &dir), ("{name}", &t.name)]);
                                left_label(ui, [name_w, row_h], RichText::new(text)).on_hover_text(&t.name);
                                let bar_text = if t.files_total == 0 && t.done == 0 {
                                    trf("counting… {n} files", &[("{n}", &t.files_done)])
                                } else if t.files_total > 1 {
                                    format!(
                                        "{}/{} · {} / {}",
                                        t.files_done,
                                        t.files_total,
                                        fmt_size_short(t.done),
                                        fmt_size_short(t.total)
                                    )
                                } else {
                                    format!("{} / {}", fmt_size_short(t.done), fmt_size_short(t.total))
                                };
                                ui.add_sized(
                                    [bar_w, row_h],
                                    egui::ProgressBar::new(frac).text(RichText::new(bar_text).small()),
                                );
                                if ui
                                    .add_sized([btn_w, row_h], egui::Button::new(tr("Cancel")).small())
                                    .clicked()
                                {
                                    actions.push(SftpAction::Cmd(SftpCmd::CancelTransfer(t.id)));
                                }
                            }
                        }
                    });
                }
            });
    }
}

// ----------------------------------------------------------------------
// shared details view
// ----------------------------------------------------------------------

/// What one frame of a details view wants its owner to do.
#[derive(Debug, Default)]
pub struct ViewOut {
    /// Entry index that was double-clicked (or Enter-activated).
    pub open: Option<usize>,
    /// The `..` row was double-clicked: go to the parent folder.
    pub open_parent: bool,
    /// A drag started on this entry.
    pub drag_started: Option<usize>,
    /// While something is dragged: the folder row under the pointer.
    pub hovered_dir: Option<usize>,
    /// While something is dragged: the pointer is somewhere over the list.
    pub over_list: bool,
    /// The list has keyboard focus this frame.
    pub focused: bool,
    pub visible_count: usize,
}

/// One row of the folder tree, in the order it is shown.
struct TreeRow {
    path: String,
    name: String,
    depth: usize,
    has_children: bool,
    known: bool,
    expanded: bool,
}

/// Expanded tree nodes, parents before their children. Built when the tree changes, not per frame.
fn flatten_tree(
    tree: &BTreeMap<String, Vec<TreeChild>>,
    expanded: &HashSet<String>,
    show_hidden: bool,
) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut stack = vec![("/".to_string(), "/".to_string(), 0usize)];
    while let Some((path, name, depth)) = stack.pop() {
        let known = tree.contains_key(&path);
        let has_children = tree.get(&path).map(|c| !c.is_empty()).unwrap_or(true);
        let is_open = expanded.contains(&path);
        let children = if is_open {
            tree.get(&path).map(|cs| {
                cs.iter()
                    .rev()
                    .filter(|c| show_hidden || !c.name.starts_with('.'))
                    .map(|c| (c.path.clone(), c.name.clone()))
                    .collect::<Vec<_>>()
            })
        } else {
            None
        };
        rows.push(TreeRow {
            path,
            name,
            depth,
            has_children,
            known,
            expanded: is_open,
        });
        if let Some(children) = children {
            for (path, name) in children {
                stack.push((path, name, depth + 1));
            }
        }
    }
    rows
}

/// Sort keys computed once per entry, so the comparison itself does not allocate.
struct SortRow {
    idx: usize,
    dir: bool,
    name: String,
    mtime: Option<u32>,
    size: u64,
    kind: Option<String>,
}

/// Entry indices in display order: filtered, folders first, then by the sort key.
fn visible_sorted(entries: &[SftpEntry], sort: SortKey, desc: bool, show_hidden: bool, filter: &str) -> Vec<usize> {
    let filter = filter.to_lowercase();
    let mut rows: Vec<SortRow> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            (show_hidden || !e.name.starts_with('.')) && (filter.is_empty() || e.name.to_lowercase().contains(&filter))
        })
        .map(|(idx, e)| SortRow {
            idx,
            dir: e.is_dir,
            name: e.name.to_lowercase(),
            mtime: e.mtime,
            size: e.size,
            kind: (sort == SortKey::Kind).then(|| kind_of(e)),
        })
        .collect();
    rows.sort_by(|a, b| {
        // Folders always come first, as in Explorer.
        let dir = b.dir.cmp(&a.dir);
        if dir != std::cmp::Ordering::Equal {
            return dir;
        }
        let ord = match sort {
            SortKey::Name => std::cmp::Ordering::Equal,
            SortKey::Modified => a.mtime.cmp(&b.mtime),
            SortKey::Kind => a.kind.cmp(&b.kind),
            SortKey::Size => a.size.cmp(&b.size),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| {
            let n = a.name.cmp(&b.name);
            if sort == SortKey::Name && desc {
                n.reverse()
            } else {
                n
            }
        })
    });
    rows.into_iter().map(|r| r.idx).collect()
}

/// Explorer-style details view over `entries`. The context menu closure gets the entry index
/// (`usize::MAX` for the empty area), the selection, and the entry itself when there is one.
/// `stamp` changes when `entries` is replaced.
#[allow(clippy::too_many_arguments)]
fn details_view(
    ui: &mut Ui,
    list_id: egui::Id,
    entries: &[SftpEntry],
    st: &mut ListState,
    show_hidden: bool,
    filter: &str,
    stamp: u64,
    dragging: bool,
    has_parent: bool,
    mut ctx_menu: impl FnMut(&mut Ui, usize, &ListState, Option<&SftpEntry>),
) -> ViewOut {
    let visuals = ui.visuals().clone();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    st.ensure_order(entries, show_hidden, filter, stamp);
    let mut out = ViewOut {
        visible_count: st.order.len(),
        focused: ui.memory(|m| m.has_focus(list_id)),
        ..Default::default()
    };
    let pointer = ui.input(|i| i.pointer.latest_pos());

    // ---- columns: drop the least important ones as the panel gets narrower ----
    let total_w = ui.available_width();
    let (w_time, w_kind, w_size, w_mode) = (108.0f32, 88.0f32, 68.0f32, 78.0f32);
    let show_size = total_w >= 200.0;
    let show_time = total_w >= 300.0;
    let show_kind = total_w >= 400.0;
    let show_mode = total_w >= 520.0 && entries.iter().any(|e| e.permissions.is_some());
    let mut fixed = 0.0;
    if show_time {
        fixed += w_time;
    }
    if show_kind {
        fixed += w_kind;
    }
    if show_size {
        fixed += w_size;
    }
    if show_mode {
        fixed += w_mode;
    }
    let w_name = (total_w - fixed - 16.0).max(60.0);
    // (sort key, label, width, right-aligned, column kind)
    let mut cols: Vec<(Option<SortKey>, &'static str, f32, bool, u8)> =
        vec![(Some(SortKey::Name), "Name", w_name, false, 0)];
    if show_time {
        cols.push((Some(SortKey::Modified), "Date modified", w_time, false, 1));
    }
    if show_kind {
        cols.push((Some(SortKey::Kind), "Type", w_kind, false, 2));
    }
    if show_size {
        cols.push((Some(SortKey::Size), "Size", w_size, true, 3));
    }
    if show_mode {
        cols.push((None, "Permissions", w_mode, false, 4));
    }

    // ---- header ----
    let (hrect, _) = ui.allocate_exact_size(Vec2::new(total_w, row_h), Sense::hover());
    {
        let p = ui.painter();
        p.rect_filled(hrect, CornerRadius::ZERO, visuals.faint_bg_color);
        p.line_segment(
            [hrect.left_bottom(), hrect.right_bottom()],
            visuals.widgets.noninteractive.bg_stroke,
        );
    }
    // Header checkbox: all visible rows selected / none.
    {
        let (clear_all, select_all) = {
            let order = &st.order;
            let selected = &st.selected;
            let all = !order.is_empty() && order.iter().all(|i| selected.contains(i));
            let some = order.iter().any(|i| selected.contains(i));
            let cb_rect = Rect::from_min_size(
                Pos2::new(hrect.min.x + 5.0, hrect.center().y - CHECK / 2.0),
                Vec2::splat(CHECK),
            );
            let cb = ui.interact(cb_rect.expand(3.0), list_id.with("hdr-cb"), Sense::click());
            cb.widget_info(|| egui::WidgetInfo::labeled(egui::Role::CheckBox, true, tr("Select all / none")));
            let cb = cb.on_hover_text(tr("Select all / none"));
            paint_checkbox(ui.painter(), cb_rect, all, some && !all, cb.hovered(), &visuals);
            let clicked = cb.clicked();
            (clicked && all, clicked && !all)
        };
        if clear_all {
            st.clear();
        } else if select_all {
            st.selected = st.order.clone();
            st.anchor = st.order.first().copied();
        }
    }
    let mut x = hrect.min.x;
    let mut clicked_col: Option<SortKey> = None;
    for (ci, (key, label, w, right, _)) in cols.iter().enumerate() {
        let crect = Rect::from_min_size(Pos2::new(x, hrect.min.y), Vec2::new(*w, row_h));
        let r = ui.interact(crect, list_id.with(("hdr", ci)), Sense::click());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ColumnHeader, true, tr(label)));
        let p = ui.painter();
        if r.hovered() && key.is_some() {
            p.rect_filled(crect, CornerRadius::ZERO, visuals.widgets.hovered.weak_bg_fill);
        }
        if ci > 0 {
            p.line_segment(
                [Pos2::new(x, crect.min.y + 3.0), Pos2::new(x, crect.max.y - 3.0)],
                visuals.widgets.noninteractive.bg_stroke,
            );
        }
        let (anchor, pos) = if *right {
            (Align2::RIGHT_CENTER, Pos2::new(crect.max.x - 6.0, crect.center().y))
        } else if ci == 0 {
            (
                Align2::LEFT_CENTER,
                Pos2::new(crect.min.x + CHECK + 12.0, crect.center().y),
            )
        } else {
            (Align2::LEFT_CENTER, Pos2::new(crect.min.x + 6.0, crect.center().y))
        };
        p.with_clip_rect(crect)
            .text(pos, anchor, tr(label), small.clone(), visuals.text_color());
        if let Some(k) = key {
            if st.sort == *k {
                let tri = Rect::from_center_size(Pos2::new(crect.center().x, crect.min.y + 4.0), Vec2::new(8.0, 4.0));
                paint_sort_triangle(p, tri, st.sort_desc, visuals.weak_text_color());
            }
            if r.clicked() {
                clicked_col = Some(*k);
            }
        }
        x += w;
    }
    if let Some(k) = clicked_col {
        if st.sort == k {
            st.sort_desc = !st.sort_desc;
        } else {
            st.sort = k;
            st.sort_desc = false;
        }
    }

    // ---- `..` row: always visible above the list, double-click goes up one folder ----
    if has_parent {
        let (prect, pr) = ui.allocate_exact_size(Vec2::new(total_w, row_h), Sense::click());
        let pr = pr.on_hover_text(tr("Go to the parent folder (double-click, or Backspace)"));
        pr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ListItem, true, tr("Parent folder")));
        let p = ui.painter();
        if pr.hovered() {
            p.rect_filled(prect, CornerRadius::same(2), visuals.widgets.hovered.weak_bg_fill);
        }
        let icon = Rect::from_min_size(
            Pos2::new(prect.min.x + CHECK + 10.0, prect.center().y - ICON / 2.0),
            Vec2::splat(ICON),
        );
        let up = SftpEntry {
            name: "..".into(),
            raw_name: "..".into(),
            path: String::new(),
            is_dir: true,
            is_symlink: false,
            is_regular: false,
            size: 0,
            mtime: None,
            permissions: None,
        };
        paint_entry_icon(p, icon, &up);
        p.text(
            Pos2::new(icon.max.x + 6.0, prect.center().y),
            Align2::LEFT_CENTER,
            "..",
            font.clone(),
            visuals.text_color(),
        );
        p.line_segment(
            [prect.left_bottom(), prect.right_bottom()],
            visuals.widgets.noninteractive.bg_stroke,
        );
        if pr.double_clicked() || pr.triple_clicked() {
            out.open_parent = true;
        }
    }

    // ---- rows ----
    let status_h = ui.text_style_height(&egui::TextStyle::Small) + 8.0;
    let list_h = (ui.available_height() - status_h).max(60.0);
    let ctrl = ui.input(|i| i.modifiers.command);
    let shift = ui.input(|i| i.modifiers.shift);
    let mut select_click: Option<usize> = None;
    let mut toggle_click: Option<usize> = None;
    let mut clear_selection = false;
    let mut focus_list = false;
    let scroll_to = st.scroll_to.take();

    let list_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(total_w, list_h));
    // Background: clicking empty space clears the selection and focuses the list.
    let bg = ui.interact(list_rect, list_id, Sense::click());
    if bg.clicked() {
        clear_selection = true;
        focus_list = true;
    }
    if dragging && pointer.map(|p| list_rect.contains(p)).unwrap_or(false) {
        out.over_list = true;
    }
    bg.context_menu(|ui| ctx_menu(ui, usize::MAX, st, None));

    let scroll = egui::ScrollArea::vertical()
        .id_salt(list_id.with("scroll"))
        .max_height(list_h)
        .auto_shrink([false, false]);
    // `show_rows` strides by row height plus `item_spacing.y`. The rows themselves
    // are packed with no gap, so a non-zero spacing leaves an empty band at the
    // bottom of the viewport and hides the next files and folders.
    let row_gap = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;
    let scroll = if let Some(target) = scroll_to.and_then(|i| st.order.iter().position(|&v| v == i)) {
        let spacing = ui.spacing().item_spacing.y;
        let y = target as f32 * (row_h + spacing);
        scroll.vertical_scroll_offset(y.max(0.0) - list_h / 2.0 + row_h)
    } else {
        scroll
    };
    scroll.show_rows(ui, row_h, st.order.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = ui.available_width().max(total_w - 16.0);
        for vi in range {
            let i = st.order[vi];
            let e = &entries[i];
            let selected = st.selected.contains(&i);
            let (rect, r) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click_and_drag());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ListItem, true, &e.name));
            // Checkbox in front of the name: toggles this row in the selection without Ctrl.
            // Registered after the row, so it is on top and gets the click.
            let cb_rect = Rect::from_min_size(
                Pos2::new(rect.min.x + 5.0, rect.center().y - CHECK / 2.0),
                Vec2::splat(CHECK),
            );
            let cb = ui.interact(cb_rect.expand(3.0), list_id.with(("cb", i)), Sense::click());
            cb.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::CheckBox, true, trf("Select {name}", &[("{name}", &e.name)]))
            });
            if cb.clicked() {
                toggle_click = Some(i);
            }
            let under_pointer = dragging && pointer.map(|p| rect.contains(p)).unwrap_or(false);
            let p = ui.painter();
            if selected {
                p.rect_filled(rect, CornerRadius::same(2), visuals.selection.bg_fill);
            } else if r.hovered() || (under_pointer && e.is_dir) {
                p.rect_filled(rect, CornerRadius::same(2), visuals.widgets.hovered.weak_bg_fill);
            } else if vi % 2 == 1 {
                p.rect_filled(rect, CornerRadius::ZERO, visuals.faint_bg_color.gamma_multiply(0.5));
            }
            if under_pointer && e.is_dir {
                out.hovered_dir = Some(i);
                p.rect_stroke(
                    rect,
                    CornerRadius::same(2),
                    visuals.selection.stroke,
                    StrokeKind::Inside,
                );
            }
            let text_col = if selected {
                visuals.selection.stroke.color
            } else {
                visuals.text_color()
            };
            let weak_col = if selected {
                visuals.selection.stroke.color.gamma_multiply(0.8)
            } else {
                visuals.weak_text_color()
            };
            let mut x = rect.min.x;
            for (_, _, w, right, kind) in cols.iter() {
                let crect = Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(*w, row_h));
                let cp = p.with_clip_rect(crect);
                if *kind == 0 {
                    paint_checkbox(&cp, cb_rect, selected, false, cb.hovered(), &visuals);
                    let icon = Rect::from_min_size(
                        Pos2::new(crect.min.x + CHECK + 10.0, crect.center().y - ICON / 2.0),
                        Vec2::splat(ICON),
                    );
                    paint_entry_icon(&cp, icon, e);
                    cp.text(
                        Pos2::new(icon.max.x + 6.0, crect.center().y),
                        Align2::LEFT_CENTER,
                        &e.name,
                        font.clone(),
                        text_col,
                    );
                } else {
                    let text = match kind {
                        1 => e.mtime.map(fmt_time).unwrap_or_default(),
                        2 => kind_of(e),
                        3 => {
                            if e.is_dir {
                                String::new()
                            } else {
                                fmt_size(e.size)
                            }
                        }
                        _ => e.permissions.map(fmt_mode).unwrap_or_default(),
                    };
                    let (anchor, pos) = if *right {
                        (Align2::RIGHT_CENTER, Pos2::new(crect.max.x - 6.0, crect.center().y))
                    } else {
                        (Align2::LEFT_CENTER, Pos2::new(crect.min.x + 6.0, crect.center().y))
                    };
                    let f = if *kind == 4 {
                        FontId::monospace(font.size * 0.9)
                    } else {
                        font.clone()
                    };
                    cp.text(pos, anchor, text, f, weak_col);
                }
                x += w;
            }
            if r.drag_started() {
                out.drag_started = Some(i);
            }
            // A rapid third click still means "open" (egui reports it as a triple-click).
            if r.double_clicked() || r.triple_clicked() {
                out.open = Some(i);
            } else if r.clicked() {
                select_click = Some(i);
                focus_list = true;
            } else if r.secondary_clicked() && !selected {
                // Right-click selects the row under the cursor, as Explorer does.
                st.selected = vec![i];
                st.anchor = Some(i);
                focus_list = true;
            }
            r.context_menu(|ui| ctx_menu(ui, i, st, Some(e)));
        }
    });
    ui.spacing_mut().item_spacing.y = row_gap;

    if clear_selection {
        st.clear();
    }
    if let Some(i) = toggle_click {
        if let Some(pos) = st.selected.iter().position(|&x| x == i) {
            st.selected.remove(pos);
        } else {
            st.selected.push(i);
        }
        st.anchor = Some(i);
        focus_list = true;
    }
    if let Some(i) = select_click {
        if ctrl {
            if let Some(pos) = st.selected.iter().position(|&x| x == i) {
                st.selected.remove(pos);
            } else {
                st.selected.push(i);
            }
            st.anchor = Some(i);
        } else if shift {
            let a = st.anchor.unwrap_or(i);
            let pa = st.order.iter().position(|&v| v == a);
            let pb = st.order.iter().position(|&v| v == i);
            if let (Some(pa), Some(pb)) = (pa, pb) {
                let (lo, hi) = if pa <= pb { (pa, pb) } else { (pb, pa) };
                st.selected = st.order[lo..=hi].to_vec();
            } else {
                st.selected = vec![i];
                st.anchor = Some(i);
            }
        } else {
            st.selected = vec![i];
            st.anchor = Some(i);
        }
    }
    if focus_list {
        ui.memory_mut(|m| m.request_focus(list_id));
        out.focused = true;
    }
    out
}

/// Explorer-style address bar: clickable path segments; clicking the empty remainder returns
/// `edit = true`. Returns `(navigate_to, edit)`.
fn breadcrumb_bar(ui: &mut Ui, id: &str, crumbs: &[(String, String)], h: f32) -> (Option<String>, bool) {
    let visuals = ui.visuals().clone();
    let mut target: Option<String> = None;
    let mut edit = false;
    let frame = egui::Frame::new()
        .fill(visuals.extreme_bg_color)
        .stroke(Stroke::new(1.0, visuals.widgets.inactive.bg_fill))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(egui::Margin::symmetric(4, 2));
    frame.show(ui, |ui| {
        ui.push_id(id, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(h - 4.0);
                ui.spacing_mut().item_spacing.x = 0.0;
                for (i, (label, path)) in crumbs.iter().enumerate() {
                    if i > 0 {
                        ui.label(RichText::new("›").weak());
                    }
                    let last = i + 1 == crumbs.len();
                    let text = if last {
                        RichText::new(label).strong()
                    } else {
                        RichText::new(label)
                    };
                    let r = ui.add(egui::Button::new(text).frame(false).min_size(Vec2::new(0.0, h - 6.0)));
                    if r.on_hover_text(path).clicked() {
                        target = Some(path.clone());
                    }
                }
                // The empty remainder of the bar: click to type a path.
                let rest = ui.available_size_before_wrap();
                let (rect, r) = ui.allocate_exact_size(Vec2::new(rest.x.max(20.0), h - 6.0), Sense::click());
                if r.hovered() {
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::ZERO,
                        Stroke::new(1.0, visuals.widgets.hovered.bg_stroke.color),
                        StrokeKind::Inside,
                    );
                }
                if r.on_hover_text(tr("Click to type a path")).clicked() {
                    edit = true;
                }
            });
        });
    });
    (target, edit)
}

/// Drive letters (Windows) or `/` (elsewhere) for the local pane's drives menu.
fn local_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        (b'A'..=b'Z')
            .map(|c| PathBuf::from(format!("{}:\\", c as char)))
            .filter(|p| p.is_dir())
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

/// Build download commands into `dir`, splitting out the ones that would overwrite a local
/// file (the app confirms those) and skipping names that fail `safe_component`.
fn downloads_into(dir: &Path, items: &[SftpEntry], actions: &mut Vec<SftpAction>, errors: &mut Vec<String>) {
    let mut conflicts: Vec<SftpCmd> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for it in items {
        let Ok(name) = sftp::safe_component(&it.raw_name) else {
            errors.push(it.name.clone());
            continue;
        };
        let local = dir.join(name);
        let cmd = SftpCmd::Download {
            remote: it.path.clone(),
            local: local.clone(),
            is_dir: it.is_dir,
        };
        if local.exists() {
            conflicts.push(cmd);
            names.push(it.name.clone());
        } else {
            actions.push(SftpAction::Cmd(cmd));
        }
    }
    if !conflicts.is_empty() {
        actions.push(SftpAction::ConfirmOverwrite { cmds: conflicts, names });
    }
}

// ----------------------------------------------------------------------
// formatting
// ----------------------------------------------------------------------

fn fmt_time(t: u32) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// Explorer-style size: whole kilobytes with thousands separators ("1,234 KB").
/// Compact size for narrow places such as the progress bar: `716 MB`, `2.0 GB`.
pub fn fmt_size_short(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

pub fn fmt_size(bytes: u64) -> String {
    let kb = if bytes == 0 { 0 } else { bytes.div_ceil(1024).max(1) };
    let digits = kb.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.push_str(" KB");
    out
}

fn fmt_mode(m: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(match m & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        _ => '-',
    });
    for shift in [6u32, 3, 0] {
        let b = (m >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn ext_of(name: &str) -> String {
    let stem = name.trim_start_matches('.');
    match stem.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => {
            ext.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

fn is_executable(e: &SftpEntry) -> bool {
    e.permissions.map(|p| p & 0o111 != 0).unwrap_or(false)
}

const CODE_EXT: &[&str] = &[
    "js", "ts", "rs", "c", "h", "cpp", "hpp", "java", "go", "rb", "php", "pl", "lua", "cs", "swift", "kt", "sql",
    "html", "css",
];
const CONFIG_EXT: &[&str] = &["json", "yaml", "yml", "toml", "ini", "cfg", "conf", "xml", "env"];
const ARCHIVE_EXT: &[&str] = &["zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar"];
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "ico", "tif", "tiff"];

/// Explorer-style "Type" column.
pub fn kind_of(e: &SftpEntry) -> String {
    if e.is_dir {
        return if e.is_symlink {
            tr("Folder shortcut").into()
        } else {
            tr("File folder").into()
        };
    }
    if e.is_symlink && !e.is_regular {
        return tr("Shortcut").into();
    }
    let ext = ext_of(&e.name);
    let up = ext.to_uppercase();
    let with_ext = |key: &'static str| trf(key, &[("{ext}", &up)]);
    let base: String = match ext.as_str() {
        "" => {
            if is_executable(e) {
                tr("Executable file").into()
            } else {
                tr("File").into()
            }
        }
        "txt" => tr("Text document").into(),
        "md" => tr("Markdown document").into(),
        "pdf" => tr("PDF document").into(),
        "log" => tr("Log file").into(),
        "sh" | "bash" | "zsh" => tr("Shell script").into(),
        "py" => tr("Python script").into(),
        x if CODE_EXT.contains(&x) => with_ext("{ext} source file"),
        x if CONFIG_EXT.contains(&x) => with_ext("{ext} configuration file"),
        x if ARCHIVE_EXT.contains(&x) => with_ext("{ext} archive"),
        x if IMAGE_EXT.contains(&x) => with_ext("{ext} image"),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" => with_ext("{ext} audio"),
        "mp4" | "mkv" | "avi" | "mov" | "webm" => with_ext("{ext} video"),
        "exe" | "msi" | "dll" | "so" | "bin" | "deb" | "rpm" | "appimage" => with_ext("{ext} executable"),
        "csv" | "tsv" => with_ext("{ext} data file"),
        "xlsx" | "xls" | "docx" | "doc" | "pptx" | "ppt" | "hwp" | "hwpx" => with_ext("{ext} document"),
        _ => with_ext("{ext} file"),
    };
    if e.is_symlink {
        trf("{kind} (link)", &[("{kind}", &base)])
    } else {
        base
    }
}

// ----------------------------------------------------------------------
// painted icons (no icon font needed; look the same in every theme)
// ----------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Icon {
    Back,
    Forward,
    Up,
    Refresh,
}

/// Side of the row checkboxes.
const CHECK: f32 = 14.0;

/// A fixed-size, left-aligned, truncating label (`add_sized` would centre it).
fn left_label(ui: &mut Ui, size: [f32; 2], text: RichText) -> egui::Response {
    ui.allocate_ui_with_layout(
        Vec2::from(size),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(Vec2::from(size));
            ui.add(egui::Label::new(text).truncate())
        },
    )
    .inner
}

/// A square checkbox: empty, ticked, or a dash for "some selected".
fn paint_checkbox(p: &egui::Painter, rect: Rect, checked: bool, partial: bool, hovered: bool, visuals: &egui::Visuals) {
    let stroke_col = if checked || partial || hovered {
        visuals.selection.stroke.color
    } else {
        visuals.weak_text_color()
    };
    let fill = if checked || partial {
        visuals.selection.bg_fill
    } else if hovered {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        visuals.extreme_bg_color
    };
    p.rect(
        rect,
        CornerRadius::same(3),
        fill,
        Stroke::new(1.0, stroke_col),
        StrokeKind::Inside,
    );
    let s = Stroke::new(1.8, visuals.selection.stroke.color.gamma_multiply(1.4));
    if checked {
        let a = Pos2::new(rect.min.x + 3.0, rect.center().y + 0.5);
        let b = Pos2::new(rect.min.x + 6.0, rect.max.y - 3.5);
        let c = Pos2::new(rect.max.x - 3.0, rect.min.y + 3.5);
        p.line_segment([a, b], s);
        p.line_segment([b, c], s);
    } else if partial {
        p.line_segment(
            [
                Pos2::new(rect.min.x + 3.0, rect.center().y),
                Pos2::new(rect.max.x - 3.0, rect.center().y),
            ],
            s,
        );
    }
}

fn icon_button(ui: &mut Ui, icon: Icon, enabled: bool, tip: &str) -> egui::Response {
    let r = ui.add_enabled(enabled, egui::Button::new("     ").min_size(Vec2::new(28.0, 22.0)));
    // The glyph is painted below, so give screen readers the tooltip as the button's name.
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, tip));
    let r = r.on_hover_text(tip);
    let col = if enabled {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color().gamma_multiply(0.6)
    };
    let c = r.rect.center();
    let p = ui.painter();
    let s = Stroke::new(1.6, col);
    match icon {
        Icon::Back | Icon::Forward => {
            let dir = if matches!(icon, Icon::Back) { -1.0 } else { 1.0 };
            let tip_p = Pos2::new(c.x + 5.0 * dir, c.y);
            let tail = Pos2::new(c.x - 5.0 * dir, c.y);
            p.line_segment([tail, tip_p], s);
            p.line_segment([tip_p, Pos2::new(tip_p.x - 4.0 * dir, c.y - 4.0)], s);
            p.line_segment([tip_p, Pos2::new(tip_p.x - 4.0 * dir, c.y + 4.0)], s);
        }
        Icon::Up => {
            let tip_p = Pos2::new(c.x, c.y - 5.0);
            p.line_segment([Pos2::new(c.x, c.y + 5.0), tip_p], s);
            p.line_segment([tip_p, Pos2::new(c.x - 4.0, tip_p.y + 4.0)], s);
            p.line_segment([tip_p, Pos2::new(c.x + 4.0, tip_p.y + 4.0)], s);
        }
        Icon::Refresh => {
            // A 270° arc (open at the top right) with an arrowhead along its tangent.
            let rad = 5.5;
            let n = 24;
            let start = -0.25 * std::f32::consts::PI; // top right
            let end = start + 1.5 * std::f32::consts::PI; // back to the top, clockwise
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let a = start + (end - start) * i as f32 / n as f32;
                    Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin())
                })
                .collect();
            // Arrowhead at the start of the arc, pointing in the direction of travel.
            let head = pts[0];
            p.add(Shape::line(pts, s));
            let (tx, ty) = (-start.sin(), start.cos()); // unit tangent (clockwise on screen)
            let (nx, ny) = (-ty, tx);
            let len = 4.5;
            let wing = 2.6;
            p.line_segment(
                [
                    head,
                    Pos2::new(head.x - tx * len + nx * wing, head.y - ty * len + ny * wing),
                ],
                s,
            );
            p.line_segment(
                [
                    head,
                    Pos2::new(head.x - tx * len - nx * wing, head.y - ty * len - ny * wing),
                ],
                s,
            );
        }
    }
    r
}

/// Give a text field the keyboard and select its first `chars` characters (Explorer selects
/// the whole path in the address bar and the file stem when renaming).
fn focus_select(ui: &Ui, r: &egui::Response, chars: usize) {
    r.request_focus();
    let mut st = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
        egui::text::CCursor::new(0),
        egui::text::CCursor::new(chars),
    )));
    egui::TextEdit::store_state(ui.ctx(), r.id, st);
}

/// Small "▾" at the right edge of a menu button (the bundled fonts have no such glyph, so
/// it is painted; the button label leaves room for it with trailing spaces).
fn paint_menu_arrow(ui: &Ui, r: &egui::Response) {
    let c = Pos2::new(r.rect.max.x - 9.0, r.rect.center().y + 0.5);
    let col = ui.visuals().text_color();
    ui.painter().add(Shape::convex_polygon(
        vec![
            Pos2::new(c.x - 3.5, c.y - 2.0),
            Pos2::new(c.x + 3.5, c.y - 2.0),
            Pos2::new(c.x, c.y + 2.0),
        ],
        col,
        Stroke::NONE,
    ));
}

fn paint_expander(p: &egui::Painter, rect: Rect, expanded: bool, col: Color32) {
    let c = rect.center();
    let pts = if expanded {
        vec![
            Pos2::new(c.x - 3.5, c.y - 1.5),
            Pos2::new(c.x + 3.5, c.y - 1.5),
            Pos2::new(c.x, c.y + 2.5),
        ]
    } else {
        vec![
            Pos2::new(c.x - 1.5, c.y - 3.5),
            Pos2::new(c.x + 2.5, c.y),
            Pos2::new(c.x - 1.5, c.y + 3.5),
        ]
    };
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn paint_sort_triangle(p: &egui::Painter, rect: Rect, desc: bool, col: Color32) {
    let pts = if desc {
        vec![rect.left_top(), rect.right_top(), rect.center_bottom()]
    } else {
        vec![rect.left_bottom(), rect.right_bottom(), rect.center_top()]
    };
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn paint_folder_icon(p: &egui::Painter, rect: Rect, open: bool) {
    let r = rect.shrink(1.0);
    let back = Color32::from_rgb(0xDC, 0xA6, 0x36);
    let front = Color32::from_rgb(0xF6, 0xCD, 0x62);
    let tab = Rect::from_min_max(
        r.min + Vec2::new(0.0, 1.0),
        Pos2::new(r.min.x + r.width() * 0.42, r.min.y + 5.0),
    );
    p.rect_filled(tab, CornerRadius::same(1), back);
    let body = Rect::from_min_max(Pos2::new(r.min.x, r.min.y + 3.5), Pos2::new(r.max.x, r.max.y - 0.5));
    p.rect_filled(body, CornerRadius::same(1), back);
    let top = if open { 7.0 } else { 6.0 };
    let face = Rect::from_min_max(
        Pos2::new(r.min.x + if open { 1.5 } else { 0.0 }, r.min.y + top),
        Pos2::new(r.max.x, r.max.y - 0.5),
    );
    p.rect_filled(face, CornerRadius::same(1), front);
}

fn paint_home_icon(p: &egui::Painter, rect: Rect) {
    let r = rect.shrink(1.0);
    let roof = Color32::from_rgb(0x4A, 0x90, 0xD9);
    let wall = Color32::from_rgb(0xA9, 0xCB, 0xEE);
    p.add(Shape::convex_polygon(
        vec![
            Pos2::new(r.center().x, r.min.y),
            Pos2::new(r.max.x + 0.5, r.min.y + r.height() * 0.5),
            Pos2::new(r.min.x - 0.5, r.min.y + r.height() * 0.5),
        ],
        roof,
        Stroke::NONE,
    ));
    let body = Rect::from_min_max(Pos2::new(r.min.x + 2.0, r.min.y + r.height() * 0.5), r.max);
    p.rect_filled(body, CornerRadius::ZERO, wall);
    let door = Rect::from_min_max(
        Pos2::new(r.center().x - 1.5, r.max.y - 5.0),
        Pos2::new(r.center().x + 1.5, r.max.y),
    );
    p.rect_filled(door, CornerRadius::ZERO, roof);
}

fn paint_file_icon(p: &egui::Painter, rect: Rect, accent: Color32) {
    let r = Rect::from_min_max(
        Pos2::new(rect.min.x + 2.5, rect.min.y + 0.5),
        Pos2::new(rect.max.x - 2.5, rect.max.y - 0.5),
    );
    let paper = Color32::from_rgb(0xFA, 0xFA, 0xFA);
    let edge = Color32::from_rgb(0x9A, 0x9A, 0x9A);
    p.rect_filled(r, CornerRadius::same(1), paper);
    p.rect_stroke(r, CornerRadius::same(1), Stroke::new(1.0, edge), StrokeKind::Inside);
    let fold = 4.0;
    p.add(Shape::convex_polygon(
        vec![
            Pos2::new(r.max.x - fold, r.min.y),
            Pos2::new(r.max.x, r.min.y + fold),
            Pos2::new(r.max.x - fold, r.min.y + fold),
        ],
        Color32::from_rgb(0xD5, 0xD5, 0xD5),
        Stroke::NONE,
    ));
    for k in 0..3 {
        let y = r.min.y + 6.5 + k as f32 * 2.5;
        p.line_segment(
            [Pos2::new(r.min.x + 2.5, y), Pos2::new(r.max.x - 2.5, y)],
            Stroke::new(1.0, accent),
        );
    }
}

fn paint_link_badge(p: &egui::Painter, rect: Rect) {
    let b = Rect::from_min_size(Pos2::new(rect.min.x, rect.max.y - 7.0), Vec2::splat(7.0));
    p.rect_filled(b, CornerRadius::same(1), Color32::WHITE);
    p.rect_stroke(
        b,
        CornerRadius::same(1),
        Stroke::new(1.0, Color32::from_rgb(0x80, 0x80, 0x80)),
        StrokeKind::Inside,
    );
    let s = Stroke::new(1.2, Color32::from_rgb(0x2B, 0x6C, 0xC4));
    let a = Pos2::new(b.min.x + 1.5, b.max.y - 1.5);
    let t = Pos2::new(b.max.x - 1.5, b.min.y + 1.5);
    p.line_segment([a, t], s);
    p.line_segment([t, Pos2::new(t.x - 2.5, t.y)], s);
    p.line_segment([t, Pos2::new(t.x, t.y + 2.5)], s);
}

fn paint_entry_icon(p: &egui::Painter, rect: Rect, e: &SftpEntry) {
    if e.is_dir {
        paint_folder_icon(p, rect, false);
    } else {
        let ext = ext_of(&e.name);
        let accent = match ext.as_str() {
            "sh" | "bash" | "zsh" | "py" => Color32::from_rgb(0x3C, 0x8C, 0xD8),
            x if CODE_EXT.contains(&x) => Color32::from_rgb(0x3C, 0x8C, 0xD8),
            x if CONFIG_EXT.contains(&x) => Color32::from_rgb(0x8E, 0x6C, 0xC8),
            x if ARCHIVE_EXT.contains(&x) => Color32::from_rgb(0xC9, 0x8A, 0x3A),
            x if IMAGE_EXT.contains(&x) => Color32::from_rgb(0x2E, 0xA8, 0x6E),
            "" if is_executable(e) => Color32::from_rgb(0x2E, 0xA8, 0x6E),
            _ => Color32::from_rgb(0xB4, 0xB4, 0xB4),
        };
        paint_file_icon(p, rect, accent);
    }
    if e.is_symlink {
        paint_link_badge(p, rect);
    }
}

fn paint_transfer_icon(p: &egui::Painter, rect: Rect, kind: TransferKind) {
    let c = rect.center();
    let col = match kind {
        TransferKind::Upload => Color32::from_rgb(0x3C, 0x8C, 0xD8),
        TransferKind::Download => Color32::from_rgb(0x2E, 0xA8, 0x6E),
        TransferKind::Copy => Color32::from_rgb(0xC9, 0x8A, 0x3A),
    };
    let s = Stroke::new(1.6, col);
    match kind {
        TransferKind::Copy => {
            let a = Rect::from_min_size(Pos2::new(c.x - 6.0, c.y - 6.0), Vec2::splat(8.0));
            let b = Rect::from_min_size(Pos2::new(c.x - 2.0, c.y - 2.0), Vec2::splat(8.0));
            p.rect_stroke(a, CornerRadius::same(1), s, StrokeKind::Inside);
            p.rect_filled(b, CornerRadius::same(1), col.gamma_multiply(0.35));
            p.rect_stroke(b, CornerRadius::same(1), s, StrokeKind::Inside);
        }
        _ => {
            let up = kind == TransferKind::Upload;
            let (from, to, dy) = if up {
                (Pos2::new(c.x, c.y + 5.0), Pos2::new(c.x, c.y - 5.0), 4.0)
            } else {
                (Pos2::new(c.x, c.y - 5.0), Pos2::new(c.x, c.y + 5.0), -4.0)
            };
            p.line_segment([from, to], s);
            p.line_segment([to, Pos2::new(to.x - 4.0, to.y + dy)], s);
            p.line_segment([to, Pos2::new(to.x + 4.0, to.y + dy)], s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dir: bool, size: u64, mode: u32) -> SftpEntry {
        SftpEntry {
            name: name.into(),
            raw_name: name.into(),
            path: format!("/x/{name}"),
            is_dir: dir,
            is_symlink: false,
            is_regular: !dir,
            size,
            mtime: None,
            permissions: Some(mode),
        }
    }

    #[test]
    fn ancestors_of_nested_path() {
        assert_eq!(ancestors("/"), vec!["/"]);
        assert_eq!(ancestors("/home"), vec!["/"]);
        assert_eq!(ancestors("/home/u/proj"), vec!["/", "/home", "/home/u"]);
        assert!(ancestors("relative").is_empty());
    }

    #[test]
    fn explorer_size_format() {
        assert_eq!(fmt_size(0), "0 KB");
        assert_eq!(fmt_size(1), "1 KB");
        assert_eq!(fmt_size(1024), "1 KB");
        assert_eq!(fmt_size(1025), "2 KB");
        assert_eq!(fmt_size(1_234_567), "1,206 KB");
    }

    #[test]
    fn short_size_format_for_progress_bar() {
        assert_eq!(fmt_size_short(0), "0 B");
        assert_eq!(fmt_size_short(999), "999 B");
        assert_eq!(fmt_size_short(1536), "1.5 KB");
        assert_eq!(fmt_size_short(734_003_200), "700 MB");
        assert_eq!(fmt_size_short(2_147_483_648), "2.0 GB");
    }

    #[test]
    fn kind_labels_in_both_languages() {
        use crate::core::i18n::{set_lang, Lang};
        let _g = crate::core::i18n::test_lock();
        let mut e = entry("a.txt", false, 1, 0o644);
        set_lang(Lang::En);
        assert_eq!(kind_of(&e), "Text document");
        e.name = "x.tar.gz".into();
        assert_eq!(kind_of(&e), "GZ archive");
        e.name = "run".into();
        e.permissions = Some(0o755);
        assert_eq!(kind_of(&e), "Executable file");
        e.is_dir = true;
        assert_eq!(kind_of(&e), "File folder");
        set_lang(Lang::Ko);
        assert_eq!(kind_of(&e), "파일 폴더");
    }

    #[test]
    fn history_push_and_back() {
        let mut s = SftpPanelState::default();
        s.push_history("/a");
        s.push_history("/a/b");
        s.push_history("/a/b"); // repeat is ignored
        assert_eq!(s.history, vec!["/a", "/a/b"]);
        assert_eq!(s.hist_pos, 1);
        // back: pending slot 0 and the matching listing arrives
        s.hist_pending = Some(0);
        s.push_history("/a");
        assert_eq!(s.hist_pos, 0);
        assert_eq!(s.history.len(), 2);
        // a fresh navigation from the middle truncates the forward history
        s.push_history("/c");
        assert_eq!(s.history, vec!["/a", "/c"]);
    }

    #[test]
    fn sorting_keeps_folders_first() {
        let entries = vec![
            entry("b.txt", false, 10, 0o644),
            entry("zdir", true, 0, 0o755),
            entry("a.txt", false, 5, 0o644),
        ];
        let mut st = ListState::default();
        let v = visible_sorted(&entries, st.sort, st.sort_desc, false, "");
        assert_eq!(v, vec![1, 2, 0]);
        st.sort = SortKey::Size;
        st.sort_desc = true;
        let v = visible_sorted(&entries, st.sort, st.sort_desc, false, "");
        assert_eq!(v, vec![1, 0, 2]);
        let v = visible_sorted(&entries, st.sort, st.sort_desc, false, "A.T");
        assert_eq!(v, vec![2]);
    }

    #[test]
    fn order_cache_is_reused_until_the_listing_changes() {
        let entries = vec![entry("b.txt", false, 10, 0o644), entry("a.txt", false, 5, 0o644)];
        let mut st = ListState::default();
        st.ensure_order(&entries, false, "", 1);
        let first = st.order.clone();
        assert_eq!(first, vec![1, 0]);
        st.order[0] = 99;
        st.ensure_order(&entries, false, "", 1);
        assert_eq!(st.order[0], 99);
        st.ensure_order(&entries, false, "", 2);
        assert_eq!(st.order, first);
    }

    #[test]
    fn flattened_tree_lists_only_expanded_branches() {
        let mut tree = BTreeMap::new();
        tree.insert(
            "/".into(),
            vec![
                TreeChild {
                    name: "a".into(),
                    path: "/a".into(),
                },
                TreeChild {
                    name: "b".into(),
                    path: "/b".into(),
                },
            ],
        );
        tree.insert(
            "/a".into(),
            vec![TreeChild {
                name: "c".into(),
                path: "/a/c".into(),
            }],
        );
        let mut expanded = HashSet::new();
        expanded.insert("/".into());
        let closed = flatten_tree(&tree, &expanded, false);
        assert_eq!(
            closed.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["/", "a", "b"]
        );
        expanded.insert("/a".into());
        let open = flatten_tree(&tree, &expanded, false);
        assert_eq!(
            open.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["/", "a", "c", "b"]
        );
    }

    #[test]
    fn paste_cut_moves_and_copy_duplicates() {
        let mut s = SftpPanelState {
            cwd: "/dst".into(),
            ..Default::default()
        };
        let a = entry("a.txt", false, 1, 0o644);
        s.clip = Some((vec![a.clone()], true));
        let mut actions = Vec::new();
        s.paste(&mut actions);
        assert!(
            matches!(&actions[0], SftpAction::Cmd(SftpCmd::Move(p)) if p == &vec![("/x/a.txt".to_string(), "/dst/a.txt".to_string())])
        );
        assert!(s.clip.is_none());
        // copy into the same folder appends " - copy"
        s.cwd = "/x".into();
        s.clip = Some((vec![a], false));
        let mut actions = Vec::new();
        s.paste(&mut actions);
        assert!(matches!(&actions[0], SftpAction::Cmd(SftpCmd::Copy { to, .. }) if to == "/x/a - copy.txt"));
        assert!(s.clip.is_some());
    }

    #[test]
    fn move_into_skips_self_and_parent() {
        let mut s = SftpPanelState::default();
        let dir = entry("d", true, 0, 0o755);
        let mut actions = Vec::new();
        // moving a folder into itself or into its own parent is a no-op
        s.move_into(std::slice::from_ref(&dir), "/x/d", &mut actions);
        s.move_into(std::slice::from_ref(&dir), "/x", &mut actions);
        assert!(actions.is_empty());
        s.move_into(&[dir], "/y", &mut actions);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn local_dir_listing() {
        let dir = std::env::temp_dir().join(format!("ruxt-local-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("f.txt"), b"hello").unwrap();
        let v = read_local_dir(&dir).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].is_dir && v[0].name == "sub");
        assert!(v[1].is_regular && v[1].size == 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
