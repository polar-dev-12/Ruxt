//! Headless UI smoke tests (egui_kittest): the SFTP explorer, the session editor and the
//! settings page are driven through the accessibility tree, and every `tr("…")` key in the UI
//! sources must have a Korean translation.

use super::dialogs::{settings_ui, AppLockDialog, AppLockMode, SessionEditor, WelcomeDialog, WELCOME_PAGES};
use super::sftp_panel::{SftpAction, SftpPanelState};
use crate::core::config::{AppConfig, SessionProfile};
use crate::core::i18n::{self, set_lang, tr, Lang};
use crate::core::sftp::{SftpCmd, SftpEntry, SftpEvent};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// Every `tr("…")` / `trf("…")` key found in a source text.
fn tr_keys(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !src.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &src[i..];
        let m = if rest.starts_with("trf(") {
            4
        } else if rest.starts_with("tr(") {
            3
        } else {
            i += 1;
            continue;
        };
        // Only a call, not e.g. `attr(` – check the preceding character.
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            i += m;
            continue;
        }
        let mut j = i + m;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'"' {
            i = j;
            continue;
        }
        j += 1;
        let mut key = String::new();
        let mut chars = src[j..].chars();
        loop {
            match chars.next() {
                None => break,
                Some('\\') => match chars.next() {
                    Some('n') => key.push('\n'),
                    Some('"') => key.push('"'),
                    Some('\\') => key.push('\\'),
                    Some(c) => key.push(c),
                    None => break,
                },
                Some('"') => break,
                Some(c) => key.push(c),
            }
        }
        out.push(key);
        i = j;
    }
    out
}

#[test]
fn every_ui_string_has_a_korean_translation() {
    let sources = [
        include_str!("mod.rs"),
        include_str!("dialogs.rs"),
        include_str!("sftp_panel.rs"),
        include_str!("tail_viewer.rs"),
        include_str!("terminal_widget.rs"),
    ];
    let mut missing: Vec<String> = Vec::new();
    for src in sources {
        for k in tr_keys(src) {
            if !i18n::known(&k) && !missing.contains(&k) {
                missing.push(k);
            }
        }
    }
    assert!(missing.is_empty(), "untranslated UI strings: {missing:#?}");
}

fn entry(name: &str, dir: bool) -> SftpEntry {
    SftpEntry {
        name: name.into(),
        raw_name: name.into(),
        path: format!("/home/u/{name}"),
        is_dir: dir,
        is_symlink: false,
        is_regular: !dir,
        size: if dir { 0 } else { 2048 },
        mtime: Some(1_761_700_000),
        permissions: Some(if dir { 0o040755 } else { 0o100644 }),
    }
}

struct Explorer {
    panel: SftpPanelState,
    actions: Vec<SftpAction>,
}

fn explorer_harness<'a>() -> Harness<'a, Explorer> {
    let mut panel = SftpPanelState::default();
    panel.started = true;
    panel.apply(SftpEvent::Home("/home/u".into()));
    panel.apply(SftpEvent::Listing {
        path: "/home/u".into(),
        entries: vec![
            entry("docs", true),
            entry("config.yaml", false),
            entry("notes.txt", false),
        ],
    });
    panel.show_tree = false;
    let state = Explorer {
        panel,
        actions: Vec::new(),
    };
    Harness::builder()
        .with_size(egui::Vec2::new(640.0, 480.0))
        .with_step_dt(1.0 / 60.0)
        .allow_missing_glyphs()
        .build_ui_state(
            |ui, st: &mut Explorer| {
                let a = st.panel.ui(ui, true);
                st.actions.extend(a);
            },
            state,
        )
}

/// Two clicks in one frame: egui counts the second release as a double-click.
fn double_click(h: &mut Harness<'_, Explorer>, label: &str) {
    let c = h.get_by_label(label).rect().center();
    h.event(egui::Event::PointerMoved(c));
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos: c,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
    }
    h.run();
}

