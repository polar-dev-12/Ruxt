use super::*;

#[test]
fn scans_system_fonts_and_finds_a_family() {
    let fonts = scan_system_fonts();
    // Every desktop OS we support ships at least a few fonts.
    assert!(!fonts.is_empty(), "no system fonts found");
    // One entry per family.
    let mut names: Vec<&str> = fonts.iter().map(|f| f.family.as_str()).collect();
    let before = names.len();
    names.sort();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate families in scan");
    // Monospace families are listed first.
    let first_non_mono = fonts.iter().position(|f| !f.monospace).unwrap_or(fonts.len());
    assert!(fonts[..first_non_mono].iter().all(|f| f.monospace));
    assert!(fonts[first_non_mono..].iter().all(|f| !f.monospace));
    // Lookup by name (case-insensitive) must resolve to a readable file.
    let pick = &fonts[0];
    let found = find_family(&pick.family.to_uppercase()).expect("find_family");
    assert_eq!(found.family, pick.family);
    assert!(std::fs::metadata(&found.path).is_ok());
    assert!(find_family("definitely-not-a-font-family-xyz").is_none());
}

#[test]
fn fallback_baseline_lines_up_with_the_primary_font() {
    // Same proportions: nothing to correct.
    assert!(baseline_shift((0.8, 1.2), (0.8, 1.2)).abs() < 1e-6);
    // A fallback with a taller ascent (relative to its row) sits lower → shift it up.
    let up = baseline_shift((0.8, 1.2), (1.0, 1.3));
    assert!(up < 0.0, "{up}");
    // After the shift, both baselines coincide: fa + (ph - fh) / 2 + shift == pa.
    assert!((1.0 + (1.2 - 1.3) / 2.0 + up - 0.8).abs() < 1e-6);
    // Broken metrics never move glyphs by more than half an em.
    assert_eq!(baseline_shift((0.0, 0.1), (5.0, 1.0)), -0.5);
    // Real fonts, when present.
    if let (Some((_, mono)), Some((_, cjk))) = (first_readable(MONO_CANDIDATES), first_readable(CJK_CANDIDATES)) {
        let p = line_metrics(&mono, 0).expect("mono metrics");
        let f = line_metrics(&cjk, 0).expect("cjk metrics");
        eprintln!("mono {p:?} cjk {f:?} shift {}", baseline_shift(p, f));
    }
}
