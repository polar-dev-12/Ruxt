//! Secondary windows: session editor, screen lock, key generation, settings.

use super::kit;
use crate::core::config::{
    self, AppConfig, AuthMethod, HwAccel, Renderer, SecurityProfile, SessionProfile, Theme, TunnelKind, TunnelSpec,
};
use crate::core::i18n::{tr, trf, Lang};
use crate::core::theme::ThemeSpec;
use crate::core::{applock, vault};
use egui::{Color32, RichText, Ui};
use secrecy::SecretString;
use zeroize::Zeroize;

const ERR_COLOR: Color32 = Color32::from_rgb(0xe0, 0x60, 0x60);
const WARN_COLOR: Color32 = Color32::from_rgb(0xe0, 0xa0, 0x40);
const OK_COLOR: Color32 = Color32::from_rgb(0x60, 0xc0, 0x60);

// ---------------------------------------------------------------------------
// Session editor
// ---------------------------------------------------------------------------

#[derive(PartialEq, Eq)]
pub enum EditorResult {
    Open,
    Save,
    SaveAndConnect,
    Cancel,
}

pub struct SessionEditor {
    pub profile: SessionProfile,
    pub is_new: bool,
    pub password: String,
    /// Second entry of a new password, required when replacing a saved one.
    pub password_repeat: String,
    /// The saved password, typed again so a replacement is not a silent overwrite.
    pub current_password: String,
    pub passphrase: String,
    pub set_password: bool,
    /// Replace a password that is already saved. Requires `current_password`.
    pub change_password: bool,
    pub set_passphrase: bool,
    pub clear_password: bool,
    pub clear_passphrase: bool,
    pub has_saved_password: bool,
    pub has_saved_passphrase: bool,
    pub error: Option<String>,
    pub x11_check: Option<String>,
}

impl Drop for SessionEditor {
    fn drop(&mut self) {
        self.wipe_secrets();
    }
}

impl SessionEditor {
    pub fn new(profile: SessionProfile, is_new: bool, has_pw: bool, has_pp: bool) -> Self {
        Self {
            profile,
            is_new,
            password: String::new(),
            password_repeat: String::new(),
            current_password: String::new(),
            passphrase: String::new(),
            set_password: false,
            change_password: false,
            set_passphrase: false,
            clear_password: false,
            clear_passphrase: false,
            has_saved_password: has_pw,
            has_saved_passphrase: has_pp,
            error: None,
            x11_check: None,
        }
    }

    /// Drop every secret the dialog is holding. Used when the window locks.
    pub fn wipe_secrets(&mut self) {
        self.password.zeroize();
        self.password.clear();
        self.password_repeat.zeroize();
        self.password_repeat.clear();
        self.current_password.zeroize();
        self.current_password.clear();
        self.passphrase.zeroize();
        self.passphrase.clear();
        self.set_password = false;
        self.change_password = false;
        self.set_passphrase = false;
    }