fn list_cmds(actions: &[SftpAction]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| match a {
            SftpAction::Cmd(SftpCmd::List(p)) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn sftp_explorer_selects_sorts_and_opens() {
    let _g = i18n::test_lock();
    set_lang(Lang::En);
    let mut h = explorer_harness();
    h.run();

    // Click selects the row.
    h.get_by_label("config.yaml").click();
    h.run();
    assert_eq!(h.state().panel.list.selected, vec![1]);

    // Clicking a column header sorts by it; clicking again flips the order.
    h.get_by_label("Size").click();
    h.run();
    assert_eq!(h.state().panel.list.sort, super::sftp_panel::SortKey::Size);
    assert!(!h.state().panel.list.sort_desc);
    h.get_by_label("Size").click();
    h.run();
    assert!(h.state().panel.list.sort_desc);

    // Double-clicking a folder asks the app to list it.
    h.state_mut().actions.clear();
    double_click(&mut h, "docs");
    assert_eq!(list_cmds(&h.state().actions), vec!["/home/u/docs".to_string()]);

    // Double-clicking a file opens it in the external editor (the path that can upload).
    h.state_mut().actions.clear();
    double_click(&mut h, "notes.txt");
    assert!(h.state().actions.iter().any(|a| matches!(
        a,
        SftpAction::OpenInEditor(p) if p.ends_with("notes.txt")
    )));

    // Context menu: Cut puts the row on the internal clipboard.
    h.get_by_label("notes.txt").click_secondary();
    h.run();
    h.get_by_label("Cut").click();
    h.run();
    assert!(h.state().panel.clip_len() == 1);

    // Keyboard: the list has focus after a click; Backspace goes to the parent folder.
    h.state_mut().actions.clear();
    h.get_by_label("notes.txt").click();
    h.run();
    h.key_press(egui::Key::Backspace);
    h.run();
    assert_eq!(list_cmds(&h.state().actions), vec!["/home".to_string()]);
    set_lang(Lang::Ko);
}

#[test]
fn sftp_parent_row_goes_up_one_folder() {
    let _g = i18n::test_lock();
    set_lang(Lang::En);
    let mut h = explorer_harness();
    h.run();
    h.get_by_label("Parent folder");
    double_click(&mut h, "Parent folder");
    let cmds = list_cmds(&h.state().actions);
    assert!(
        cmds.iter().any(|c| c == "/home"),
        "parent row must list /home: {cmds:?}"
    );
    set_lang(Lang::Ko);
}

#[test]
fn sftp_explorer_renders_in_korean() {
    let _g = i18n::test_lock();
    set_lang(Lang::Ko);
    let mut h = explorer_harness();
    h.run();
    // Column headers and the command bar are Korean.
    h.get_by_label("수정한 날짜");
    h.get_by_label("새 폴더");
    assert!(h.query_by_label("Date modified").is_none());
}

#[test]
fn session_editor_validation_and_rendering() {
    let _g = i18n::test_lock();
    set_lang(Lang::En);
    let mut ed = SessionEditor::new(SessionProfile::default(), true, false, false);
    assert!(!ed.validate_with(None));
    assert_eq!(ed.error.as_deref(), Some(tr("Host is required")));
    ed.profile.host = "bad host".into();
    assert!(!ed.validate_with(None));
    ed.profile.host = "example.org".into();
    ed.profile.jump = ed.profile.id.clone();
    assert!(!ed.validate_with(None));
    ed.profile.jump.clear();
    assert!(ed.validate_with(None));

    let ed = SessionEditor::new(SessionProfile::default(), true, false, false);
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(900.0, 1100.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, ed: &mut SessionEditor| {
                let others = vec![("other-id".to_string(), "bastion".to_string())];
                let _ = ed.ui(ui.ctx(), Some("no lock"), &others, None);
            },
            ed,
        );
    h.run_steps(8);
    h.get_by_label("Jump host");
    // Saving without a host shows the validation error in the window. (The accessibility
    // action is used because the window is still settling its layout in the first frames.)
    h.get_by_label("Save").click_accesskit();
    h.run_steps(8);
    assert_eq!(h.state().error.as_deref(), Some("Host is required"));
    h.get_by_label("Host is required");

    let mut ed = SessionEditor::new(SessionProfile::default(), false, true, false);
    ed.profile.host = "example.org".into();
    ed.change_password = true;
    ed.current_password = "wrong".into();
    ed.password = "new-secret".into();
    ed.password_repeat = "new-secret".into();
    assert!(!ed.validate_with(Some("old-secret")));
    assert_eq!(ed.error.as_deref(), Some("Wrong password"));
    ed.current_password = "old-secret".into();
    ed.password_repeat = "nope".into();
    assert!(!ed.validate_with(Some("old-secret")));
    assert_eq!(ed.error.as_deref(), Some("Passwords do not match"));
    ed.password_repeat = "new-secret".into();
    assert!(ed.validate_with(Some("old-secret")));
    set_lang(Lang::Ko);
}

#[test]
fn settings_page_renders_in_both_languages() {
    let _g = i18n::test_lock();
    for (lang, label) in [(Lang::En, "Open audit log"), (Lang::Ko, "감사 로그 열기")] {
        set_lang(lang);
        let mut h = Harness::builder()
            .with_size(egui::Vec2::new(640.0, 900.0))
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .build_ui_state(
                |ui, cfg: &mut AppConfig| {
                    let mut rescan = false;
                    let _ = settings_ui(ui, cfg, false, false, Some(&[]), &mut rescan, 0);
                },
                AppConfig::default(),
            );
        h.run();
        h.get_by_label(label);
        h.get_by_label("wgpu (DirectX 12 / Vulkan / Metal)");
        h.get_by_label(tr("Clear command history"));
        h.get_by_label(tr("Monitor connected servers (CPU, memory, disk, load)"));
    }
    set_lang(Lang::Ko);
}

/// The completion popup lists the candidates under the cursor; clicking one accepts it.
#[test]
fn completion_popup_renders_and_click_accepts() {
    use super::terminal_widget::TermView;
    use crate::core::term::{Size, TermState};
    let _g = i18n::test_lock();
    set_lang(Lang::En);
    struct S {
        view: TermView,
        term: std::sync::Arc<parking_lot::Mutex<TermState>>,
        accepted: Option<usize>,
    }
    let (term, _rx) = TermState::new(egui::Context::default(), Size { cols: 80, rows: 24 }, 100, false);
    term.lock().advance(b"$ gi");
    let mut view = TermView::new(7u64);
    view.candidates = vec!["git status".into(), "git pull --rebase".into()];
    view.cand_idx = 0;
    view.ghost = Some("t status".into());
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(800.0, 500.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, s: &mut S| {
                let out = s.view.show(ui, &s.term, 14.0, true, false);
                if out.accept.is_some() {
                    s.accepted = out.accept;
                }
            },
            S {
                view,
                term,
                accepted: None,
            },
        );
    for _ in 0..4 {
        h.step();
    }
    h.get_by_label("git status");
    h.get_by_label("→ accept · Ctrl+↑/↓ choose · Esc hide");
    let c = h.get_by_label("git pull --rebase").rect().center();
    h.event(egui::Event::PointerMoved(c));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: c,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.state().accepted, Some(1));
    set_lang(Lang::Ko);
}

#[test]
fn app_lock_settings_and_dialogs_render() {
    let _g = i18n::test_lock();
    set_lang(Lang::En);
    // Settings: with a lock password set, the idle slider and management buttons appear and
    // the configured minutes are snapped to the 10-minute grid.
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(640.0, 900.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, cfg: &mut AppConfig| {
                let mut rescan = false;
                let _ = settings_ui(ui, cfg, true, true, Some(&[]), &mut rescan, 3);
            },
            AppConfig {
                app_lock_minutes: 17,
                ..AppConfig::default()
            },
        );
    h.run();
    h.get_by_label("Lock now");
    h.get_by_label("Remove screen lock…");
    // An old vault still to be moved is offered from here.
    h.get_by_label("Move saved passwords…");
    assert_eq!(h.state().app_lock_minutes, 20);

    // Lock screen and first-run prompt.
    for (mode, title, button) in [
        (AppLockMode::Unlock, "Ruxt is locked", "Unlock"),
        (
            AppLockMode::Setup { first_run: true },
            "Protect Ruxt with a password?",
            "Not now",
        ),
        (AppLockMode::Change, "Change screen lock password", "Cancel"),
        (
            AppLockMode::Remove,
            "Remove screen lock",
            "Remove and delete saved passwords",
        ),
    ] {
        let mut h = Harness::builder()
            .with_size(egui::Vec2::new(640.0, 480.0))
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .build_ui_state(
                |ui, d: &mut AppLockDialog| {
                    let _ = d.ui(ui.ctx());
                },
                AppLockDialog::new(mode),
            );
        h.run();
        h.get_by_label(title);
        h.get_by_label(button);
    }
    set_lang(Lang::Ko);
}

/// Writes PNGs of the main dialogs (at a normal and a small viewport) into `$RUXT_SHOTS` for
/// a visual check of the layout. Needs a GPU-capable wgpu backend, so it is ignored by default:
/// `RUXT_SHOTS=<dir> cargo test render_screens -- --ignored`.
#[test]
#[ignore]
fn render_screens() {
    use super::dialogs::{MigrateDialog, ThemeEditor};
    use super::kit;
    use crate::core::theme::ThemeSpec;
    let Ok(dir) = std::env::var("RUXT_SHOTS") else { return };
    let _g = i18n::test_lock();
    set_lang(Lang::Ko);
    std::fs::create_dir_all(&dir).unwrap();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Screen {
        Settings,
        Editor,
        Theme,
        Vault,
        AppLock,
        Welcome(usize),
        About,
    }
    struct S {
        screen: Screen,
        spec: ThemeSpec,
        installed: bool,
        cfg: AppConfig,
        editor: SessionEditor,
        theme: ThemeEditor,
        vault: MigrateDialog,
        lock: AppLockDialog,
        welcome: WelcomeDialog,
    }
    for (spec, tag) in [(ThemeSpec::matcha(), "matcha"), (ThemeSpec::light(), "light")] {
        for (screen, name) in [
            (Screen::Settings, "settings"),
            (Screen::Editor, "session-editor"),
            (Screen::Theme, "theme-editor"),
            (Screen::Vault, "vault"),
            (Screen::AppLock, "applock"),
            (Screen::About, "about"),
            (Screen::Welcome(0), "welcome-1"),
            (Screen::Welcome(4), "welcome-5"),
            (Screen::Welcome(5), "welcome-6"),
        ] {
            for (w, h, size_tag) in [(1100.0, 820.0, "large"), (720.0, 460.0, "small")] {
                let profile = SessionProfile {
                    host: "example.org".into(),
                    username: "deploy".into(),
                    x11_forwarding: true,
                    tunnels: vec![Default::default()],
                    ..Default::default()
                };
                let state = S {
                    screen,
                    spec: spec.clone(),
                    installed: false,
                    cfg: AppConfig::default(),
                    editor: SessionEditor::new(profile, true, false, false),
                    theme: ThemeEditor::new(spec.clone(), None),
                    vault: MigrateDialog::new(),
                    lock: AppLockDialog::new(AppLockMode::Setup { first_run: true }),
                    welcome: WelcomeDialog {
                        page: if let Screen::Welcome(p) = screen { p } else { 0 },
                    },
                };
                let mut harness = Harness::builder()
                    .with_size(egui::Vec2::new(w, h))
                    .with_accessibility_check(false)
                    .allow_missing_glyphs()
                    .wgpu()
                    .build_ui_state(
                        |ui, s: &mut S| {
                            let ctx = ui.ctx().clone();
                            if !s.installed {
                                s.installed = true;
                                super::fonts::install(&ctx, "");
                                let t = if s.spec.dark {
                                    egui::Theme::Dark
                                } else {
                                    egui::Theme::Light
                                };
                                ctx.set_theme(t);
                                ctx.set_visuals_of(t, s.spec.visuals());
                                kit::apply_style(&ctx);
                            }
                            ui.label("Ruxt");
                            match s.screen {
                                Screen::Settings => {
                                    let mut open = true;
                                    kit::window(&ctx, tr("Settings"), egui::Id::unique("settings-window"), 520.0)
                                        .open(&mut open)
                                        .show(&ctx, |ui| {
                                            kit::body(ui, &ctx, |ui| {
                                                let mut rescan = false;
                                                let _ =
                                                    settings_ui(ui, &mut s.cfg, true, true, Some(&[]), &mut rescan, 12);
                                            });
                                        });
                                }
                                Screen::Editor => {
                                    let _ = s.editor.ui(&ctx, None, &[], None);
                                }
                                Screen::Theme => {
                                    let _ = s.theme.ui(&ctx);
                                }
                                Screen::Vault => {
                                    let _ = s.vault.ui(&ctx);
                                }
                                Screen::Welcome(_) => {
                                    let _ = s.welcome.ui(&ctx);
                                }
                                Screen::AppLock => {
                                    let _ = s.lock.ui(&ctx);
                                }
                                Screen::About => {
                                    kit::window(&ctx, tr("Help & about"), egui::Id::unique("about-window"), 520.0)
                                        .default_height(680.0)
                                        .show(&ctx, |ui| {
                                            kit::body(ui, &ctx, |ui| {
                                                let _ = super::dialogs::about_ui(
                                                    ui,
                                                    &crate::core::update::UpdateState::Idle,
                                                );
                                            })
                                        });
                                }
                            }
                        },
                        state,
                    );
                for _ in 0..6 {
                    harness.step();
                }
                let img = harness.render().expect("render");
                let path = format!("{dir}/{name}-{tag}-{size_tag}.png");
                img.save(&path).unwrap();
            }
        }
    }
}

/// PNG of the SFTP explorer with a few transfers, for a visual check:
/// `RUXT_SHOTS=<dir> cargo test render_sftp -- --ignored`.
#[test]
#[ignore]
fn render_sftp() {
    use crate::core::sftp::TransferKind;
    use crate::core::theme::ThemeSpec;
    let Ok(dir) = std::env::var("RUXT_SHOTS") else { return };
    let _g = i18n::test_lock();
    set_lang(Lang::Ko);
    let mut panel = SftpPanelState::default();
    panel.started = true;
    panel.apply(SftpEvent::Home("/home/u".into()));
    panel.apply(SftpEvent::Listing {
        path: "/home/u/projects".into(),
        entries: vec![
            entry("docs", true),
            entry("config.yaml", false),
            entry("notes.txt", false),
        ],
    });
    panel.list.selected = vec![1];
    panel.show_tree = false;
    panel.apply(SftpEvent::Progress {
        id: 1,
        name: "backups / 2026-10-29-full-dump-of-the-production-database.tar.gz".into(),
        done: 734_003_200,
        total: 2_147_483_648,
        files_done: 37,
        files_total: 120,
        kind: TransferKind::Download,
    });
    panel.apply(SftpEvent::Progress {
        id: 2,
        name: "a.txt".into(),
        done: 100,
        total: 2048,
        files_done: 0,
        files_total: 1,
        kind: TransferKind::Upload,
    });
    panel.apply(SftpEvent::Finished {
        id: 3,
        name: "report.pdf".into(),
        result: Ok(()),
    });
    let state = Explorer {
        panel,
        actions: Vec::new(),
    };
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(760.0, 620.0))
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .wgpu()
        .build_ui_state(
            |ui, s: &mut Explorer| {
                let ctx = ui.ctx().clone();
                let flag = ui.scope_id().with("installed");
                if !ui.data(|d| d.get_temp::<bool>(flag).unwrap_or(false)) {
                    ui.data_mut(|d| d.insert_temp(flag, true));
                    super::fonts::install(&ctx, "");
                    ctx.set_theme(egui::Theme::Dark);
                    ctx.set_visuals_of(egui::Theme::Dark, ThemeSpec::matcha().visuals());
                    super::kit::apply_style(&ctx);
                }
                let acts = s.panel.ui(ui, true);
                s.actions.extend(acts);
            },
            state,
        );
    for _ in 0..6 {
        harness.step();
    }
    harness.render().unwrap().save(format!("{dir}/sftp-panel.png")).unwrap();
}

