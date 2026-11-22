//! egui widget that renders an `alacritty_terminal` grid and translates input into VT bytes.
//!
//! The terminal mutex is held only while the grid is *read* into plain draw lists; all
//! painting happens after it is released so the SSH reader task is never blocked by layout.

use crate::core::i18n::tr;
use crate::core::term::{resolve_color, SearchMatch, Size, TermState, MAX_SEARCH_MATCHES};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use egui::{
    Align2, Color32, Event, EventFilter, FontId, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A window-resize drag produces a new size every frame; the grid is only reflowed once the
/// size has been stable for this long.
const RESIZE_SETTLE: Duration = Duration::from_millis(60);

#[derive(Default)]
pub struct TermOutput {
    pub input: Vec<u8>,
    pub resized: Option<Size>,
    pub copy: Option<String>,
    pub paste: bool,
    pub paste_text: Option<String>,
    pub zoom: i8,
    pub bell: bool,
    /// Accept completion candidate `i`: the app sends the part beyond the typed prefix.
    pub accept: Option<usize>,
    /// Move the highlighted completion candidate by this many entries.
    pub cycle: i32,
    /// Hide the completions for the current input line.
    pub dismiss: bool,
}

/// Completion candidates shown at most in the popup.
pub const MAX_CANDIDATES: usize = 8;

pub struct TermView {
    id: egui::Id,
    cell: Vec2,
    scroll_accum: f32,
    zoom_accum: f32,
    drag_anchor: Option<Point>,
    mouse_pressed: Option<u8>,
    last_mouse_cell: Option<(usize, usize)>,
    pending_size: Option<(Size, Instant)>,
    created: Instant,
    ime_preedit: String,
    pub search_open: bool,
    pub search_query: String,
    search_focus_next: bool,
    search_matches: Vec<SearchMatch>,
    /// Matches grouped by grid line for O(1) lookup while painting.
    search_rows: HashMap<i32, Vec<(usize, usize)>>,
    search_idx: usize,
    /// (query, grid generation) the current matches were computed for.
    search_cached: Option<(String, u64)>,
    /// Set by the search bar when the query changed; the next frame recomputes and jumps.
    search_dirty: bool,
    /// Command completions for the current input line (full commands), set by the app
    /// before every frame; `cand_idx` is the highlighted one.
    pub candidates: Vec<String>,
    pub cand_idx: usize,
    /// Ghost text drawn after the cursor: `candidates[cand_idx]` minus the typed prefix.
    pub ghost: Option<String>,
}

/// Everything the painter needs, captured while the terminal lock is held.
struct Frame {
    bg_default: Color32,
    cursor_color: Color32,
    fg_default: Color32,
    bg_rects: Vec<(Rect, Color32)>,
    glyphs: Vec<(usize, usize, char, Color32, Flags, bool)>,
    cursor: Option<(usize, usize, CursorShape, char)>,
    history_size: usize,
    display_offset: usize,
    closed: bool,
}

impl TermView {
    /// The widget id that owns keyboard focus while the terminal is active.
    pub fn id(&self) -> egui::Id {
        self.id
    }

    pub fn new(id_source: impl std::hash::Hash + std::fmt::Debug) -> Self {
        Self {
            id: egui::Id::unique(("termview", id_source)),
            cell: Vec2::new(8.0, 16.0),
            scroll_accum: 0.0,
            zoom_accum: 0.0,
            drag_anchor: None,
            mouse_pressed: None,
            last_mouse_cell: None,
            pending_size: None,
            created: Instant::now(),
            ime_preedit: String::new(),
            search_open: false,
            search_query: String::new(),
            search_focus_next: false,
            search_matches: Vec::new(),
            search_rows: HashMap::new(),
            search_idx: 0,
            search_cached: None,
            search_dirty: false,
            candidates: Vec::new(),
            cand_idx: 0,
            ghost: None,
        }
    }

    pub fn toggle_search(&mut self) {
        self.search_open = !self.search_open;
        if self.search_open {
            self.search_focus_next = true;
        }
    }

    fn clear_search(&mut self) {
        self.search_matches.clear();
        self.search_rows.clear();
        self.search_cached = None;
        self.search_idx = 0;
    }

    fn set_matches(&mut self, matches: Vec<SearchMatch>) {
        self.search_rows.clear();
        for m in &matches {
            self.search_rows
                .entry(m.line)
                .or_default()
                .push((m.col_start, m.col_end));
        }
        self.search_matches = matches;
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        term: &Mutex<TermState>,
        font_size: f32,
        want_focus: bool,
        copy_on_select: bool,
    ) -> TermOutput {
        let mut out = TermOutput::default();
        let font_id = FontId::monospace(font_size);
        let (cw, ch) = ui
            .ctx()
            .fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)));
        self.cell = Vec2::new(cw.max(1.0), (ch * 1.02).max(1.0));

        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let response = ui.interact(rect, self.id, Sense::click_and_drag());

        // ---- geometry / resize (debounced) ---------------------------------
        let cols = ((rect.width() - 4.0) / self.cell.x).floor().max(2.0) as u16;
        let rows = ((rect.height() - 4.0) / self.cell.y).floor().max(1.0) as u16;
        let origin = rect.min + Vec2::new(2.0, 2.0);
        let wanted = Size { cols, rows };

        let mut t = term.lock();
        if wanted.cols != t.size.cols || wanted.rows != t.size.rows {
            match self.pending_size {
                Some((p, since)) if p.cols == wanted.cols && p.rows == wanted.rows => {
                    if since.elapsed() >= RESIZE_SETTLE {
                        if t.resize(wanted) {
                            out.resized = Some(wanted);
                        }
                        self.pending_size = None;
                    } else {
                        ui.ctx().request_repaint_after(RESIZE_SETTLE);
                    }
                }
                _ => {
                    self.pending_size = Some((wanted, Instant::now()));
                    ui.ctx().request_repaint_after(RESIZE_SETTLE);
                }
            }
        } else {
            self.pending_size = None;
        }
        // Until the pending size settles, paint with the grid's real size.
        let (cols, rows) = (t.size.cols, t.size.rows);
        out.bell = t.take_bell();

        // ---- focus ---------------------------------------------------------
        // A press (not just a completed click) takes focus: selecting text by dragging never
        // produces a click, so without this the following Ctrl+C went to whatever widget had
        // focus before (e.g. the SFTP list, which then "copied" files) instead of the terminal.
        if response.clicked()
            || response.secondary_clicked()
            || response.middle_clicked()
            || response.drag_started()
            || response.is_pointer_button_down_on()
        {
            response.request_focus();
        }
        if want_focus && ui.memory(|m| m.focused().is_none()) {
            response.request_focus();
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        let mode = *t.term.mode();
        let mouse_mode = mode.intersects(TermMode::MOUSE_MODE);
        let sgr_mouse = mode.contains(TermMode::SGR_MOUSE);
        let modifiers = ui.input(|i| i.modifiers);
        let shift = modifiers.shift;

        // ---- keyboard ------------------------------------------------------
        if focused {
            let events = ui.input(|i| i.events.clone());
            // An IME commit can surface both as `Ime(Commit)` and as `Text` in the same frame;
            // only that pairing is collapsed. Two identical `Text` events (typing "ll" during a
            // slow frame) must both go through.
            let mut last_commit: Option<String> = None;
            let mut last_text: Option<String> = None;
            for ev in events {
                match ev {
                    Event::Text(s) => {
                        self.ime_preedit.clear();
                        if last_commit.as_deref() == Some(s.as_str()) {
                            last_commit = None;
                            continue;
                        }
                        out.input.extend_from_slice(s.as_bytes());
                        t.scroll(Scroll::Bottom);
                        last_text = Some(s);
                    }
                    Event::Ime(egui::ImeEvent::Preedit { text: s, .. }) => {
                        self.ime_preedit = s;
                    }
                    Event::Ime(egui::ImeEvent::Commit(s)) => {
                        self.ime_preedit.clear();
                        if s.is_empty() {
                            continue;
                        }
                        if last_text.as_deref() == Some(s.as_str()) {
                            last_text = None;
                            continue;
                        }
                        out.input.extend_from_slice(s.as_bytes());
                        t.scroll(Scroll::Bottom);
                        last_commit = Some(s);
                    }
                    #[allow(deprecated)]
                    Event::Ime(egui::ImeEvent::Disabled | egui::ImeEvent::Enabled) => {
                        self.ime_preedit.clear();
                    }
                    Event::Paste(s) => {
                        out.paste_text = Some(s);
                    }
                    Event::Copy => {
                        out.copy = t.term.selection_to_string();
                    }
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        self.handle_key(key, modifiers, &mut t, &mut out, mode);
                    }
                    _ => {}
                }
            }
        }

        // ---- mouse wheel -----------------------------------------------------
        if response.hovered() {
            let (wheel_lines, zoom_steps) = ui.input(|i| {
                let mut lines = 0.0f32;
                let mut zoom = 0.0f32;
                for ev in &i.events {
                    if let Event::MouseWheel {
                        unit, delta, modifiers, ..
                    } = ev
                    {
                        let ctrl = modifiers.ctrl || (modifiers.mac_cmd && cfg!(target_os = "macos"));
                        let amount = match unit {
                            MouseWheelUnit::Point => delta.y / self.cell.y,
                            MouseWheelUnit::Line => delta.y,
                            MouseWheelUnit::Page => delta.y * rows as f32,
                        };
                        if ctrl {
                            // Ctrl+wheel: font zoom, one step per notch regardless of unit.
                            zoom += amount.signum()
                                * if unit == &MouseWheelUnit::Point {
                                    (amount.abs() / 3.0).clamp(0.34, 1.0)
                                } else {
                                    1.0
                                };
                        } else {
                            lines += amount;
                        }
                    }
                }
                (lines, zoom)
            });
            if zoom_steps != 0.0 {
                self.zoom_accum += zoom_steps;
                let n = self.zoom_accum.trunc() as i8;
                self.zoom_accum -= n as f32;
                out.zoom = out.zoom.saturating_add(n);
            }
            if wheel_lines != 0.0 {
                self.scroll_accum += wheel_lines * 3.0;
                let n = self.scroll_accum.trunc() as i32;
                self.scroll_accum -= n as f32;
                if n != 0 {
                    if mouse_mode && !shift {
                        let button = if n > 0 { 64 } else { 65 };
                        if let Some(p) = response.hover_pos() {
                            let (col, row) = self.cell_at(origin, p);
                            for _ in 0..n.abs() {
                                out.input
                                    .extend_from_slice(&mouse_report(sgr_mouse, button, col, row, false, modifiers));
                            }
                        }
                    } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                        let app = mode.contains(TermMode::APP_CURSOR);
                        let seq: &[u8] = match (n > 0, app) {
                            (true, true) => b"\x1bOA",
                            (true, false) => b"\x1b[A",
                            (false, true) => b"\x1bOB",
                            (false, false) => b"\x1b[B",
                        };
                        for _ in 0..n.abs() {
                            out.input.extend_from_slice(seq);
                        }
                    } else {
                        t.scroll(Scroll::Delta(n));
                    }
                }
            }
        }

        // ---- mouse buttons / selection / Ctrl+click URLs -------------------
        let display_offset = t.term.grid().display_offset();
        let ctrl_held = modifiers.ctrl || (modifiers.mac_cmd && cfg!(target_os = "macos"));
        let hovered_url = if ctrl_held {
            response.hover_pos().and_then(|pos| {
                let (col, row) = self.cell_at(origin, pos);
                if col < cols as usize && row < rows as usize {
                    let line = row as i32 - display_offset as i32;
                    url_at_row(&t, line, col).map(|(cs, ce, url)| (row, cs, ce, url))
                } else {
                    None
                }
            })
        } else {
            None
        };
        if hovered_url.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let to_point = |pos: Pos2| -> (Point, Side) {
            let (col, row) = self.cell_at(origin, pos);
            let col = col.min(cols as usize - 1);
            let row = row.min(rows as usize - 1);
            let fx = (pos.x - origin.x) / self.cell.x - col as f32;
            let side = if fx < 0.5 { Side::Left } else { Side::Right };
            (Point::new(Line(row as i32 - display_offset as i32), Column(col)), side)
        };

        if mouse_mode && !shift {
            // Report clicks to the application in the encoding it asked for (SGR 1006 or X10).
            if let Some(pos) = response.interact_pointer_pos().or(response.hover_pos()) {
                let (col, row) = self.cell_at(origin, pos);
                let pressed = ui.input(|i| {
                    if i.pointer.button_pressed(PointerButton::Primary) {
                        Some(0)
                    } else if i.pointer.button_pressed(PointerButton::Middle) {
                        Some(1)
                    } else if i.pointer.button_pressed(PointerButton::Secondary) {
                        Some(2)
                    } else {
                        None
                    }
                });
                if let Some(b) = pressed {
                    self.mouse_pressed = Some(b);
                    self.last_mouse_cell = Some((col, row));
                    out.input
                        .extend_from_slice(&mouse_report(sgr_mouse, b, col, row, false, modifiers));
                } else if self.last_mouse_cell != Some((col, row)) {
                    // Motion is reported once per cell change: 1002 while a button is held,
                    // 1003 for every movement (button code 3 = none).
                    let report = match self.mouse_pressed {
                        Some(b) if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) => Some(b + 32),
                        None if mode.contains(TermMode::MOUSE_MOTION) => Some(3 + 32),
                        _ => None,
                    };
                    if let Some(b) = report {
                        self.last_mouse_cell = Some((col, row));
                        out.input
                            .extend_from_slice(&mouse_report(sgr_mouse, b, col, row, false, modifiers));
                    }
                }
            }
            if ui.input(|i| i.pointer.any_released()) {
                if let Some(b) = self.mouse_pressed.take() {
                    if let Some(pos) = response.hover_pos().or(response.interact_pointer_pos()) {
                        let (col, row) = self.cell_at(origin, pos);
                        out.input
                            .extend_from_slice(&mouse_report(sgr_mouse, b, col, row, true, modifiers));
                    }
                }
            }
        } else {
            if response.drag_started_by(PointerButton::Primary) {
                // Anchor at the press position: a drag is only recognised after the pointer has
                // moved a few pixels, so the current position would skip the first character.
                if let Some(pos) = ui
                    .input(|i| i.pointer.press_origin())
                    .or(response.interact_pointer_pos())
                {
                    let (p, side) = to_point(pos);
                    self.drag_anchor = Some(p);
                    t.term.selection = Some(Selection::new(SelectionType::Simple, p, side));
                }
            } else if response.dragged_by(PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    // Dragging past the top/bottom edge scrolls the view so a selection can
                    // extend into the scrollback.
                    if pos.y < rect.min.y {
                        t.scroll(Scroll::Delta(1));
                        ui.ctx().request_repaint_after(Duration::from_millis(40));
                    } else if pos.y > rect.max.y {
                        t.scroll(Scroll::Delta(-1));
                        ui.ctx().request_repaint_after(Duration::from_millis(40));
                    }
                    let display_offset = t.term.grid().display_offset();
                    let (col, row) = self.cell_at(origin, pos);
                    let col = col.min(cols as usize - 1);
                    let row = row.min(rows as usize - 1);
                    let fx = (pos.x - origin.x) / self.cell.x - col as f32;
                    let side = if fx < 0.5 { Side::Left } else { Side::Right };
                    let p = Point::new(Line(row as i32 - display_offset as i32), Column(col));
                    if let Some(sel) = t.term.selection.as_mut() {
                        sel.update(p, side);
                    }
                }
            } else if response.drag_stopped_by(PointerButton::Primary) {
                self.drag_anchor = None;
                if copy_on_select {
                    if let Some(s) = t.term.selection_to_string() {
                        if !s.is_empty() {
                            out.copy = Some(s);
                        }
                    }
                }
            } else if response.triple_clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (p, side) = to_point(pos);
                    t.term.selection = Some(Selection::new(SelectionType::Lines, p, side));
                }
            } else if response.double_clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (p, side) = to_point(pos);
                    t.term.selection = Some(Selection::new(SelectionType::Semantic, p, side));
                }
            } else if response.clicked_by(PointerButton::Primary) {
                if let Some((_, _, _, ref url)) = hovered_url {
                    let _ = open::that(url);
                } else {
                    t.term.selection = None;
                }
            }
            if response.clicked_by(PointerButton::Secondary) || response.clicked_by(PointerButton::Middle) {
                out.paste = true;
            }
        }

        // ---- search matches (recomputed only when the query or the grid changed) ----------
        let mut jump_after_search = false;
        if self.search_open && !self.search_query.is_empty() {
            let key = (self.search_query.clone(), t.generation);
            if self.search_dirty || self.search_cached.as_ref() != Some(&key) {
                let matches = t.search(&self.search_query);
                self.set_matches(matches);
                self.search_cached = Some(key);
                if self.search_dirty {
                    self.search_idx = self.search_matches.len().saturating_sub(1);
                    jump_after_search = true;
                    self.search_dirty = false;
                } else if !self.search_matches.is_empty() && self.search_idx >= self.search_matches.len() {
                    self.search_idx = self.search_matches.len() - 1;
                }
            }
        } else if !self.search_matches.is_empty() || self.search_cached.is_some() {
            self.clear_search();
        }
        if jump_after_search {
            if let Some(m) = self.search_matches.get(self.search_idx).copied() {
                t.scroll_to_line(m.line);
            }
        }
        let active_match = if self.search_open {
            self.search_matches.get(self.search_idx).copied()
        } else {
            None
        };

        // ---- read the grid into draw lists -------------------------------------
        let frame = self.capture(&t, origin, mode, active_match);
        let url_color = rgb(resolve_color(&t.term, &t.palette, Color::Named(NamedColor::Cursor)));
        drop(t);

        // ---- paint (lock released) ---------------------------------------------
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, frame.bg_default);
        for (r, c) in &frame.bg_rects {
            painter.rect_filled(*r, 0.0, *c);
        }

        let mut run = String::new();
        let mut run_start: Option<(usize, usize)> = None; // (row, col)
        let mut run_color = Color32::WHITE;
        let mut run_flags = Flags::empty();
        let cell = self.cell;
        let flush_run = |run: &mut String, run_start: &mut Option<(usize, usize)>, color: Color32, flags: Flags| {
            if let Some((row, col)) = run_start.take() {
                if !run.trim().is_empty() {
                    let pos = origin + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                    painter.text(pos, Align2::LEFT_TOP, run.as_str(), font_id.clone(), color);
                }
                let n = run.chars().count() as f32;
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    let y = origin.y + (row as f32 + 0.92) * cell.y;
                    let x0 = origin.x + col as f32 * cell.x;
                    painter.line_segment(
                        [Pos2::new(x0, y), Pos2::new(x0 + n * cell.x, y)],
                        Stroke::new(1.0, color),
                    );
                }
                if flags.contains(Flags::STRIKEOUT) {
                    let y = origin.y + (row as f32 + 0.5) * cell.y;
                    let x0 = origin.x + col as f32 * cell.x;
                    painter.line_segment(
                        [Pos2::new(x0, y), Pos2::new(x0 + n * cell.x, y)],
                        Stroke::new(1.0, color),
                    );
                }
                run.clear();
            }
        };

        for (row, col, c, color, flags, wide) in &frame.glyphs {
            let (row, col, c, color, flags, wide) = (*row, *col, *c, *color, *flags, *wide);
            let deco = flags & (Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
            let contiguous = match run_start {
                Some((r, sc)) => r == row && sc + run.chars().count() == col,
                None => false,
            };
            if wide || !c.is_ascii() || !contiguous || color != run_color || deco != run_flags {
                flush_run(&mut run, &mut run_start, run_color, run_flags);
            }
            if wide || !c.is_ascii() {
                let pos = origin + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                // Box drawing, blocks and Braille are drawn from the cell so pictures join
                // up whatever the font has (see `glyphs`).
                if c != ' ' && !super::glyphs::draw(&painter, Rect::from_min_size(pos, cell), c, color) {
                    painter.text(pos, Align2::LEFT_TOP, c, font_id.clone(), color);
                }
                if deco.intersects(Flags::ALL_UNDERLINES) {
                    let y = pos.y + 0.92 * cell.y;
                    let w = if wide { 2.0 } else { 1.0 } * cell.x;
                    painter.line_segment([Pos2::new(pos.x, y), Pos2::new(pos.x + w, y)], Stroke::new(1.0, color));
                }
                continue;
            }
            if run_start.is_none() {
                run_start = Some((row, col));
                run_color = color;
                run_flags = deco;
            }
            run.push(c);
        }
        flush_run(&mut run, &mut run_start, run_color, run_flags);

        // Underline a Ctrl+hovered URL.
        if let Some((urow, cs, ce, _)) = &hovered_url {
            let y = origin.y + (*urow as f32 + 0.94) * self.cell.y;
            let x0 = origin.x + *cs as f32 * self.cell.x;
            let x1 = origin.x + *ce as f32 * self.cell.x;
            painter.line_segment([Pos2::new(x0, y), Pos2::new(x1, y)], Stroke::new(1.5, url_color));
        }

        // Ghost text of the highlighted completion, starting in the cursor cell (as fish and
        // zsh-autosuggestions do), in a dim colour. Hidden while an IME composition is shown.
        let mut ghost_first = None;
        if let (Some(ghost), Some((crow, ccol, _, _))) = (self.ghost.as_deref(), frame.cursor) {
            if focused && !frame.closed && self.ime_preedit.is_empty() && ccol < cols as usize {
                let room = cols as usize - ccol;
                let shown: String = ghost.chars().take(room).collect();
                ghost_first = shown.chars().next();
                let pos = origin + Vec2::new(ccol as f32 * cell.x, crow as f32 * cell.y);
                let [r, g, b, _] = frame.fg_default.to_array();
                painter.text(
                    pos,
                    Align2::LEFT_TOP,
                    shown,
                    font_id.clone(),
                    Color32::from_rgba_unmultiplied(r, g, b, 110),
                );
            }
        }
        if focused && !frame.closed && self.candidates.len() > 1 {
            if let Some((crow, ccol, _, _)) = frame.cursor {
                let anchor = origin + Vec2::new(ccol as f32 * self.cell.x, (crow as f32 + 1.0) * self.cell.y + 2.0);
                self.render_candidates(ui, rect, anchor, &mut out);
            }
        }

        // Cursor & IME preedit overlay.
        if let Some((crow, ccol, shape, under)) = frame.cursor {
            let r = self.cells_rect(origin, crow, ccol, ccol + 1);
            if !self.ime_preedit.is_empty() {
                let cols_w = preedit_cells(&self.ime_preedit).max(2);
                let pr = self.cells_rect(origin, crow, ccol, ccol + cols_w);
                painter.rect_filled(pr, 2.0, frame.bg_default);
                painter.text(
                    pr.min,
                    Align2::LEFT_TOP,
                    &self.ime_preedit,
                    font_id.clone(),
                    frame.fg_default,
                );
                painter.line_segment(
                    [Pos2::new(pr.min.x, pr.max.y - 1.0), Pos2::new(pr.max.x, pr.max.y - 1.0)],
                    Stroke::new(1.5, frame.cursor_color),
                );
            } else {
                let blink_on = (self.created.elapsed().as_millis() / 530).is_multiple_of(2);
                match (focused, shape) {
                    (_, CursorShape::Hidden) => {}
                    (false, _) => {
                        painter.rect_stroke(r, 0.0, Stroke::new(1.0, frame.cursor_color), StrokeKind::Inside);
                    }
                    (true, CursorShape::Block) => {
                        if blink_on {
                            painter.rect_filled(r, 0.0, frame.cursor_color);
                            // redraw the glyph under the cursor in the background colour; on an
                            // empty cell that is the first character of the ghost text
                            let under = match (under, ghost_first) {
                                (' ' | '\0', Some(g)) => g,
                                _ => under,
                            };
                            if under != ' '
                                && under != '\0'
                                && !super::glyphs::draw(&painter, r, under, frame.bg_default)
                            {
                                painter.text(r.min, Align2::LEFT_TOP, under, font_id.clone(), frame.bg_default);
                            }
                        }
                    }
                    (true, CursorShape::Underline) => {
                        painter.rect_filled(
                            Rect::from_min_max(Pos2::new(r.min.x, r.max.y - 2.0), r.max),
                            0.0,
                            frame.cursor_color,
                        );
                    }
                    (true, CursorShape::Beam) => {
                        painter.rect_filled(
                            Rect::from_min_max(r.min, Pos2::new(r.min.x + 2.0, r.max.y)),
                            0.0,
                            frame.cursor_color,
                        );
                    }
                    (true, CursorShape::HollowBlock) => {
                        painter.rect_stroke(r, 0.0, Stroke::new(1.0, frame.cursor_color), StrokeKind::Inside);
                    }
                }
                if focused {
                    ui.ctx().request_repaint_after(Duration::from_millis(530));
                }
            }
            // Tell the platform where the composition window belongs; without this the OS never
            // enables the IME for the terminal (egui only does it for its own TextEdit).
            if focused {
                ui.ctx().output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        purpose: egui::IMEPurpose::Terminal,
                        rect,
                        cursor_rect: r,
                        should_interrupt_composition: false,
                    })
                });
            }
        }

        // Subtle scrollbar thumb when scrollback history exists.
        let history_size = frame.history_size;
        let display_offset = frame.display_offset;
        if history_size > 0 {
            let total = (history_size + rows as usize).max(1) as f32;
            let track_h = (rect.height() - 4.0).max(16.0);
            let thumb_h = ((rows as f32 / total) * track_h).clamp(16.0, track_h);
            let top_frac = (history_size.saturating_sub(display_offset)) as f32 / history_size as f32;
            let thumb_y = rect.min.y + 2.0 + top_frac * (track_h - thumb_h);
            let thumb_rect = Rect::from_min_size(Pos2::new(rect.max.x - 5.0, thumb_y), Vec2::new(3.5, thumb_h));
            let thumb_col = if display_offset > 0 {
                Color32::from_rgba_unmultiplied(0xa8, 0xc6, 0x6c, 180)
            } else {
                Color32::from_rgba_unmultiplied(150, 160, 150, 80)
            };
            painter.rect_filled(thumb_rect, 2.0, thumb_col);
        }

        if display_offset > 0 && !self.search_open {
            let label = format!(" scrollback {} / {} ", display_offset, history_size);
            let pos = Pos2::new(rect.max.x - 10.0, rect.min.y + 4.0);
            let galley = painter.layout_no_wrap(label, FontId::proportional(12.0), Color32::BLACK);
            let r = Rect::from_min_size(Pos2::new(pos.x - galley.size().x, pos.y), galley.size());
            painter.rect_filled(r.expand(2.0), 3.0, Color32::from_rgb(0xf0, 0xd0, 0x60));
            painter.galley(r.min, galley, Color32::BLACK);
        }
        if frame.closed {
            let label = " session closed — press Enter to reconnect ";
            let galley = painter.layout_no_wrap(label.into(), FontId::proportional(13.0), Color32::WHITE);
            let r = Rect::from_min_size(
                Pos2::new(rect.center().x - galley.size().x / 2.0, rect.min.y + 6.0),
                galley.size(),
            );
            painter.rect_filled(r.expand(3.0), 4.0, Color32::from_rgb(0xa0, 0x30, 0x30));
            painter.galley(r.min, galley, Color32::WHITE);
        }

        if self.search_open {
            self.render_search_bar(ui, rect, term, &response);
        }
        out
    }

    /// Read the visible grid into draw lists. Runs with the terminal lock held and does no
    /// text layout, so it stays cheap.
    fn capture(&self, t: &TermState, origin: Pos2, mode: TermMode, active_match: Option<SearchMatch>) -> Frame {
        let bg_default_rgb = resolve_color(&t.term, &t.palette, Color::Named(NamedColor::Background));
        let bg_default = rgb(bg_default_rgb);
        let content = t.term.renderable_content();
        let display_offset = content.display_offset;
        let selection = content.selection;
        let cursor = content.cursor;
        let show_cursor = mode.contains(TermMode::SHOW_CURSOR) && !t.closed;
        let rows = t.size.rows;

        let mut bg_rects: Vec<(Rect, Color32)> = Vec::new();
        let mut bg_run: Option<(usize, usize, usize, Color32)> = None; // row, col_start, col_end, color
        let mut glyphs: Vec<(usize, usize, char, Color32, Flags, bool)> = Vec::new();
        let mut row_matches: Option<(i32, &[(usize, usize)])> = None;

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let row = (point.line.0 + display_offset as i32) as usize;
            let col = point.column.0;
            let flags = cell.flags;
            if flags.contains(Flags::WIDE_CHAR_SPACER) || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let mut fg = resolve_color(&t.term, &t.palette, cell.fg);
            let mut bg = resolve_color(&t.term, &t.palette, cell.bg);
            if flags.contains(Flags::BOLD) {
                if let Color::Named(n) = cell.fg {
                    let i = n as usize;
                    if i < 8 {
                        fg = resolve_color(&t.term, &t.palette, Color::Indexed((i + 8) as u8));
                    }
                } else if let Color::Indexed(i) = cell.fg {
                    if i < 8 {
                        fg = resolve_color(&t.term, &t.palette, Color::Indexed(i + 8));
                    }
                }
            }
            if flags.contains(Flags::DIM) {
                fg = Rgb {
                    r: fg.r / 3 * 2,
                    g: fg.g / 3 * 2,
                    b: fg.b / 3 * 2,
                };
            }
            let selected = selection.map(|s| s.contains(point)).unwrap_or(false);
            if flags.contains(Flags::INVERSE) != selected {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut search_hit = false;
            if !self.search_rows.is_empty() {
                if row_matches.map(|(l, _)| l) != Some(point.line.0) {
                    row_matches = Some((
                        point.line.0,
                        self.search_rows.get(&point.line.0).map(Vec::as_slice).unwrap_or(&[]),
                    ));
                }
                let in_match = row_matches
                    .map(|(_, ms)| ms.iter().any(|&(s, e)| col >= s && col < e))
                    .unwrap_or(false);
                if in_match {
                    let is_active =
                        active_match.is_some_and(|m| m.line == point.line.0 && col >= m.col_start && col < m.col_end);
                    bg = if is_active {
                        Rgb {
                            r: 0xf0,
                            g: 0x98,
                            b: 0x30,
                        }
                    } else {
                        Rgb {
                            r: 0xc8,
                            g: 0xb0,
                            b: 0x40,
                        }
                    };
                    fg = Rgb {
                        r: 0x10,
                        g: 0x10,
                        b: 0x10,
                    };
                    search_hit = true;
                }
            }
            let bgc = rgb(bg);
            let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            if bg != bg_default_rgb || selected || search_hit {
                match bg_run {
                    Some((r, s, e, c)) if r == row && e == col && c == bgc => {
                        bg_run = Some((r, s, col + width, c));
                    }
                    other => {
                        if let Some((r, s, e, c)) = other {
                            bg_rects.push((self.cells_rect(origin, r, s, e), c));
                        }
                        bg_run = Some((row, col, col + width, bgc));
                    }
                }
            } else if let Some((r, s, e, c)) = bg_run.take() {
                bg_rects.push((self.cells_rect(origin, r, s, e), c));
            }
            if !flags.contains(Flags::HIDDEN) && cell.c != ' ' && cell.c != '\0' {
                glyphs.push((row, col, cell.c, rgb(fg), flags, flags.contains(Flags::WIDE_CHAR)));
            } else if flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
                glyphs.push((row, col, ' ', rgb(fg), flags, false));
            }
        }
        if let Some((r, s, e, c)) = bg_run.take() {
            bg_rects.push((self.cells_rect(origin, r, s, e), c));
        }

        let cursor = if show_cursor {
            let crow = cursor.point.line.0 + display_offset as i32;
            if crow >= 0 && (crow as u16) < rows {
                let under = t.term.grid()[cursor.point].c;
                Some((crow as usize, cursor.point.column.0, cursor.shape, under))
            } else {
                None
            }
        } else {
            None
        };

        Frame {
            bg_default,
            cursor_color: rgb(resolve_color(&t.term, &t.palette, Color::Named(NamedColor::Cursor))),
            fg_default: rgb(resolve_color(&t.term, &t.palette, Color::Named(NamedColor::Foreground))),
            bg_rects,
            glyphs,
            cursor,
            history_size: t.term.grid().history_size(),
            display_offset,
            closed: t.closed,
        }
    }

    fn render_search_bar(&mut self, ui: &mut Ui, rect: Rect, term: &Mutex<TermState>, term_response: &egui::Response) {
        let pos = Pos2::new((rect.max.x - 350.0).max(rect.min.x + 8.0), rect.min.y + 6.0);
        let mut jump_to: Option<usize> = None;
        let mut close = false;

        egui::Area::new(self.id.with("search-bar"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::window(ui.style())
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .hint_text("Find…")
                                    .desired_width(150.0),
                            );
                            if self.search_focus_next {
                                r.request_focus();
                                self.search_focus_next = false;
                            }
                            if r.changed() {
                                self.search_dirty = true;
                            }
                            let enter = (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
                            let shift = ui.input(|i| i.modifiers.shift);
                            let esc = ui.input(|i| i.key_pressed(Key::Escape));
                            if esc {
                                close = true;
                            }

                            let total = self.search_matches.len();
                            let current = if total == 0 { 0 } else { self.search_idx + 1 };
                            let plus = if total >= MAX_SEARCH_MATCHES { "+" } else { "" };
                            ui.label(
                                egui::RichText::new(format!("{current}/{total}{plus}"))
                                    .small()
                                    .monospace(),
                            );
                            if ui
                                .add_enabled(total > 0, egui::Button::new("▲").small())
                                .on_hover_text("Previous match (Shift+Enter)")
                                .clicked()
                                || (enter && shift && total > 0)
                            {
                                jump_to = Some(if self.search_idx == 0 {
                                    total - 1
                                } else {
                                    self.search_idx - 1
                                });
                            }
                            if ui
                                .add_enabled(total > 0, egui::Button::new("▼").small())
                                .on_hover_text("Next match (Enter)")
                                .clicked()
                                || (enter && !shift && total > 0)
                            {
                                jump_to = Some((self.search_idx + 1) % total);
                            }
                            if ui.small_button("x").on_hover_text("Close (Esc)").clicked() {
                                close = true;
                            }
                        });
                    });
            });

        if let Some(idx) = jump_to {
            self.search_idx = idx;
            if let Some(m) = self.search_matches.get(idx).copied() {
                term.lock().scroll_to_line(m.line);
            }
        }
        if close {
            self.search_open = false;
            self.clear_search();
            term_response.request_focus();
        }
    }

    /// The completion popup under the cursor; clicking a row accepts it.
    fn render_candidates(&mut self, ui: &mut Ui, rect: Rect, anchor: Pos2, out: &mut TermOutput) {
        let row_h = self.cell.y.max(16.0) + 4.0;
        let est_h = self.candidates.len() as f32 * row_h + 28.0;
        let mut pos = anchor;
        if pos.y + est_h > rect.max.y {
            pos.y = (anchor.y - self.cell.y - 2.0 - est_h).max(rect.min.y);
        }
        pos.x = pos.x.min((rect.max.x - 320.0).max(rect.min.x));
        egui::Area::new(self.id.with("completions"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.set_min_width(220.0);
                        for (i, c) in self.candidates.iter().enumerate() {
                            let shown: String = c.chars().take(90).collect();
                            let r = ui.selectable_label(i == self.cand_idx, egui::RichText::new(shown).monospace());
                            if r.clicked() {
                                out.accept = Some(i);
                            }
                        }
                        ui.label(
                            egui::RichText::new(tr("→ accept · Ctrl+↑/↓ choose · Esc hide"))
                                .small()
                                .weak(),
                        );
                    });
            });
    }

    fn cell_at(&self, origin: Pos2, pos: Pos2) -> (usize, usize) {
        let col = ((pos.x - origin.x) / self.cell.x).floor().max(0.0) as usize;
        let row = ((pos.y - origin.y) / self.cell.y).floor().max(0.0) as usize;
        (col, row)
    }

    fn cells_rect(&self, origin: Pos2, row: usize, col_start: usize, col_end: usize) -> Rect {
        Rect::from_min_size(
            origin + Vec2::new(col_start as f32 * self.cell.x, row as f32 * self.cell.y),
            Vec2::new((col_end - col_start) as f32 * self.cell.x, self.cell.y),
        )
    }

    fn handle_key(&mut self, key: Key, m: Modifiers, t: &mut TermState, out: &mut TermOutput, mode: TermMode) {
        let ctrl = m.ctrl || (m.mac_cmd && cfg!(target_os = "macos"));
        // Command completion: → accepts the ghost text, Ctrl+↑/↓ moves through the popup,
        // Esc hides it for this line. Without a visible completion the keys go to the shell.
        let has_popup = self.candidates.len() > 1;
        match key {
            Key::ArrowRight if m.is_none() && self.ghost.is_some() => {
                out.accept = Some(self.cand_idx);
                return;
            }
            Key::ArrowDown if ctrl && !m.shift && !m.alt && has_popup => {
                out.cycle = 1;
                return;
            }
            Key::ArrowUp if ctrl && !m.shift && !m.alt && has_popup => {
                out.cycle = -1;
                return;
            }
            Key::Escape if m.is_none() && (self.ghost.is_some() || has_popup) => {
                out.dismiss = true;
                return;
            }
            _ => {}
        }
        // Clipboard / zoom / scrollback shortcuts.
        match key {
            Key::C if ctrl && m.shift => {
                out.copy = t.term.selection_to_string();
                return;
            }
            // Ctrl+C copies while something is selected (and drops the selection, so the next
            // Ctrl+C interrupts again); with nothing selected it stays ^C for the shell.
            Key::C if ctrl && !m.alt => {
                if let Some(s) = t.term.selection_to_string().filter(|s| !s.is_empty()) {
                    out.copy = Some(s);
                    t.term.selection = None;
                    return;
                }
            }
            Key::V if ctrl && !m.alt => {
                out.paste = true;
                return;
            }
            Key::F if ctrl && m.shift => {
                self.search_open = true;
                self.search_focus_next = true;
                return;
            }
            Key::Insert if ctrl => {
                out.copy = t.term.selection_to_string();
                return;
            }
            Key::V if ctrl && m.shift => {
                out.paste = true;
                return;
            }
            Key::Insert if m.shift => {
                out.paste = true;
                return;
            }
            Key::PageUp if m.shift => {
                t.scroll(Scroll::PageUp);
                return;
            }
            Key::PageDown if m.shift => {
                t.scroll(Scroll::PageDown);
                return;
            }
            Key::Home if m.shift => {
                t.scroll(Scroll::Top);
                return;
            }
            Key::End if m.shift => {
                t.scroll(Scroll::Bottom);
                return;
            }
            Key::Plus if ctrl => {
                out.zoom = 1;
                return;
            }
            Key::Equals if ctrl && !m.shift => {
                out.zoom = 1;
                return;
            }
            // Ctrl+- zooms out; Ctrl+Shift+- is Ctrl+_ (0x1f) for the application.
            Key::Minus if ctrl && !m.shift => {
                out.zoom = -1;
                return;
            }
            _ => {}
        }
        if let Some(bytes) = key_bytes(key, m, mode.contains(TermMode::APP_CURSOR)) {
            out.input.extend_from_slice(&bytes);
            t.scroll(Scroll::Bottom);
        }
    }
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Encode a mouse event the way xterm does: SGR (1006) when the application enabled it,
/// otherwise the legacy X10 byte encoding (coordinates clamp at 223).
pub fn mouse_report(sgr: bool, button: u8, col: usize, row: usize, release: bool, m: Modifiers) -> Vec<u8> {
    let mut b = button;
    if m.shift {
        b += 4;
    }
    if m.alt {
        b += 8;
    }
    if m.ctrl {
        b += 16;
    }
    if sgr {
        format!("\x1b[<{b};{};{}{}", col + 1, row + 1, if release { 'm' } else { 'M' }).into_bytes()
    } else {
        let b = if release { 3 } else { b };
        let cx = (col + 1).min(223) as u8 + 32;
        let cy = (row + 1).min(223) as u8 + 32;
        vec![0x1b, b'[', b'M', 32 + b, cx, cy]
    }
}

