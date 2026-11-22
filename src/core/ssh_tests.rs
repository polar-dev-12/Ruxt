//! End-to-end tests: drive `run_session` against an in-process russh server.

use super::*;
use crate::core::config::{TunnelKind, TunnelSpec};
use crate::core::term::Size;
use russh::keys::ssh_key::{Algorithm as KeyAlg, PrivateKey};
use russh::server::{self, Auth, Msg as SMsg, Session as SSession};
use russh::ChannelId;
use std::net::SocketAddr;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Default)]
struct TestServer {
    shell: Arc<Mutex<Option<ChannelId>>>,
    /// When set, the server tries to open an unsolicited forwarded-tcpip channel as soon as
    /// the shell starts and records whether the client accepted it.
    probe_forward: bool,
    forward_accepted: Arc<Mutex<Option<bool>>>,
    /// `exec` requests served so far (the monitor script gets changing CPU counters).
    execs: Arc<std::sync::atomic::AtomicU64>,
}

impl server::Server for TestServer {
    type Handler = TestServer;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        // One client per test server, so sharing the state is fine.
        self.clone()
    }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "tester" && password == "s3cret" {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<SMsg>,
        reply: server::ChannelOpenHandle,
        _session: &mut SSession,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _c: u32,
        _r: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut SSession,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut SSession) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        *self.shell.lock() = Some(channel);
        session.data(channel, &b"Welcome to testsrv\r\n"[..])?;
        if self.probe_forward {
            let handle = session.handle();
            let slot = self.forward_accepted.clone();
            tokio::spawn(async move {
                let r = handle
                    .channel_open_forwarded_tcpip("localhost", 7777, "10.0.0.1", 5555)
                    .await;
                *slot.lock() = Some(r.is_ok());
            });
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut SSession,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data);
        if !cmd.contains("@@HOST") {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let n = self.execs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        // Each call advances the counters by 1000 jiffies, 800 of them idle → 20 % busy.
        let out = format!(
            "@@HOST\ntestsrv\n@@UNAME\nLinux 6.1.0 x86_64\n@@LOAD\n0.10 0.20 0.30 1/2 3\n@@STAT\ncpu {} 0 {} {} 0 0 0 0 0 0\n@@NCPU\n4\n@@MEM\nMemTotal: 1000 kB\nMemAvailable: 400 kB\n@@DF\nFilesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 100 40 60 40% /\n@@END\n",
            100 * n,
            100 * n,
            800 * n
        );
        session.data(channel, out.into_bytes())?;
        session.exit_status_request(channel, 0)?;
        session.close(channel)?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut SSession) -> Result<(), Self::Error> {
        // Only the shell channel gets the "echo:" treatment; forwarded channels are
        // served by the stream task in `channel_open_direct_tcpip`.
        if *self.shell.lock() == Some(channel) {
            let reply = format!("echo:{}\r\n", String::from_utf8_lossy(data).trim());
            session.data(channel, reply.into_bytes())?;
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<SMsg>,
        _host: &str,
        _port: u32,
        _oa: &str,
        _op: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut SSession,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        tokio::spawn(async move {
            let mut s = channel.into_stream();
            let mut buf = [0u8; 256];
            loop {
                match s.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if s.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(())
    }
}

async fn start_server() -> (u16, PublicKey) {
    start_server_with(TestServer::default()).await
}

async fn start_server_with(mut srv: TestServer) -> (u16, PublicKey) {
    let key = PrivateKey::random(&mut rand::rng(), KeyAlg::Ed25519).unwrap();
    let pubkey = key.public_key().clone();
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = socket.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = server::Server::run_on_socket(&mut srv, config, &socket).await;
    });
    (port, pubkey)
}

fn temp_store() -> (Arc<HostKeyStore>, PathBuf) {
    let mut r = [0u8; 4];
    rand::fill(&mut r);
    let dir = std::env::temp_dir().join(format!("ruxt-e2e-{}-{}", std::process::id(), hex::encode(r)));
    std::fs::create_dir_all(&dir).unwrap();
    (Arc::new(HostKeyStore::new(dir.join("known_hosts"))), dir)
}

fn term_text(term: &Arc<Mutex<TermState>>) -> String {
    let t = term.lock();
    let content = t.term.renderable_content();
    let mut s = String::new();
    let mut last = i32::MIN;
    for c in content.display_iter {
        if c.point.line.0 != last {
            s.push('\n');
            last = c.point.line.0;
        }
        s.push(c.cell.c);
    }
    s
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Harness {
    term: Arc<Mutex<TermState>>,
    ui_rx: mpsc::UnboundedReceiver<UiEvent>,
    cmd_tx: mpsc::UnboundedSender<SessionCmd>,
}

fn launch(profile: SessionProfile, store: Arc<HostKeyStore>) -> Harness {
    launch_with(profile, store, MonitorControl::new(false, 5))
}

fn launch_with(profile: SessionProfile, store: Arc<HostKeyStore>, monitor: MonitorControl) -> Harness {
    let ctx = egui::Context::default();
    let (term, pty_rx) = TermState::new(ctx.clone(), Size { cols: 80, rows: 24 }, 1000, false);
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(run_session(SessionArgs {
        tab: 1,
        profile,
        secrets: None,
        jumps: Vec::new(),
        term: term.clone(),
        pty_rx,
        ui: ui_tx,
        cmds: cmd_rx,
        ctx,
        hostkeys: store,
        allow_public_bind: false,
        monitor,
    }));
    Harness { term, ui_rx, cmd_tx }
}

#[test]
fn security_profiles_offer_no_sha1_or_legacy_ciphers_except_legacy_hostkeys() {
    for profile in [
        SecurityProfile::Strict,
        SecurityProfile::Compatible,
        SecurityProfile::Legacy,
    ] {
        let (kex, keys, ciphers, macs) = profile_algorithms(profile);
        assert!(!kex.iter().any(|k| k.contains("sha1")), "{profile:?}: {kex:?}");
        assert!(!keys.iter().any(|k| k == "ssh-dss"), "{profile:?}: {keys:?}");
        assert!(
            !ciphers
                .iter()
                .any(|c| c.contains("cbc") || c.contains("3des") || c.contains("arcfour")),
            "{profile:?}: {ciphers:?}"
        );
        assert!(!macs.iter().any(|m| m.contains("sha1")), "{profile:?}: {macs:?}");
        let has_ssh_rsa = keys.iter().any(|k| k == "ssh-rsa");
        assert_eq!(has_ssh_rsa, profile == SecurityProfile::Legacy, "{profile:?}: {keys:?}");
        assert!(keys.iter().any(|k| k == "rsa-sha2-256"), "{profile:?}: {keys:?}");
    }
    let (kex, _, ciphers, _) = profile_algorithms(SecurityProfile::Strict);
    assert_eq!(kex[0], "mlkem768x25519-sha256");
    assert!(
        ciphers.iter().all(|c| c.contains("gcm") || c.contains("chacha20")),
        "{ciphers:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_server_monitor_samples_cpu_memory_and_disk() {
    let (port, pubkey) = start_server().await;
    let (store, dir) = temp_store();
    store.learn("127.0.0.1", port, &pubkey).unwrap();
    let profile = SessionProfile {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth: crate::core::config::AuthMethod::Password,
        ..Default::default()
    };
    let ctl = MonitorControl::new(true, 2);
    let mut h = launch_with(profile, store, ctl.clone());
    // password prompt
    loop {
        match next(&mut h).await {
            UiEvent::Prompt { mut prompt, .. } => {
                assert!(matches!(prompt.kind, PromptKind::Password { .. }));
                prompt.answer(PromptReply::Secret(SecretString::from("s3cret".to_string()), false));
                break;
            }
            UiEvent::Status { .. } => {}
            other => panic!("unexpected event before the password prompt: {}", event_name(&other)),
        }
    }
    let mut samples = Vec::new();
    while samples.len() < 2 {
        match next(&mut h).await {
            UiEvent::Monitor { tab, ev } => {
                assert_eq!(tab, 1);
                match ev {
                    MonitorEvent::Sample(s) => samples.push(*s),
                    other => panic!("monitor: {other:?}"),
                }
            }
            UiEvent::Disconnected { reason, .. } => panic!("disconnected: {reason}"),
            _ => {}
        }
    }
    assert_eq!(samples[0].hostname, "testsrv");
    assert_eq!(samples[0].cpu_pct, None, "first sample has no delta");
    assert_eq!(samples[0].load, Some([0.1, 0.2, 0.3]));
    assert_eq!(samples[0].ncpu, Some(4));
    assert!((samples[0].mem_used_pct().unwrap() - 60.0).abs() < 0.01);
    assert_eq!(samples[0].disks.len(), 1);
    assert!((samples[0].disks[0].used_pct() - 40.0).abs() < 0.01);
    assert!(
        (samples[1].cpu_pct.unwrap() - 20.0).abs() < 0.01,
        "{:?}",
        samples[1].cpu_pct
    );
    // Switching the monitor off stops the polling; the session itself keeps running.
    ctl.set_enabled(false);
    let _ = h.cmd_tx.send(SessionCmd::Disconnect);
    loop {
        if let UiEvent::Disconnected { .. } = next(&mut h).await {
            break;
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

fn event_name(ev: &UiEvent) -> &'static str {
    match ev {
        UiEvent::Connected { .. } => "Connected",
        UiEvent::Disconnected { .. } => "Disconnected",
        UiEvent::Status { .. } => "Status",
        UiEvent::Prompt { .. } => "Prompt",
        UiEvent::Sftp { .. } => "Sftp",
        UiEvent::Tunnel { .. } => "Tunnel",
        UiEvent::SaveSecret { .. } => "SaveSecret",
        UiEvent::VaultOp(_) => "VaultOp",
        UiEvent::AppLock(_) => "AppLock",
        UiEvent::Monitor { .. } => "Monitor",
        UiEvent::Tail { .. } => "Tail",
    }
}

async fn next(h: &mut Harness) -> UiEvent {
    tokio::time::timeout(Duration::from_secs(20), h.ui_rx.recv())
        .await
        .expect("timed out waiting for UI event")
        .expect("ui channel closed")
}

fn local_spec(port: u16) -> TunnelSpec {
    TunnelSpec {
        kind: TunnelKind::Local,
        bind_addr: "127.0.0.1".into(),
        bind_port: port,
        target_host: "127.0.0.1".into(),
        target_port: 9,
        auto_start: false,
    }
}

fn dynamic_spec(port: u16) -> TunnelSpec {
    TunnelSpec {
        kind: TunnelKind::Dynamic,
        bind_addr: "127.0.0.1".into(),
        bind_port: port,
        target_host: String::new(),
        target_port: 0,
        auto_start: false,
    }
}

/// Round-trip three bytes through the test server's echoing direct-tcpip channel.
async fn echo_through(mut s: TcpStream, payload: &[u8; 3]) {
    s.write_all(payload).await.unwrap();
    let mut buf = [0u8; 3];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf, payload);
}

/// Perform a SOCKS5 CONNECT handshake with the -D listener on `port`.
async fn socks5_connect(port: u16) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [5, 0]);
    s.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0, 9]).await.unwrap();
    let mut r = [0u8; 10];
    s.read_exact(&mut r).await.unwrap();
    assert_eq!(r[1], 0, "SOCKS5 reply must be 'succeeded'");
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_tofu_password_shell_local_forward_and_socks() {
    let (port, server_key) = start_server().await;
    let (store, dir) = temp_store();
    let fwd_port = free_port();
    let socks_port = free_port();
    let profile = SessionProfile {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth: AuthMethod::Password,
        tunnels: vec![local_spec(fwd_port), dynamic_spec(socks_port)],
        ..Default::default()
    };
    let mut h = launch(profile, store.clone());

    let mut saw_hostkey = false;
    let mut saw_password = false;
    let mut tunnel_ok = false;
    let mut socks_ok = false;
    loop {
        match next(&mut h).await {
            UiEvent::Prompt { mut prompt, .. } => match prompt.kind.clone() {
                PromptKind::HostKeyUnknown { fingerprint, .. } => {
                    assert_eq!(fingerprint, hostkeys::fingerprint_sha256(&server_key));
                    saw_hostkey = true;
                    prompt.answer(PromptReply::Accept);
                }
                PromptKind::Password { user, .. } => {
                    assert_eq!(user, "tester");
                    saw_password = true;
                    prompt.answer(PromptReply::Secret(SecretString::from("s3cret".to_string()), false));
                }
                other => panic!("unexpected prompt {other:?}"),
            },
            UiEvent::Connected { info, .. } => {
                assert_eq!(info.auth_method, "password");
                assert_eq!(info.host_key_algorithm, "ssh-ed25519");
                h.cmd_tx.send(SessionCmd::Input(b"ping\r".to_vec())).unwrap();
                h.cmd_tx
                    .send(SessionCmd::StartTunnel {
                        idx: 0,
                        spec: local_spec(fwd_port),
                    })
                    .unwrap();
                h.cmd_tx
                    .send(SessionCmd::StartTunnel {
                        idx: 1,
                        spec: dynamic_spec(socks_port),
                    })
                    .unwrap();
            }
            UiEvent::Tunnel {
                idx,
                ev: TunnelEvent::Started(_),
                ..
            } => {
                if idx == 0 {
                    echo_through(TcpStream::connect(("127.0.0.1", fwd_port)).await.unwrap(), b"abc").await;
                    tunnel_ok = true;
                } else {
                    echo_through(socks5_connect(socks_port).await, b"xyz").await;
                    socks_ok = true;
                }
                if tunnel_ok && socks_ok {
                    // wait for the shell echo to land in the emulator
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !term_text(&h.term).contains("echo:ping") {
                        assert!(
                            Instant::now() < deadline,
                            "shell echo never arrived:\n{}",
                            term_text(&h.term)
                        );
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    assert!(term_text(&h.term).contains("Welcome to testsrv"));
                    h.cmd_tx.send(SessionCmd::Disconnect).unwrap();
                }
            }
            UiEvent::Tunnel {
                ev: TunnelEvent::Error(e),
                ..
            } => panic!("tunnel error: {e}"),
            UiEvent::Disconnected { reason, .. } => {
                assert!(reason.contains("user"), "reason = {reason}");
                break;
            }
            _ => {}
        }
    }
    assert!(saw_hostkey && saw_password && tunnel_ok && socks_ok);
    assert!(h.term.lock().closed);
    assert!(matches!(
        store.check("127.0.0.1", port, &server_key),
        HostKeyStatus::Known
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_changed_host_key_accepted_only_after_explicit_replace() {
    let (port, server_key) = start_server().await;
    let (store, dir) = temp_store();
    let bogus = PrivateKey::random(&mut rand::rng(), KeyAlg::Ed25519).unwrap();
    store.learn("127.0.0.1", port, bogus.public_key()).unwrap();

    let profile = SessionProfile {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth: AuthMethod::Password,
        ..Default::default()
    };
    let mut h = launch(profile, store.clone());
    let mut connected = false;
    loop {
        match next(&mut h).await {
            UiEvent::Prompt { mut prompt, .. } => match prompt.kind.clone() {
                PromptKind::HostKeyChanged { file, .. } => {
                    assert_eq!(file, store.app_file());
                    prompt.answer(PromptReply::Accept);
                }
                PromptKind::Password { .. } => {
                    prompt.answer(PromptReply::Secret(SecretString::from("s3cret".to_string()), false))
                }
                other => panic!("unexpected prompt {other:?}"),
            },
            UiEvent::Connected { .. } => {
                connected = true;
                h.cmd_tx.send(SessionCmd::Disconnect).unwrap();
            }
            UiEvent::Disconnected { .. } => break,
            _ => {}
        }
    }
    assert!(connected);
    // The stale key is gone and the new one is pinned, with exactly one line left.
    assert!(matches!(
        store.check("127.0.0.1", port, &server_key),
        HostKeyStatus::Known
    ));
    assert!(matches!(
        store.check("127.0.0.1", port, bogus.public_key()),
        HostKeyStatus::Changed { .. }
    ));
    let lines = std::fs::read_to_string(store.app_file())
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(lines, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_unsolicited_forwarded_tcpip_is_rejected() {
    let srv = TestServer {
        probe_forward: true,
        ..Default::default()
    };
    let slot = srv.forward_accepted.clone();
    let (port, _k) = start_server_with(srv).await;
    let (store, dir) = temp_store();
    let profile = SessionProfile {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth: AuthMethod::Password,
        ..Default::default()
    };
    let mut h = launch(profile, store);
    loop {
        match next(&mut h).await {
            UiEvent::Prompt { mut prompt, .. } => match prompt.kind.clone() {
                PromptKind::HostKeyUnknown { .. } => prompt.answer(PromptReply::Accept),
                PromptKind::Password { .. } => {
                    prompt.answer(PromptReply::Secret(SecretString::from("s3cret".to_string()), false))
                }
                other => panic!("unexpected prompt {other:?}"),
            },
            UiEvent::Connected { .. } => {
                let deadline = Instant::now() + Duration::from_secs(10);
                while slot.lock().is_none() {
                    assert!(Instant::now() < deadline, "server never got a verdict on its channel");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                assert_eq!(
                    *slot.lock(),
                    Some(false),
                    "a forwarded-tcpip channel nobody asked for must be refused"
                );
                h.cmd_tx.send(SessionCmd::Disconnect).unwrap();
            }
            UiEvent::Disconnected { .. } => break,
            _ => {}
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_changed_host_key_is_refused() {
    let (port, _server_key) = start_server().await;
    let (store, dir) = temp_store();
    // Pin a *different* key for this host:port first.
    let bogus = PrivateKey::random(&mut rand::rng(), KeyAlg::Ed25519).unwrap();
    store.learn("127.0.0.1", port, bogus.public_key()).unwrap();

    let profile = SessionProfile {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth: AuthMethod::Password,
        ..Default::default()
    };
    let mut h = launch(profile, store.clone());
    let mut saw_changed = false;
    loop {
        match next(&mut h).await {
            UiEvent::Prompt { mut prompt, .. } => match prompt.kind.clone() {
                PromptKind::HostKeyChanged { .. } => {
                    saw_changed = true;
                    prompt.answer(PromptReply::Reject);
                }
                other => panic!("unexpected prompt {other:?}"),
            },
            UiEvent::Connected { .. } => panic!("must not connect with a changed host key"),
            UiEvent::Disconnected { .. } => break,
            _ => {}
        }
    }
    assert!(saw_changed);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_wrong_password_then_cancel() {
    let (port, _k) = start_server().await;
    let (store, dir) = temp_store();
    let profile = SessionProfile {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth: AuthMethod::Password,
        ..Default::default()
    };
    let mut h = launch(profile, store);
    let mut attempts = 0;
    loop {
        match next(&mut h).await {
            UiEvent::Prompt { mut prompt, .. } => match prompt.kind.clone() {
                PromptKind::HostKeyUnknown { .. } => prompt.answer(PromptReply::Accept),
                PromptKind::Password { attempt, .. } => {
                    attempts = attempt;
                    if attempt == 1 {
                        prompt.answer(PromptReply::Secret(SecretString::from("wrong".to_string()), false));
                    } else {
                        prompt.answer(PromptReply::Cancel);
                    }
                }
                other => panic!("unexpected prompt {other:?}"),
            },
            UiEvent::Connected { .. } => panic!("must not connect"),
            UiEvent::Disconnected { reason, .. } => {
                assert!(reason.contains("authentication"), "{reason}");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(attempts, 2);
    let _ = std::fs::remove_dir_all(dir);
}