/// Marketing shot of the SFTP explorer filled with made-up files.
/// `RUXT_SHOTS=<dir> cargo test render_sftp_dummy -- --ignored`.
#[test]
#[ignore]
fn render_sftp_dummy() {
    use crate::core::theme::ThemeSpec;
    let Ok(dir) = std::env::var("RUXT_SHOTS") else { return };
    let _g = i18n::test_lock();
    set_lang(Lang::Ko);
    let mut panel = SftpPanelState::default();
    panel.started = true;
    panel.show_tree = true;
    panel.show_local = false;
    let dir_entry = |parent: &str, name: &str| demo_entry(parent, name, true, 0, 1_767_225_600);
    panel.apply(SftpEvent::Home("/home/demo".into()));
    panel.apply(SftpEvent::Subdirs {
        path: "/".into(),
        dirs: vec![
            dir_entry("/", "bin"),
            dir_entry("/", "etc"),
            dir_entry("/", "home"),
            dir_entry("/", "opt"),
            dir_entry("/", "tmp"),
            dir_entry("/", "usr"),
            dir_entry("/", "var"),
        ],
    });
    panel.apply(SftpEvent::Subdirs {
        path: "/home".into(),
        dirs: vec![dir_entry("/home", "demo")],
    });
    panel.apply(SftpEvent::Listing {
        path: "/home/demo".into(),
        entries: vec![
            dir_entry("/home/demo", "documents"),
            dir_entry("/home/demo", "pictures"),
            dir_entry("/home/demo", "project"),
            demo_entry("/home/demo", "README.md", false, 1_248, 1_763_001_600),
            demo_entry("/home/demo", "hello.py", false, 860, 1_764_200_000),
            demo_entry("/home/demo", "notes.txt", false, 412, 1_765_800_000),
            demo_entry("/home/demo", "picture.png", false, 248_320, 1_766_400_000),
            demo_entry("/home/demo", "sample.json", false, 2_048, 1_766_900_000),
            demo_entry("/home/demo", "backup.tar.gz", false, 4_194_304, 1_767_100_000),
        ],
    });
    let state = Explorer {
        panel,
        actions: Vec::new(),
    };
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(980.0, 420.0))
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .wgpu()
        .build_ui_state(
            |ui, s: &mut Explorer| {
                let ctx = ui.ctx().clone();
                let flag = ui.scope_id().with("installed");
                if !ui.data(|d| d.get_temp::<bool>(flag).unwrap_or(false)) {
                    ui.data_mut(|d| d.insert_temp(flag, true));
                    super::fonts::install(&ctx, "");
                    ctx.set_theme(egui::Theme::Dark);
                    ctx.set_visuals_of(egui::Theme::Dark, super::theme::chrome(ThemeSpec::matcha().visuals()));
                    super::kit::apply_style(&ctx);
                }
                let acts = s.panel.ui(ui, true);
                s.actions.extend(acts);
            },
            state,
        );
    for _ in 0..8 {
        harness.step();
    }
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/sftp-explorer.png"))
        .unwrap();
}

