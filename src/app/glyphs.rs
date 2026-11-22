//! Terminal glyphs drawn as shapes instead of taken from a font.
//!
//! Box drawing (U+2500–257F), block elements (U+2580–259F), Braille patterns (U+2800–28FF)
//! and the sextants of "Symbols for Legacy Computing" (U+1FB00–1FB3B) are what TUIs and
//! command-line animations build pictures from. Many monospace fonts lack some of them (no
//! Windows font has Braille), and a fallback font's glyphs rarely fill the cell exactly, so
//! the pictures break up or show the missing-glyph box. Drawing them from the cell rectangle
//! makes neighbouring cells join seamlessly whatever font is chosen, as Alacritty, kitty and
//! WezTerm do.

use egui::epaint::PathShape;
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

/// Weights of the four arms of U+2500..=U+256C: (up, right, down, left), with
/// 0 = none, 1 = light, 2 = heavy, 3 = double. Dashed lines are drawn solid.
#[rustfmt::skip]
const BOX: [[u8; 4]; 0x6D] = [
    [0, 1, 0, 1], [0, 2, 0, 2], [1, 0, 1, 0], [2, 0, 2, 0], [0, 1, 0, 1], [0, 2, 0, 2],
    [1, 0, 1, 0], [2, 0, 2, 0], [0, 1, 0, 1], [0, 2, 0, 2], [1, 0, 1, 0], [2, 0, 2, 0],
    [0, 1, 1, 0], [0, 2, 1, 0], [0, 1, 2, 0], [0, 2, 2, 0], [0, 0, 1, 1], [0, 0, 1, 2],
    [0, 0, 2, 1], [0, 0, 2, 2], [1, 1, 0, 0], [1, 2, 0, 0], [2, 1, 0, 0], [2, 2, 0, 0],
    [1, 0, 0, 1], [1, 0, 0, 2], [2, 0, 0, 1], [2, 0, 0, 2], [1, 1, 1, 0], [1, 2, 1, 0],
    [2, 1, 1, 0], [1, 1, 2, 0], [2, 1, 2, 0], [2, 2, 1, 0], [1, 2, 2, 0], [2, 2, 2, 0],
    [1, 0, 1, 1], [1, 0, 1, 2], [2, 0, 1, 1], [1, 0, 2, 1], [2, 0, 2, 1], [2, 0, 1, 2],
    [1, 0, 2, 2], [2, 0, 2, 2], [0, 1, 1, 1], [0, 1, 1, 2], [0, 2, 1, 1], [0, 2, 1, 2],
    [0, 1, 2, 1], [0, 1, 2, 2], [0, 2, 2, 1], [0, 2, 2, 2], [1, 1, 0, 1], [1, 1, 0, 2],
    [1, 2, 0, 1], [1, 2, 0, 2], [2, 1, 0, 1], [2, 1, 0, 2], [2, 2, 0, 1], [2, 2, 0, 2],
    [1, 1, 1, 1], [1, 1, 1, 2], [1, 2, 1, 1], [1, 2, 1, 2], [2, 1, 1, 1], [1, 1, 2, 1],
    [2, 1, 2, 1], [2, 1, 1, 2], [2, 2, 1, 1], [1, 1, 2, 2], [1, 2, 2, 1], [2, 2, 1, 2],
    [1, 2, 2, 2], [2, 1, 2, 2], [2, 2, 2, 1], [2, 2, 2, 2], [0, 1, 0, 1], [0, 2, 0, 2],
    [1, 0, 1, 0], [2, 0, 2, 0], [0, 3, 0, 3], [3, 0, 3, 0], [0, 3, 1, 0], [0, 1, 3, 0],
    [0, 3, 3, 0], [0, 0, 1, 3], [0, 0, 3, 1], [0, 0, 3, 3], [1, 3, 0, 0], [3, 1, 0, 0],
    [3, 3, 0, 0], [1, 0, 0, 3], [3, 0, 0, 1], [3, 0, 0, 3], [1, 3, 1, 0], [3, 1, 3, 0],
    [3, 3, 3, 0], [1, 0, 1, 3], [3, 0, 3, 1], [3, 0, 3, 3], [0, 3, 1, 3], [0, 1, 3, 1],
    [0, 3, 3, 3], [1, 3, 0, 3], [3, 1, 0, 1], [3, 3, 0, 3], [1, 3, 1, 3], [3, 1, 3, 1],
    [3, 3, 3, 3],
];

