//! Terminal emulator state shared between the I/O task and the UI thread.
//!
//! The VT emulation is `alacritty_terminal` (the same engine as the Alacritty emulator);
//! we only feed it bytes and render its grid.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Osc52, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Cap on search results kept in memory (a one-letter query on a huge scrollback).
pub const MAX_SEARCH_MATCHES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub line: i32,
    pub col_start: usize,
    pub col_end: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

#[derive(Default)]
pub struct ListenerShared {
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub bell: bool,
    /// Text a remote program asked us to put on the clipboard (OSC 52), if allowed.
    pub clipboard_store: Option<String>,
}

/// Receives events from the emulator (title changes, bells, replies that must be
/// written back to the pty such as cursor-position reports).
#[derive(Clone)]
pub struct Listener {
    ctx: egui::Context,
    pty_writer: mpsc::UnboundedSender<Vec<u8>>,
    shared: Arc<Mutex<ListenerShared>>,
    allow_osc52: bool,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(s) => {
                let _ = self.pty_writer.send(s.into_bytes());
            }
            Event::Title(t) => {
                let clean = sanitize_title(&t);
                let mut g = self.shared.lock();
                if let Some(cwd) = extract_cwd_from_title(&clean) {
                    g.cwd = Some(cwd);
                }
                g.title = Some(clean);
            }
            Event::ResetTitle => {
                self.shared.lock().title = None;
            }
            Event::Bell => {
                self.shared.lock().bell = true;
            }
            Event::ClipboardStore(_, text) => {
                if self.allow_osc52 {
                    self.shared.lock().clipboard_store = Some(text);
                }
            }
            Event::Wakeup | Event::MouseCursorDirty => {
                self.ctx.request_repaint();
            }
            _ => {}
        }
    }
}

/// Unicode bidi controls and other invisible formatting characters that can visually reorder
/// or hide text (e.g. `evil\u{202E}txt.exe` renders as `evilexe.txt`).
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'   // zero-width space/joiners, LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
        | '\u{061C}'              // ALM
        | '\u{FEFF}'              // BOM / ZWNBSP
        | '\u{2028}' | '\u{2029}' // line/paragraph separators
        | '\u{180E}'
    )
}

/// Strip control characters (and bidi/invisible formatting) so a hostile remote can't smuggle
/// escapes through window titles, prompts or file names.
pub fn sanitize_title(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .take(200)
        .collect()
}

/// Extract a working directory from standard shell window titles such as
/// `user@host: /var/log`, `user@host:~/projects`, or `[user@host /etc]`.
pub fn extract_cwd_from_title(title: &str) -> Option<String> {
    let t = title.trim();
    if let Some((_, rhs)) = t.rsplit_once(':') {
        let p = rhs.trim();
        if (p.starts_with('/') || p == "~" || p.starts_with("~/")) && !p.contains('\0') {
            return Some(p.to_string());
        }
    }
    if t.starts_with('/') || t == "~" || t.starts_with("~/") {
        return Some(t.to_string());
    }
    None
}

/// Extract an OSC 7 working directory (`\x1b]7;file://host/path\x07` or `\x1b]7;file://host/path\x1b\\`).
#[cfg(test)]
pub fn extract_cwd_from_osc7(bytes: &[u8]) -> Option<String> {
    scan_osc7(bytes).0
}

/// Largest OSC 7 fragment we keep between chunks while waiting for its terminator.
const OSC7_CARRY_MAX: usize = 4096;