fn demo_entry(parent: &str, name: &str, dir: bool, size: u64, mtime: u32) -> SftpEntry {
    let path = if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    };
    SftpEntry {
        name: name.into(),
        raw_name: name.into(),
        path,
        is_dir: dir,
        is_symlink: false,
        is_regular: !dir,
        size,
        mtime: Some(mtime),
        permissions: Some(if dir { 0o040755 } else { 0o100644 }),
    }
}

/// PNG of mixed Korean/English text drawn the way the terminal draws it (ASCII runs, one
/// galley per wide character), to check that both scripts share a baseline:
/// `RUXT_SHOTS=<dir> cargo test render_cjk_baseline -- --ignored`.
#[test]
#[ignore]
fn render_cjk_baseline() {
    let Ok(dir) = std::env::var("RUXT_SHOTS") else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        "2 추가 보안 업데이트는 ESM Apps에 적용될 수 있습니다.",
        "ESM Apps 서비스 at https://ubuntu.com/esm 활성화에 대해 자세히 알아보십시오.",
        "____가나다____abc____",
    ];
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(760.0, 120.0))
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .wgpu()
        .build_ui_state(
            move |ui, installed: &mut bool| {
                if !*installed {
                    *installed = true;
                    super::fonts::install(ui.ctx(), "");
                }
                let font_id = egui::FontId::monospace(15.0);
                let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)));
                let painter = ui.painter();
                painter.rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(0x1e, 0x1e, 0x1e));
                let origin = ui.max_rect().min + egui::Vec2::new(8.0, 8.0);
                for (row, line) in lines.iter().enumerate() {
                    let mut col = 0usize;
                    for c in line.chars() {
                        let wide = !c.is_ascii();
                        let pos = origin + egui::Vec2::new(col as f32 * cw, row as f32 * ch);
                        painter.text(pos, egui::Align2::LEFT_TOP, c, font_id.clone(), egui::Color32::WHITE);
                        col += if wide { 2 } else { 1 };
                    }
                }
            },
            false,
        );
    for _ in 0..4 {
        harness.step();
    }
    harness
        .render()
        .expect("render")
        .save(format!("{dir}/cjk-baseline.png"))
        .unwrap();
}

