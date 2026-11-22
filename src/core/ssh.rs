//! SSH session driver: connection, host-key verification, authentication, interactive
//! shell, and the plumbing for SFTP / port-forwarding / X11 that hang off a session.

use super::audit;
use super::config::{AuthMethod, SecurityProfile, SessionProfile, TunnelKind, TunnelSpec};
use super::forward;
use super::hostkeys::{self, HostKeyStatus, HostKeyStore};
use super::monitor::{self, MonitorControl, MonitorEvent};
use super::sftp::{self, SftpCmd, SftpEvent};
use super::tail::{self, TailEvent, TailId, TailSpec};
use super::term::TermState;
use super::vault::SecretEntry;
use super::x11::{self, X11Target};
use super::TabId;

use anyhow::{bail, Result};
use parking_lot::Mutex;
use russh::client::{self, Handle, KeyboardInteractiveAuthResponse, Msg, Session};
use russh::keys::ssh_key::PublicKey;
use russh::keys::{Algorithm, EcdsaCurve, HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg, Disconnect, Preferred};
use secrecy::{ExposeSecret, SecretString};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle;

/// TCP connect + key exchange + host-key check budget (the clock pauses while the user is
/// looking at a host-key prompt).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a write to the shell channel may stall before the session is declared dead.
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
/// Server-initiated channels (X11, remote forwards) alive at once; the rest are refused.
const MAX_SERVER_CHANNELS: usize = 64;
/// Queue depth between the session loop and the channel writer task.
const WRITE_QUEUE: usize = 1024;

// ---------------------------------------------------------------------------
// Messages between the UI thread and session tasks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum PromptKind {
    HostKeyUnknown {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        randomart: String,
    },
    HostKeyChanged {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        randomart: String,
        file: PathBuf,
        line: usize,
    },
    Password {
        user: String,
        host: String,
        attempt: u32,
    },
    Passphrase {
        key_path: String,
    },
    KeyboardInteractive {
        name: String,
        instructions: String,
        prompts: Vec<(String, bool)>,
    },
}

pub enum PromptReply {
    Accept,
    Reject,
    /// A secret plus "remember in vault".
    Secret(SecretString, bool),
    Answers(Vec<SecretString>),
    Cancel,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub reply: Option<oneshot::Sender<PromptReply>>,
}