    /// `sessions` are the other saved profiles (id, display name) offered as jump hosts.
    /// `cred_note` explains why passwords cannot be saved right now (`None`: they can).
    /// `stored_password` is the saved password for this session, borrowed for this frame so a
    /// change can be checked against it. It is not kept on the editor.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        cred_note: Option<&str>,
        sessions: &[(String, String)],
        stored_password: Option<&str>,
    ) -> EditorResult {
        let mut result = EditorResult::Open;
        let mut open = true;
        let title = if self.is_new {
            tr("New session")
        } else {
            tr("Edit session")
        };
        kit::window(ctx, title, egui::Id::unique("session-editor"), 560.0)
            .open(&mut open)
            .show(ctx, |ui| {
                kit::body(ui, ctx, |ui| {
                    self.form(ui, cred_note, sessions);
                });
                kit::divider(ui);
                if let Some(e) = &self.error {
                    ui.colored_label(ERR_COLOR, e);
                }
                kit::actions(ui, |ui| {
                    if kit::primary(ui, tr("Save & Connect")).clicked() && self.validate_with(stored_password) {
                        result = EditorResult::SaveAndConnect;
                    }
                    if kit::outlined(ui, tr("Save")).clicked() && self.validate_with(stored_password) {
                        result = EditorResult::Save;
                    }
                    if kit::text_button(ui, tr("Cancel")).clicked() {
                        result = EditorResult::Cancel;
                    }
                });
            });
        if !open {
            result = EditorResult::Cancel;
        }
        result
    }

    /// `stored_password` is the password already saved for this session (`None` if none).
    pub fn validate_with(&mut self, stored_password: Option<&str>) -> bool {
        let p = &self.profile;
        if p.host.trim().is_empty() {
            self.error = Some(tr("Host is required").into());
            return false;
        }
        if p.host.trim().chars().any(|c| c.is_whitespace() || c.is_control()) {
            self.error = Some(tr("Host contains invalid characters").into());
            return false;
        }
        if let AuthMethod::PublicKey { path } = &p.auth {
            if path.trim().is_empty() {
                self.error = Some(tr("Key file path is required for public-key auth").into());
                return false;
            }
        }
        for t in &p.tunnels {
            if t.bind_port == 0 || (t.kind != TunnelKind::Dynamic && t.target_host.trim().is_empty()) {
                self.error = Some(tr("Each tunnel needs a bind port and a target host").into());
                return false;
            }
        }
        if p.jump == p.id {
            self.error = Some(tr("A session cannot jump through itself").into());
            return false;
        }
        if self.change_password {
            if let Some(msg) = password_change_error(
                &self.current_password,
                &self.password,
                &self.password_repeat,
                stored_password,
            ) {
                self.error = Some(msg.into());
                return false;
            }
        }
        self.error = None;
        true
    }

    fn form(&mut self, ui: &mut Ui, cred_note: Option<&str>, sessions: &[(String, String)]) {
        let p = &mut self.profile;
        kit::section(ui, tr("Connection"), |ui| {
            egui::Grid::new("session-grid").num_columns(2).spacing([8.0, 6.0]).striped(true).show(ui, |ui| {
            ui.label(tr("Name"));
            ui.text_edit_singleline(&mut p.name);
            ui.end_row();
            ui.label(tr("Group"));
            ui.text_edit_singleline(&mut p.group);
            ui.end_row();
            ui.label(tr("Tags"));
            ui.add(egui::TextEdit::singleline(&mut p.tags).hint_text(tr("comma-separated, used by the filter")));
            ui.end_row();
            ui.label(tr("Host"));
            ui.text_edit_singleline(&mut p.host);
            ui.end_row();
            ui.label(tr("Port"));
            ui.add(egui::DragValue::new(&mut p.port).range(1..=65535));
            ui.end_row();
            ui.label(tr("Username"));
            ui.text_edit_singleline(&mut p.username);
            ui.end_row();

            ui.label(tr("Authentication"));
            ui.vertical(|ui| {
                let mut sel = match &p.auth {
                    AuthMethod::Auto => 0,
                    AuthMethod::Password => 1,
                    AuthMethod::PublicKey { .. } => 2,
                    AuthMethod::Agent => 3,
                    AuthMethod::KeyboardInteractive => 4,
                };
                let names = [
                    tr("Auto (agent → keys → password)"),
                    tr("Password"),
                    tr("Public key file"),
                    tr("SSH agent"),
                    tr("Keyboard-interactive"),
                ];
                egui::ComboBox::from_id_salt("auth-method")
                    .selected_text(names[sel])
                    .show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut sel, i, *n);
                        }
                    });
                let cur_path = if let AuthMethod::PublicKey { path } = &p.auth {
                    path.clone()
                } else {
                    String::new()
                };
                p.auth = match sel {
                    0 => AuthMethod::Auto,
                    1 => AuthMethod::Password,
                    2 => AuthMethod::PublicKey { path: cur_path },
                    3 => AuthMethod::Agent,
                    _ => AuthMethod::KeyboardInteractive,
                };
                if let AuthMethod::PublicKey { path } = &mut p.auth {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(path)
                                .desired_width(260.0)
                                .hint_text("~/.ssh/id_ed25519"),
                        );
                        if ui.button(tr("Browse…")).clicked() {
                            let mut d = rfd::FileDialog::new().set_title(tr("Private key"));
                            if let Some(h) = crate::core::hostkeys::home_dir() {
                                d = d.set_directory(h.join(".ssh"));
                            }
                            if let Some(f) = d.pick_file() {
                                *path = f.display().to_string();
                            }
                        }
                    });
                }
            });
            ui.end_row();

            ui.label(tr("Jump host"));
            {
                let current = sessions
                    .iter()
                    .find(|(id, _)| *id == p.jump)
                    .map(|(_, n)| n.as_str())
                    .unwrap_or(tr("(none: connect directly)"));
                egui::ComboBox::from_id_salt("jump-host")
                    .selected_text(current)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut p.jump, String::new(), tr("(none: connect directly)"));
                        for (id, name) in sessions.iter().filter(|(id, _)| *id != p.id) {
                            ui.selectable_value(&mut p.jump, id.clone(), name);
                        }
                    })
                    .response
                    .on_hover_text(tr(
                        "Connect through another saved session first (ProxyJump); its host key, credentials and tunnels are its own.",
                    ));
            }
            ui.end_row();

            ui.label(tr("Security profile"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut p.security, SecurityProfile::Strict, tr("Strict"))
                    .on_hover_text(tr("ML-KEM-768+X25519 / X25519 key exchange, AEAD ciphers only, no SHA-1. Recommended."));
                ui.radio_value(&mut p.security, SecurityProfile::Compatible, tr("Compatible"))
                    .on_hover_text(tr("Adds DH group exchange / group14+ and CTR ciphers for older servers. Still no SHA-1 anywhere, no CBC."));
                ui.radio_value(&mut p.security, SecurityProfile::Legacy, tr("Legacy"))
                    .on_hover_text(tr("Compatible plus ssh-rsa (RSA/SHA-1) host-key signatures for servers older than OpenSSH 7.2. Use only for a box that cannot be upgraded."));
            });
            if p.security == SecurityProfile::Legacy {
                ui.label("");
                ui.colored_label(WARN_COLOR, tr("! SHA-1 host-key signatures are enabled for this session"));
                ui.end_row();
            }
            ui.end_row();

            ui.label(tr("Startup command"));
            ui.text_edit_singleline(&mut p.startup_command);
            ui.end_row();
            ui.label(tr("Keepalive (s)"));
            ui.add(egui::DragValue::new(&mut p.keepalive_secs).range(0..=3600));
            ui.end_row();
            ui.label("TERM");
            ui.text_edit_singleline(&mut p.term_type);
            ui.end_row();
            ui.label(tr("Scrollback lines"));
            ui.add(egui::DragValue::new(&mut p.scrollback).range(100..=crate::core::config::MAX_SCROLLBACK))
                .on_hover_text(tr("Each 10k lines of history costs roughly 2–5 MB per tab at 100 columns."));
            ui.end_row();
            ui.label(tr("Compression"));
            ui.checkbox(&mut p.compression, tr("enable zlib (off = safest)"));
            ui.end_row();
        });
        });

        kit::section(ui, tr("Forwarding"), |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut p.x11_forwarding, tr("X11 forwarding"))
                    .on_hover_text(tr(
                        "Tunnels remote X11 clients to the bundled Cygwin/X server (untrusted mode by default).",
                    ));
                if p.x11_forwarding {
                    ui.label(tr("local display:"));
                    ui.add(
                        egui::TextEdit::singleline(&mut p.x11_display)
                            .desired_width(90.0)
                            .hint_text(":0"),
                    );
                    if ui.button(tr("Check X server")).clicked() {
                        let t = crate::core::x11::resolve(&p.x11_display);
                        let ok = std::net::TcpStream::connect_timeout(
                            &std::net::SocketAddr::from((
                                t.host
                                    .parse::<std::net::IpAddr>()
                                    .unwrap_or(std::net::Ipv4Addr::LOCALHOST.into()),
                                t.port,
                            )),
                            std::time::Duration::from_millis(300),
                        )
                        .is_ok();
                        self.x11_check = Some(if ok {
                            let cookie = if t.real_cookie.is_some() {
                                tr(", .Xauthority cookie found")
                            } else {
                                tr(", no .Xauthority cookie (server must allow local clients)")
                            };
                            trf(
                                "X server listening on {host}:{port} (display :{display}){cookie}",
                                &[
                                    ("{host}", &t.host),
                                    ("{port}", &t.port),
                                    ("{display}", &t.display_number),
                                    ("{cookie}", &cookie),
                                ],
                            )
                        } else {
                            trf(
                                "No X server on {host}:{port} — start one from the X menu first",
                                &[("{host}", &t.host), ("{port}", &t.port)],
                            )
                        });
                    }
                }
            });
            if p.x11_forwarding {
                ui.horizontal(|ui| {
                ui.checkbox(&mut p.x11_trusted, tr("Trusted (-Y)")).on_hover_text(tr(
                    "Off (default, like ssh -X): remote X clients get an untrusted cookie from the X SECURITY extension and cannot read other windows or snoop keyboard input. On (like ssh -Y): full access to the local display; needed by some toolkits and by X servers without the SECURITY extension.",
                ));
                if p.x11_trusted {
                    ui.colored_label(
                        WARN_COLOR,
                        tr("! remote X programs can read every window and all keyboard input on the local display"),
                    );
                }
            });
            }
            if let Some(msg) = &self.x11_check {
                ui.label(RichText::new(msg).small());
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut p.monitor, tr("Server status monitor"))
                    .on_hover_text(tr(
                        "Runs a small read-only script (hostname, uptime, /proc/stat, /proc/meminfo, df, who, ps) on this server every few seconds. Off by default: it shows up in the server's audit logs, so enable it only where that is acceptable.",
                    ));
                ui.add_enabled_ui(p.monitor, |ui| {
                    ui.label(tr("every"));
                    ui.add(
                        egui::DragValue::new(&mut p.monitor_interval_secs)
                            .range(0..=*crate::core::monitor::INTERVAL_RANGE.end())
                            .custom_formatter(|v, _| if v == 0.0 { tr("default").to_string() } else { format!("{v:.0} s") }),
                    )
                    .on_hover_text(tr("Seconds between polls for this server. 0 = the interval in Settings → Server status."));
                });
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut p.agent_forwarding, tr("Agent forwarding"));
                if p.agent_forwarding {
                    ui.colored_label(
                        WARN_COLOR,
                        tr("! a compromised server can use your keys while connected"),
                    );
                }
            });
        });

        kit::section(ui, tr("Tunnels"), |ui| {
            let mut remove: Option<usize> = None;
            for (i, t) in p.tunnels.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("tk", i))
                        .width(80.0)
                        .selected_text(match t.kind {
                            TunnelKind::Local => tr("Local"),
                            TunnelKind::Remote => tr("Remote"),
                            TunnelKind::Dynamic => tr("SOCKS5"),
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut t.kind, TunnelKind::Local, tr("Local"));
                            ui.selectable_value(&mut t.kind, TunnelKind::Remote, tr("Remote"));
                            ui.selectable_value(&mut t.kind, TunnelKind::Dynamic, tr("SOCKS5"));
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut t.bind_addr)
                            .desired_width(90.0)
                            .hint_text("127.0.0.1"),
                    );
                    ui.add(egui::DragValue::new(&mut t.bind_port).range(1..=65535));
                    if t.kind != TunnelKind::Dynamic {
                        ui.label("->");
                        ui.add(
                            egui::TextEdit::singleline(&mut t.target_host)
                                .desired_width(110.0)
                                .hint_text("host"),
                        );
                        ui.add(egui::DragValue::new(&mut t.target_port).range(1..=65535));
                    }
                    ui.checkbox(&mut t.auto_start, tr("auto"));
                    if ui.small_button("x").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                p.tunnels.remove(i);
            }
            if kit::outlined(ui, tr("+ add tunnel")).clicked() {
                p.tunnels.push(TunnelSpec::default());
            }
        });

        kit::section(ui, tr("Saved password"), |ui| {
            if let Some(note) = cred_note {
                ui.label(note);
            } else {
                if self.has_saved_password {
                    let was_changing = self.change_password;
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut self.change_password, tr("Change password"));
                        if !self.change_password {
                            ui.label(tr("(saved)"));
                            ui.checkbox(&mut self.clear_password, tr("clear"));
                        }
                    });
                    if was_changing && !self.change_password {
                        self.current_password.zeroize();
                        self.current_password.clear();
                        self.password.zeroize();
                        self.password.clear();
                        self.password_repeat.zeroize();
                        self.password_repeat.clear();
                    }
                    if self.change_password {
                        self.clear_password = false;
                        secret_field(ui, &mut self.current_password, tr("current password"));
                        secret_field(ui, &mut self.password, tr("new password"));
                        secret_field(ui, &mut self.password_repeat, tr("repeat"));
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.set_password, tr("Set password"));
                        if self.set_password {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.password)
                                    .password(true)
                                    .desired_width(200.0),
                            );
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.set_passphrase, tr("Set key passphrase"));
                    if self.set_passphrase {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.passphrase)
                                .password(true)
                                .desired_width(200.0),
                        );
                    } else if self.has_saved_passphrase {
                        ui.label(tr("(saved)"));
                        ui.checkbox(&mut self.clear_passphrase, tr("clear"));
                    }
                });
            }
        });
    }
}

fn secret_field(ui: &mut Ui, value: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(value)
            .password(true)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
}

/// Why a saved-password replacement cannot be stored, or `None` when it can.
fn password_change_error(current: &str, new_pw: &str, repeat: &str, stored: Option<&str>) -> Option<&'static str> {
    if current.is_empty() {
        return Some(tr("Enter the current password"));
    }
    match stored {
        Some(saved) if vault::secrets_eq(saved, current) => {}
        _ => return Some(tr("Wrong password")),
    }
    if new_pw.is_empty() {
        return Some(tr("Enter a new password"));
    }
    if new_pw != repeat {
        return Some(tr("Passwords do not match"));
    }
    None
}

// ---------------------------------------------------------------------------
// Old vault migration
// ---------------------------------------------------------------------------