/// PNG of the toolbar toggles (on / off / disabled) in both built-in themes:
/// `RUXT_SHOTS=<dir> cargo test render_toggles -- --ignored`.
#[test]
#[ignore]
fn render_toggles() {
    use super::kit;
    use crate::core::theme::ThemeSpec;
    let Ok(dir) = std::env::var("RUXT_SHOTS") else { return };
    let _g = i18n::test_lock();
    set_lang(Lang::Ko);
    for (spec, tag) in [(ThemeSpec::matcha(), "matcha"), (ThemeSpec::light(), "light")] {
        let mut installed = false;
        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(760.0, 90.0))
            .with_accessibility_check(false)
            .allow_missing_glyphs()
            .wgpu()
            .build_ui(move |ui| {
                let ctx = ui.ctx().clone();
                if !installed {
                    installed = true;
                    super::fonts::install(&ctx, "");
                    let t = if spec.dark {
                        egui::Theme::Dark
                    } else {
                        egui::Theme::Light
                    };
                    ctx.set_theme(t);
                    ctx.set_visuals_of(t, spec.visuals());
                    kit::apply_style(&ctx);
                }
                ui.horizontal(|ui| {
                    kit::toggle(ui, true, tr("Sessions"));
                    kit::toggle(ui, false, tr("Find"));
                    kit::toggle(ui, true, "SFTP");
                    ui.add_enabled_ui(false, |ui| kit::toggle(ui, false, tr("Tunnels")));
                    kit::toggle(ui, false, tr("Broadcast input"));
                    kit::toggle(ui, false, tr("Settings"));
                });
                ui.horizontal(|ui| {
                    kit::toggle(ui, false, tr("Sessions"));
                    kit::toggle(ui, true, tr("Find"));
                    kit::toggle(ui, false, "SFTP");
                    ui.add_enabled_ui(false, |ui| kit::toggle(ui, true, tr("Tunnels")));
                    kit::toggle(ui, true, tr("Broadcast input"));
                    kit::toggle(ui, true, tr("Settings"));
                });
            });
        for _ in 0..4 {
            harness.step();
        }
        harness
            .render()
            .expect("render")
            .save(format!("{dir}/toggles-{tag}.png"))
            .unwrap();
    }
}