impl Prompt {
    pub fn answer(&mut self, r: PromptReply) {
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(r);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    Passphrase,
}

#[derive(Debug, Clone, Default)]
pub struct ConnInfo {
    pub auth_method: String,
    pub fingerprint: String,
    pub host_key_algorithm: String,
}

#[derive(Debug, Clone)]
pub enum TunnelEvent {
    Started(String),
    Stopped,
    Error(String),
}

/// Moving saved passwords from an old vault (separate master password) to the screen lock.
pub enum VaultOpOutcome {
    Migrated(super::vault::Vault),
    Failed(String),
}

pub enum UiEvent {
    Connected {
        tab: TabId,
        info: ConnInfo,
    },
    Disconnected {
        tab: TabId,
        reason: String,
    },
    Status {
        tab: TabId,
        msg: String,
    },
    Prompt {
        tab: TabId,
        prompt: Prompt,
    },
    Sftp {
        tab: TabId,
        ev: SftpEvent,
    },
    Tunnel {
        tab: TabId,
        idx: usize,
        ev: TunnelEvent,
    },
    SaveSecret {
        session_id: String,
        kind: SecretKind,
        value: SecretString,
    },
    VaultOp(VaultOpOutcome),
    AppLock(AppLockOutcome),
    /// Server status monitor (see `core::monitor`).
    Monitor {
        tab: TabId,
        ev: MonitorEvent,
    },
    /// Background `tail` viewer stream (see `core::tail`).
    Tail {
        tab: TabId,
        ev: TailEvent,
    },
}

/// Result of a screen-lock operation run off the UI thread (Argon2id is slow on purpose).
/// The saved-password store is opened with the same password, so unlocking also returns it.
pub enum AppLockOutcome {
    /// The password was wrong for the given purpose.
    WrongPassword {
        then: AppLockThen,
    },
    /// Unlocked; `store` is the saved-password store (or why it could not be opened).
    Unlocked {
        store: Result<super::vault::Opened, String>,
    },
    /// A password was set for the first time.
    Set {
        store: Result<super::vault::Opened, String>,
    },
    /// The password was replaced; re-encrypt the open store with this key.
    Changed {
        key: zeroize::Zeroizing<[u8; 32]>,
        salt: [u8; 32],
    },
    /// The lock and every saved password are gone.
    Removed,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppLockThen {
    Unlock,
    Change,
    Remove,
}

pub enum SessionCmd {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Sftp(SftpCmd),
    StartTunnel { idx: usize, spec: TunnelSpec },
    StopTunnel { idx: usize },
    StartTail { id: TailId, spec: TailSpec },
    StopTail { id: TailId },
    Disconnect,
}

pub struct SessionArgs {
    pub tab: TabId,
    pub profile: SessionProfile,
    pub secrets: Option<SecretEntry>,
    /// Jump hosts to connect through first (ProxyJump), first hop first. Each hop has its own
    /// host-key check, authentication and credentials.
    pub jumps: Vec<(SessionProfile, Option<SecretEntry>)>,
    pub term: Arc<Mutex<TermState>>,
    pub pty_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    pub ui: mpsc::UnboundedSender<UiEvent>,
    pub cmds: mpsc::UnboundedReceiver<SessionCmd>,
    pub ctx: egui::Context,
    pub hostkeys: Arc<HostKeyStore>,
    pub allow_public_bind: bool,
    /// Server status monitor switches (the UI keeps a clone to toggle them live).
    pub monitor: MonitorControl,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

type RemoteForwardMap = Arc<Mutex<HashMap<(String, u32), (String, u16)>>>;

pub struct ClientHandler {
    tab: TabId,
    host: String,
    port: u16,
    hostkeys: Arc<HostKeyStore>,
    ui: mpsc::UnboundedSender<UiEvent>,
    term: Arc<Mutex<TermState>>,
    remote_forwards: RemoteForwardMap,
    x11: Option<(X11Target, [u8; 16])>,
    agent_forwarding: bool,
    info: Arc<Mutex<ConnInfo>>,
    /// True while a host-key prompt is waiting on the user (pauses the connect timeout).
    prompting: Arc<AtomicBool>,
    /// Limits channels the *server* opens towards us.
    server_channels: Arc<Semaphore>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_public_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: PublicKey = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => {
                // We do not validate host certificates against a CA; fall back to pinning the
                // certified key itself (TOFU), which is still strictly safer than blind trust.
                PublicKey::new(cert.public_key().clone(), "")
            }
        };
        let algorithm = key.algorithm().as_str().to_string();
        let fingerprint = hostkeys::fingerprint_sha256(&key);
        {
            let mut i = self.info.lock();
            i.fingerprint = fingerprint.clone();
            i.host_key_algorithm = algorithm.clone();
        }
        let status = self.hostkeys.check(&self.host, self.port, &key);
        match status {
            HostKeyStatus::Known => Ok(true),
            HostKeyStatus::Unknown => {
                let kind = PromptKind::HostKeyUnknown {
                    host: self.host.clone(),
                    port: self.port,
                    algorithm: algorithm.clone(),
                    fingerprint: fingerprint.clone(),
                    randomart: hostkeys::randomart(&key),
                };
                match self.ask(kind).await {
                    PromptReply::Accept => {
                        if let Err(e) = self.hostkeys.learn(&self.host, self.port, &key) {
                            tracing::error!("cannot record host key: {e}");
                            self.status(format!(
                                "Warning: host key could not be pinned ({e}); you will be asked again next time."
                            ));
                        }
                        audit::log(
                            "hostkey.trusted",
                            serde_json::json!({"host": self.host, "port": self.port, "algorithm": algorithm, "fingerprint": fingerprint}),
                        );
                        Ok(true)
                    }
                    _ => {
                        audit::log(
                            "hostkey.rejected",
                            serde_json::json!({"host": self.host, "port": self.port, "fingerprint": fingerprint}),
                        );
                        Ok(false)
                    }
                }
            }
            HostKeyStatus::Changed { line, file } => {
                audit::log(
                    "hostkey.MISMATCH",
                    serde_json::json!({"host": self.host, "port": self.port, "algorithm": algorithm, "fingerprint": fingerprint, "file": file.display().to_string(), "line": line}),
                );
                let kind = PromptKind::HostKeyChanged {
                    host: self.host.clone(),
                    port: self.port,
                    algorithm,
                    fingerprint,
                    randomart: hostkeys::randomart(&key),
                    file: file.clone(),
                    line,
                };
                match self.ask(kind).await {
                    PromptReply::Accept => {
                        // The user explicitly chose to forget the old key. Only our own file is
                        // writable; a mismatch in ~/.ssh/known_hosts must be fixed by the user.
                        if file == self.hostkeys.app_file() {
                            let replaced = self
                                .hostkeys
                                .forget(&file, &self.host, self.port)
                                .and_then(|_| self.hostkeys.learn(&self.host, self.port, &key));
                            match replaced {
                                Ok(()) => {
                                    audit::log(
                                        "hostkey.replaced",
                                        serde_json::json!({"host": self.host, "port": self.port, "fingerprint": self.info.lock().fingerprint}),
                                    );
                                    Ok(true)
                                }
                                Err(e) => {
                                    // Never connect on a mismatch unless the new key is actually
                                    // pinned; otherwise the next connect would silently re-prompt.
                                    audit::log(
                                        "hostkey.replace_failed",
                                        serde_json::json!({"host": self.host, "port": self.port, "error": e.to_string()}),
                                    );
                                    self.status(format!("Could not replace the pinned host key: {e}"));
                                    Ok(false)
                                }
                            }
                        } else {
                            self.status(format!(
                                "Host key mismatch recorded in {} line {} — remove it manually to proceed.",
                                file.display(),
                                line
                            ));
                            Ok(false)
                        }
                    }
                    _ => Ok(false),
                }
            }
        }
    }

    async fn auth_banner(&mut self, banner: &str, _session: &mut Session) -> Result<(), Self::Error> {
        let mut t = self.term.lock();
        for line in banner.lines().take(50) {
            let s = format!("{}\r\n", super::term::sanitize_title(line));
            t.advance(s.as_bytes());
        }
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let target = {
            let map = self.remote_forwards.lock();
            map.get(&(connected_address.to_string(), connected_port))
                .cloned()
                .or_else(|| {
                    map.iter()
                        .find(|((a, p), _)| {
                            *p == connected_port
                                && matches!(
                                    a.as_str(),
                                    "" | "*" | "0.0.0.0" | "::" | "localhost" | "127.0.0.1" | "::1"
                                )
                        })
                        .map(|(_, v)| v.clone())
                })
        };
        match target {
            Some((host, port)) => {
                let Ok(permit) = self.server_channels.clone().try_acquire_owned() else {
                    tracing::warn!("too many server-opened channels; refusing forwarded-tcpip");
                    reply.reject(russh::ChannelOpenFailure::ResourceShortage).await;
                    return Ok(());
                };
                reply.accept().await;
                audit::log(
                    "forward.remote.conn",
                    serde_json::json!({"tab": self.tab, "from": format!("{originator_address}:{originator_port}"), "to": format!("{host}:{port}")}),
                );
                tokio::spawn(async move {
                    let _permit = permit;
                    forward::bridge_to_tcp(channel, host, port).await;
                });
            }
            None => {
                tracing::warn!("rejecting unexpected forwarded-tcpip for {connected_address}:{connected_port}");
                reply
                    .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
            }
        }
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        channel: Channel<Msg>,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        match &self.x11 {
            Some((target, cookie)) => {
                let Ok(permit) = self.server_channels.clone().try_acquire_owned() else {
                    tracing::warn!("too many server-opened channels; refusing x11");
                    reply.reject(russh::ChannelOpenFailure::ResourceShortage).await;
                    return Ok(());
                };
                reply.accept().await;
                audit::log(
                    "x11.conn",
                    serde_json::json!({"tab": self.tab, "from": format!("{originator_address}:{originator_port}")}),
                );
                let target = target.clone();
                let cookie = *cookie;
                let ui = self.ui.clone();
                let tab = self.tab;
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = x11::bridge(channel, target, cookie).await {
                        tracing::warn!("x11 bridge: {e}");
                        let _ = ui.send(UiEvent::Status {
                            tab,
                            msg: format!("X11: {e}"),
                        });
                    }
                });
            }
            None => {
                reply
                    .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
            }
        }
        Ok(())
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: Channel<Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.agent_forwarding {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        reply.accept().await;
        audit::log("agent.forward.conn", serde_json::json!({"tab": self.tab}));
        tokio::spawn(async move {
            if let Err(e) = forward::bridge_to_agent(channel).await {
                tracing::warn!("agent forward bridge: {e}");
            }
        });
        Ok(())
    }

