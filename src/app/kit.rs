//! Shared layout vocabulary, after Material 3: one spacing grid, tonal surface cards with
//! titles, dividers, a button hierarchy (filled / outlined / text) and windows and modals
//! that never grow past the viewport, their body scrolling instead.
//!
//! Colours always come from the active theme (`egui::Visuals`), never from constants here,
//! so custom themes restyle every surface.

use super::theme;
use egui::{
    Align, Color32, Context, CornerRadius, Frame, Id, Layout, Margin, Response, RichText, ScrollArea, Shadow, Stroke,
    Ui, Vec2, WidgetText,
};

/// Base unit of the spacing grid (dp). Dialogs stay on this grid; chrome uses `theme`.
pub const GRID: f32 = 8.0;
/// Corner radius of cards and windows / of controls.
pub const RADIUS_LARGE: u8 = 12;
pub const RADIUS_SMALL: u8 = theme::RADIUS;
/// Margin the viewport keeps around a window or modal.
const VIEWPORT_MARGIN: Vec2 = Vec2::new(32.0, 48.0);

/// Spacing, radii and shadows shared by every style (dark, light, custom).
pub fn apply_style(ctx: &Context) {
    ctx.all_styles_mut(|s| {
        // Compact desktop grid: tight enough for a terminal, not a phone layout.
        s.spacing.item_spacing = Vec2::new(6.0, 4.0);
        s.spacing.button_padding = Vec2::new(8.0, 2.0);
        s.spacing.window_margin = Margin::same(12);
        s.spacing.menu_margin = Margin::same(4);
        s.spacing.indent = 12.0;
        s.spacing.interact_size.y = theme::ROW;
        s.spacing.combo_width = 160.0;
        s.visuals.window_corner_radius = CornerRadius::same(RADIUS_LARGE);
        s.visuals.menu_corner_radius = CornerRadius::same(RADIUS_SMALL);
        s.visuals.window_shadow = Shadow {
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: Color32::from_black_alpha(96),
        };
        s.visuals.popup_shadow = Shadow {
            offset: [0, 3],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(72),
        };
        for w in [
            &mut s.visuals.widgets.noninteractive,
            &mut s.visuals.widgets.inactive,
            &mut s.visuals.widgets.hovered,
            &mut s.visuals.widgets.active,
            &mut s.visuals.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(RADIUS_SMALL);
        }
        s.visuals.striped = true;
    });
}

/// Largest size a floating window or modal may take.
pub fn max_window_size(ctx: &Context) -> Vec2 {
    let r = ctx.content_rect().size() - VIEWPORT_MARGIN;
    Vec2::new(r.x.max(240.0), r.y.max(160.0))
}

/// A floating window that is resizable, never larger than the viewport, and opens centred.
pub fn window<'a>(ctx: &Context, title: impl Into<WidgetText>, id: Id, default_width: f32) -> egui::Window<'a> {
    let max = max_window_size(ctx);
    egui::Window::new(title)
        .id(id)
        .collapsible(false)
        .resizable(true)
        .default_width(default_width.min(max.x))
        .max_size(max)
        .default_pos(ctx.content_rect().center() - Vec2::new(default_width.min(max.x) / 2.0, max.y * 0.4))
}

/// Scrolling body of a window: takes the height the viewport leaves, no more.
pub fn body<R>(ui: &mut Ui, ctx: &Context, add: impl FnOnce(&mut Ui) -> R) -> R {
    let max_h = (max_window_size(ctx).y - 110.0).max(100.0);
    ScrollArea::vertical()
        .id_salt(ui.scope_id().with("kit-body"))
        .max_height(max_h)
        .auto_shrink([false, true])
        .show(ui, add)
        .inner
}

/// Content of a modal dialog: a width bounded by the viewport and a scrolling body, so a
/// long message or a small window never hides the buttons.
pub fn modal_body<R>(ui: &mut Ui, ctx: &Context, desired_width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let max = max_window_size(ctx);
    let max_h = (max.y - 72.0).max(80.0);
    ui.set_width(desired_width.min(max.x));
    // A modal's area is sized from the previous frame, so a scroll area that only takes "what
    // is available" could never let the dialog grow; give it the viewport-bounded height.
    ui.set_max_height(max_h);
    ScrollArea::vertical()
        .id_salt(ui.scope_id().with("kit-modal"))
        .max_height(max_h)
        .auto_shrink([false, true])
        .show(ui, add)
        .inner
}

/// A titled tonal card: the surface-container of a settings page.
pub fn section<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let fill = ui.visuals().faint_bg_color;
    let r = Frame::new()
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(RADIUS_LARGE))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(15.0).strong());
            ui.add_space(GRID * 0.5);
            add(ui)
        })
        .inner;
    ui.add_space(GRID);
    r
}