/// Does `c` belong to the ranges drawn here?
#[cfg(test)]
fn is_builtin(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259F | 0x2800..=0x28FF | 0x1FB00..=0x1FB3B)
}

/// Draw `c` into `cell` (one terminal cell) if it is one of the built-in glyphs. Returns
/// `false` for every other character, which the caller then draws from the font.
pub fn draw(painter: &Painter, cell: Rect, c: char, color: Color32) -> bool {
    match shapes(cell, c, color) {
        Some(v) => {
            painter.extend(v);
            true
        }
        None => false,
    }
}

/// The shapes for `c` in `cell`, or `None` if `c` is not a built-in glyph.
pub fn shapes(cell: Rect, c: char, color: Color32) -> Option<Vec<Shape>> {
    let cp = c as u32;
    let mut out = Vec::new();
    let painter = &mut out;
    match cp {
        0x2500..=0x256C => draw_lines(painter, cell, BOX[(cp - 0x2500) as usize], color),
        0x256D..=0x2570 => draw_arc(painter, cell, cp, color),
        0x2571..=0x2573 => draw_diagonals(painter, cell, cp, color),
        0x2574..=0x257F => {
            // Half lines: one or two arms, light or heavy.
            let arms: [u8; 4] = match cp {
                0x2574 => [0, 0, 0, 1],
                0x2575 => [1, 0, 0, 0],
                0x2576 => [0, 1, 0, 0],
                0x2577 => [0, 0, 1, 0],
                0x2578 => [0, 0, 0, 2],
                0x2579 => [2, 0, 0, 0],
                0x257A => [0, 2, 0, 0],
                0x257B => [0, 0, 2, 0],
                0x257C => [0, 2, 0, 1],
                0x257D => [1, 0, 2, 0],
                0x257E => [0, 1, 0, 2],
                _ => [2, 0, 1, 0],
            };
            draw_lines(painter, cell, arms, color);
        }
        0x2580..=0x259F => draw_block(painter, cell, cp, color),
        0x2800..=0x28FF => draw_braille(painter, cell, (cp - 0x2800) as u8, color),
        0x1FB00..=0x1FB3B => {
            // Sextants 1..=62 in order, skipping the two that already exist as ▌ and ▐.
            let mut p = cp - 0x1FB00 + 1;
            if p >= 21 {
                p += 1;
            }
            if p >= 42 {
                p += 1;
            }
            draw_sextant(painter, cell, p as u8, color);
        }
        _ => return None,
    }
    Some(out)
}

/// Thickness of a light line for this cell, in points.
fn light(cell: Rect) -> f32 {
    (cell.width() / 8.0).clamp(1.0, 3.0)
}

fn fill(painter: &mut Vec<Shape>, r: Rect, color: Color32) {
    painter.push(Shape::rect_filled(r, 0.0, color));
}

/// Straight lines from the centre to the edges. Light and heavy arms are filled rectangles
/// that overlap at the centre; double arms are two thin lines laid out so corners, tees and
/// crosses leave the right gaps (╔ ╦ ╬).
fn draw_lines(painter: &mut Vec<Shape>, cell: Rect, [up, right, down, left]: [u8; 4], color: Color32) {
    let c = cell.center();
    let lt = light(cell);
    let thick = |w: u8| match w {
        1 => lt,
        2 => lt * 2.0,
        _ => lt,
    };
    // Half the width of the widest crossing line, so single arms meet without a notch.
    let joint = [up, right, down, left]
        .iter()
        .map(|&w| if w == 3 { 3.0 * lt } else { thick(w) })
        .fold(0.0_f32, f32::max)
        / 2.0;
    let g = lt; // half the gap between the two strokes of a double line

    // Horizontal arms.
    for (w, to_right) in [(right, true), (left, false)] {
        if w == 0 {
            continue;
        }
        let edge = if to_right { cell.max.x } else { cell.min.x };
        if w == 3 {
            // Upper stroke: stops short of the centre if an arm goes up, else reaches across.
            for (dy, side_has_arm) in [(-g, up > 0), (g, down > 0)] {
                let inner = if side_has_arm { g } else { -g };
                let start = if to_right { c.x + inner } else { c.x - inner };
                let y = c.y + dy;
                fill(painter, span_x(start, edge, y, lt), color);
            }
        } else {
            let t = thick(w);
            let start = if to_right { c.x - joint } else { c.x + joint };
            fill(painter, span_x(start, edge, c.y, t), color);
        }
    }
    // Vertical arms.
    for (w, downwards) in [(down, true), (up, false)] {
        if w == 0 {
            continue;
        }
        let edge = if downwards { cell.max.y } else { cell.min.y };
        if w == 3 {
            for (dx, side_has_arm) in [(-g, left > 0), (g, right > 0)] {
                let inner = if side_has_arm { g } else { -g };
                let start = if downwards { c.y + inner } else { c.y - inner };
                let x = c.x + dx;
                fill(painter, span_y(start, edge, x, lt), color);
            }
        } else {
            let t = thick(w);
            let start = if downwards { c.y - joint } else { c.y + joint };
            fill(painter, span_y(start, edge, c.x, t), color);
        }
    }
}