    async fn disconnected(&mut self, reason: client::DisconnectReason<Self::Error>) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                self.status(format!(
                    "Server disconnected: {}",
                    super::term::sanitize_title(&info.message)
                ));
                Ok(())
            }
            client::DisconnectReason::Error(e) => Err(e),
        }
    }
}

impl ClientHandler {
    async fn ask(&self, kind: PromptKind) -> PromptReply {
        let (tx, rx) = oneshot::channel();
        let _ = self.ui.send(UiEvent::Prompt {
            tab: self.tab,
            prompt: Prompt { kind, reply: Some(tx) },
        });
        self.prompting.store(true, Ordering::Relaxed);
        let r = rx.await.unwrap_or(PromptReply::Cancel);
        self.prompting.store(false, Ordering::Relaxed);
        r
    }
    fn status(&self, msg: String) {
        let _ = self.ui.send(UiEvent::Status { tab: self.tab, msg });
    }
}

// ---------------------------------------------------------------------------
// Algorithm policy
// ---------------------------------------------------------------------------

fn preferred_for(profile: SecurityProfile, compression: bool) -> Preferred {
    match profile {
        SecurityProfile::Strict => Preferred {
            kex: Cow::Borrowed(&[
                russh::kex::MLKEM768X25519_SHA256,
                russh::kex::CURVE25519,
                russh::kex::CURVE25519_PRE_RFC_8731,
                russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
                russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
            ]),
            key: Cow::Borrowed(&[
                Algorithm::Ed25519,
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP384,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP521,
                },
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha512),
                },
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha256),
                },
            ]),
            host_key_certificates: Cow::Borrowed(&[]),
            cipher: Cow::Borrowed(&[
                russh::cipher::CHACHA20_POLY1305,
                russh::cipher::AES_256_GCM,
                russh::cipher::AES_128_GCM,
            ]),
            mac: Cow::Borrowed(&[russh::mac::HMAC_SHA512_ETM, russh::mac::HMAC_SHA256_ETM]),
            compression: Cow::Borrowed(if compression {
                &[russh::compression::NONE, russh::compression::ZLIB_LEGACY]
            } else {
                &[russh::compression::NONE]
            }),
        },
        SecurityProfile::Compatible => {
            // russh's defaults minus `ssh-rsa` (SHA-1) host-key signatures: DH-GEX / group14+
            // with SHA-2, CTR ciphers, SHA-2 MACs.
            let mut p = Preferred::DEFAULT.clone();
            p.key = Cow::Borrowed(&[
                Algorithm::Ed25519,
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP384,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP521,
                },
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha512),
                },
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha256),
                },
            ]);
            if !compression {
                p.compression = Cow::Borrowed(&[russh::compression::NONE]);
            }
            p
        }
        SecurityProfile::Legacy => {
            // Adds `ssh-rsa` (RSA/SHA-1) host-key signatures for pre-2016 servers. Still no
            // DSA, no CBC, no RC4/3DES, no SHA-1 MACs or SHA-1 key exchange.
            let mut p = Preferred::DEFAULT.clone();
            if !compression {
                p.compression = Cow::Borrowed(&[russh::compression::NONE]);
            }
            p
        }
    }
}

/// Algorithm names a profile offers (kex, host keys, ciphers, macs); checked by tests.
#[cfg(test)]
pub fn profile_algorithms(profile: SecurityProfile) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let p = preferred_for(profile, false);
    (
        p.kex.iter().map(|k| k.as_ref().to_string()).collect(),
        p.key.iter().map(|k| k.as_str().to_string()).collect(),
        p.cipher.iter().map(|c| c.as_ref().to_string()).collect(),
        p.mac.iter().map(|m| m.as_ref().to_string()).collect(),
    )
}

// ---------------------------------------------------------------------------
// Session driver
// ---------------------------------------------------------------------------

pub async fn run_session(args: SessionArgs) {
    let tab = args.tab;
    let ui = args.ui.clone();
    let term = args.term.clone();
    let (host, port) = (args.profile.host.trim().to_string(), args.profile.port);
    let user = args.profile.username.trim().to_string();
    let result = run_session_inner(args).await;
    let reason = match result {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("{e:#}");
            // Failed connects and authentication attempts are security-relevant: record them.
            audit::log(
                "session.failed",
                serde_json::json!({"tab": tab, "host": host, "port": port, "user": user, "error": msg}),
            );
            term.lock().print_local(&format!("Connection failed: {msg}"));
            msg
        }
    };
    {
        let mut t = term.lock();
        t.closed = true;
        t.exit_message = Some(reason.clone());
        t.print_local(&format!("Session closed ({reason}). Press Enter to reconnect."));
    }
    let _ = ui.send(UiEvent::Disconnected { tab, reason });
}

struct AuthCtx<'a> {
    tab: TabId,
    ui: &'a mpsc::UnboundedSender<UiEvent>,
    profile: &'a SessionProfile,
    secrets: Option<&'a SecretEntry>,
}

impl AuthCtx<'_> {
    async fn ask(&self, kind: PromptKind) -> PromptReply {
        let (tx, rx) = oneshot::channel();
        let _ = self.ui.send(UiEvent::Prompt {
            tab: self.tab,
            prompt: Prompt { kind, reply: Some(tx) },
        });
        rx.await.unwrap_or(PromptReply::Cancel)
    }
    fn status(&self, msg: impl Into<String>) {
        let _ = self.ui.send(UiEvent::Status {
            tab: self.tab,
            msg: msg.into(),
        });
    }
}

