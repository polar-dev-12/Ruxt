//! System font discovery and installation into egui.
//!
//! * `scan_system_fonts` enumerates .ttf/.otf/.ttc files in the OS font folders and reads
//!   their family names with `ttf-parser` (pure Rust, no system font APIs).
//! * `install` loads the chosen family (or a sensible default monospace font) as the primary
//!   monospace font and adds a CJK fallback so Korean/Japanese/Chinese text renders.

use egui::{FontData, FontDefinitions, FontFamily};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct FontEntry {
    pub family: String,
    pub path: PathBuf,
    pub index: u32,
    pub monospace: bool,
    pub style: String,
}

fn font_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        if let Some(w) = std::env::var_os("WINDIR") {
            v.push(Path::new(&w).join("Fonts"));
        } else {
            v.push(PathBuf::from(r"C:\Windows\Fonts"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(Path::new(&l).join("Microsoft").join("Windows").join("Fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.push(PathBuf::from("/System/Library/Fonts"));
        v.push(PathBuf::from("/System/Library/Fonts/Supplemental"));
        v.push(PathBuf::from("/Library/Fonts"));
        if let Some(h) = crate::core::hostkeys::home_dir() {
            v.push(h.join("Library").join("Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        v.push(PathBuf::from("/usr/share/fonts"));
        v.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(h) = crate::core::hostkeys::home_dir() {
            v.push(h.join(".fonts"));
            v.push(h.join(".local").join("share").join("fonts"));
        }
    }
    v
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 4 {
                walk(&p, depth + 1, out);
            }
        } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
            if matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc") {
                out.push(p);
            }
        }
    }
}

fn style_rank(style: &str) -> u8 {
    let s = style.to_ascii_lowercase();
    if s == "regular" || s == "normal" || s == "book" || s == "roman" {
        0
    } else if s == "medium" {
        1
    } else if !s.contains("bold") && !s.contains("italic") && !s.contains("oblique") {
        2
    } else {
        3
    }
}

/// One face's identity, read from just its `name` and `post` tables.
struct FaceInfo {
    family: String,
    style: String,
    monospace: bool,
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}
fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn name_from_table(table: &ttf_parser::name::Table<'_>, id: u16) -> Option<String> {
    let mut fallback = None;
    for n in table.names {
        if n.name_id != id {
            continue;
        }
        let Some(s) = n.to_string() else { continue };
        if s.trim().is_empty() {
            continue;
        }
        if n.language_id == 0x0409 || n.platform_id == ttf_parser::PlatformId::Unicode {
            return Some(s);
        }
        if fallback.is_none() {
            fallback = Some(s);
        }
    }
    fallback
}

/// Read the `name` and `post` tables of every face in a font file without loading the whole
/// file: the table directory is a few hundred bytes and the two tables a few KB, while the
/// glyph data (which is all the rest, often tens of MB for CJK fonts) is never touched.
/// Returns `(face index within the file, info)`.
fn read_face_infos(path: &Path) -> Vec<(u32, FaceInfo)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut out = Vec::new();
    let Ok(mut f) = std::fs::File::open(path) else {
        return out;
    };
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_err() {
        return out;
    }
    let offsets: Vec<u32> = if &head[..4] == b"ttcf" {
        let n = be_u32(&head, 8).unwrap_or(0).min(64) as usize;
        let mut tbl = vec![0u8; n * 4];
        if f.read_exact(&mut tbl).is_err() {
            return out;
        }
        (0..n).filter_map(|i| be_u32(&tbl, i * 4)).collect()
    } else {
        vec![0]
    };
    for (face_index, off) in offsets.into_iter().enumerate() {
        let mut dir_head = [0u8; 12];
        if f.seek(SeekFrom::Start(off as u64)).is_err() || f.read_exact(&mut dir_head).is_err() {
            continue;
        }
        let num_tables = be_u16(&dir_head, 4).unwrap_or(0).min(512) as usize;
        let mut records = vec![0u8; num_tables * 16];
        if f.read_exact(&mut records).is_err() {
            continue;
        }
        let mut name_loc = None;
        let mut post_loc = None;
        for i in 0..num_tables {
            let r = &records[i * 16..i * 16 + 16];
            let tag = &r[..4];
            let (o, l) = (be_u32(r, 8).unwrap_or(0), be_u32(r, 12).unwrap_or(0));
            if l == 0 || l > 4 * 1024 * 1024 {
                continue;
            }
            match tag {
                b"name" => name_loc = Some((o, l)),
                b"post" => post_loc = Some((o, l)),
                _ => {}
            }
        }
        let Some((no, nl)) = name_loc else { continue };
        let mut name_bytes = vec![0u8; nl as usize];
        if f.seek(SeekFrom::Start(no as u64)).is_err() || f.read_exact(&mut name_bytes).is_err() {
            continue;
        }
        let Some(name_table) = ttf_parser::name::Table::parse(&name_bytes) else {
            continue;
        };
        let family = name_from_table(&name_table, ttf_parser::name_id::TYPOGRAPHIC_FAMILY)
            .or_else(|| name_from_table(&name_table, ttf_parser::name_id::FAMILY));
        let Some(family) = family else { continue };
        let style = name_from_table(&name_table, ttf_parser::name_id::SUBFAMILY).unwrap_or_default();
        // `post.isFixedPitch` is at byte offset 12 of the table.
        let monospace = post_loc
            .and_then(|(po, _)| {
                let mut b = [0u8; 16];
                f.seek(SeekFrom::Start(po as u64)).ok()?;
                f.read_exact(&mut b).ok()?;
                be_u32(&b, 12).map(|v| v != 0)
            })
            .unwrap_or(false);
        out.push((
            face_index as u32,
            FaceInfo {
                family,
                style,
                monospace,
            },
        ));
    }
    out
}

/// Enumerate installed font families (one entry per family, preferring the Regular face).
pub fn scan_system_fonts() -> Vec<FontEntry> {
    let mut files = Vec::new();
    for d in font_dirs() {
        walk(&d, 0, &mut files);
    }
    let mut best: BTreeMap<String, FontEntry> = BTreeMap::new();
    for path in files {
        for (index, info) in read_face_infos(&path) {
            let entry = FontEntry {
                family: info.family.clone(),
                path: path.clone(),
                index,
                monospace: info.monospace,
                style: info.style.clone(),
            };
            match best.get(&info.family) {
                Some(cur) if style_rank(&cur.style) <= style_rank(&info.style) => {}
                _ => {
                    best.insert(info.family, entry);
                }
            }
        }
    }
    let mut v: Vec<FontEntry> = best.into_values().collect();
    // Monospace families first, then alphabetical (case-insensitive).
    v.sort_by(|a, b| {
        b.monospace
            .cmp(&a.monospace)
            .then_with(|| a.family.to_lowercase().cmp(&b.family.to_lowercase()))
    });
    v
}

#[cfg(windows)]
const MONO_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\CascadiaMono.ttf",
    r"C:\Windows\Fonts\CascadiaCode.ttf",
    r"C:\Windows\Fonts\consola.ttf",
    r"C:\Windows\Fonts\lucon.ttf",
];
#[cfg(windows)]
const CJK_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\malgun.ttf",
    r"C:\Windows\Fonts\msgothic.ttc",
    r"C:\Windows\Fonts\msyh.ttc",
];

