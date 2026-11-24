//! Compact desktop Material 3 chrome.
//!
//! Surfaces stack by luminance, not by a 1px stroke: the terminal is the darkest, the sidebar
//! and toolbars sit one step up, and the active tab / hover state layer sit one step above that.
//! Secondary buttons are ghost controls: no fill and no stroke until the pointer is over them.
//! Colours still come from the active [`crate::core::theme::ThemeSpec`]; this module only
//! decides how those colours are used.

use egui::{
    Color32, CornerRadius, Frame, Margin, Painter, Rect, Response, RichText, Sense, Stroke, TextStyle, Ui, Vec2,
};

/// Corner radius of ordinary controls (buttons, fields, rows).
pub const RADIUS: u8 = 6;
/// Corner radius of primary action buttons.
pub const RADIUS_PILL: u8 = RADIUS;
/// Shared row height for the toolbar, tabs and status bar so their baselines match.
pub const ROW: f32 = 24.0;

/// Phosphor glyphs mixed into button labels. The font is installed in `fonts::install`.
pub mod icon {
    pub use egui_phosphor::regular::{
        ARROWS_LEFT_RIGHT, BROADCAST, COMPUTER_TOWER, DOTS_THREE_VERTICAL, FOLDER, GEAR, KEY, LOCK, MAGNIFYING_GLASS,
        PLUS, QUESTION, SHIELD, TERMINAL_WINDOW, X,
    };
}

/// Connected / idle status colours. Drawn as dots, not as a second text style.
pub const CONNECTED: Color32 = Color32::from_rgb(0x3d, 0xdc, 0x84);
pub const IDLE: Color32 = Color32::from_rgb(0x8b, 0x93, 0x88);

/// Hover (and pressed) state layer. M3 uses a translucent scrim, not a new solid fill.
pub fn state_layer(dark: bool, pressed: bool) -> Color32 {
    if dark {
        Color32::from_white_alpha(if pressed { 36 } else { 20 })
    } else {
        Color32::from_black_alpha(if pressed { 28 } else { 14 })
    }
}

/// Text colour that reads on `bg`.
pub fn on_color(bg: Color32) -> Color32 {
    let dark = Color32::from_rgb(0x14, 0x1a, 0x13);
    let lum = |c: Color32| {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    };
    let l = lum(bg);
    let vs_white = 1.05 / (l + 0.05);
    let vs_dark = (l + 0.05) / (lum(dark) + 0.05);
    if vs_dark > vs_white {
        dark
    } else {
        Color32::WHITE
    }
}

/// Install M3 widget chrome on top of a theme's colours.
///
/// Button frames lose their 1px `bg_stroke`. Regions separate by the surface stack already
/// stored on the visuals: `extreme_bg` (terminal / fields) < `panel_fill` (bars) < `faint_bg`
/// (active tab). Hover uses [`state_layer`] via `weak_bg_fill`, which is what egui buttons paint.
pub fn chrome(mut v: egui::Visuals) -> egui::Visuals {
    let radius = CornerRadius::same(RADIUS);
    let hover = state_layer(v.dark_mode, false);
    let pressed = state_layer(v.dark_mode, true);
    let w = &mut v.widgets;
    for slot in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        slot.bg_stroke = Stroke::NONE;
        slot.corner_radius = radius;
        slot.expansion = 0.0;
    }
    w.noninteractive.corner_radius = radius;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.hovered.weak_bg_fill = hover;
    w.active.weak_bg_fill = pressed;
    w.open.weak_bg_fill = hover;
    w.open.bg_stroke = Stroke::NONE;
    // Windows separate by fill and shadow. A hairline here reads as the old bordered chrome.
    v.window_stroke = Stroke::NONE;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = radius;
    v.button_frame = true;
    v
}

/// Sidebar, toolbar and status bar. One step above the terminal.
pub fn chrome_frame(visuals: &egui::Visuals) -> Frame {
    Frame::new()
        .fill(visuals.panel_fill)
        .stroke(Stroke::NONE)
        .inner_margin(Margin::symmetric(6, 2))
}

/// Status bar: same surface as the toolbar, shorter vertically.
pub fn status_frame(visuals: &egui::Visuals) -> Frame {
    Frame::new()
        .fill(visuals.panel_fill)
        .stroke(Stroke::NONE)
        .inner_margin(Margin::symmetric(8, 0))
}

/// The terminal surface. Pass the theme's terminal background so the cells and the
/// surrounding panel are the same colour.
pub fn terminal_frame(term_bg: Color32) -> Frame {
    Frame::new().fill(term_bg).stroke(Stroke::NONE)
}

/// Icon plus label. An empty label is an icon button; an empty icon is plain text.
pub fn label(icon: &str, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        icon.to_string()
    } else if icon.is_empty() {
        text.to_string()
    } else {
        format!("{icon}  {text}")
    }
}