async fn run_session_inner(args: SessionArgs) -> Result<String> {
    let SessionArgs {
        tab,
        profile,
        secrets,
        jumps,
        term,
        mut pty_rx,
        ui,
        mut cmds,
        ctx,
        hostkeys,
        allow_public_bind,
        monitor: monitor_ctl,
    } = args;

    if profile.host.trim().is_empty() {
        bail!("no host configured");
    }
    let host = profile.host.trim().to_string();
    let port = profile.port;
    let user = if profile.username.trim().is_empty() {
        whoami().ok_or_else(|| anyhow::anyhow!("no username in the profile and USER/USERNAME are not set"))?
    } else {
        profile.username.trim().to_string()
    };

    term.lock()
        .print_local(&format!("Connecting to {user}@{host}:{port} ..."));
    audit::log(
        "session.connect",
        serde_json::json!({"tab": tab, "host": host, "port": port, "user": user, "profile": profile.display_name(), "security": format!("{:?}", profile.security)}),
    );

    let config = client::Config {
        preferred: preferred_for(profile.security, profile.compression),
        inactivity_timeout: None,
        keepalive_interval: if profile.keepalive_secs > 0 {
            Some(Duration::from_secs(profile.keepalive_secs.clamp(5, 3600)))
        } else {
            None
        },
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    };
    let config = Arc::new(config);

    // X11: the remote side only ever sees a fake cookie (swapped by the bridge). In the default
    // untrusted mode the cookie the bridge swaps in is a confined one minted through the X
    // SECURITY extension; the master cookie is kept only to revoke it at the end.
    let mut x11_untrusted: Option<(X11Target, Vec<u8>, u32)> = None;
    let x11 = if profile.x11_forwarding {
        let mut cookie = [0u8; 16];
        rand::fill(&mut cookie);
        let mut target = x11::resolve(&profile.x11_display);
        match target.real_cookie.clone() {
            None => term.lock().print_local(&format!(
                "X11: no local authority cookie for display :{}; forwarded X clients will be refused (start the X server from the X menu or set XAUTHORITY).",
                target.display_number
            )),
            Some(_) if profile.x11_trusted => {
                term.lock().print_local(
                    "X11: TRUSTED forwarding (-Y): remote X clients get full access to the local display.",
                );
                audit::log(
                    "x11.trusted",
                    serde_json::json!({"tab": tab, "host": host, "display": target.display_number}),
                );
            }
            Some(master) => match x11::generate_untrusted(&target, &master, 0).await {
                Ok(auth) => {
                    target.real_cookie = Some(auth.cookie);
                    x11_untrusted = Some((target.clone(), master, auth.auth_id));
                    term.lock().print_local(
                        "X11: untrusted forwarding (-X): remote X clients are confined by the X SECURITY extension.",
                    );
                    audit::log(
                        "x11.untrusted",
                        serde_json::json!({"tab": tab, "host": host, "display": target.display_number}),
                    );
                }
                Err(e) => {
                    target.real_cookie = None;
                    term.lock().print_local(&format!(
                        "X11: untrusted mode unavailable ({e:#}). Forwarded X clients will be REFUSED; enable 'Trusted (-Y)' in the session editor to grant the remote full display access instead."
                    ));
                    audit::log(
                        "x11.untrusted_failed",
                        serde_json::json!({"tab": tab, "host": host, "error": format!("{e:#}")}),
                    );
                }
            },
        }
        Some((target, cookie))
    } else {
        None
    };
    let remote_forwards: RemoteForwardMap = Arc::new(Mutex::new(HashMap::new()));
    let info = Arc::new(Mutex::new(ConnInfo::default()));
    let prompting = Arc::new(AtomicBool::new(false));

    let handler = ClientHandler {
        tab,
        host: host.clone(),
        port,
        hostkeys: hostkeys.clone(),
        ui: ui.clone(),
        term: term.clone(),
        remote_forwards: remote_forwards.clone(),
        x11: x11.clone(),
        agent_forwarding: profile.agent_forwarding,
        info: info.clone(),
        prompting: prompting.clone(),
        server_channels: Arc::new(Semaphore::new(MAX_SERVER_CHANNELS)),
    };

    // ---- jump hosts (ProxyJump) ----------------------------------------
    // Every hop is a full SSH session of its own: its host key is pinned separately, it is
    // authenticated with its own profile and vault entry, and the next hop (or the target)
    // is reached through a direct-tcpip channel on it. The handles stay alive until this
    // session ends.
    let mut hops: Vec<Arc<Handle<ClientHandler>>> = Vec::new();
    for (jp, jsecrets) in &jumps {
        let jhost = jp.host.trim().to_string();
        if jhost.is_empty() {
            bail!("jump host '{}' has no host configured", jp.display_name());
        }
        let juser = if jp.username.trim().is_empty() {
            whoami().ok_or_else(|| anyhow::anyhow!("no username for jump host {jhost}"))?
        } else {
            jp.username.trim().to_string()
        };
        term.lock()
            .print_local(&format!("Jump host: connecting to {juser}@{jhost}:{} ...", jp.port));
        let jconfig = Arc::new(client::Config {
            preferred: preferred_for(jp.security, jp.compression),
            inactivity_timeout: None,
            keepalive_interval: if jp.keepalive_secs > 0 {
                Some(Duration::from_secs(jp.keepalive_secs.clamp(5, 3600)))
            } else {
                None
            },
            keepalive_max: 3,
            nodelay: true,
            ..Default::default()
        });
        let jhandler = ClientHandler {
            tab,
            host: jhost.clone(),
            port: jp.port,
            hostkeys: hostkeys.clone(),
            ui: ui.clone(),
            term: term.clone(),
            remote_forwards: Arc::new(Mutex::new(HashMap::new())),
            x11: None,
            agent_forwarding: false,
            info: Arc::new(Mutex::new(ConnInfo::default())),
            prompting: prompting.clone(),
            server_channels: Arc::new(Semaphore::new(MAX_SERVER_CHANNELS)),
        };
        let mut jhandle = match hops.last() {
            None => {
                await_connect(
                    client::connect(jconfig, (jhost.as_str(), jp.port), jhandler),
                    &prompting,
                )
                .await?
            }
            Some(prev) => {
                let ch = prev
                    .channel_open_direct_tcpip(jhost.clone(), jp.port as u32, "127.0.0.1", 0)
                    .await
                    .map_err(|e| anyhow::anyhow!("tunnel to {jhost}:{} through the jump host: {e}", jp.port))?;
                await_connect(client::connect_stream(jconfig, ch.into_stream(), jhandler), &prompting).await?
            }
        };
        let jmethod = {
            let actx = AuthCtx {
                tab,
                ui: &ui,
                profile: jp,
                secrets: jsecrets.as_ref(),
            };
            authenticate(&mut jhandle, &juser, &actx).await?
        };
        audit::log(
            "session.jump",
            serde_json::json!({"tab": tab, "host": jhost, "port": jp.port, "user": juser, "method": jmethod}),
        );
        term.lock()
            .print_local(&format!("Jump host {jhost} authenticated ({jmethod})."));
        hops.push(Arc::new(jhandle));
    }
    // Jump-host secrets are only needed for their authentication.
    drop(jumps);

    // The handshake budget must not include the time the user spends comparing a host-key
    // fingerprint, so the deadline is pushed out for as long as a prompt is open.
    let mut handle = match hops.last() {
        None => await_connect(client::connect(config, (host.as_str(), port), handler), &prompting).await?,
        Some(prev) => {
            let ch = prev
                .channel_open_direct_tcpip(host.clone(), port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| anyhow::anyhow!("tunnel to {host}:{port} through the jump host: {e}"))?;
            await_connect(client::connect_stream(config, ch.into_stream(), handler), &prompting).await?
        }
    };

    // ---- authentication -------------------------------------------------
    let method = {
        let actx = AuthCtx {
            tab,
            ui: &ui,
            profile: &profile,
            secrets: secrets.as_ref(),
        };
        authenticate(&mut handle, &user, &actx).await?
    };
    // Vault secrets are only needed for authentication; drop (and zeroize) them now so a
    // long-lived session does not keep passwords in memory after the vault auto-locks.
    drop(secrets);
    let handle = Arc::new(handle);
    info.lock().auth_method = method.clone();
    audit::log(
        "session.authenticated",
        serde_json::json!({"tab": tab, "host": host, "user": user, "method": method}),
    );
    let _ = ui.send(UiEvent::Connected {
        tab,
        info: info.lock().clone(),
    });
    term.lock()
        .print_local(&format!("Authenticated ({method}). Opening shell..."));

    // ---- shell channel -------------------------------------------------
    let mut channel = handle.channel_open_session().await?;
    if profile.agent_forwarding {
        if let Err(e) = channel.agent_forward(true).await {
            tracing::warn!("agent forward request failed: {e}");
        }
    }
    if let Some((_, cookie)) = &x11 {
        channel
            .request_x11(true, false, "MIT-MAGIC-COOKIE-1", hex::encode(cookie), 0)
            .await?;
        // Wait for the server's verdict so the user learns when X11 forwarding is disabled
        // server-side (sshd `X11Forwarding no`) instead of silently getting nothing.
        let verdict = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Success) => break Some(true),
                    Some(ChannelMsg::Failure) => break Some(false),
                    Some(_) => continue,
                    None => break None,
                }
            }
        })
        .await
        .ok()
        .flatten();
        match verdict {
            Some(true) => {}
            Some(false) => {
                term.lock()
                    .print_local("X11 forwarding was refused by the server (check sshd X11Forwarding).");
                audit::log("x11.refused", serde_json::json!({"tab": tab, "host": host}));
            }
            None => tracing::debug!("no reply to x11-req"),
        }
    }
    let size = term.lock().size;
    channel
        .request_pty(true, &profile.term_type, size.cols as u32, size.rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;
    if !profile.startup_command.trim().is_empty() {
        let mut cmd = profile.startup_command.trim().to_string();
        cmd.push('\n');
        channel.data_bytes(cmd.into_bytes()).await?;
    }
    let (mut rh, wh) = channel.split();

    // Writes go through a dedicated task so a server that stops granting window space can
    // never wedge the main loop (which must keep draining incoming data and UI commands).
    let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(WRITE_QUEUE);
    let writer: JoinHandle<()> = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let r = match msg {
                Outbound::Data(bytes) => wh.data_bytes(bytes).await,
                Outbound::Resize(cols, rows) => wh.window_change(cols, rows, 0, 0).await,
            };
            if r.is_err() {
                break;
            }
        }
        let _ = wh.eof().await;
        let _ = wh.close().await;
    });
    let send_out = |msg: Outbound| {
        let tx = out_tx.clone();
        async move {
            tokio::time::timeout(WRITE_TIMEOUT, tx.send(msg))
                .await
                .map(|r| r.is_ok())
                .unwrap_or(false)
        }
    };

    // ---- tunnels -------------------------------------------------------
    let mut tunnels: HashMap<usize, TunnelTask> = HashMap::new();
    for (idx, spec) in profile.tunnels.iter().enumerate() {
        if spec.auto_start {
            start_tunnel(
                &handle,
                tab,
                idx,
                spec.clone(),
                &ui,
                &remote_forwards,
                allow_public_bind,
                &mut tunnels,
            )
            .await;
        }
    }

    // ---- SFTP (lazy) ----------------------------------------------------
    let mut sftp_tx: Option<mpsc::UnboundedSender<SftpCmd>> = None;

    // ---- tail streams ---------------------------------------------------
    let mut tails: HashMap<TailId, oneshot::Sender<()>> = HashMap::new();

    // ---- server status monitor -------------------------------------------
    let monitor_task = tokio::spawn(monitor::run(handle.clone(), tab, ui.clone(), ctx.clone(), monitor_ctl));

    // ---- main loop -----------------------------------------------------
    let reason;
    loop {
        tokio::select! {
            msg = rh.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        term.lock().advance(&data);
                        ctx.request_repaint();
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        term.lock().advance(&data);
                        ctx.request_repaint();
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        reason = format!("exit status {exit_status}");
                        break;
                    }
                    Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                        reason = format!("exit signal {signal_name:?}");
                        break;
                    }
                    Some(ChannelMsg::Close) | None => {
                        reason = "channel closed".into();
                        break;
                    }
                    Some(ChannelMsg::Eof) => {}
                    _ => {}
                }
            }
            Some(bytes) = pty_rx.recv() => {
                if !send_out(Outbound::Data(bytes)).await {
                    reason = "write stalled".into();
                    break;
                }
            }
            cmd = cmds.recv() => {
                match cmd {
                    Some(SessionCmd::Input(bytes)) => {
                        if !send_out(Outbound::Data(bytes)).await {
                            reason = "write stalled".into();
                            break;
                        }
                    }
                    Some(SessionCmd::Resize { cols, rows }) => {
                        let _ = send_out(Outbound::Resize(cols as u32, rows as u32)).await;
                    }
                    Some(SessionCmd::Sftp(c)) => {
                        if sftp_tx.is_none() {
                            let (tx, rx) = mpsc::unbounded_channel();
                            tokio::spawn(sftp::run_sftp(handle.clone(), tab, ui.clone(), rx, ctx.clone()));
                            sftp_tx = Some(tx);
                        }
                        if let Some(tx) = &sftp_tx {
                            let _ = tx.send(c);
                        }
                    }
                    Some(SessionCmd::StartTunnel { idx, spec }) => {
                        start_tunnel(&handle, tab, idx, spec, &ui, &remote_forwards, allow_public_bind, &mut tunnels).await;
                    }
                    Some(SessionCmd::StopTunnel { idx }) => {
                        if let Some(t) = tunnels.remove(&idx) {
                            t.stop(&handle, &remote_forwards).await;
                            let _ = ui.send(UiEvent::Tunnel { tab, idx, ev: TunnelEvent::Stopped });
                        }
                    }
                    Some(SessionCmd::StartTail { id, spec }) => {
                        if let Some(prev) = tails.remove(&id) {
                            let _ = prev.send(());
                        }
                        let (stop_tx, stop_rx) = oneshot::channel();
                        tails.insert(id, stop_tx);
                        tokio::spawn(tail::run_ssh_tail(
                            handle.clone(),
                            tab,
                            id,
                            spec,
                            ui.clone(),
                            ctx.clone(),
                            stop_rx,
                        ));
                    }
                    Some(SessionCmd::StopTail { id }) => {
                        if let Some(stop) = tails.remove(&id) {
                            let _ = stop.send(());
                        }
                    }
                    Some(SessionCmd::Disconnect) | None => {
                        reason = "disconnected by user".into();
                        break;
                    }
                }
            }
        }
    }

    monitor_task.abort();
    for (_, stop) in tails.drain() {
        let _ = stop.send(());
    }
    for (_, t) in tunnels.drain() {
        t.abort();
    }
    if let Some((target, master, auth_id)) = x11_untrusted {
        if let Err(e) =
            tokio::time::timeout(Duration::from_secs(3), x11::revoke_untrusted(&target, &master, auth_id)).await
        {
            tracing::debug!("revoking the untrusted X11 authorisation: {e}");
        }
    }
    drop(out_tx);
    // Let the writer flush and close the channel, but never wait on a wedged server.
    if tokio::time::timeout(Duration::from_secs(3), writer).await.is_err() {
        tracing::debug!("channel writer did not finish in time");
    }
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        handle.disconnect(Disconnect::ByApplication, "bye", "en"),
    )
    .await;
    audit::log(
        "session.closed",
        serde_json::json!({"tab": tab, "host": host, "reason": reason}),
    );
    Ok(reason)
}