/// Scan for OSC 7 sequences. Returns the last complete working directory found, plus the
/// offset of an unterminated `\x1b]7;` prefix (if any) so the caller can carry that tail into
/// the next chunk: shells emit the sequence in one `write`, but SSH packetisation may split it.
fn scan_osc7(bytes: &[u8]) -> (Option<String>, Option<usize>) {
    const PREFIX: &[u8] = b"\x1b]7;";
    let mut i = 0;
    let mut found = None;
    while i + PREFIX.len() <= bytes.len() {
        if &bytes[i..i + PREFIX.len()] == PREFIX {
            let start = i + PREFIX.len();
            let mut end = None;
            let mut next_i = bytes.len();
            let mut j = start;
            while j < bytes.len() {
                if bytes[j] == 0x07 {
                    end = Some(j);
                    next_i = j + 1;
                    break;
                }
                if j + 1 < bytes.len() && bytes[j] == 0x1b && bytes[j + 1] == b'\\' {
                    end = Some(j);
                    next_i = j + 2;
                    break;
                }
                j += 1;
            }
            let Some(end) = end else {
                // Unterminated: only worth carrying if it is still short enough to be genuine.
                return (found, (bytes.len() - i <= OSC7_CARRY_MAX).then_some(i));
            };
            if let Ok(raw) = std::str::from_utf8(&bytes[start..end]) {
                let raw = raw.trim();
                let path_part = if let Some(rest) = raw.strip_prefix("file://") {
                    rest.find('/').map(|idx| &rest[idx..])
                } else if raw.starts_with('/') {
                    Some(raw)
                } else {
                    None
                };
                if let Some(p) = path_part {
                    let decoded = percent_decode(p);
                    let clean = sanitize_title(&decoded);
                    if clean.starts_with('/') {
                        found = Some(clean);
                    }
                }
            }
            i = next_i;
        } else {
            i += 1;
        }
    }
    // A chunk may end in the middle of the 4-byte prefix itself ("\x1b", "\x1b]", "\x1b]7").
    let tail_start = bytes.len().saturating_sub(PREFIX.len() - 1);
    for start in tail_start..bytes.len() {
        let tail = &bytes[start..];
        if PREFIX.starts_with(tail) {
            return (found, Some(start));
        }
    }
    (found, None)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16)) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct TermState {
    pub term: Term<Listener>,
    parser: Processor,
    pub size: Size,
    pub palette: Palette,
    pub shared: Arc<Mutex<ListenerShared>>,
    /// Set once the backing session has ended; the UI renders it read-only.
    pub closed: bool,
    pub exit_message: Option<String>,
    /// Bumped on every change to the grid (new bytes, resize); lets the UI cache work such as
    /// search results until something actually changed.
    pub generation: u64,
    /// Tail of the previous chunk holding an unterminated OSC 7 sequence.
    osc7_carry: Vec<u8>,
}

impl TermState {
    /// Returns the state plus the receiver for bytes the emulator wants written to the pty.
    #[cfg(test)]
    pub fn new(
        ctx: egui::Context,
        size: Size,
        scrollback: usize,
        allow_osc52: bool,
    ) -> (Arc<Mutex<TermState>>, mpsc::UnboundedReceiver<Vec<u8>>) {
        Self::with_palette(ctx, size, scrollback, allow_osc52, Palette::matcha())
    }