/// A horizontal bar from `x0` to `x1` (either order) centred on `y`.
fn span_x(x0: f32, x1: f32, y: f32, t: f32) -> Rect {
    Rect::from_min_max(Pos2::new(x0.min(x1), y - t / 2.0), Pos2::new(x0.max(x1), y + t / 2.0))
}

/// A vertical bar from `y0` to `y1` (either order) centred on `x`.
fn span_y(y0: f32, y1: f32, x: f32, t: f32) -> Rect {
    Rect::from_min_max(Pos2::new(x - t / 2.0, y0.min(y1)), Pos2::new(x + t / 2.0, y0.max(y1)))
}

/// ╭ ╮ ╯ ╰: a quarter circle joining the two edge midpoints.
fn draw_arc(painter: &mut Vec<Shape>, cell: Rect, cp: u32, color: Color32) {
    let c = cell.center();
    let r = (cell.width() / 2.0).min(cell.height() / 2.0);
    // (horizontal direction, vertical direction) of the two arms.
    let (hx, vy) = match cp {
        0x256D => (1.0, 1.0),   // ╭ right, down
        0x256E => (-1.0, 1.0),  // ╮ left, down
        0x256F => (-1.0, -1.0), // ╯ left, up
        _ => (1.0, -1.0),       // ╰ right, up
    };
    let centre = Pos2::new(c.x + hx * r, c.y + vy * r);
    let mut pts = vec![Pos2::new(c.x, if vy > 0.0 { cell.max.y } else { cell.min.y })];
    let steps = 12;
    for i in 0..=steps {
        // From the vertical arm (angle pointing at -hx) to the horizontal arm (angle at -vy).
        let t = i as f32 / steps as f32 * std::f32::consts::FRAC_PI_2;
        let p = Pos2::new(centre.x - hx * r * t.cos(), centre.y - vy * r * t.sin());
        pts.push(p);
    }
    pts.push(Pos2::new(if hx > 0.0 { cell.max.x } else { cell.min.x }, c.y));
    painter.push(Shape::Path(PathShape::line(pts, Stroke::new(light(cell), color))));
}

/// ╱ ╲ ╳
fn draw_diagonals(painter: &mut Vec<Shape>, cell: Rect, cp: u32, color: Color32) {
    let s = Stroke::new(light(cell), color);
    if cp != 0x2572 {
        painter.push(Shape::line_segment([cell.left_bottom(), cell.right_top()], s));
    }
    if cp != 0x2571 {
        painter.push(Shape::line_segment([cell.left_top(), cell.right_bottom()], s));
    }
}

/// Block elements: eighths, halves, shades and quadrants.
fn draw_block(painter: &mut Vec<Shape>, cell: Rect, cp: u32, color: Color32) {
    let (x0, y0, w, h) = (cell.min.x, cell.min.y, cell.width(), cell.height());
    let part = |fx0: f32, fy0: f32, fx1: f32, fy1: f32| {
        Rect::from_min_max(
            Pos2::new(x0 + fx0 * w, y0 + fy0 * h),
            Pos2::new(x0 + fx1 * w, y0 + fy1 * h),
        )
    };
    match cp {
        0x2580 => fill(painter, part(0.0, 0.0, 1.0, 0.5), color),
        0x2581..=0x2588 => {
            let n = (cp - 0x2580) as f32 / 8.0; // lower n/8 … full block
            fill(painter, part(0.0, 1.0 - n, 1.0, 1.0), color);
        }
        0x2589..=0x258F => {
            let n = (0x2590 - cp) as f32 / 8.0; // left 7/8 … 1/8
            fill(painter, part(0.0, 0.0, n, 1.0), color);
        }
        0x2590 => fill(painter, part(0.5, 0.0, 1.0, 1.0), color),
        0x2591..=0x2593 => {
            let a = [0.25, 0.5, 0.75][(cp - 0x2591) as usize];
            fill(painter, cell, color.gamma_multiply(a));
        }
        0x2594 => fill(painter, part(0.0, 0.0, 1.0, 0.125), color),
        0x2595 => fill(painter, part(0.875, 0.0, 1.0, 1.0), color),
        _ => {
            // Quadrants: bit 1 upper left, 2 upper right, 4 lower left, 8 lower right.
            let q: u8 = match cp {
                0x2596 => 4,
                0x2597 => 8,
                0x2598 => 1,
                0x2599 => 1 | 4 | 8,
                0x259A => 1 | 8,
                0x259B => 1 | 2 | 4,
                0x259C => 1 | 2 | 8,
                0x259D => 2,
                0x259E => 2 | 4,
                _ => 2 | 4 | 8,
            };
            for (bit, (fx, fy)) in [(1, (0.0, 0.0)), (2, (0.5, 0.0)), (4, (0.0, 0.5)), (8, (0.5, 0.5))] {
                if q & bit != 0 {
                    fill(painter, part(fx, fy, fx + 0.5, fy + 0.5), color);
                }
            }
        }
    }
}