/// Drive a russh connect future with the connect timeout, pausing the clock while a host-key
/// prompt is open (`prompting`).
async fn await_connect<F>(fut: F, prompting: &AtomicBool) -> Result<Handle<ClientHandler>>
where
    F: std::future::Future<Output = std::result::Result<Handle<ClientHandler>, russh::Error>>,
{
    tokio::pin!(fut);
    let mut started = Instant::now();
    loop {
        tokio::select! {
            r = &mut fut => return r.map_err(|e| anyhow::anyhow!("{}", describe_err(&e))),
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if prompting.load(Ordering::Relaxed) {
                    started = Instant::now();
                } else if started.elapsed() > CONNECT_TIMEOUT {
                    bail!("connection timed out");
                }
            }
        }
    }
}

fn describe_err(e: &russh::Error) -> String {
    match e {
        russh::Error::UnknownKey => "host key rejected".into(),
        russh::Error::NoCommonAlgo { kind, theirs, .. } => format!(
            "no common {kind:?} algorithm (server offers: {}); try the 'Compatible' security profile",
            theirs.join(",")
        ),
        other => other.to_string(),
    }
}

/// Bytes and control messages for the shell channel writer task.
enum Outbound {
    Data(Vec<u8>),
    Resize(u32, u32),
}

/// Local user name for sessions without an explicit one (no silent "root" fallback).
fn whoami() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