/// Symbol and emoji faces appended after the CJK fallback, so characters no earlier font
/// has (arrows, dingbats, geometric shapes, emoji used by CLI tools) render instead of the
/// missing-glyph box. Every readable one is added. Box drawing, blocks and Braille are drawn
/// without fonts (see `glyphs`).
#[cfg(windows)]
const SYMBOL_CANDIDATES: &[&str] = &[r"C:\Windows\Fonts\seguisym.ttf", r"C:\Windows\Fonts\seguiemj.ttf"];
#[cfg(target_os = "macos")]
const SYMBOL_CANDIDATES: &[&str] = &["/System/Library/Fonts/Apple Symbols.ttf"];
#[cfg(all(unix, not(target_os = "macos")))]
const SYMBOL_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansSymbols-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
];

#[cfg(target_os = "macos")]
const MONO_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/Library/Fonts/SF-Mono-Regular.otf",
    "/System/Library/Fonts/Menlo.ttc",
];
#[cfg(target_os = "macos")]
const CJK_CANDIDATES: &[&str] = &["/System/Library/Fonts/AppleSDGothicNeo.ttc"];

#[cfg(all(unix, not(target_os = "macos")))]
const MONO_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
];
#[cfg(all(unix, not(target_os = "macos")))]
const CJK_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

fn first_readable(cands: &[&str]) -> Option<(String, Vec<u8>)> {
    for c in cands {
        if let Ok(bytes) = std::fs::read(c) {
            if !bytes.is_empty() {
                return Some((c.to_string(), bytes));
            }
        }
    }
    None
}

/// Find the file for a family name (scans the system; use the cached list in the UI).
#[cfg(test)]
pub fn find_family(family: &str) -> Option<FontEntry> {
    find_family_cached(family, None)
}

/// Find the file for a family name using a pre-scanned cache if available. Without a cache
/// the scan stops at the first file that contains the family (startup path).
pub fn find_family_cached(family: &str, cached: Option<&[FontEntry]>) -> Option<FontEntry> {
    let want = family.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    if let Some(list) = cached {
        return list.iter().find(|e| e.family.to_lowercase() == want).cloned();
    }
    let mut files = Vec::new();
    for d in font_dirs() {
        walk(&d, 0, &mut files);
    }
    let mut best: Option<FontEntry> = None;
    for path in files {
        for (index, info) in read_face_infos(&path) {
            if info.family.to_lowercase() != want {
                continue;
            }
            let better = best
                .as_ref()
                .map(|b| style_rank(&info.style) < style_rank(&b.style))
                .unwrap_or(true);
            if better {
                let regular = style_rank(&info.style) == 0;
                best = Some(FontEntry {
                    family: info.family,
                    path: path.clone(),
                    index,
                    monospace: info.monospace,
                    style: info.style,
                });
                if regular {
                    return best;
                }
            }
        }
    }
    best
}