/// Braille: 2 columns × 4 rows of dots. Bits 0-2 and 6 are the left column top to bottom,
/// bits 3-5 and 7 the right column.
fn draw_braille(painter: &mut Vec<Shape>, cell: Rect, bits: u8, color: Color32) {
    const DOTS: [(u8, u8); 8] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];
    let (w, h) = (cell.width(), cell.height());
    let r = (w / 4.0).min(h / 8.0) * 0.8;
    for (i, (col, row)) in DOTS.iter().enumerate() {
        if bits & (1 << i) != 0 {
            let p = cell.min + Vec2::new(w * (2.0 * *col as f32 + 1.0) / 4.0, h * (2.0 * *row as f32 + 1.0) / 8.0);
            painter.push(Shape::circle_filled(p, r, color));
        }
    }
}

/// Sextants: 2 columns × 3 rows; bit 0 upper left, 1 upper right, 2 middle left, …
fn draw_sextant(painter: &mut Vec<Shape>, cell: Rect, bits: u8, color: Color32) {
    let (w, h) = (cell.width(), cell.height());
    for i in 0..6 {
        if bits & (1 << i) != 0 {
            let (col, row) = ((i % 2) as f32, (i / 2) as f32);
            let r = Rect::from_min_size(
                cell.min + Vec2::new(col * w / 2.0, row * h / 3.0),
                Vec2::new(w / 2.0, h / 3.0),
            );
            fill(painter, r, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes_for(c: char) -> usize {
        shapes(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(10.0, 20.0)),
            c,
            Color32::WHITE,
        )
        .expect("built-in glyph")
        .len()
    }

    #[test]
    fn ranges_and_spot_checks() {
        for c in [
            '─',
            '┼',
            '╬',
            '╭',
            '╳',
            '╿',
            '▀',
            '█',
            '▏',
            '░',
            '▟',
            '⠀',
            '⣿',
            '\u{1FB00}',
            '\u{1FB3B}',
        ] {
            assert!(is_builtin(c), "{c:?}");
        }
        for c in ['a', '●', '★', 'ㄱ', '\u{2600}', '\u{1FB3C}'] {
            assert!(!is_builtin(c), "{c:?}");
        }
        // The table follows the Unicode order.
        assert_eq!(BOX['┌' as usize - 0x2500], [0, 1, 1, 0]);
        assert_eq!(BOX['┷' as usize - 0x2500], [1, 2, 0, 2]);
        assert_eq!(BOX['╔' as usize - 0x2500], [0, 3, 3, 0]);
        assert_eq!(BOX['╬' as usize - 0x2500], [3, 3, 3, 3]);
    }

    #[test]
    fn shape_counts() {
        assert_eq!(shapes_for('⠀'), 0, "blank Braille draws nothing");
        assert_eq!(shapes_for('⣿'), 8, "all eight dots");
        assert_eq!(shapes_for('⠁'), 1);
        assert_eq!(shapes_for('█'), 1);
        assert_eq!(shapes_for('▚'), 2, "two quadrants");
        assert_eq!(shapes_for('\u{1FB00}'), 1, "one sextant");
        assert_eq!(shapes_for('┼'), 4, "four arms");
        assert_eq!(shapes_for('╬'), 8, "two strokes per double arm");
    }
}