async fn authenticate(handle: &mut Handle<ClientHandler>, user: &str, actx: &AuthCtx<'_>) -> Result<String> {
    match &actx.profile.auth {
        AuthMethod::Auto => {
            if let Some(m) = try_agent(handle, user, actx).await? {
                return Ok(m);
            }
            if let Some(m) = try_default_keys(handle, user, actx).await? {
                return Ok(m);
            }
            if let Some(m) = try_password(handle, user, actx).await? {
                return Ok(m);
            }
            if let Some(m) = try_keyboard_interactive(handle, user, actx).await? {
                return Ok(m);
            }
            bail!("all authentication methods failed")
        }
        AuthMethod::Password => {
            if let Some(m) = try_password(handle, user, actx).await? {
                return Ok(m);
            }
            if let Some(m) = try_keyboard_interactive(handle, user, actx).await? {
                return Ok(m);
            }
            bail!("password authentication failed")
        }
        AuthMethod::PublicKey { path } => {
            if let Some(m) = try_key_file(handle, user, actx, &PathBuf::from(path), true).await? {
                return Ok(m);
            }
            bail!("public key authentication failed")
        }
        AuthMethod::Agent => {
            if let Some(m) = try_agent(handle, user, actx).await? {
                return Ok(m);
            }
            bail!("agent authentication failed (no agent or no accepted identity)")
        }
        AuthMethod::KeyboardInteractive => {
            if let Some(m) = try_keyboard_interactive(handle, user, actx).await? {
                return Ok(m);
            }
            bail!("keyboard-interactive authentication failed")
        }
    }
}