/// Asks once for the master password of a vault written by an older version, so its saved
/// passwords can be re-encrypted under the screen-lock password.
pub struct MigrateDialog {
    pub pw: String,
    pub error: Option<String>,
    pub busy: bool,
    /// Put the cursor in the field once; grabbing it every frame would steal the keyboard
    /// from any other dialog drawn above this one.
    focus_pending: bool,
}

impl Drop for MigrateDialog {
    fn drop(&mut self) {
        self.pw.zeroize();
    }
}

pub enum MigrateResult {
    Open,
    Submit(SecretString),
    /// Throw the old saved passwords away and start an empty store.
    Discard,
    Cancel,
}

impl Default for MigrateDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl MigrateDialog {
    pub fn new() -> Self {
        Self {
            pw: String::new(),
            error: None,
            busy: false,
            focus_pending: true,
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> MigrateResult {
        let mut result = MigrateResult::Open;
        let modal = egui::Modal::new(egui::Id::unique("migrate-modal")).show(ctx, |ui| {
            kit::modal_body(ui, ctx, 440.0, |ui| {
                ui.heading(tr("Move saved passwords"));
                ui.label(tr(
                    "Passwords saved by an earlier Ruxt are locked with the old vault master password. Enter it once to re-encrypt them with the screen lock password; the master password is not used after that.",
                ));
                ui.add_space(6.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.pw)
                        .password(true)
                        .hint_text(tr("old master password"))
                        .desired_width(f32::INFINITY),
                );
                // Again after a failed attempt, which clears the field.
                if (std::mem::take(&mut self.focus_pending) || self.error.is_some()) && !self.busy && self.pw.is_empty() {
                    r.request_focus();
                }
                let enter = (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if let Some(e) = &self.error {
                    ui.colored_label(ERR_COLOR, e);
                }
                kit::actions(ui, |ui| {
                    let ok = !self.busy
                        && (ui.add_enabled_ui(!self.busy, |ui| kit::primary(ui, tr("Move"))).inner.clicked() || enter);
                    if ok && !self.pw.is_empty() {
                        result = MigrateResult::Submit(SecretString::from(self.pw.clone()));
                    }
                    if ui
                        .add_enabled_ui(!self.busy, |ui| kit::danger(ui, tr("Delete old passwords")))
                        .inner
                        .clicked()
                    {
                        result = MigrateResult::Discard;
                    }
                    if ui.add_enabled_ui(!self.busy, |ui| kit::text_button(ui, tr("Later"))).inner.clicked() {
                        result = MigrateResult::Cancel;
                    }
                });
                if self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Deriving key (Argon2id)…"));
                    });
                }
            });
        });
        if !self.busy && modal.should_close() {
            result = MigrateResult::Cancel;
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Key generation
// ---------------------------------------------------------------------------

pub struct KeygenDialog {
    pub path: String,
    pub comment: String,
    pub pp1: String,
    pub pp2: String,
    /// A passphrase made by the "Generate" button, shown once so it can be stored.
    pub generated: Option<String>,
    pub result: Option<Result<String, String>>,
}

impl Drop for KeygenDialog {
    fn drop(&mut self) {
        self.pp1.zeroize();
        self.pp2.zeroize();
        if let Some(g) = self.generated.as_mut() {
            g.zeroize();
        }
    }
}

/// 24 random characters from a 62-letter alphabet in four groups (~143 bits of entropy),
/// readable enough to type once into a password manager.
pub fn random_passphrase() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut out = String::with_capacity(27);
    for group in 0..4 {
        if group > 0 {
            out.push('-');
        }
        for _ in 0..6 {
            let i = rand::random_range(0..ALPHABET.len());
            out.push(ALPHABET[i] as char);
        }
    }
    out
}

impl KeygenDialog {
    pub fn new() -> Self {
        let path = crate::core::hostkeys::home_dir()
            .map(|h| h.join(".ssh").join("id_ed25519_ruxt").display().to_string())
            .unwrap_or_else(|| "id_ed25519_ruxt".into());
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        Self {
            path,
            comment: format!("{user}@Ruxt"),
            pp1: String::new(),
            pp2: String::new(),
            generated: None,
            result: None,
        }
    }

    /// Returns false when the window was closed.
    pub fn ui(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        kit::window(
            ctx,
            tr("Generate Ed25519 key pair"),
            egui::Id::unique("keygen-window"),
            520.0,
        )
        .open(&mut open)
        .show(ctx, |ui| {
            kit::body(ui, ctx, |ui| {
                egui::Grid::new("keygen-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(tr("Private key file"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(300.0));
                            if ui.button("...").clicked() {
                                if let Some(p) = rfd::FileDialog::new().set_file_name("id_ed25519_ruxt").save_file() {
                                    self.path = p.display().to_string();
                                }
                            }
                        });
                        ui.end_row();
                        ui.label(tr("Comment"));
                        ui.text_edit_singleline(&mut self.comment);
                        ui.end_row();
                        ui.label(tr("Passphrase"));
                        ui.add(egui::TextEdit::singleline(&mut self.pp1).password(true));
                        ui.end_row();
                        ui.label(tr("Repeat"));
                        ui.add(egui::TextEdit::singleline(&mut self.pp2).password(true));
                        ui.end_row();
                        ui.label("");
                        if kit::outlined(ui, tr("Generate random passphrase"))
                            .on_hover_text(tr("Fills both fields with 24 random characters and shows them once below so you can store them in a password manager or the vault."))
                            .clicked()
                        {
                            let p = random_passphrase();
                            self.pp1 = p.clone();
                            self.pp2 = p.clone();
                            self.generated = Some(p);
                        }
                        ui.end_row();
                    });
                if let Some(g) = &self.generated {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(g).monospace().strong());
                        if ui.small_button(tr("copy")).clicked() {
                            ui.ctx().copy_text(g.clone());
                        }
                    });
                    ui.colored_label(
                        WARN_COLOR,
                        tr("! Store this passphrase now; it is not shown again and the key is useless without it."),
                    );
                }
                kit::hint(ui, tr("A passphrase is required: the private key is written encrypted (bcrypt-pbkdf, OpenSSH format)."));
                if kit::primary(ui, tr("Generate")).clicked() {
                    if self.pp1.chars().count() < 8 {
                        self.result = Some(Err(tr("A passphrase of at least 8 characters is required").into()));
                    } else if self.pp1 != self.pp2 {
                        self.result = Some(Err(tr("Passphrases do not match").into()));
                    } else {
                        let pp = Some(SecretString::from(self.pp1.clone()));
                        let r = crate::core::keygen::generate_ed25519(
                            std::path::Path::new(&self.path),
                            &self.comment,
                            pp.as_ref(),
                        );
                        self.result = Some(r.map_err(|e| format!("{e:#}")));
                        if let Some(Ok(_)) = &self.result {
                            crate::core::audit::log("keygen", serde_json::json!({"path": self.path}));
                        }
                    }
                }
                match &self.result {
                    Some(Ok(pubkey)) => {
                        ui.colored_label(
                            OK_COLOR,
                            trf("Written {path} and {path}.pub", &[("{path}", &self.path)]),
                        );
                        ui.label(tr("Public key (add to ~/.ssh/authorized_keys on the server):"));
                        let mut s = pubkey.clone();
                        ui.add(
                            egui::TextEdit::multiline(&mut s)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        );
                        if ui.button(tr("Copy public key")).clicked() {
                            ui.ctx().copy_text(pubkey.clone());
                        }
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ERR_COLOR, e);
                    }
                    None => {}
                }
            });
        });
        open
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

pub enum SettingsAction {
    None,
    /// Move passwords saved under an old vault master password.
    MigrateVault,
    OpenAuditLog,
    OpenConfigDir,
    SetAppLock,
    ChangeAppLock,
    RemoveAppLock,
    LockApp,
    ClearHistory,
    /// Open the theme editor on a copy of the current colours.
    NewTheme,
    EditTheme(String),
    DeleteTheme(String),
    ImportTheme,
    ExportTheme(String),
}

/// English name of a built-in theme, translated. [`Theme::Custom`] is "Custom".
fn theme_label(theme: Theme) -> &'static str {
    match theme {
        Theme::DarkMatcha => tr("Matcha"),
        Theme::PurpleGreen => tr("Purple Green"),
        Theme::Harbor => tr("Harbor"),
        Theme::Ember => tr("Ember"),
        Theme::Ink => tr("Ink"),
        Theme::Pine => tr("Pine"),
        Theme::Light => tr("Light"),
        Theme::Porcelain => tr("Porcelain"),
        Theme::Sand => tr("Sand"),
        Theme::Mint => tr("Mint"),
        Theme::Rose => tr("Rose"),
        Theme::Lavender => tr("Lavender"),
        Theme::Custom => tr("Custom"),
    }
}