    pub fn with_palette(
        ctx: egui::Context,
        size: Size,
        scrollback: usize,
        allow_osc52: bool,
        palette: Palette,
    ) -> (Arc<Mutex<TermState>>, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Mutex::new(ListenerShared::default()));
        let listener = Listener {
            ctx,
            pty_writer: tx,
            shared: shared.clone(),
            allow_osc52,
        };
        let config = TermConfig {
            scrolling_history: scrollback.clamp(100, super::config::MAX_SCROLLBACK),
            osc52: if allow_osc52 { Osc52::OnlyCopy } else { Osc52::Disabled },
            ..Default::default()
        };
        let term = Term::new(config, &size, listener);
        let st = TermState {
            term,
            parser: Processor::new(),
            size,
            palette,
            shared,
            closed: false,
            exit_message: None,
            generation: 0,
            osc7_carry: Vec::new(),
        };
        (Arc::new(Mutex::new(st)), rx)
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.generation = self.generation.wrapping_add(1);
        let found = if self.osc7_carry.is_empty() {
            let (found, carry_from) = scan_osc7(bytes);
            if let Some(off) = carry_from {
                self.osc7_carry.extend_from_slice(&bytes[off..]);
            }
            found
        } else {
            self.osc7_carry.extend_from_slice(bytes);
            let (found, carry_from) = scan_osc7(&self.osc7_carry);
            match carry_from {
                Some(off) => {
                    self.osc7_carry.drain(..off);
                }
                None => self.osc7_carry.clear(),
            }
            found
        };
        if let Some(cwd) = found {
            self.shared.lock().cwd = Some(cwd);
        }
        self.parser.advance(&mut self.term, bytes);
    }

    /// Print a local status line (not from the remote) in a distinct colour.
    pub fn print_local(&mut self, msg: &str) {
        let s = format!("\r\n\x1b[1;33m[Ruxt] {}\x1b[0m\r\n", sanitize_title(msg));
        self.advance(s.as_bytes());
    }

    pub fn resize(&mut self, size: Size) -> bool {
        if size.cols == self.size.cols && size.rows == self.size.rows {
            return false;
        }
        if size.cols < 2 || size.rows < 1 {
            return false;
        }
        self.size = size;
        self.term.resize(size);
        self.generation = self.generation.wrapping_add(1);
        true
    }

    pub fn scroll(&mut self, s: Scroll) {
        self.term.scroll_display(s);
    }

    pub fn take_title(&self) -> Option<String> {
        self.shared.lock().title.clone()
    }

    pub fn take_cwd(&self) -> Option<String> {
        self.shared.lock().cwd.take()
    }

    pub fn take_bell(&self) -> bool {
        std::mem::take(&mut self.shared.lock().bell)
    }

    pub fn take_clipboard_store(&self) -> Option<String> {
        self.shared.lock().clipboard_store.take()
    }

    /// Search the entire scrollback + screen grid (case-insensitive) and return all matches,
    /// sorted by (line, column). Stops after `MAX_SEARCH_MATCHES` (the UI shows "N+").
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        let needle: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let grid = self.term.grid();
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        let cols = grid.columns();
        let mut matches = Vec::new();
        let mut line_chars: Vec<(usize, usize, char)> = Vec::with_capacity(cols);

        for line in top..=bottom {
            line_chars.clear();
            for col in 0..cols {
                let cell = &grid[Point::new(Line(line), Column(col))];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                for lc in ch.to_lowercase() {
                    line_chars.push((col, col + width, lc));
                }
            }
            if line_chars.len() < needle.len() {
                continue;
            }
            let mut i = 0;
            while i + needle.len() <= line_chars.len() {
                if line_chars[i..i + needle.len()]
                    .iter()
                    .zip(&needle)
                    .all(|((_, _, a), b)| a == b)
                {
                    let col_start = line_chars[i].0;
                    let col_end = line_chars[i + needle.len() - 1].1;
                    matches.push(SearchMatch {
                        line,
                        col_start,
                        col_end,
                    });
                    if matches.len() >= MAX_SEARCH_MATCHES {
                        return matches;
                    }
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        matches
    }

    /// Absolute index (scrollback + screen) of the cursor's row, and the cursor column.
    pub fn cursor_abs(&self) -> (usize, usize) {
        let grid = self.term.grid();
        let p = grid.cursor.point;
        ((p.line.0 + grid.history_size() as i32).max(0) as usize, p.column.0)
    }

    /// Text of the input line that started at `(abs_line, col)`, read back from the grid:
    /// from that cell to the cursor (`to_cursor`) or to the end of the cursor's row, following
    /// soft-wrapped rows. `None` once the cursor has left that row chain (a new prompt was
    /// printed, the screen was cleared, the row scrolled out), which tells the caller the
    /// line it was tracking is gone.
    pub fn line_text_from(&self, abs_line: usize, col: usize, to_cursor: bool) -> Option<String> {
        let grid = self.term.grid();
        let history = grid.history_size();
        let cursor = grid.cursor.point;
        let cur_abs = cursor.line.0 + history as i32;
        if cur_abs < 0 {
            return None;
        }
        let cur_abs = cur_abs as usize;
        if abs_line > cur_abs || cur_abs - abs_line > 64 {
            return None;
        }
        let cols = grid.columns();
        if cols == 0 {
            return None;
        }
        let mut out = String::new();
        for abs in abs_line..=cur_abs {
            let line = Line(abs as i32 - history as i32);
            if line.0 < grid.topmost_line().0 {
                return None;
            }
            let row = &grid[line];
            if abs < cur_abs && !row[Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
                return None;
            }
            let start = if abs == abs_line { col.min(cols) } else { 0 };
            let end = if abs == cur_abs && to_cursor {
                cursor.column.0.min(cols)
            } else {
                cols
            };
            for c in start..end {
                let cell = &row[Column(c)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                out.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
        }
        Some(out)
    }

    /// Scroll the terminal viewport so that `line` is visible near the vertical center.
    pub fn scroll_to_line(&mut self, line: i32) {
        let grid = self.term.grid();
        let history = grid.history_size() as i32;
        let current = grid.display_offset() as i32;
        let half_rows = (self.size.rows as i32) / 2;
        let target = (half_rows - line).clamp(0, history);
        let delta = target - current;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }
}

// ---------------------------------------------------------------------------
// Colour palettes
// ---------------------------------------------------------------------------

const fn rgb(v: u32) -> Rgb {
    Rgb {
        r: ((v >> 16) & 0xff) as u8,
        g: ((v >> 8) & 0xff) as u8,
        b: (v & 0xff) as u8,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub ansi: [Rgb; 16],
}

impl Palette {
    /// Dark "matcha": deep green-black background, matcha-cream text, tea-green cursor.
    pub const fn matcha() -> Self {
        Self {
            fg: rgb(0xd9e3cf),
            bg: rgb(0x1a2218),
            cursor: rgb(0xa8c66c),
            ansi: [
                rgb(0x2b3528),
                rgb(0xd7696a),
                rgb(0x8fbf6a),
                rgb(0xd9c468),
                rgb(0x6fa7c7),
                rgb(0xb48ab8),
                rgb(0x6fbfb0),
                rgb(0xc5d1bb),
                rgb(0x5d6b58),
                rgb(0xe68a8b),
                rgb(0xa9d77f),
                rgb(0xebd87f),
                rgb(0x8cc1dd),
                rgb(0xc9a3cd),
                rgb(0x8fd4c6),
                rgb(0xe8efe0),
            ],
        }
    }
}

fn indexed(p: &Palette, i: u8) -> Rgb {
    match i {
        0..=15 => p.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let f = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb {
                r: f(r),
                g: f(g),
                b: f(b),
            }
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb {
        r: (c.r as u16 * 2 / 3) as u8,
        g: (c.g as u16 * 2 / 3) as u8,
        b: (c.b as u16 * 2 / 3) as u8,
    }
}

/// Resolve an emulator colour to RGB, honouring OSC 4/10/11 overrides stored in the term.
pub fn resolve_color(term: &Term<Listener>, p: &Palette, color: Color) -> Rgb {
    let colors = term.colors();
    match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(i) => colors[i as usize].unwrap_or_else(|| indexed(p, i)),
        Color::Named(n) => {
            if let Some(rgb) = colors[n] {
                return rgb;
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => p.fg,
                NamedColor::Cursor => p.cursor,
                NamedColor::Background => p.bg,
                NamedColor::DimForeground => dim(p.fg),
                NamedColor::DimBlack => dim(p.ansi[0]),
                NamedColor::DimRed => dim(p.ansi[1]),
                NamedColor::DimGreen => dim(p.ansi[2]),
                NamedColor::DimYellow => dim(p.ansi[3]),
                NamedColor::DimBlue => dim(p.ansi[4]),
                NamedColor::DimMagenta => dim(p.ansi[5]),
                NamedColor::DimCyan => dim(p.ansi[6]),
                NamedColor::DimWhite => dim(p.ansi[7]),
                other => {
                    let idx = other as usize;
                    if idx < 16 {
                        p.ansi[idx]
                    } else {
                        p.fg
                    }
                }
            }
        }
    }
}