/// Drop a leading "+" when the icon already supplies it.
pub fn without_plus(text: &str) -> &str {
    text.trim().trim_start_matches('+').trim()
}

/// Secondary action: no resting fill or stroke. The theme paints the hover state layer.
pub fn ghost(ui: &mut Ui, icon: &str, text: &str) -> Response {
    ui.add(
        egui::Button::new(label(icon, text))
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(RADIUS)),
    )
}

/// Square icon button, used for the new-tab `+` and the help mark.
pub fn icon_button(ui: &mut Ui, icon: &str) -> Response {
    let h = ui.spacing().interact_size.y;
    ui.add(
        egui::Button::new(icon)
            .min_size(Vec2::splat(h))
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(RADIUS)),
    )
}

/// The one filled action on the toolbar. Pill, accent, no stroke.
pub fn accent(ui: &mut Ui, icon: &str, text: &str) -> Response {
    let fill = ui.visuals().selection.stroke.color;
    ui.add(
        egui::Button::new(RichText::new(label(icon, text)).strong().color(on_color(fill)))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(RADIUS_PILL)),
    )
}

/// Filled circle used as a connection mark. `h` is the row it should sit in.
pub fn paint_dot(painter: &Painter, rect: Rect, color: Color32) {
    painter.circle_filled(rect.center(), 3.5, color);
}

/// Status-bar dot, allocated in the same row height as [`status_text`].
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let h = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, h), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_dot(ui.painter(), rect, color);
    }
}

fn status_galley(ui: &Ui, text: &str, color: Color32) -> std::sync::Arc<egui::Galley> {
    let font = TextStyle::Body.resolve(ui.style());
    ui.painter().layout_no_wrap(text.to_owned(), font, color)
}

/// One status-bar label. Every caller uses Body in a `ROW`-tall slot, so the baseline matches.
pub fn status_text(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = status_galley(ui, text, color);
    let h = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x, h), Sense::hover());
    if ui.is_rect_visible(rect) {
        let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, color);
    }
    resp
}

/// Mini usage meter: dim label, 36×5 rounded bar, fixed-width bright value.
const METER_BAR: Vec2 = Vec2::new(36.0, 5.0);
const METER_GAP: f32 = 6.0;
/// Space on each side of the hairline between meters.
const METER_RULE: f32 = 8.0;

/// Bar colour from usage. Below 60% stays on the theme accent; 60–85% is the warning
/// tone; 85% and above is the error tone.
pub fn usage_color(visuals: &egui::Visuals, pct: f32) -> Color32 {
    if pct >= 85.0 {
        visuals.error_fg_color
    } else if pct >= 60.0 {
        visuals.warn_fg_color
    } else {
        visuals.selection.stroke.color
    }
}

/// CPU / MEM / disk cluster. Each value sits in a slot as wide as `100%`, so a change of
/// digits does not move the next meter. A hairline separates the items.
///
/// The whole cluster is one click target and shares the status-bar row height.
pub fn status_usages(ui: &mut Ui, items: &[(&str, f32)]) -> Response {
    let h = ui.spacing().interact_size.y;
    let font = TextStyle::Body.resolve(ui.style());
    let weak = ui.visuals().weak_text_color();
    let strong = ui.visuals().strong_text_color();
    // Slot width is "100%", not the current text, so 9% and 100% occupy the same space.
    let value_w = ui
        .painter()
        .layout_no_wrap("100%".to_owned(), font.clone(), strong)
        .size()
        .x;
    let labels: Vec<_> = items
        .iter()
        .map(|(label, _)| ui.painter().layout_no_wrap((*label).to_owned(), font.clone(), weak))
        .collect();
    let meter_w = |label_w: f32| label_w + METER_GAP + METER_BAR.x + METER_GAP + value_w;
    let rules = items.len().saturating_sub(1) as f32 * (METER_RULE + 1.0 + METER_RULE);
    let width = labels.iter().map(|g| meter_w(g.size().x)).sum::<f32>() + rules;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width.max(0.0), h), Sense::click());
    if ui.is_rect_visible(rect) && !items.is_empty() {
        if resp.hovered() || resp.highlighted() {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(RADIUS),
                state_layer(ui.visuals().dark_mode, resp.is_pointer_button_down_on()),
            );
        }
        let track = if ui.visuals().dark_mode {
            Color32::from_white_alpha(32)
        } else {
            Color32::from_black_alpha(28)
        };
        let mut x = rect.left();
        for (i, ((_, pct), galley)) in items.iter().zip(labels).enumerate() {
            if i > 0 {
                x += METER_RULE;
                let mid_y = rect.center().y;
                ui.painter().line_segment(
                    [egui::pos2(x, mid_y - 5.0), egui::pos2(x, mid_y + 5.0)],
                    Stroke::new(1.0, weak.gamma_multiply(0.55)),
                );
                x += 1.0 + METER_RULE;
            }
            let label_w = galley.size().x;
            let text_y = rect.center().y - galley.size().y / 2.0;
            ui.painter().galley(egui::pos2(x, text_y), galley, weak);
            x += label_w + METER_GAP;
            let bar = Rect::from_center_size(egui::pos2(x + METER_BAR.x * 0.5, rect.center().y), METER_BAR);
            ui.painter().rect_filled(bar, CornerRadius::same(2), track);
            let clamped = pct.clamp(0.0, 100.0);
            let fill_w = METER_BAR.x * (clamped / 100.0);
            if fill_w > 0.5 {
                let fill = Rect::from_min_size(bar.min, Vec2::new(fill_w, METER_BAR.y));
                let radius = (fill_w * 0.5).min(2.0) as u8;
                ui.painter().rect_filled(
                    fill,
                    CornerRadius::same(radius.max(1)),
                    usage_color(ui.visuals(), clamped),
                );
            }
            x += METER_BAR.x + METER_GAP;
            let value = ui
                .painter()
                .layout_no_wrap(format!("{clamped:.0}%"), font.clone(), strong);
            let value_y = rect.center().y - value.size().y / 2.0;
            ui.painter()
                .galley(egui::pos2(x + value_w - value.size().x, value_y), value, strong);
            x += value_w;
        }
    }
    resp
}