/// The welcome tour turns pages with Next / Back and the arrow keys, and closes on the last.
#[test]
fn welcome_tour_pages_and_closes() {
    use super::dialogs::WelcomeResult;
    let _g = i18n::test_lock();
    set_lang(Lang::En);
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(900.0, 900.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut (WelcomeDialog, bool)| {
                if let WelcomeResult::Close = st.0.ui(ui.ctx()) {
                    st.1 = true;
                }
            },
            (WelcomeDialog::new(), false),
        );
    h.run();
    h.get_by_label("Welcome to Ruxt");
    h.get_by_label("Next").click();
    h.run();
    assert_eq!(h.state().0.page, 1);
    h.get_by_label("Connect");
    h.get_by_label("Back").click();
    h.run();
    assert_eq!(h.state().0.page, 0);
    for _ in 1..WELCOME_PAGES {
        h.get_by_label("Next").click();
        h.run();
    }
    assert_eq!(h.state().0.page, WELCOME_PAGES - 1);
    h.get_by_label("Keyboard shortcuts");
    h.get_by_label("Ctrl+Shift+F");
    assert!(h.query_by_label("Next").is_none(), "no Next on the last slide");
    assert!(!h.state().1);
    h.get_by_label("Get started").click();
    h.run();
    assert!(h.state().1, "Get started closes the tour");
    set_lang(Lang::Ko);
}