/// The font list is scanned on a background thread; `None` means "still scanning".
#[allow(clippy::too_many_arguments)]
pub fn settings_ui(
    ui: &mut Ui,
    cfg: &mut AppConfig,
    legacy_vault: bool,
    app_lock_set: bool,
    font_cache: Option<&[super::fonts::FontEntry]>,
    rescan_fonts: &mut bool,
    history_len: usize,
) -> SettingsAction {
    let mut action = SettingsAction::None;
    kit::section(ui, tr("Appearance"), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Theme"));
            egui::ComboBox::from_id_salt("builtin-theme")
                .selected_text(theme_label(cfg.theme))
                .show_ui(ui, |ui| {
                    ui.label(RichText::new(tr("Dark themes")).small().weak());
                    for t in Theme::dark_builtins() {
                        ui.selectable_value(&mut cfg.theme, *t, theme_label(*t));
                    }
                    ui.separator();
                    ui.label(RichText::new(tr("Light themes")).small().weak());
                    for t in Theme::light_builtins() {
                        ui.selectable_value(&mut cfg.theme, *t, theme_label(*t));
                    }
                });
        });
        ui.horizontal_wrapped(|ui| {
        let names: Vec<String> = cfg.custom_themes.iter().map(|t| t.name.clone()).collect();
        if !names.is_empty() {
            let shown = if names.contains(&cfg.custom_theme) {
                cfg.custom_theme.clone()
            } else {
                tr("(choose)").to_string()
            };
            egui::ComboBox::from_id_salt("custom-theme")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for n in &names {
                        if ui.selectable_value(&mut cfg.custom_theme, n.clone(), n).clicked() {
                            cfg.theme = Theme::Custom;
                        }
                    }
                });
        }
        if ui
            .button(tr("New theme…"))
            .on_hover_text(tr("Starts from the colours in use; pick every UI and terminal colour, name it, and it is saved with the settings."))
            .clicked()
        {
            action = SettingsAction::NewTheme;
        }
        let selected = names.contains(&cfg.custom_theme);
        if ui.add_enabled(selected, egui::Button::new(tr("Edit…"))).clicked() {
            action = SettingsAction::EditTheme(cfg.custom_theme.clone());
        }
        if ui
            .add_enabled(selected, egui::Button::new(tr("Export…")))
            .on_hover_text(tr("Writes the theme as a small JSON file (#rrggbb colours) you can share or edit by hand."))
            .clicked()
        {
            action = SettingsAction::ExportTheme(cfg.custom_theme.clone());
        }
        if ui.button(tr("Import…")).clicked() {
            action = SettingsAction::ImportTheme;
        }
        if ui.add_enabled(selected, egui::Button::new(tr("Delete"))).clicked() {
            action = SettingsAction::DeleteTheme(cfg.custom_theme.clone());
        }
    });
        ui.horizontal(|ui| {
            ui.label(tr("Language"));
            ui.radio_value(&mut cfg.language, Lang::Ko, Lang::Ko.label());
            ui.radio_value(&mut cfg.language, Lang::En, Lang::En.label());
        });
        egui::Grid::new("ui-size-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr("UI scale"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Slider::new(&mut cfg.ui_scale, config::UI_SCALE_RANGE)
                            .step_by(0.05)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                            .custom_parser(|s| {
                                s.trim()
                                    .trim_end_matches('%')
                                    .trim()
                                    .parse::<f64>()
                                    .ok()
                                    .map(|v| v / 100.0)
                            }),
                    )
                    .on_hover_text(tr(
                        "Enlarges or shrinks the whole window, terminal included. Applied when you release the mouse.",
                    ));
                    if ui.small_button(tr("reset")).clicked() {
                        cfg.ui_scale = 1.0;
                    }
                });
                ui.end_row();
                ui.label(tr("UI font size"));
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut cfg.ui_font_size, config::UI_FONT_SIZE_RANGE).step_by(0.5))
                        .on_hover_text(tr(
                            "Text size of menus, panels and dialogs. The terminal has its own font size below.",
                        ));
                    if ui.small_button(tr("reset")).clicked() {
                        cfg.ui_font_size = config::UI_FONT_SIZE_DEFAULT;
                    }
                });
                ui.end_row();
            });
    });
    kit::section(ui, tr("Graphics"), |ui| {
        egui::Grid::new("graphics-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label(tr("Renderer"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut cfg.renderer, Renderer::OpenGl, "OpenGL")
                        .on_hover_text(tr("Smallest footprint; works with every desktop driver."));
                    ui.radio_value(&mut cfg.renderer, Renderer::Wgpu, "wgpu (DirectX 12 / Vulkan / Metal)")
                        .on_hover_text(tr(
                            "Modern GPU API; use it when OpenGL is slow or broken (remote desktop, virtual machines).",
                        ));
                });
                ui.end_row();
                ui.label(tr("Hardware acceleration"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut cfg.hw_accel, HwAccel::Preferred, tr("Preferred"))
                        .on_hover_text(tr(
                            "Use the GPU when available, otherwise fall back to software rendering.",
                        ));
                    ui.radio_value(&mut cfg.hw_accel, HwAccel::Required, tr("Required"))
                        .on_hover_text(tr("Fail to start without a GPU context (OpenGL only)."));
                    ui.radio_value(&mut cfg.hw_accel, HwAccel::Off, tr("Off"))
                        .on_hover_text(tr("Software rendering only (OpenGL only)."));
                });
                ui.end_row();
                ui.label("VSync");
                ui.checkbox(&mut cfg.vsync, tr("synchronise with the display refresh rate"))
                    .on_hover_text(tr(
                        "Off lowers input latency on high-refresh displays at the cost of tearing.",
                    ));
                ui.end_row();
            });
        kit::hint(ui, tr("Graphics changes apply the next time Ruxt starts."));
    });
    kit::section(ui, tr("Terminal"), |ui| {
        egui::Grid::new("settings-grid").num_columns(2).spacing([8.0, 6.0]).striped(true).show(ui, |ui| {
        ui.label(tr("Font"));
        ui.horizontal(|ui| {
            let fonts = font_cache.unwrap_or(&[]);
            let current = if cfg.font_family.is_empty() {
                tr("(auto: system monospace)").to_string()
            } else {
                cfg.font_family.clone()
            };
            let filter_id = ui.scope_id().with("font-filter");
            let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
            egui::ComboBox::from_id_salt("font-family")
                .selected_text(current)
                .width(260.0)
                .show_ui(ui, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut filter)
                            .hint_text(tr("filter"))
                            .desired_width(f32::INFINITY),
                    );
                    let f = filter.to_lowercase();
                    ui.selectable_value(&mut cfg.font_family, String::new(), tr("(auto: system monospace)"));
                    if font_cache.is_none() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr("scanning system fonts…"));
                        });
                    }
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for e in fonts
                            .iter()
                            .filter(|e| f.is_empty() || e.family.to_lowercase().contains(&f))
                        {
                            let label = if e.monospace {
                                format!("{}  [mono]", e.family)
                            } else {
                                e.family.clone()
                            };
                            ui.selectable_value(&mut cfg.font_family, e.family.clone(), label);
                        }
                    });
                });
            ui.data_mut(|d| d.insert_temp(filter_id, filter));
            if font_cache.is_none() {
                ui.spinner();
            } else if ui
                .small_button(tr("rescan"))
                .on_hover_text(tr("Re-read the system font folders"))
                .clicked()
            {
                *rescan_fonts = true;
            }
        });
        ui.end_row();
        ui.label("");
        ui.label(
            RichText::new("The quick brown fox 0O1lI| {}[] 한글 日本語 — 선택한 글꼴 미리보기")
                .monospace()
                .size(cfg.font_size),
        );
        ui.end_row();
        ui.label(tr("Font size"));
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut cfg.font_size, config::TERM_FONT_SIZE_RANGE).step_by(1.0))
                .on_hover_text(tr("Also Ctrl+wheel or Ctrl+plus/minus in a terminal."));
            if ui.small_button(tr("reset")).clicked() {
                cfg.font_size = 14.0;
            }
        });
        ui.end_row();
        ui.label(tr("Local shell"));
        ui.add(egui::TextEdit::singleline(&mut cfg.local_shell).hint_text(crate::core::local_pty::default_shell()));
        ui.end_row();
        ui.label(tr("Copy on select"));
        ui.checkbox(&mut cfg.copy_on_select, "");
        ui.end_row();
        ui.label("Tail Viewer");
        ui.checkbox(
            &mut cfg.tail_viewer,
            tr("Open tail commands in a separate Tail Viewer window"),
        )
        .on_hover_text(tr(
            "When enabled, typing a command like 'tail -f -n 100 test.log' opens a dedicated Tail Viewer window while keeping the terminal prompt free.",
        ));
        ui.end_row();
        ui.label(tr("External editor"));
        external_editor_picker(ui, &mut cfg.external_editor);
        ui.end_row();
    });
    });
    kit::section(ui, tr("Command completion"), |ui| {
        ui.checkbox(&mut cfg.cmd_history, tr("Suggest previously typed commands while typing"))
        .on_hover_text(tr(
            "Submitted command lines are kept in history.db, encrypted with the screen lock password, and offered as ghost text: → accepts, Ctrl+↑/↓ chooses, Esc hides. Only text the program echoed is recorded, so password prompts never are; lines that look like they carry a secret, or start with a space, are skipped too.",
        ));
        if !app_lock_set {
            ui.colored_label(
                WARN_COLOR,
                tr("Needs a screen lock password (Settings → Security): the history is encrypted with it."),
            );
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(trf("{n} commands stored", &[("{n}", &history_len)]))
                    .small()
                    .weak(),
            );
            if ui
                .add_enabled(
                    history_len > 0 || !app_lock_set,
                    egui::Button::new(tr("Clear command history")),
                )
                .clicked()
            {
                action = SettingsAction::ClearHistory;
            }
        });
    });
    kit::section(ui, tr("Server status"), |ui| {
        ui.checkbox(&mut cfg.server_monitor, tr("Monitor connected servers (CPU, memory, disk, load)"))
        .on_hover_text(tr(
            "Runs a small read-only script (hostname, uptime, /proc/stat, /proc/meminfo, df, /proc/net/dev) over the existing SSH session; the result is shown in the status bar and the Server info window. Servers without a POSIX shell are detected and left alone.",
        ));
        ui.horizontal(|ui| {
            ui.label(tr("Poll every"));
            ui.add(
                egui::DragValue::new(&mut cfg.monitor_interval_secs)
                    .range(crate::core::monitor::INTERVAL_RANGE)
                    .suffix(" s"),
            );
        });
    });
    kit::section(ui, tr("X server"), |ui| {
        kit::hint(ui, tr(
        "Ruxt starts the bundled Cygwin/X (XWin.exe) with a random MIT-MAGIC-COOKIE-1 authority file; it never uses -ac. VcXsrv and Xming are no longer supported.",
    ));
        egui::Grid::new("xserver-grid").num_columns(2).spacing([8.0, 6.0]).striped(true).show(ui, |ui| {
        ui.label(tr("Server executable"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut cfg.x11_server_path)
                    .hint_text(tr("auto-detect"))
                    .desired_width(220.0),
            );
            if ui.button("...").clicked() {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter(tr("Executable"), &["exe"])
                    .pick_file()
                {
                    cfg.x11_server_path = f.display().to_string();
                }
            }
        });
        ui.end_row();
        ui.label(tr("Auto-start"));
        ui.checkbox(&mut cfg.x11_auto_start, tr("when a session with X11 forwarding connects"));
        ui.end_row();
        ui.label(tr("At launch"));
        ui.checkbox(&mut cfg.x11_start_on_launch, tr("start the bundled X server when Ruxt starts"))
            .on_hover_text(tr("Hidden console, random cookie, first free display. Applies at the next start."));
        ui.end_row();
        ui.label(tr("On exit"));
        ui.checkbox(&mut cfg.x11_stop_on_exit, tr("stop the X server Ruxt started"));
        ui.end_row();
        ui.label(tr("Clipboard"));
        ui.checkbox(&mut cfg.x11_clipboard, tr("share the local clipboard with remote X clients"))
            .on_hover_text(tr("Off by default: with it on, any forwarded X program (xclip, xsel, GUI apps) can read and write your clipboard — the same exposure OSC 52 is blocked for. Applies to the next server start."));
        ui.end_row();
    });
    });
    kit::section(ui, tr("Security"), |ui| {
        ui.checkbox(
            &mut cfg.confirm_multiline_paste,
            tr("Confirm before pasting multi-line text"),
        )
        .on_hover_text(tr(
            "Defends against clipboard injection: a hidden newline would otherwise execute immediately.",
        ));
        ui.checkbox(
            &mut cfg.osc52_clipboard,
            tr("Allow remote programs to write the clipboard (OSC 52)"),
        )
        .on_hover_text(tr(
            "Off by default: a hostile remote could plant text in your clipboard.",
        ));
        if crate::core::agent_guard::SUPPORTED {
            ui.checkbox(&mut cfg.agent_guard, tr("Agent guard"))
                .on_hover_text(tr("Hide Ruxt from screenshots, recording and screen sharing, and block keyboard / mouse input sent by programs (AI agents, macros). Blocked input locks the window when a screen lock password is set.\n\nAlso blocks password-manager auto-type and on-screen keyboards. Not a hard boundary: programs running as your Windows user can still read Ruxt's files and memory."));
        }
        ui.checkbox(
            &mut cfg.allow_public_bind,
            tr("Allow tunnels to bind non-loopback addresses"),
        )
        .on_hover_text(tr(
            "Off by default: local/dynamic tunnels are only reachable from this machine.",
        ));
        ui.checkbox(
            &mut cfg.confirm_close_session,
            tr("Ask before closing a connected session or quitting with live sessions"),
        )
        .on_hover_text(tr("The confirmation offers \"Don't ask again\", which turns this off."));
        ui.horizontal(|ui| {
            ui.label(tr("Clear the clipboard"));
            ui.add(
                egui::DragValue::new(&mut cfg.clipboard_clear_secs)
                    .range(0..=600)
                    .suffix(tr(" s")),
            )
            .on_hover_text(tr(
                "Seconds after Ruxt copies something (terminal selection, fingerprint, OSC 52) until it is wiped again, if the clipboard still holds it. Protects against clipboard history and cloud clipboard sync. 0 = never.",
            ));
            ui.label(tr("after copying (0 = never)"));
        });
        ui.horizontal(|ui| {
            ui.label(tr("Disconnect all sessions after"));
            ui.add(
                egui::DragValue::new(&mut cfg.idle_disconnect_minutes)
                    .range(0..=1440)
                    .suffix(tr(" min")),
            )
            .on_hover_text(tr(
                "Idle time (no keyboard or mouse in Ruxt) after which every SSH session is closed. 0 = never.",
            ));
            ui.label(tr("idle (0 = never)"));
        });
        ui.checkbox(&mut cfg.audit_log, tr("Keep an audit log"))
            .on_hover_text(tr(
                "Records connections, authentication, host keys, tunnels, transfers and lock events as plain-text JSON lines (no passwords or terminal contents) in a file only you can read. Turning it off stops new entries; the existing file stays until you delete it.",
            ));
        ui.horizontal(|ui| {
            ui.label(tr("Keep command history for"));
            ui.add(
                egui::DragValue::new(&mut cfg.history_days)
                    .range(0..=3650)
                    .suffix(tr(" days")),
            );
            ui.label(tr("and audit log for"));
            ui.add(
                egui::DragValue::new(&mut cfg.audit_days)
                    .range(0..=3650)
                    .suffix(tr(" days")),
            );
            ui.label(tr("(0 = forever; applied at start)"));
        });
    });
    kit::section(ui, tr("Screen lock"), |ui| {
        kit::hint(ui, tr(
        "Locks the whole window after a period of inactivity; sessions keep running behind the lock screen. The same password encrypts saved session passwords and key passphrases (Argon2id + XChaCha20-Poly1305), which are only usable while Ruxt is unlocked. Without it nothing is saved.",
    ));
        if !app_lock_set {
            ui.label(tr("No screen lock password set."));
            if ui.button(tr("Set screen lock password…")).clicked() {
                action = SettingsAction::SetAppLock;
            }
        } else {
            ui.horizontal(|ui| {
                ui.label(tr("Lock after"));
                ui.add(
                    egui::Slider::new(&mut cfg.app_lock_minutes, 0..=applock::MAX_MINUTES)
                        .step_by(applock::STEP_MINUTES as f64)
                        .custom_formatter(|v, _| {
                            if v == 0.0 {
                                tr("off").to_string()
                            } else {
                                trf("{n} min", &[("{n}", &format!("{v:.0}"))])
                            }
                        }),
                )
                .on_hover_text(tr("0 = never lock; otherwise in 10-minute steps."));
            });
            cfg.app_lock_minutes = applock::normalize_minutes(cfg.app_lock_minutes);
            ui.horizontal(|ui| {
                if ui.button(tr("Lock now")).clicked() {
                    action = SettingsAction::LockApp;
                }
                if ui.button(tr("Change screen lock password…")).clicked() {
                    action = SettingsAction::ChangeAppLock;
                }
                if ui.button(tr("Remove screen lock…")).clicked() {
                    action = SettingsAction::RemoveAppLock;
                }
            });
            if legacy_vault {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        WARN_COLOR,
                        tr("Passwords saved by an earlier version are not moved yet."),
                    );
                    if ui.button(tr("Move saved passwords…")).clicked() {
                        action = SettingsAction::MigrateVault;
                    }
                });
            }
        }
    });
    kit::section(ui, tr("Files"), |ui| {
        ui.horizontal(|ui| {
            if kit::outlined(ui, tr("Open audit log")).clicked() {
                action = SettingsAction::OpenAuditLog;
            }
            if kit::outlined(ui, tr("Open config folder")).clicked() {
                action = SettingsAction::OpenConfigDir;
            }
        });
    });
    action
}