/// Clickable status-bar item with the same metrics as [`status_text`].
pub fn status_button(ui: &mut Ui, text: &str) -> Response {
    let color = ui.visuals().text_color();
    let galley = status_galley(ui, text, color);
    let h = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 8.0, h), Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() || resp.highlighted() {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(RADIUS),
                state_layer(ui.visuals().dark_mode, resp.is_pointer_button_down_on()),
            );
        }
        let pos = egui::pos2(rect.left() + 4.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, color);
    }
    resp
}

/// Session (or recent) row. The highlight uses the full width the sidebar offers.
pub fn session_row(ui: &mut Ui, selected: bool, name: &str) -> Response {
    let h = ui.spacing().interact_size.y;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            ui.visuals().faint_bg_color
        } else if resp.hovered() || resp.highlighted() {
            state_layer(ui.visuals().dark_mode, resp.is_pointer_button_down_on())
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), fill);
        }
        let color = if selected {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().text_color()
        };
        let font = TextStyle::Body.resolve(ui.style());
        let galley = ui
            .painter()
            .layout_no_wrap(label(icon::COMPUTER_TOWER, name), font, color);
        let pos = egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, color);
    }
    resp
}

/// One terminal tab. The close mark lives inside the tab, on the right.
///
/// The active tab rounds only its top corners so the square bottom sits flush on the terminal.
/// Returns `(response, close_clicked)`.
pub fn tab(ui: &mut Ui, title: &str, active: bool, connected: bool) -> (Response, bool) {
    let font = TextStyle::Body.resolve(ui.style());
    let color = if active {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    let galley = ui.painter().layout_no_wrap(title.to_owned(), font, color);
    let h = ui.spacing().interact_size.y;
    let close_w = 16.0;
    let width = 8.0 + 12.0 + 6.0 + galley.size().x + 4.0 + close_w + 4.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, h), Sense::click());
    let close_rect = Rect::from_min_size(
        egui::pos2(rect.right() - 4.0 - close_w, rect.top()),
        Vec2::new(close_w, rect.height()),
    );
    let pointer = resp
        .interact_pointer_pos()
        .or_else(|| ui.input(|i| i.pointer.hover_pos()));
    let over_close = pointer.is_some_and(|p| close_rect.contains(p));
    let close_clicked = resp.clicked() && over_close;
    if ui.is_rect_visible(rect) {
        let radius = if active {
            CornerRadius {
                nw: RADIUS,
                ne: RADIUS,
                sw: 0,
                se: 0,
            }
        } else {
            CornerRadius::same(RADIUS)
        };
        let fill = if active {
            ui.visuals().faint_bg_color
        } else if resp.hovered() || resp.highlighted() {
            state_layer(ui.visuals().dark_mode, false)
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, radius, fill);
        }
        paint_dot(
            ui.painter(),
            Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, rect.top()),
                Vec2::new(12.0, rect.height()),
            ),
            if connected { CONNECTED } else { IDLE },
        );
        let text_pos = egui::pos2(rect.left() + 8.0 + 12.0 + 4.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(text_pos, galley, color);
        let x_color = if over_close {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        ui.painter().text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            icon::X,
            TextStyle::Body.resolve(ui.style()),
            x_color,
        );
    }
    (resp, close_clicked)
}