/// Writes a PNG of the terminal drawing box drawing, blocks, Braille, sextants and symbol /
/// emoji fallbacks into `$RUXT_SHOTS`, to check that pictures join up and nothing falls back
/// to the missing-glyph box. Needs a GPU-capable wgpu backend:
/// `RUXT_SHOTS=<dir> cargo test render_terminal_glyphs -- --ignored`.
#[test]
#[ignore]
fn render_terminal_glyphs() {
    use super::terminal_widget::TermView;
    use crate::core::term::{Size, TermState};
    let Ok(dir) = std::env::var("RUXT_SHOTS") else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = egui::Context::default();
    let (term, _rx) = TermState::new(ctx, Size { cols: 64, rows: 16 }, 100, false);
    {
        let mut t = term.lock();
        let lines = [
            "╭──────┬──────╮ ┏━━━┓ ╔═══╦═══╗ ┌─┐",
            "│ box  │ ┼ ╋  │ ┃ ━ ┃ ║ ═ ╬ ║ ║ └─┘ ╱╲╳",
            "╰──────┴──────╯ ┗━━━┛ ╚═══╩═══╝ ╴╵╶╷",
            "▁▂▃▄▅▆▇█ ▏▎▍▌▋▊▉ ░▒▓ ▖▗▘▙▚▛▜▝▞▟ ▀▐▔▕",
            "⠀⠁⠃⠇⡇⣇⣧⣷⣿ ⢀⣠⣴⣾⣿⣷⣦⣄⡀  🬀🬁🬂🬃🬋🬎🬹🬻",
            "   ⢀⣀⣤⣤⣤⣀⡀      ▗▄▄▖      ",
            "  ⣴⣿⣿⣿⣿⣿⣿⣿⣦    ▟████▙     ",
            "  ⠻⣿⣿⣿⣿⣿⣿⣿⠟    ▜████▛     ",
            "   ⠈⠉⠛⠛⠛⠉⠁      ▝▀▀▘      ",
            "symbols: ★ ✓ ✗ ⚡ ☀ ☾ ⚙ ♥ → ⇒ ∞ ≈ ° © … • ● ◆ ◐ ◓",
            "emoji: 🪐 🚀 🌍 ✨ 🔥 ✅ ❌",
            "한글과 English 함께: 상태 ✓ 완료",
        ];
        for l in lines {
            t.advance(l.as_bytes());
            t.advance(b"\r\n");
        }
    }
    let mut view = TermView::new("glyph-shot");
    let mut fonts_installed = false;
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(1500.0, 700.0))
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .build_ui(move |ui| {
            if !std::mem::replace(&mut fonts_installed, true) {
                super::fonts::install(ui.ctx(), "");
            }
            let _ = view.show(ui, &term, 30.0, false, false);
        });
    for _ in 0..4 {
        harness.step();
    }
    harness
        .render()
        .expect("render")
        .save(format!("{dir}/terminal-glyphs.png"))
        .unwrap();
}