// ---------------------------------------------------------------------------
// App lock
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppLockMode {
    /// Choose a password. `first_run` adds a "not now" button and an explanation.
    Setup { first_run: bool },
    /// The lock screen.
    Unlock,
    /// Current password + new password.
    Change,
    /// Current password, then the lock file is removed.
    Remove,
}

pub struct AppLockDialog {
    pub mode: AppLockMode,
    pub current: String,
    pub pw1: String,
    pub pw2: String,
    pub error: Option<String>,
    pub busy: bool,
}

impl Drop for AppLockDialog {
    fn drop(&mut self) {
        self.current.zeroize();
        self.pw1.zeroize();
        self.pw2.zeroize();
    }
}

pub enum AppLockResult {
    Open,
    Cancel,
    /// First-run prompt declined: leave the lock off and do not ask again.
    Skip,
    /// `current` is set for Unlock/Change/Remove, `new` for Setup/Change.
    Submit {
        current: Option<SecretString>,
        new: Option<SecretString>,
    },
}

impl AppLockDialog {
    pub fn new(mode: AppLockMode) -> Self {
        Self {
            mode,
            current: String::new(),
            pw1: String::new(),
            pw2: String::new(),
            error: None,
            busy: false,
        }
    }

    pub fn clear_fields(&mut self) {
        self.current.zeroize();
        self.pw1.zeroize();
        self.pw2.zeroize();
        self.current.clear();
        self.pw1.clear();
        self.pw2.clear();
    }