/// Install fonts into `ctx`: the requested family (if found) or a default monospace font as
/// the primary monospace face, plus a CJK fallback for both families.
pub fn install(ctx: &egui::Context, family: &str) {
    install_with_cache(ctx, family, None);
}

/// Install fonts into `ctx`, reusing an already scanned font list when available.
pub fn install_with_cache(ctx: &egui::Context, family: &str, cached: Option<&[FontEntry]>) {
    let mut defs = FontDefinitions::default();
    let mut primary: Option<(String, FontData)> = None;
    if let Some(e) = find_family_cached(family, cached) {
        if let Ok(bytes) = std::fs::read(&e.path) {
            tracing::info!("terminal font: {} ({})", e.family, e.path.display());
            primary = Some((e.family.clone(), FontData::from_blob(Arc::new(bytes), e.index)));
        }
    } else if !family.trim().is_empty() {
        tracing::warn!("font family {family:?} not found; using default");
    }
    if primary.is_none() {
        if let Some((path, bytes)) = first_readable(MONO_CANDIDATES) {
            tracing::info!("monospace font: {path}");
            primary = Some((path, FontData::from_owned(bytes)));
        }
    }
    if let Some((_, data)) = primary {
        defs.font_data.insert("sys-mono".into(), Arc::new(data));
        defs.families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "sys-mono".into());
    }
    if let Some((path, bytes)) = first_readable(CJK_CANDIDATES) {
        tracing::info!("CJK fallback font: {path}");
        let blob: egui::epaint::text::Blob = Arc::new(bytes);
        let cjk_metrics = line_metrics(blob.as_ref().as_ref(), 0);
        // One entry per family: the baseline correction depends on that family's primary font.
        for (fam, key) in [
            (FontFamily::Monospace, "sys-cjk-mono"),
            (FontFamily::Proportional, "sys-cjk"),
        ] {
            let mut data = FontData::from_blob(blob.clone(), 0);
            let primary_metrics = defs
                .families
                .get(&fam)
                .and_then(|names| names.first())
                .and_then(|name| defs.font_data.get(name))
                .and_then(|d| line_metrics(d.font.as_ref().as_ref(), d.index));
            if let (Some(p), Some(f)) = (primary_metrics, cjk_metrics) {
                data.tweak.y_offset_factor = baseline_shift(p, f);
            }
            defs.font_data.insert(key.into(), Arc::new(data));
            defs.families.entry(fam).or_default().push(key.into());
        }
    }
    for (n, path) in SYMBOL_CANDIDATES.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        if bytes.is_empty() {
            continue;
        }
        tracing::info!("symbol fallback font: {path}");
        let blob: egui::epaint::text::Blob = Arc::new(bytes);
        let metrics = line_metrics(blob.as_ref().as_ref(), 0);
        for fam in [FontFamily::Monospace, FontFamily::Proportional] {
            let key = format!("sys-sym-{n}-{fam:?}");
            let mut data = FontData::from_blob(blob.clone(), 0);
            let primary_metrics = defs
                .families
                .get(&fam)
                .and_then(|names| names.first())
                .and_then(|name| defs.font_data.get(name))
                .and_then(|d| line_metrics(d.font.as_ref().as_ref(), d.index));
            if let (Some(p), Some(f)) = (primary_metrics, metrics) {
                data.tweak.y_offset_factor = baseline_shift(p, f);
            }
            defs.font_data.insert(key.clone(), Arc::new(data));
            defs.families.entry(fam).or_default().push(key);
        }
    }
    // Fallback so toolbar labels can mix a Phosphor glyph with ordinary text.
    egui_phosphor::add_to_fonts(&mut defs, egui_phosphor::Variant::Regular);
    ctx.set_fonts(defs);
}

/// Vertical metrics of a face as fractions of the em: `(ascent, row height)`, read from
/// the same tables egui uses (OS/2 typo metrics when `USE_TYPO_METRICS` is set, else hhea).
fn line_metrics(data: &[u8], index: u32) -> Option<(f32, f32)> {
    let face = ttf_parser::Face::parse(data, index).ok()?;
    let upm = face.units_per_em() as f32;
    if upm <= 0.0 {
        return None;
    }
    let ascent = face.ascender() as f32 / upm;
    let height = (face.ascender() as f32 - face.descender() as f32 + face.line_gap() as f32) / upm;
    (height > 0.0).then_some((ascent, height))
}

/// egui centres a fallback glyph in the primary font's row height, so a fallback face with
/// different ascent/descent proportions (Malgun Gothic next to Cascadia Mono) sits on a
/// different baseline. Returns the `FontTweak::y_offset_factor` that moves the fallback
/// glyphs back onto the primary baseline.
fn baseline_shift(primary: (f32, f32), fallback: (f32, f32)) -> f32 {
    let (pa, ph) = primary;
    let (fa, fh) = fallback;
    // Primary baseline: pa. Fallback baseline: fa + (ph - fh) / 2.
    (pa - fa - 0.5 * (ph - fh)).clamp(-0.5, 0.5)
}

#[cfg(test)]
#[path = "fonts_tests.rs"]
mod tests;