/// One-line list item with a leading tonal marker, a headline and supporting text.
pub fn list_item(ui: &mut Ui, headline: &str, supporting: &str) {
    let accent = ui.visuals().selection.stroke.color;
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(GRID * 3.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), GRID * 1.5, accent.gamma_multiply(0.2));
        ui.painter().circle_filled(rect.center(), GRID * 0.5, accent);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(headline).strong());
            ui.label(RichText::new(supporting).weak());
        });
    });
    ui.add_space(GRID * 0.5);
}

/// Small tonal label in the accent colour (a version, a status).
pub fn chip(ui: &mut Ui, text: &str) -> Response {
    let accent = ui.visuals().selection.stroke.color;
    Frame::new()
        .fill(accent.gamma_multiply(0.18))
        .corner_radius(CornerRadius::same(RADIUS_SMALL))
        .inner_margin(Margin::symmetric(GRID as i8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).small().strong().color(accent)))
        .response
}

/// A keyboard key, drawn as an outlined keycap.
pub fn keycap(ui: &mut Ui, text: &str) -> Response {
    let v = ui.visuals();
    Frame::new()
        .fill(v.extreme_bg_color)
        .stroke(v.widgets.noninteractive.bg_stroke)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(6, 1))
        // Never wrap inside a keycap: a narrow row would otherwise stack "Shift+End" one
        // character per line. The surrounding row wraps whole keycaps instead.
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).monospace().small()).extend())
        })
        .response
}

/// Divider with breathing room.
pub fn divider(ui: &mut Ui) {
    ui.add_space(GRID * 0.5);
    ui.separator();
    ui.add_space(GRID * 0.5);
}

/// Supporting text under a control.
pub fn hint(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).small().weak());
}

/// Text colour that reads on `bg`.
fn on_color(bg: Color32) -> Color32 {
    theme::on_color(bg)
}

fn pill(ui: &mut Ui, text: impl Into<String>, fill: Color32) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text.into()).color(on_color(fill)).strong())
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(RADIUS_SMALL)),
    )
}

/// Filled button: the one confirming action of a dialog.
pub fn primary(ui: &mut Ui, text: impl Into<String>) -> Response {
    let accent = ui.visuals().selection.stroke.color;
    pill(ui, text, accent)
}

/// Filled button in the error colour for destructive confirmations.
pub fn danger(ui: &mut Ui, text: impl Into<String>) -> Response {
    let c = ui.visuals().error_fg_color;
    pill(ui, text, c)
}

/// Ghost button: secondary actions. No fill or stroke until the theme's hover state layer.
pub fn outlined(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.add(
        egui::Button::new(text.into())
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(RADIUS_SMALL)),
    )
}

/// Text button: the lowest emphasis (Cancel, Not now). Hover still shows the state layer.
pub fn text_button(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.add(
        egui::Button::new(text.into())
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(RADIUS_SMALL)),
    )
}

/// On/off control. Off is a ghost button (state layer only while hovered). On is a translucent
/// accent container, still without a stroke.
pub fn toggle(ui: &mut Ui, on: bool, text: impl Into<WidgetText>) -> Response {
    let pad = ui.spacing().button_padding;
    let accent = ui.visuals().selection.stroke.color;
    let fg = if on { accent } else { ui.visuals().text_color() };
    // The colour goes into the layout: a galley already carries its text colour, so the one
    // handed to the painter later would be ignored.
    let galley = text.into().color(fg).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let size = Vec2::new(
        pad.x * 2.0 + galley.size().x,
        (galley.size().y + pad.y * 2.0).max(ui.spacing().interact_size.y),
    );
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let label = galley.text().to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), on, &label));
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered() || resp.has_focus();
        let fill = if on {
            accent.gamma_multiply(if hovered { 0.30 } else { 0.18 })
        } else if hovered {
            theme::state_layer(ui.visuals().dark_mode, true)
        } else {
            Color32::TRANSPARENT
        };
        let p = ui.painter();
        p.rect(
            rect,
            CornerRadius::same(RADIUS_SMALL),
            fill,
            Stroke::NONE,
            egui::StrokeKind::Inside,
        );
        p.galley(
            egui::pos2(rect.left() + pad.x, rect.center().y - galley.size().y / 2.0),
            galley,
            fg,
        );
    }
    resp
}

/// Dialog action row, right-aligned: add the confirming action first so it ends up at the
/// far right, then the dismissing one.
pub fn actions<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.add_space(GRID * 0.5);
    // A horizontal row first, so the right-to-left layout gets one row's height instead of
    // everything left in the (scrolling) parent.
    ui.horizontal(|ui| ui.with_layout(Layout::right_to_left(Align::Center), add).inner)
        .inner
}