    /// Draw the dialog. For `Unlock` the caller has already blanked the rest of the window;
    /// the lock screen and the first-run prompt cannot be dismissed by clicking outside.
    pub fn ui(&mut self, ctx: &egui::Context) -> AppLockResult {
        let mut result = AppLockResult::Open;
        let (title, text) = match self.mode {
            AppLockMode::Setup { first_run: true } => (
                tr("Protect Ruxt with a password?"),
                tr("When you leave Ruxt idle it locks itself; the password is then needed to get back to your sessions. The same password encrypts the session passwords and key passphrases you choose to save — without it Ruxt saves none. It cannot be recovered. Change the idle time (10-minute steps) or turn the lock off in Settings → Security."),
            ),
            AppLockMode::Setup { first_run: false } => (
                tr("Set screen lock password"),
                tr("Choose the password that unlocks Ruxt. It also encrypts saved passwords and key passphrases, and cannot be recovered."),
            ),
            AppLockMode::Unlock => (
                tr("Ruxt is locked"),
                tr("Enter the screen lock password to continue. It also opens your saved passwords. Open sessions keep running while Ruxt is locked."),
            ),
            AppLockMode::Change => (tr("Change screen lock password"), tr("Enter the current password and a new one.")),
            AppLockMode::Remove => (
                tr("Remove screen lock"),
                tr("Enter the current password to remove the lock. Saved passwords, key passphrases and the command history are deleted with it."),
            ),
        };
        let wants_current = matches!(
            self.mode,
            AppLockMode::Unlock | AppLockMode::Change | AppLockMode::Remove
        );
        let wants_new = matches!(self.mode, AppLockMode::Setup { .. } | AppLockMode::Change);
        let modal = egui::Modal::new(egui::Id::unique("applock-modal")).show(ctx, |ui| {
            kit::modal_body(ui, ctx, 420.0, |ui| {
                ui.heading(title);
                ui.label(text);
                ui.add_space(6.0);
                let mut enter = false;
                let mut first_focus = true;
                if wants_current {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.current)
                            .password(true)
                            .hint_text(if self.mode == AppLockMode::Unlock {
                                tr("password")
                            } else {
                                tr("current password")
                            })
                            .desired_width(f32::INFINITY),
                    );
                    if first_focus && !self.busy && self.current.is_empty() && self.pw1.is_empty() {
                        r.request_focus();
                    }
                    first_focus = false;
                    enter |= (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
                }
                if wants_new {
                    let r1 = ui.add(
                        egui::TextEdit::singleline(&mut self.pw1)
                            .password(true)
                            .hint_text(tr("new password"))
                            .desired_width(f32::INFINITY),
                    );
                    if first_focus && !self.busy && self.pw1.is_empty() && self.pw2.is_empty() {
                        r1.request_focus();
                    }
                    let r2 = ui.add(
                        egui::TextEdit::singleline(&mut self.pw2)
                            .password(true)
                            .hint_text(tr("repeat"))
                            .desired_width(f32::INFINITY),
                    );
                    enter |= (r1.lost_focus() || r1.has_focus() || r2.lost_focus() || r2.has_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let bits = vault::password_entropy_bits(&self.pw1);
                    let (txt, col) = if bits < 30.0 {
                        (tr("weak"), ERR_COLOR)
                    } else if bits < 60.0 {
                        (tr("fair"), WARN_COLOR)
                    } else {
                        (tr("strong"), OK_COLOR)
                    };
                    ui.horizontal(|ui| {
                        ui.label(tr("Strength:"));
                        ui.colored_label(
                            col,
                            trf(
                                "{txt} (~{bits} bits)",
                                &[("{txt}", &txt), ("{bits}", &format!("{bits:.0}"))],
                            ),
                        );
                    });
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ERR_COLOR, e);
                }
                kit::actions(ui, |ui| {
                    let label = match self.mode {
                        AppLockMode::Unlock => tr("Unlock"),
                        AppLockMode::Remove => tr("Remove and delete saved passwords"),
                        _ => tr("OK"),
                    };
                    let ok = !self.busy
                        && (ui
                            .add_enabled_ui(!self.busy, |ui| kit::primary(ui, label))
                            .inner
                            .clicked()
                            || enter);
                    if ok {
                        if wants_new && self.pw1 != self.pw2 {
                            self.error = Some(tr("Passwords do not match").into());
                        } else if wants_new && self.pw1.chars().count() < applock::MIN_LEN {
                            self.error = Some(trf("Use at least {n} characters", &[("{n}", &applock::MIN_LEN)]));
                        } else if wants_current && self.current.is_empty() {
                            self.error = Some(tr("Enter the current password").into());
                        } else {
                            result = AppLockResult::Submit {
                                current: wants_current.then(|| SecretString::from(self.current.clone())),
                                new: wants_new.then(|| SecretString::from(self.pw1.clone())),
                            };
                        }
                    }
                    match self.mode {
                        AppLockMode::Unlock => {}
                        AppLockMode::Setup { first_run: true } => {
                            if ui
                                .add_enabled_ui(!self.busy, |ui| kit::text_button(ui, tr("Not now")))
                                .inner
                                .on_hover_text(tr(
                                    "Leaves the lock off; passwords are then never saved. You can set a password later in Settings → Security.",
                                ))
                                .clicked()
                            {
                                result = AppLockResult::Skip;
                            }
                        }
                        _ => {
                            if ui
                                .add_enabled_ui(!self.busy, |ui| kit::text_button(ui, tr("Cancel")))
                                .inner
                                .clicked()
                            {
                                result = AppLockResult::Cancel;
                            }
                        }
                    }
                });
                if self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Checking password (Argon2id)…"));
                    });
                }
            });
        });
        // Only Setup (from Settings), Change and Remove may be dismissed by clicking outside;
        // the lock screen and the first-run prompt stay until answered.
        if !self.busy
            && modal.should_close()
            && matches!(
                self.mode,
                AppLockMode::Setup { first_run: false } | AppLockMode::Change | AppLockMode::Remove
            )
        {
            result = AppLockResult::Cancel;
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Theme editor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeEditorResult {
    Open,
    Save,
    Cancel,
}

/// Edits a [`ThemeSpec`] with live preview; the app applies `spec` whenever `changed` is set.
pub struct ThemeEditor {
    pub spec: ThemeSpec,
    /// Name of the custom theme being edited (`None` = a new one).
    pub original: Option<String>,
    pub error: Option<String>,
    /// Set on every edit; the app clears it after applying the preview.
    pub changed: bool,
}

const ANSI_NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

impl ThemeEditor {
    pub fn new(spec: ThemeSpec, original: Option<String>) -> Self {
        Self {
            spec,
            original,
            error: None,
            changed: true,
        }
    }

    fn color_row(ui: &mut Ui, label: &str, c: &mut crate::core::theme::Rgb8, changed: &mut bool) {
        ui.label(label);
        ui.horizontal(|ui| {
            if ui.color_edit_button_srgb(&mut c.0).changed() {
                *changed = true;
            }
            ui.label(RichText::new(c.to_string()).monospace().weak());
        });
        ui.end_row();
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> ThemeEditorResult {
        let mut result = ThemeEditorResult::Open;
        let mut open = true;
        let max = kit::max_window_size(ctx);
        kit::window(ctx, tr("Theme editor"), egui::Id::unique("theme-editor"), 600.0)
            .open(&mut open)
            .show(ctx, |ui| {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(tr("Name"));
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.spec.name).desired_width(220.0))
                        .changed()
                    {
                        changed = true;
                    }
                    if ui.checkbox(&mut self.spec.dark, tr("dark base")).changed() {
                        changed = true;
                    }
                    ui.menu_button(tr("Start from:"), |ui| {
                        for t in Theme::dark_builtins().iter().chain(Theme::light_builtins()) {
                            if ui.button(theme_label(*t)).clicked() {
                                let name = std::mem::take(&mut self.spec.name);
                                self.spec = t.builtin_spec().expect("built-in theme");
                                self.spec.name = name;
                                changed = true;
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height((max.y - 150.0).max(120.0))
                    .show(ui, |ui| {
                        ui.columns(2, |cols| {
                            cols[0].label(RichText::new(tr("Interface")).strong());
                            egui::Grid::new("theme-ui-grid")
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(&mut cols[0], |ui| {
                                    let s = &mut self.spec;
                                    Self::color_row(ui, tr("Panels"), &mut s.panel, &mut changed);
                                    Self::color_row(ui, tr("Windows"), &mut s.window, &mut changed);
                                    Self::color_row(ui, tr("Text fields"), &mut s.field, &mut changed);
                                    Self::color_row(ui, tr("Striped rows"), &mut s.faint, &mut changed);
                                    Self::color_row(ui, tr("Buttons"), &mut s.widget, &mut changed);
                                    Self::color_row(ui, tr("Buttons (hover)"), &mut s.widget_hover, &mut changed);
                                    Self::color_row(ui, tr("Buttons (pressed)"), &mut s.widget_active, &mut changed);
                                    Self::color_row(ui, tr("Text"), &mut s.text, &mut changed);
                                    Self::color_row(ui, tr("Secondary text"), &mut s.weak_text, &mut changed);
                                    Self::color_row(ui, tr("Accent"), &mut s.accent, &mut changed);
                                    Self::color_row(ui, tr("Selection"), &mut s.selection, &mut changed);
                                    Self::color_row(ui, tr("Links"), &mut s.hyperlink, &mut changed);
                                    Self::color_row(ui, tr("Warning"), &mut s.warn, &mut changed);
                                    Self::color_row(ui, tr("Error"), &mut s.error, &mut changed);
                                    Self::color_row(ui, tr("Borders"), &mut s.stroke, &mut changed);
                                });
                            cols[1].label(RichText::new(tr("Terminal")).strong());
                            egui::Grid::new("theme-term-grid")
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(&mut cols[1], |ui| {
                                    let s = &mut self.spec;
                                    Self::color_row(ui, tr("Text"), &mut s.term_fg, &mut changed);
                                    Self::color_row(ui, tr("Background"), &mut s.term_bg, &mut changed);
                                    Self::color_row(ui, tr("Cursor"), &mut s.term_cursor, &mut changed);
                                });
                            cols[1].label(RichText::new(tr("ANSI colours (normal / bright)")).small());
                            for row in 0..2 {
                                cols[1].horizontal(|ui| {
                                    for (i, name) in ANSI_NAMES.iter().enumerate() {
                                        let idx = row * 8 + i;
                                        let r = ui.color_edit_button_srgb(&mut self.spec.ansi[idx].0);
                                        let r = r.on_hover_text(if row == 0 {
                                            name.to_string()
                                        } else {
                                            format!("bright {name}")
                                        });
                                        if r.changed() {
                                            changed = true;
                                        }
                                    }
                                });
                            }
                            cols[1].add_space(6.0);
                            self.preview(&mut cols[1]);
                        });
                    });
                kit::divider(ui);
                if let Some(e) = &self.error {
                    ui.colored_label(ERR_COLOR, e);
                }
                kit::hint(ui, tr("Changes preview live; Cancel restores the previous theme."));
                kit::actions(ui, |ui| {
                    if kit::primary(ui, tr("Save")).clicked() {
                        result = ThemeEditorResult::Save;
                    }
                    if kit::text_button(ui, tr("Cancel")).clicked() {
                        result = ThemeEditorResult::Cancel;
                    }
                });
                if changed {
                    self.changed = true;
                }
            });
        if !open {
            result = ThemeEditorResult::Cancel;
        }
        result
    }

    /// A painted sample of the terminal colours.
    fn preview(&self, ui: &mut Ui) {
        let s = &self.spec;
        let font = egui::FontId::monospace(12.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), 92.0), egui::Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, s.term_bg.color32());
        let line_h = 16.0;
        let mut y = rect.min.y + 6.0;
        let x = rect.min.x + 8.0;
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            "user@host:~$ ls -la --color",
            font.clone(),
            s.term_fg.color32(),
        );
        y += line_h;
        let mut cx = x;
        for (i, name) in ["drwxr-xr-x", "docs/", "run.sh", "notes.txt"].iter().enumerate() {
            let c = [s.ansi[4], s.ansi[12], s.ansi[2], s.term_fg][i];
            let g = p.layout_no_wrap(format!("{name}  "), font.clone(), c.color32());
            let w = g.size().x;
            p.galley(egui::pos2(cx, y), g, c.color32());
            cx += w;
        }
        y += line_h;
        cx = x;
        for c in &s.ansi {
            let r = egui::Rect::from_min_size(egui::pos2(cx, y + 2.0), egui::vec2(12.0, 12.0));
            p.rect_filled(r, 2.0, c.color32());
            cx += 14.0;
        }
        y += line_h;
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            "user@host:~$ ",
            font.clone(),
            s.term_fg.color32(),
        );
        let cur = egui::Rect::from_min_size(egui::pos2(x + 13.0 * 7.2, y), egui::vec2(7.0, 14.0));
        p.rect_filled(cur, 0.0, s.term_cursor.color32());
    }
}