/// Approximate terminal cell width of an IME preedit string (CJK = 2 cells).
fn preedit_cells(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            let wide = (0x1100..=0x115F).contains(&u)
                || (0x2E80..=0xA4CF).contains(&u)
                || (0xAC00..=0xD7A3).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0xFE30..=0xFE4F).contains(&u)
                || (0xFF00..=0xFF60).contains(&u)
                || (0xFFE0..=0xFFE6).contains(&u)
                || (0x1F300..=0x1FAFF).contains(&u)
                || (0x20000..=0x3FFFD).contains(&u);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

/// Detect an `http://` or `https://` URL on `line` covering `target_col`.
fn url_at_row(t: &TermState, line: i32, target_col: usize) -> Option<(usize, usize, String)> {
    let grid = t.term.grid();
    if line < grid.topmost_line().0 || line > grid.bottommost_line().0 {
        return None;
    }
    let cols = grid.columns();
    let mut row_chars: Vec<(usize, usize, char)> = Vec::with_capacity(cols);
    for col in 0..cols {
        let cell = &grid[Point::new(Line(line), Column(col))];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let w = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        let ch = if cell.c == '\0' { ' ' } else { cell.c };
        row_chars.push((col, col + w, ch));
    }
    let text: String = row_chars.iter().map(|(_, _, c)| *c).collect();
    for scheme in ["https://", "http://"] {
        let mut search_from = 0;
        while let Some(pos) = text[search_from..].find(scheme) {
            let byte_start = search_from + pos;
            let rest = &text[byte_start..];
            let byte_len = rest
                .find(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '[' | ']'))
                .unwrap_or(rest.len());
            let raw_url = &rest[..byte_len];
            let trimmed = raw_url.trim_end_matches(['.', ',', ';', ':', ')', '!']);
            if trimmed.len() > scheme.len() {
                let char_start = text[..byte_start].chars().count();
                let char_end = char_start + trimmed.chars().count();
                if char_end <= row_chars.len() && char_start < char_end {
                    let col_start = row_chars[char_start].0;
                    let col_end = row_chars[char_end - 1].1;
                    if target_col >= col_start && target_col < col_end {
                        return Some((col_start, col_end, trimmed.to_string()));
                    }
                }
            }
            search_from = byte_start + scheme.len();
        }
    }
    None
}