async fn try_password(handle: &mut Handle<ClientHandler>, user: &str, actx: &AuthCtx<'_>) -> Result<Option<String>> {
    // Saved password first (silently). Borrowed, not cloned: the vault entry zeroizes itself
    // and russh takes its own copy for the wire.
    if let Some(pw) = actx.secrets.and_then(|s| s.password.as_deref()) {
        actx.status("Trying saved password...");
        if handle.authenticate_password(user, pw).await?.success() {
            return Ok(Some("password (vault)".into()));
        }
        actx.status("Saved password rejected.");
        audit::log(
            "auth.failed",
            serde_json::json!({"tab": actx.tab, "host": actx.profile.host, "user": user, "method": "password (vault)"}),
        );
    }
    for attempt in 1..=3u32 {
        let reply = actx
            .ask(PromptKind::Password {
                user: user.to_string(),
                host: actx.profile.host.clone(),
                attempt,
            })
            .await;
        match reply {
            PromptReply::Secret(pw, remember) => {
                let ok = handle.authenticate_password(user, pw.expose_secret()).await?.success();
                if ok {
                    if remember {
                        let _ = actx.ui.send(UiEvent::SaveSecret {
                            session_id: actx.profile.id.clone(),
                            kind: SecretKind::Password,
                            value: pw,
                        });
                    }
                    return Ok(Some("password".into()));
                }
                actx.status("Permission denied, please try again.");
                audit::log(
                    "auth.failed",
                    serde_json::json!({"tab": actx.tab, "host": actx.profile.host, "user": user, "method": "password", "attempt": attempt}),
                );
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

async fn try_keyboard_interactive(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    actx: &AuthCtx<'_>,
) -> Result<Option<String>> {
    let mut resp = handle
        .authenticate_keyboard_interactive_start(user, None::<String>)
        .await?;
    let mut rounds = 0;
    loop {
        match resp {
            KeyboardInteractiveAuthResponse::Success => return Ok(Some("keyboard-interactive".into())),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(None),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                rounds += 1;
                if rounds > 10 {
                    return Ok(None);
                }
                let answers: Vec<String> = if prompts.is_empty() {
                    Vec::new()
                } else {
                    let kind = PromptKind::KeyboardInteractive {
                        name: super::term::sanitize_title(&name),
                        instructions: instructions
                            .lines()
                            .map(super::term::sanitize_title)
                            .collect::<Vec<_>>()
                            .join("\n"),
                        prompts: prompts
                            .iter()
                            .map(|p| (super::term::sanitize_title(&p.prompt), p.echo))
                            .collect(),
                    };
                    match actx.ask(kind).await {
                        // russh takes ownership of plain `String`s here; this is the one copy we
                        // cannot wrap, and it lives only until the response is sent.
                        PromptReply::Answers(a) => a.iter().map(|s| s.expose_secret().to_string()).collect(),
                        _ => return Ok(None),
                    }
                };
                resp = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
}

async fn try_default_keys(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    actx: &AuthCtx<'_>,
) -> Result<Option<String>> {
    let Some(home) = hostkeys::home_dir() else {
        return Ok(None);
    };
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let p = home.join(".ssh").join(name);
        if p.exists() {
            if let Some(m) = try_key_file(handle, user, actx, &p, false).await? {
                return Ok(Some(m));
            }
        }
    }
    Ok(None)
}

async fn try_key_file(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    actx: &AuthCtx<'_>,
    path: &PathBuf,
    explicit: bool,
) -> Result<Option<String>> {
    let path_str = path.display().to_string();
    actx.status(format!("Trying key {path_str}"));
    let text = match std::fs::read_to_string(path) {
        Ok(t) => zeroize::Zeroizing::new(t),
        Err(e) => {
            if explicit {
                bail!("cannot read key file {path_str}: {e}");
            }
            return Ok(None);
        }
    };
    let mut key = match russh::keys::decode_secret_key(&text, None) {
        Ok(k) => Some(k),
        Err(russh::keys::Error::KeyIsEncrypted) => None,
        Err(e) => {
            if explicit {
                bail!("cannot parse key file {path_str}: {e}");
            }
            tracing::warn!("skipping key {path_str}: {e}");
            return Ok(None);
        }
    };
    if key.is_none() {
        // Encrypted: saved passphrase first, then prompt.
        if let Some(pp) = actx.secrets.and_then(|s| s.passphrase.as_deref()) {
            if let Ok(k) = russh::keys::decode_secret_key(&text, Some(pp)) {
                key = Some(k);
            }
        }
        let mut attempts = 0;
        while key.is_none() && attempts < 3 {
            attempts += 1;
            match actx
                .ask(PromptKind::Passphrase {
                    key_path: path_str.clone(),
                })
                .await
            {
                PromptReply::Secret(pp, remember) => {
                    match russh::keys::decode_secret_key(&text, Some(pp.expose_secret())) {
                        Ok(k) => {
                            key = Some(k);
                            if remember {
                                let _ = actx.ui.send(UiEvent::SaveSecret {
                                    session_id: actx.profile.id.clone(),
                                    kind: SecretKind::Passphrase,
                                    value: pp,
                                });
                            }
                        }
                        Err(_) => actx.status("Wrong passphrase."),
                    }
                }
                _ => return Ok(None),
            }
        }
    }
    let Some(key) = key else {
        return Ok(None);
    };
    let hash_alg = if key.algorithm().is_rsa() {
        handle.best_supported_rsa_hash().await?.flatten()
    } else {
        None
    };
    let kwh = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
    let res = handle.authenticate_publickey(user, kwh).await?;
    if res.success() {
        Ok(Some(format!(
            "publickey ({})",
            path.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        )))
    } else {
        actx.status(format!("Key {path_str} not accepted."));
        audit::log(
            "auth.failed",
            serde_json::json!({"tab": actx.tab, "host": actx.profile.host, "user": user, "method": "publickey", "key": path_str}),
        );
        Ok(None)
    }
}

async fn try_agent(handle: &mut Handle<ClientHandler>, user: &str, actx: &AuthCtx<'_>) -> Result<Option<String>> {
    let Some(mut agent) = forward::connect_agent().await else {
        return Ok(None);
    };
    let identities = match agent.request_identities().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("agent: {e}");
            return Ok(None);
        }
    };
    if identities.is_empty() {
        return Ok(None);
    }
    actx.status(format!("Agent offers {} identit(y/ies)", identities.len()));
    for id in identities {
        let pk: PublicKey = id.public_key().into_owned();
        let hash_alg = if pk.algorithm().is_rsa() {
            handle.best_supported_rsa_hash().await?.flatten()
        } else {
            None
        };
        match handle
            .authenticate_publickey_with(user, pk.clone(), hash_alg, &mut agent)
            .await
        {
            Ok(r) if r.success() => {
                return Ok(Some(format!(
                    "agent ({} {})",
                    pk.algorithm().as_str(),
                    super::term::sanitize_title(id.comment())
                )));
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("agent identity rejected: {e}"),
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Tunnels
// ---------------------------------------------------------------------------

enum TunnelTask {
    Listener(JoinHandle<()>),
    Remote { addr: String, port: u32 },
}

impl TunnelTask {
    fn abort(&self) {
        if let TunnelTask::Listener(h) = self {
            h.abort();
        }
    }
    async fn stop(self, handle: &Arc<Handle<ClientHandler>>, map: &RemoteForwardMap) {
        match self {
            TunnelTask::Listener(h) => h.abort(),
            TunnelTask::Remote { addr, port } => {
                map.lock().remove(&(addr.clone(), port));
                let _ = handle.cancel_tcpip_forward(addr, port).await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_tunnel(
    handle: &Arc<Handle<ClientHandler>>,
    tab: TabId,
    idx: usize,
    spec: TunnelSpec,
    ui: &mpsc::UnboundedSender<UiEvent>,
    map: &RemoteForwardMap,
    allow_public_bind: bool,
    tunnels: &mut HashMap<usize, TunnelTask>,
) {
    if tunnels.contains_key(&idx) {
        return;
    }
    if !allow_public_bind && matches!(spec.kind, TunnelKind::Local | TunnelKind::Dynamic) {
        if let Err(e) = forward::ensure_loopback(&spec.bind_addr) {
            let _ = ui.send(UiEvent::Tunnel {
                tab,
                idx,
                ev: TunnelEvent::Error(e.to_string()),
            });
            return;
        }
    }
    let desc = spec.describe();
    match spec.kind {
        TunnelKind::Local => match forward::bind(&spec.bind_addr, spec.bind_port).await {
            Ok(listener) => {
                let h = handle.clone();
                let (ui2, spec2) = (ui.clone(), spec.clone());
                let jh = tokio::spawn(async move {
                    if let Err(e) = forward::run_local(h, listener, spec2, tab).await {
                        let _ = ui2.send(UiEvent::Tunnel {
                            tab,
                            idx,
                            ev: TunnelEvent::Error(e.to_string()),
                        });
                    }
                });
                tunnels.insert(idx, TunnelTask::Listener(jh));
                audit::log("forward.local.start", serde_json::json!({"tab": tab, "spec": desc}));
                let _ = ui.send(UiEvent::Tunnel {
                    tab,
                    idx,
                    ev: TunnelEvent::Started(desc),
                });
            }
            Err(e) => {
                let _ = ui.send(UiEvent::Tunnel {
                    tab,
                    idx,
                    ev: TunnelEvent::Error(format!("bind failed: {e}")),
                });
            }
        },
        TunnelKind::Dynamic => match forward::bind(&spec.bind_addr, spec.bind_port).await {
            Ok(listener) => {
                let h = handle.clone();
                let ui2 = ui.clone();
                let jh = tokio::spawn(async move {
                    if let Err(e) = forward::run_socks5(h, listener, tab).await {
                        let _ = ui2.send(UiEvent::Tunnel {
                            tab,
                            idx,
                            ev: TunnelEvent::Error(e.to_string()),
                        });
                    }
                });
                tunnels.insert(idx, TunnelTask::Listener(jh));
                audit::log("forward.dynamic.start", serde_json::json!({"tab": tab, "spec": desc}));
                let _ = ui.send(UiEvent::Tunnel {
                    tab,
                    idx,
                    ev: TunnelEvent::Started(desc),
                });
            }
            Err(e) => {
                let _ = ui.send(UiEvent::Tunnel {
                    tab,
                    idx,
                    ev: TunnelEvent::Error(format!("bind failed: {e}")),
                });
            }
        },
        TunnelKind::Remote => {
            let addr = if spec.bind_addr.is_empty() {
                "localhost".to_string()
            } else {
                spec.bind_addr.clone()
            };
            map.lock().insert(
                (addr.clone(), spec.bind_port as u32),
                (spec.target_host.clone(), spec.target_port),
            );
            match handle.tcpip_forward(addr.clone(), spec.bind_port as u32).await {
                Ok(actual) => {
                    let port = if actual == 0 { spec.bind_port as u32 } else { actual };
                    if port != spec.bind_port as u32 {
                        let mut m = map.lock();
                        let v = m.remove(&(addr.clone(), spec.bind_port as u32));
                        if let Some(v) = v {
                            m.insert((addr.clone(), port), v);
                        }
                    }
                    tunnels.insert(idx, TunnelTask::Remote { addr, port });
                    audit::log(
                        "forward.remote.start",
                        serde_json::json!({"tab": tab, "spec": desc, "port": port}),
                    );
                    let _ = ui.send(UiEvent::Tunnel {
                        tab,
                        idx,
                        ev: TunnelEvent::Started(format!("{desc} (remote port {port})")),
                    });
                }
                Err(e) => {
                    map.lock().remove(&(addr, spec.bind_port as u32));
                    let _ = ui.send(UiEvent::Tunnel {
                        tab,
                        idx,
                        ev: TunnelEvent::Error(format!("server refused remote forward: {e}")),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "ssh_tests.rs"]
mod tests;