// ---------------------------------------------------------------------------
// Help & about
// ---------------------------------------------------------------------------

const REPO: &str = env!("CARGO_PKG_REPOSITORY");

/// Body of the help window: header, documentation actions, security properties, shortcuts.
pub fn about_ui(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Ruxt").size(24.0).strong());
        kit::chip(ui, &format!("v{}", env!("CARGO_PKG_VERSION")));
    });
    ui.label(RichText::new(tr("Security-first SSH / SFTP workstation in pure Rust.")).weak());
    ui.add_space(kit::GRID);
    ui.horizontal_wrapped(|ui| {
        let open = |ui: &Ui, path: &str| ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{REPO}{path}")));
        if kit::primary(ui, tr("User guide")).clicked() {
            open(ui, "/blob/main/docs/USER_GUIDE.md");
        }
        if kit::outlined(ui, tr("Threat model")).clicked() {
            open(ui, "/blob/main/docs/THREAT_MODEL.md");
        }
        if kit::text_button(ui, tr("Check for updates"))
            .on_hover_text(tr("Opens the release page in your browser"))
            .clicked()
        {
            open(ui, "/releases/latest");
        }
    });
    ui.add_space(kit::GRID * 1.5);

    kit::section(ui, tr("Security properties"), |ui| {
        kit::list_item(
            ui,
            tr("Memory-safe Rust"),
            tr("#![forbid(unsafe_code)]; SSH via russh, cryptography via RustCrypto / ring."),
        );
        kit::list_item(
            ui,
            tr("Pinned host keys"),
            tr("Trust on first use; a changed key blocks the connection."),
        );
        kit::list_item(
            ui,
            tr("Strict algorithms"),
            tr("ML-KEM-768+X25519 / X25519 key exchange, ChaCha20-Poly1305 and AES-GCM only; no SHA-1, no CBC."),
        );
        kit::list_item(
            ui,
            tr("Encrypted saved passwords"),
            tr("Saved passwords are encrypted with the screen lock password (Argon2id + XChaCha20-Poly1305), usable only while Ruxt is unlocked and wiped from memory when it locks."),
        );
        kit::list_item(
            ui,
            tr("Loopback-only tunnels"),
            tr("Tunnels bind loopback by default; SOCKS5 is CONNECT-only; agent and X11 forwarding are opt-in."),
        );
        kit::list_item(
            ui,
            tr("Clipboard and output hygiene"),
            tr("OSC 52 off by default, multi-line paste confirmation, sanitised window titles and banners."),
        );
        kit::list_item(
            ui,
            tr("Audit log"),
            tr("Optional JSON audit log of security-relevant events (off by default; Settings → Security)."),
        );
    });

    kit::section(ui, tr("Shortcuts"), |ui| shortcuts_grid(ui, "about-shortcuts"));
}

/// External editor setting: a drop-down of "OS default", the editors installed here and
/// "Custom…", which shows a command field and a file picker.
fn external_editor_picker(ui: &mut Ui, editor: &mut String) {
    let found = crate::core::platform::installed_editors();
    let custom_id = egui::Id::unique("external-editor-custom");
    let mut custom = ui.data(|d| d.get_temp::<bool>(custom_id)).unwrap_or(false);
    let current = editor.trim().to_string();
    let matched = found.iter().find(|e| e.command == current);
    // A command that is neither empty nor a detected editor can only be shown as custom.
    if !current.is_empty() && matched.is_none() {
        custom = true;
    }
    let selected = if custom {
        tr("Custom…").to_string()
    } else {
        matched
            .map(|e| e.name.to_string())
            .unwrap_or_else(|| tr("OS default").to_string())
    };
    ui.vertical(|ui| {
        egui::ComboBox::from_id_salt("external-editor")
            .selected_text(selected)
            .width(260.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(!custom && current.is_empty(), tr("OS default")).clicked() {
                    editor.clear();
                    custom = false;
                }
                for e in found {
                    if ui.selectable_label(!custom && e.command == current, e.name).on_hover_text(&e.command).clicked() {
                        *editor = e.command.clone();
                        custom = false;
                    }
                }
                if ui.selectable_label(custom, tr("Custom…")).clicked() {
                    custom = true;
                }
            })
            .response
            .on_hover_text(tr(
                "Used by SFTP \"Edit\". With no editor set, the OS default handler is used and executable/script file types are refused (they would run, not open).",
            ));
        if custom {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(editor)
                        .hint_text(tr("e.g. \"C:\\Program Files\\Editor\\editor.exe\" --wait"))
                        .desired_width(200.0),
                );
                if ui.button(tr("Browse…")).clicked() {
                    if let Some(f) = rfd::FileDialog::new().pick_file() {
                        *editor = format!("\"{}\"", f.display());
                    }
                }
            });
        }
    });
    ui.data_mut(|d| d.insert_temp(custom_id, custom));
}