fn mod_param(m: Modifiers) -> u8 {
    1 + (m.shift as u8) + ((m.alt as u8) << 1) + ((m.ctrl as u8) << 2)
}

/// Map a key press to the bytes a VT100/xterm would send.
pub fn key_bytes(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = m.ctrl;
    let alt = m.alt;
    let mp = mod_param(m);
    let csi = |fin: char| -> Vec<u8> {
        if mp > 1 {
            format!("\x1b[1;{mp}{fin}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if mp > 1 {
            format!("\x1b[{n};{mp}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let ss3 = |c: char| -> Vec<u8> {
        if mp > 1 {
            format!("\x1b[1;{mp}{c}").into_bytes()
        } else {
            format!("\x1bO{c}").into_bytes()
        }
    };
    let b = match key {
        Key::Enter => {
            if alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        Key::Backspace => {
            if ctrl {
                b"\x08".to_vec()
            } else if alt {
                b"\x1b\x7f".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        Key::Tab => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        Key::Escape => b"\x1b".to_vec(),
        Key::Space => {
            if ctrl {
                vec![0]
            } else {
                return None; // Text event covers it
            }
        }
        Key::ArrowUp => csi('A'),
        Key::ArrowDown => csi('B'),
        Key::ArrowRight => csi('C'),
        Key::ArrowLeft => csi('D'),
        Key::Home => csi('H'),
        Key::End => csi('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => ss3('P'),
        Key::F2 => ss3('Q'),
        Key::F3 => ss3('R'),
        Key::F4 => ss3('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::OpenBracket if ctrl => vec![0x1b],
        Key::Backslash if ctrl => vec![0x1c],
        Key::CloseBracket if ctrl => vec![0x1d],
        Key::Minus if ctrl => vec![0x1f],
        _ => {
            // Letters: only when a modifier makes them non-text.
            if let Some(ch) = key_letter(key) {
                // On Windows, AltGr arrives as Ctrl+Alt and the layout produces a `Text` event
                // for it (e.g. AltGr+Q = `@` on German keyboards); sending ESC+^Q as well would
                // corrupt the input, so Ctrl+Alt+letter is left to the Text event there.
                if ctrl && alt && cfg!(windows) {
                    return None;
                }
                if ctrl {
                    let c = ch.to_ascii_lowercase() as u8 - b'a' + 1;
                    if alt {
                        vec![0x1b, c]
                    } else {
                        vec![c]
                    }
                } else if alt {
                    let c = if m.shift {
                        ch.to_ascii_uppercase()
                    } else {
                        ch.to_ascii_lowercase()
                    };
                    vec![0x1b, c as u8]
                } else {
                    return None;
                }
            } else if let Some(d) = key_digit(key) {
                if alt && !ctrl {
                    vec![0x1b, d as u8]
                } else {
                    return None;
                }
            } else {
                let p = key_punct(key)?;
                // Alt+punctuation (Alt+. recalls the last argument in bash/zsh).
                if alt && !ctrl {
                    vec![0x1b, p as u8]
                } else {
                    return None;
                }
            }
        }
    };
    Some(b)
}

fn key_letter(key: Key) -> Option<char> {
    Some(match key {
        Key::A => 'a',
        Key::B => 'b',
        Key::C => 'c',
        Key::D => 'd',
        Key::E => 'e',
        Key::F => 'f',
        Key::G => 'g',
        Key::H => 'h',
        Key::I => 'i',
        Key::J => 'j',
        Key::K => 'k',
        Key::L => 'l',
        Key::M => 'm',
        Key::N => 'n',
        Key::O => 'o',
        Key::P => 'p',
        Key::Q => 'q',
        Key::R => 'r',
        Key::S => 's',
        Key::T => 't',
        Key::U => 'u',
        Key::V => 'v',
        Key::W => 'w',
        Key::X => 'x',
        Key::Y => 'y',
        Key::Z => 'z',
        _ => return None,
    })
}

fn key_digit(key: Key) -> Option<char> {
    Some(match key {
        Key::Num0 => '0',
        Key::Num1 => '1',
        Key::Num2 => '2',
        Key::Num3 => '3',
        Key::Num4 => '4',
        Key::Num5 => '5',
        Key::Num6 => '6',
        Key::Num7 => '7',
        Key::Num8 => '8',
        Key::Num9 => '9',
        _ => return None,
    })
}

fn key_punct(key: Key) -> Option<char> {
    Some(match key {
        Key::Period => '.',
        Key::Comma => ',',
        Key::Slash => '/',
        Key::Semicolon => ';',
        Key::Minus => '-',
        Key::Equals => '=',
        Key::Quote => '\'',
        Key::Backtick => '`',
        _ => return None,
    })
}

/// Sanitise pasted text: normalise newlines to CR and strip escape/control characters that
/// could be used to inject commands (clipboard-injection defence).
pub fn sanitize_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut s = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                s.push('\r');
            }
            '\n' => s.push('\r'),
            '\t' => s.push('\t'),
            c if c.is_control() => {}
            c => s.push(c),
        }
    }
    if bracketed {
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(s.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        s.into_bytes()
    }
}

#[cfg(test)]
#[path = "terminal_widget_tests.rs"]
mod tests;