/// The keyboard reference shown in Help & about and on the last welcome slide.
fn shortcuts_grid(ui: &mut Ui, id: &str) {
    let rows: [(&str, &[&str]); 10] = [
        (tr("Copy"), &["Ctrl+C", "Ctrl+Shift+C", "Ctrl+Insert"]),
        (
            tr("Paste"),
            &["Ctrl+V", "Ctrl+Shift+V", "Shift+Insert", tr("Right-click")],
        ),
        (tr("Find in terminal"), &["Ctrl+Shift+F"]),
        (
            tr("Scroll back"),
            &["Shift+PgUp", "Shift+PgDn", "Shift+Home", "Shift+End"],
        ),
        (tr("Font size"), &[tr("Ctrl+wheel"), "Ctrl +", "Ctrl -"]),
        (tr("Accept completion"), &["→"]),
        (tr("Choose / hide completion"), &["Ctrl+↑", "Ctrl+↓", "Esc"]),
        (tr("Open a link"), &[tr("Ctrl+click")]),
        (tr("Reconnect a closed session"), &["Enter"]),
        (
            tr("SFTP: up / open / rename / delete / refresh"),
            &["Backspace", "Enter", "F2", "Delete", "F5"],
        ),
    ];
    // A fixed-width action column and a key column that wraps whole keycaps, on alternating
    // row backgrounds. (A Grid gives the key column no width bound, so keycaps got squeezed.)
    ui.push_id(id, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let label_w = (ui.available_width() * 0.42).clamp(120.0, 240.0);
        for (i, (action, keys)) in rows.into_iter().enumerate() {
            let fill = if i % 2 == 0 {
                ui.visuals().faint_bg_color
            } else {
                egui::Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(label_w, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(label_w);
                                ui.add(egui::Label::new(action).wrap());
                            },
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 0.0),
                            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                            |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(kit::GRID * 0.5, kit::GRID * 0.5);
                                for k in keys {
                                    kit::keycap(ui, k);
                                }
                            },
                        );
                    });
                });
        }
    });
}

// ---------------------------------------------------------------------------
// Welcome tour
// ---------------------------------------------------------------------------

/// Number of welcome slides.
pub const WELCOME_PAGES: usize = 6;

/// First-run tour: one slide per area of the app, then the keyboard shortcuts. Arrow keys
/// turn the pages; it can be opened again from Help & about.
pub struct WelcomeDialog {
    pub page: usize,
}

pub enum WelcomeResult {
    Open,
    Close,
}

impl Default for WelcomeDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl WelcomeDialog {
    pub fn new() -> Self {
        Self { page: 0 }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> WelcomeResult {
        let mut result = WelcomeResult::Open;
        let last = WELCOME_PAGES - 1;
        let (left, right) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
            )
        });
        if left {
            self.page = self.page.saturating_sub(1);
        }
        if right {
            self.page = (self.page + 1).min(last);
        }
        // Every slide gets the same body height (scrolling if it is longer), so the buttons stay
        // put while paging and remain visible in a small window.
        let max = kit::max_window_size(ctx);
        let body_h = (max.y - 200.0).clamp(120.0, 370.0);
        let modal = egui::Modal::new(egui::Id::unique("welcome-modal")).show(ctx, |ui| {
            {
                ui.set_width(560.0_f32.min(max.x));
                ui.horizontal(|ui| {
                    kit::chip(ui, &format!("{} / {}", self.page + 1, WELCOME_PAGES));
                });
                ui.add_space(kit::GRID);
                let (title, intro) = welcome_page_text(self.page);
                ui.heading(title);
                egui::ScrollArea::vertical()
                    .id_salt(("welcome-body", self.page))
                    .max_height(body_h)
                    .min_scrolled_height(body_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if !intro.is_empty() {
                            ui.label(RichText::new(intro).weak());
                        }
                        ui.add_space(kit::GRID * 1.5);
                        welcome_page_body(ui, self.page);
                    });
                ui.add_space(kit::GRID);
                // Page dots; each one jumps to its slide.
                ui.horizontal(|ui| {
                    let accent = ui.visuals().selection.stroke.color;
                    let idle = ui.visuals().weak_text_color();
                    for i in 0..WELCOME_PAGES {
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::Vec2::splat(kit::GRID * 2.0), egui::Sense::click());
                        let (r, c) = if i == self.page {
                            (kit::GRID * 0.6, accent)
                        } else {
                            (kit::GRID * 0.4, idle)
                        };
                        ui.painter().circle_filled(rect.center(), r, c);
                        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            self.page = i;
                        }
                    }
                });
                kit::divider(ui);
                kit::actions(ui, |ui| {
                    if self.page == last {
                        if kit::primary(ui, tr("Get started")).clicked() {
                            result = WelcomeResult::Close;
                        }
                    } else if kit::primary(ui, tr("Next")).clicked() {
                        self.page += 1;
                    }
                    if ui
                        .add_enabled_ui(self.page > 0, |ui| kit::outlined(ui, tr("Back")))
                        .inner
                        .clicked()
                    {
                        self.page -= 1;
                    }
                    if self.page != last && kit::text_button(ui, tr("Skip")).clicked() {
                        result = WelcomeResult::Close;
                    }
                });
            }
        });
        if modal.should_close() {
            result = WelcomeResult::Close;
        }
        result
    }
}

fn welcome_page_text(page: usize) -> (&'static str, &'static str) {
    match page {
        0 => (
            tr("Welcome to Ruxt"),
            tr("An SSH and SFTP workstation with secure defaults. This short tour shows where things are; open it again any time from ? → Show the tour."),
        ),
        1 => (tr("Connect"), tr("Everything starts from a session.")),
        2 => (tr("Files, tunnels and X11"), tr("Open these from the toolbar while a session is connected.")),
        3 => (tr("Terminal tools"), ""),
        4 => (tr("Security"), tr("A few choices worth making on the first day.")),
        _ => (tr("Keyboard shortcuts"), tr("In the terminal Ctrl+C copies the selection; with nothing selected it interrupts the remote program. Ctrl+V pastes.")),
    }
}

fn welcome_page_body(ui: &mut Ui, page: usize) {
    let item = |ui: &mut Ui, h: &str, s: &str| kit::list_item(ui, h, s);
    match page {
        0 => {
            item(
                ui,
                tr("One window for every server"),
                tr("Terminals, SFTP, tunnels and X11 forwarding in tabs, with a session list on the left."),
            );
            item(ui, tr("Safe by default"), tr("Host keys are pinned, X11 forwarding is untrusted, tunnels listen on this PC only and remote clipboard writes are blocked."));
            item(
                ui,
                tr("Korean and English"),
                tr("Switch the language in Settings → Appearance."),
            );
        }
        1 => {
            item(
                ui,
                tr("Quick connect"),
                tr("Type user@host:port in the toolbar box and press Enter. Quick connections are not saved."),
            );
            item(ui, tr("Saved sessions"), tr("+ New opens the session editor: authentication, jump host, tunnels, X11, server status. Double-click a session to connect."));
            item(
                ui,
                tr("Import"),
                tr("Import above the session list reads ~/.ssh/config, PuTTY sessions or a Ruxt export."),
            );
            item(ui, tr("Host key check"), tr("On the first connection compare the fingerprint with the server's. If the key changes later, the connection is blocked until you confirm."));
        }
        2 => {
            item(ui, tr("SFTP"), tr("An Explorer-style browser beside the terminal: drag to upload or download, right-click to edit a remote file locally."));
            item(
                ui,
                tr("Tunnels"),
                tr("Local (-L), remote (-R) and SOCKS5 (-D) forwarding, set per session in the editor."),
            );
            item(ui, tr("X11"), tr("Remote GUI programs open on the bundled X server, untrusted (ssh -X) unless you choose trusted mode."));
            item(
                ui,
                "Tail Viewer",
                tr("tail -f opens in its own window with filters and keyword alerts; the prompt stays free."),
            );
        }
        3 => {
            item(ui, tr("Find"), tr("Ctrl+Shift+F searches the scrollback."));
            item(ui, tr("Broadcast input"), tr("Types into every SSH tab at once."));
            item(ui, tr("Command completion"), tr("Commands you ran before appear as grey text. Needs a screen lock password, which encrypts the history."));
            item(ui, tr("Server status"), tr("CPU, memory and disk in the status bar; click it for the Server info window. Turn it on per session."));
        }
        4 => {
            item(ui, tr("Screen lock"), tr("One password locks the window when you are away and encrypts saved passwords and command history. Without it nothing is saved. Settings → Security."));
            item(ui, tr("Audit log"), tr("Off by default. Turn it on in Settings → Security to record connections, transfers and lock events."));
            item(
                ui,
                tr("Pastes and clipboard"),
                tr("Multi-line pastes ask first; remote programs cannot write your clipboard unless you allow OSC 52."),
            );
        }
        _ => shortcuts_grid(ui, "welcome-shortcuts"),
    }
}
