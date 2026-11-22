//! X server integration: detection, hardened launch, lifecycle.
//!
//! The only server Ruxt runs is **Cygwin/X (`XWin.exe`)**: normally the tree embedded in the
//! Windows executable and unpacked on first use (see `xbundle` and
//! `installer/fetch-xserver.ps1`). An `xserver/` tree next to the executable, or the one
//! `RUXT_XSERVER_DIR` names, takes precedence; then an `XWin.exe` on `PATH`; the path set in
//! Settings comes before all of them. It tracks X.org point releases closely and is built
//! with the SECURITY extension that untrusted X11 forwarding needs. VcXsrv and Xming are not supported any more: their last
//! releases lag X.org and they lack the SECURITY extension.
//!
//! Ruxt starts the server with a freshly generated MIT-MAGIC-COOKIE-1
//! authority file (so *only* clients that hold the cookie — i.e. our own X11 forwarding
//! bridge — can connect), never with `-ac`, tracks the process, and stops it again on
//! request or at exit.
//!
//! X.org has no "listen on loopback only" switch: `-listen tcp` binds port 6000+n on every
//! interface, and only the cookie keeps strangers out. Before each start on Windows, Ruxt
//! asks (UAC, once) for an inbound *block* rule on `XWin.exe`. The firewall never filters
//! loopback, so the X11 bridge still connects while the LAN sees a closed port. Declining
//! the prompt still starts the server; the status menu keeps the warning.

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Display name of the only supported server.
pub const SERVER_NAME: &str = "Cygwin/X";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XState {
    /// Started by Ruxt on this display.
    Running(u16),
    /// Somebody else's server listens on this display.
    External(u16),
    Stopped,
    NotInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XServerInfo {
    /// Path of `XWin.exe`.
    pub path: PathBuf,
    /// True when it came from the bundled tree (or `RUXT_XSERVER_DIR`).
    pub bundled: bool,
    /// The tree embedded in the executable: unpacked and verified by `start`.
    pub embedded: bool,
}

/// Where to read about getting an X server when neither the bundled tree nor a fallback exists.
pub const DOWNLOAD_URL: &str = "https://x.cygwin.com/";

/// Name of the Windows Firewall rule that keeps the X server's TCP port off the network.
#[cfg(windows)]
pub const FIREWALL_RULE_NAME: &str = "Ruxt X server (block inbound)";

/// Is there an inbound block rule for `exe`? `None` where Windows Firewall does not apply.
#[cfg(windows)]
pub fn firewall_rule_present(exe: &Path) -> Option<bool> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={FIREWALL_RULE_NAME}"),
            "verbose",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return Some(false);
    }
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    let want = exe.to_string_lossy().to_lowercase();
    Some(text.contains(&want))
}

#[cfg(not(windows))]
pub fn firewall_rule_present(_exe: &Path) -> Option<bool> {
    None
}

/// Add the inbound block rule for `exe` through an elevated `netsh` (UAC prompt).
#[cfg(windows)]
pub fn add_firewall_rule(exe: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = exe.to_string_lossy().replace('\'', "''");
    let args = format!(
        "advfirewall firewall add rule name=\"{FIREWALL_RULE_NAME}\" dir=in action=block program=\"{exe}\" enable=yes profile=any"
    );
    // Wait so the rule exists before XWin binds. A declined UAC prompt makes Start-Process throw.
    let script = format!(
        "$p = Start-Process -FilePath netsh -Verb RunAs -WindowStyle Hidden -Wait -PassThru -ArgumentList '{args}'; if ($null -eq $p) {{ exit 1 }}; exit $p.ExitCode"
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("starting PowerShell")?;
    if !status.success() {
        bail!("the elevation prompt was declined or netsh failed");
    }
    super::audit::log("xserver.firewall_rule_added", serde_json::json!({"exe": exe}));
    Ok(())
}

#[cfg(not(windows))]
pub fn add_firewall_rule(_exe: &Path) -> Result<()> {
    bail!("only applicable on Windows")
}

/// Directory of the bundled Cygwin/X tree: `$RUXT_XSERVER_DIR`, else `xserver/` beside the
/// executable. The layout is a minimal Cygwin root (`bin/XWin.exe`, `bin/cygwin1.dll`, …).
pub fn bundled_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RUXT_XSERVER_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("xserver")))
}

/// Is this file name Cygwin/X's server? Other servers are refused even as an override.
pub fn is_xwin(p: &Path) -> bool {
    // Split on both separators: off Windows `Path` does not treat `\` as one.
    let s = p.to_string_lossy();
    s.rsplit(['/', '\\'])
        .next()
        .is_some_and(|f| f.eq_ignore_ascii_case("xwin.exe"))
}

/// Locate Cygwin/X: the Settings override (must be an `XWin.exe`), the bundled tree, then
/// an `XWin.exe` on `PATH` (a full Cygwin install).
pub fn find_installed(override_path: &str) -> Option<XServerInfo> {
    let o = override_path.trim();
    if !o.is_empty() {
        let p = PathBuf::from(o);
        if p.is_file() && is_xwin(&p) {
            return Some(XServerInfo {
                path: p,
                bundled: false,
                embedded: false,
            });
        }
    }
    if let Some(d) = bundled_dir() {
        let p = d.join("bin").join("XWin.exe");
        if p.is_file() {
            return Some(XServerInfo {
                path: p,
                bundled: true,
                embedded: false,
            });
        }
    }
    // An explicit RUXT_XSERVER_DIR replaces the embedded tree, which need not be unpacked yet.
    #[cfg(ruxt_embedded_xserver)]
    if std::env::var_os("RUXT_XSERVER_DIR").is_none_or(|d| d.is_empty()) {
        if let Some(d) = super::xbundle::dir() {
            return Some(XServerInfo {
                path: d.join("bin").join("XWin.exe"),
                bundled: true,
                embedded: true,
            });
        }
    }
    if let Some(p) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&p) {
            let p = d.join("XWin.exe");
            if p.is_file() {
                return Some(XServerInfo {
                    path: p,
                    bundled: false,
                    embedded: false,
                });
            }
        }
    }
    None
}

/// A server Ruxt has just launched, and the latest moment it may still be starting.
///
/// XWin stops answering *every* client, for good, when one connects (even just to close the
/// socket again) before its own window-manager and clipboard clients have connected, about a
/// second after the port opens. Reproduced with 21.1.21 and 21.1.25. Nothing in Ruxt may touch
/// the port until [`READY_MARKER`] shows up in the server log.
static STARTING: parking_lot::Mutex<Option<(u16, Instant)>> = parking_lot::Mutex::new(None);

/// Logged by XWin once its internal window manager is connected (always on: `-multiwindow`).
const READY_MARKER: &str = "winMultiWindowXMsgProc - xcb_connect() returned";
/// Upper bound on the wait; a slower start is then treated as ready.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether `display` is a server Ruxt launched that has not finished starting.
pub fn is_starting(display: u16) -> bool {
    let mut g = STARTING.lock();
    match *g {
        Some((d, until)) if d == display => {
            if Instant::now() < until {
                true
            } else {
                *g = None;
                false
            }
        }
        _ => false,
    }
}

fn finish_starting(display: u16) {
    let mut g = STARTING.lock();
    if matches!(*g, Some((d, _)) if d == display) {
        *g = None;
    }
}

/// Wait until a server Ruxt just launched on `display` accepts clients safely (returns at
/// once for any other display).
pub async fn wait_until_ready(display: u16) {
    while is_starting(display) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll the server log for [`READY_MARKER`]; true if it appeared before `deadline`.
fn wait_for_ready_marker(log: &Path, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if std::fs::read(log).is_ok_and(|b| String::from_utf8_lossy(&b).contains(READY_MARKER)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Probe which displays (:0 … :9) have something listening on 127.0.0.1:6000+n.
/// Probes all 10 display ports concurrently so the total wall time is bounded by one timeout.
/// A server of ours that is still starting counts as listening and is not connected to.
pub fn listening_displays() -> Vec<u16> {
    std::thread::scope(|s| {
        let handles: Vec<_> = (0u16..10)
            .map(|n| {
                s.spawn(move || {
                    if is_starting(n) {
                        return Some(n);
                    }
                    TcpStream::connect_timeout(
                        &SocketAddr::from((Ipv4Addr::LOCALHOST, 6000 + n)),
                        Duration::from_millis(50),
                    )
                    .is_ok()
                    .then_some(n)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    })
}

/// Write an .Xauthority file holding one MIT-MAGIC-COOKIE-1 entry for `display`.
fn write_xauth(path: &Path, display: u16, cookie: &[u8; 16]) -> Result<()> {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".into());
    fn field(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
        out.extend_from_slice(b);
    }
    let number = display.to_string();
    let mut out = Vec::new();
    // (family, address): FamilyLocal (256) with the host name, and FamilyInternet (0) with
    // 127.0.0.1 so ordinary local X clients can find the cookie as well.
    let entries: [(u16, &[u8]); 2] = [(256, host.as_bytes()), (0, &[127, 0, 0, 1])];
    for (family, addr) in entries {
        out.extend_from_slice(&family.to_be_bytes());
        field(&mut out, addr);
        field(&mut out, number.as_bytes());
        field(&mut out, b"MIT-MAGIC-COOKIE-1");
        field(&mut out, cookie);
    }
    super::config::write_private_atomic(path, &out)
}

/// Installed binary, listening displays, and (when the binary changed) its firewall state.
type ProbeResult = (Option<XServerInfo>, Vec<u16>, Option<(PathBuf, Option<bool>)>);

#[derive(Default)]
struct ProbeState {
    in_flight: bool,
    result: Option<ProbeResult>,
}

fn probe(override_path: &str, fw_known_for: Option<&Path>) -> ProbeResult {
    let installed = find_installed(override_path);
    let listening = listening_displays();
    let fw = installed
        .as_ref()
        .filter(|i| fw_known_for != Some(i.path.as_path()))
        .map(|i| (i.path.clone(), firewall_rule_present(&i.path)));
    (installed, listening, fw)
}

pub struct XServerManager {
    config_dir: PathBuf,
    override_path: String,
    /// Pass `-clipboard` so remote X clients can use the local clipboard (opt-in).
    clipboard: bool,
    pub installed: Option<XServerInfo>,
    pub listening: Vec<u16>,
    last_refresh: Option<Instant>,
    probe: Arc<Mutex<ProbeState>>,
    child: Option<Child>,
    managed_display: Option<u16>,
    auth_file: Option<PathBuf>,
    pub last_error: Option<String>,
    /// Firewall block rule present for this executable (`None` = unknown / not Windows).
    firewall: Option<(PathBuf, Option<bool>)>,
}

impl XServerManager {
    pub fn new(config_dir: PathBuf, override_path: String, clipboard: bool) -> Self {
        let mut m = Self {
            config_dir,
            override_path,
            clipboard,
            installed: None,
            listening: Vec::new(),
            last_refresh: None,
            probe: Arc::new(Mutex::new(ProbeState::default())),
            child: None,
            managed_display: None,
            auth_file: None,
            last_error: None,
            firewall: None,
        };
        // The first probe runs off-thread too; the toolbar picks the result up within a frame.
        m.refresh(false);
        m
    }

    pub fn set_override_path(&mut self, p: &str) {
        if p != self.override_path {
            self.override_path = p.to_string();
            self.refresh(true);
        }
    }

    /// Applies to the next server start.
    pub fn set_clipboard(&mut self, on: bool) {
        self.clipboard = on;
    }

    /// Re-scan (installed binary, listening displays, managed child state). Cheap enough to
    /// call every frame: when `force` is false, probing runs in a background thread so the UI
    /// thread never blocks on TCP timeouts.
    pub fn refresh(&mut self, force: bool) {
        if let Some((installed, listening, fw)) = self.probe.lock().result.take() {
            self.installed = installed;
            self.listening = listening;
            if fw.is_some() {
                self.firewall = fw;
            }
        }
        if let Some(c) = self.child.as_mut() {
            match c.try_wait() {
                Ok(Some(status)) => {
                    super::audit::log(
                        "xserver.exited",
                        serde_json::json!({"display": self.managed_display, "status": status.to_string()}),
                    );
                    self.child = None;
                    self.managed_display = None;
                    super::x11::clear_managed_auth();
                    self.last_error = Some(format!("X server exited ({status})"));
                }
                Ok(None) => {}
                Err(e) => self.last_error = Some(e.to_string()),
            }
        }
        if force {
            self.last_refresh = Some(Instant::now());
            let (installed, listening, fw) = probe(&self.override_path, None);
            self.installed = installed;
            self.listening = listening;
            if fw.is_some() {
                self.firewall = fw;
            }
            return;
        }
        if let Some(t) = self.last_refresh {
            if t.elapsed() < Duration::from_secs(3) {
                return;
            }
        }
        let mut guard = self.probe.lock();
        if guard.in_flight {
            return;
        }
        guard.in_flight = true;
        drop(guard);
        self.last_refresh = Some(Instant::now());
        let override_path = self.override_path.clone();
        let state = self.probe.clone();
        let fw_known_for = self.firewall.as_ref().map(|(p, _)| p.clone());
        let _ = std::thread::Builder::new().name("xserver-probe".into()).spawn(move || {
            let r = probe(&override_path, fw_known_for.as_deref());
            let mut g = state.lock();
            g.result = Some(r);
            g.in_flight = false;
        });
    }

    /// `Some(false)` when the installed server has no inbound firewall block rule (its TCP
    /// port is reachable from the network, protected by the cookie alone).
    pub fn firewall_rule(&self) -> Option<bool> {
        match (&self.installed, &self.firewall) {
            (Some(i), Some((p, state))) if *p == i.path => *state,
            _ => None,
        }
    }

    /// Forget the cached firewall state so the next probe re-checks it.
    pub fn recheck_firewall(&mut self) {
        self.firewall = None;
        self.last_refresh = None;
    }

    pub fn is_managed_running(&self) -> bool {
        self.child.is_some()
    }

    /// Kill X servers a previous Ruxt left behind: the one named in `xserver.pid` and any
    /// copy of *our* bundled `XWin.exe` whose parent process is gone (a crash or a forced
    /// kill never reaches `stop`). Servers started by hand or by another running Ruxt are
    /// left alone. Returns how many were killed.
    pub fn reap_orphans(&mut self) -> usize {
        let mut killed = 0usize;
        let pidf = pid_file(&self.config_dir);
        let recorded: Option<u32> = std::fs::read_to_string(&pidf)
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|p| p.parse().ok()));
        let ours = self.installed.as_ref().map(|i| i.path.clone());
        for (pid, parent_alive, path) in running_xwins() {
            let is_recorded = recorded == Some(pid);
            let is_our_binary = ours
                .as_ref()
                .map(|o| o.to_string_lossy().eq_ignore_ascii_case(&path.to_string_lossy()))
                .unwrap_or(false);
            if is_recorded || (is_our_binary && !parent_alive) {
                kill_tree(pid);
                killed += 1;
            }
        }
        let _ = std::fs::remove_file(&pidf);
        if killed > 0 {
            super::audit::log("xserver.reaped", serde_json::json!({"count": killed}));
            // Give the ports a moment to close before anything probes them.
            std::thread::sleep(Duration::from_millis(300));
            self.last_refresh = None;
        }
        killed
    }

    /// Any X server reachable (ours or a foreign one)?
    pub fn any_running(&self) -> bool {
        !self.listening.is_empty() || self.child.is_some()
    }

    /// Display to use for new sessions when the profile does not pin one.
    pub fn default_display(&self) -> Option<u16> {
        self.managed_display.or_else(|| self.listening.first().copied())
    }

    /// What the toolbar should say (the UI translates it).
    pub fn state(&self) -> XState {
        if let Some(d) = self.managed_display {
            XState::Running(d)
        } else if let Some(d) = self.listening.first().copied() {
            XState::External(d)
        } else if self.installed.is_some() {
            XState::Stopped
        } else {
            XState::NotInstalled
        }
    }

    /// Ask for the inbound block rule when it is missing. Returns whether the port will
    /// still be reachable from the network (the user declined, or `netsh` failed).
    fn arm_firewall(&mut self, exe: &Path) -> bool {
        #[cfg(windows)]
        {
            match firewall_rule_present(exe) {
                Some(true) => {
                    self.firewall = Some((exe.to_path_buf(), Some(true)));
                    false
                }
                Some(false) => match add_firewall_rule(exe) {
                    Ok(()) => {
                        self.firewall = Some((exe.to_path_buf(), Some(true)));
                        false
                    }
                    Err(_) => {
                        self.firewall = Some((exe.to_path_buf(), Some(false)));
                        true
                    }
                },
                None => false,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = exe;
            false
        }
    }

    /// Start the installed X server on the first free display with a random cookie.
    pub fn start(&mut self) -> Result<u16> {
        if let Some(d) = self.managed_display {
            return Ok(d);
        }
        let info = self
            .installed
            .clone()
            .ok_or_else(|| anyhow!("no X server found (bundled Cygwin/X tree missing)"))?;
        #[cfg(ruxt_embedded_xserver)]
        if info.embedded {
            let dir = super::xbundle::dir().ok_or_else(|| anyhow!("no local data directory"))?;
            super::xbundle::ensure(&dir)?;
        }
        // Block LAN access before the port opens. A declined prompt is recorded and does not
        // abort the start; the cookie remains the remaining control.
        self.arm_firewall(&info.path);
        self.listening = listening_displays();
        let display = (0u16..10)
            .find(|n| !self.listening.contains(n))
            .ok_or_else(|| anyhow!("displays :0-:9 are all in use"))?;

        let mut cookie = [0u8; 16];
        rand::fill(&mut cookie);
        let auth_file = self.config_dir.join(format!("xauth-{display}"));
        write_xauth(&auth_file, display, &cookie).context("writing X authority file")?;

        let mut args: Vec<String> = vec![format!(":{display}"), "-multiwindow".into()];
        if self.clipboard {
            // Bridges X selections to the Windows clipboard: convenient, but it lets any remote
            // X client read and write the local clipboard, so it stays opt-in.
            args.push("-clipboard".into());
        } else {
            args.push("-noclipboard".into());
        }
        // Cygwin's build does not listen on TCP unless asked; our bridge connects to
        // 127.0.0.1:6000+n, so enable IPv4 TCP only. The font path is explicit because the
        // bundled tree has no `fontpath.d` catalogue (its entry names contain ':'), and
        // `built-ins` always provides `fixed` and `cursor`. The log goes next to the cookie
        // (Cygwin accepts Win32 paths) instead of /var/log in the tree.
        for a in [
            "-wgl",
            "-listen",
            "tcp",
            "-nolisten",
            "inet6",
            "-silent-dup-error",
            "-fp",
            "/usr/share/X11/fonts/misc,built-ins",
            "-logfile",
        ] {
            args.push(a.into());
        }
        // A log left by an earlier run would already hold the ready marker.
        let log_file = self.config_dir.join(format!("xwin-{display}.log"));
        let _ = std::fs::remove_file(&log_file);
        args.push(log_file.display().to_string());
        args.push("-auth".into());
        args.push(auth_file.display().to_string());
        let mut cmd = launch_command(&info.path, &args);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let started = Instant::now();
        let deadline = started + READY_TIMEOUT;
        *STARTING.lock() = Some((display, deadline));
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                finish_starting(display);
                return Err(e).with_context(|| format!("starting {}", info.path.display()));
            }
        };

        // Remembered on disk so a later Ruxt (after a crash or kill) can clean the server up.
        let _ = super::config::write_private_atomic(
            &pid_file(&self.config_dir),
            format!("{} {display}\n", child.id()).as_bytes(),
        );
        self.child = Some(child);
        self.managed_display = Some(display);
        self.auth_file = Some(auth_file.clone());
        super::x11::set_managed_auth(display, auth_file);
        self.last_refresh = None;
        self.last_error = None;
        super::audit::log(
            "xserver.start",
            serde_json::json!({"path": info.path.display().to_string(), "bundled": info.bundled, "display": display, "clipboard": self.clipboard}),
        );
        // Confirm the start-up off-thread so the UI never waits on it. This reads the server
        // log rather than connecting: an early connection would hang XWin (see `STARTING`).
        let probe_state = self.probe.clone();
        let override_path = self.override_path.clone();
        let spawned = std::thread::Builder::new()
            .name("xserver-start-probe".into())
            .spawn(move || {
                let up = wait_for_ready_marker(&log_file, deadline);
                finish_starting(display);
                super::audit::log(
                    "xserver.ready",
                    serde_json::json!({"display": display, "up": up, "ms": started.elapsed().as_millis() as u64}),
                );
                let r = probe(&override_path, None);
                let mut g = probe_state.lock();
                g.result = Some(r);
                g.in_flight = false;
            });
        if spawned.is_err() {
            finish_starting(display);
        }
        Ok(display)
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            // XWin forks helper copies of itself (Cygwin fork); killing only the parent leaves
            // them behind, so take the whole process tree down.
            kill_tree(c.id());
            let _ = c.kill();
            let _ = c.wait();
            super::audit::log("xserver.stop", serde_json::json!({"display": self.managed_display}));
        }
        let _ = std::fs::remove_file(pid_file(&self.config_dir));
        if let Some(f) = self.auth_file.take() {
            let _ = std::fs::remove_file(f);
        }
        if let Some(d) = self.managed_display.take() {
            finish_starting(d);
        }
        super::x11::clear_managed_auth();
        self.last_refresh = None;
    }

    /// Leave a server we started running after Ruxt exits (`x11_stop_on_exit = false`).
    pub fn detach(&mut self) {
        if self.child.take().is_some() {
            super::audit::log("xserver.detached", serde_json::json!({"display": self.managed_display}));
        }
        let _ = std::fs::remove_file(pid_file(&self.config_dir));
        self.auth_file = None;
        self.managed_display = None;
        super::x11::clear_managed_auth();
    }

    /// Make sure *some* X server is reachable, starting ours if possible.
    pub fn ensure_running(&mut self) -> Result<Option<u16>> {
        self.refresh(true);
        if let Some(d) = self.default_display() {
            return Ok(Some(d));
        }
        if self.installed.is_none() {
            bail!("no X server installed");
        }
        self.start().map(Some)
    }
}

impl Drop for XServerManager {
    fn drop(&mut self) {
        // Only ever kills a process we started ourselves (the app's exit hook already honoured
        // `x11_stop_on_exit`; this covers the unwinding path). The cookie file never outlives us.
        if let Some(mut c) = self.child.take() {
            kill_tree(c.id());
            let _ = c.kill();
        }
        let _ = std::fs::remove_file(pid_file(&self.config_dir));
        if let Some(f) = self.auth_file.take() {
            let _ = std::fs::remove_file(f);
        }
        super::x11::clear_managed_auth();
    }
}

fn pid_file(config_dir: &Path) -> PathBuf {
    config_dir.join("xserver.pid")
}

/// Quote one argument for `cmd.exe /C "..."`.
#[cfg(windows)]
fn cmd_quote(a: &str) -> String {
    if a.is_empty() || a.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", a.replace('"', "\"\""))
    } else {
        a.to_string()
    }
}

/// How XWin is started. `XWin.exe` is a GUI-subsystem Cygwin program: launched straight from
/// a GUI process it has no console, so the Cygwin runtime allocates one, and on Windows 11
/// the default terminal (Windows Terminal) turns that into a visible window for as long as
/// the server runs (the creation flags only apply to console-subsystem children). Starting
/// it through `cmd.exe` created with CREATE_NO_WINDOW gives it a parent console that exists
/// but has no window; Cygwin attaches to that one and nothing is shown. `cmd` waits for
/// XWin, so its exit status and process tree follow the server.
#[cfg(windows)]
fn launch_command(exe: &Path, args: &[String]) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut line = String::from("/C \"");
    line.push_str(&cmd_quote(&exe.display().to_string()));
    for a in args {
        line.push(' ');
        line.push_str(&cmd_quote(a));
    }
    line.push('"');
    let mut cmd = Command::new("cmd.exe");
    cmd.raw_arg(line);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
fn launch_command(exe: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(exe);
    cmd.args(args);
    cmd
}

/// Terminate `pid` and every descendant.
#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Running `XWin.exe` processes: (pid, parent alive, executable path).
#[cfg(windows)]
fn running_xwins() -> Vec<(u32, bool, PathBuf)> {
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // XWin itself, plus the hidden `cmd.exe` launchers (reported with the XWin path they run).
    let script = "Get-CimInstance Win32_Process -Filter \"Name='XWin.exe' OR (Name='cmd.exe' AND CommandLine LIKE '%XWin.exe%')\" | ForEach-Object { $a = Get-Process -Id $_.ParentProcessId -ErrorAction SilentlyContinue; $p = $_.ExecutablePath; if ($_.Name -eq 'cmd.exe') { if ($_.CommandLine -match '\"([^\"]*XWin\\.exe)\"') { $p = $Matches[1] } }; \"$($_.ProcessId)|$(if ($a) {1} else {0})|$p\" }";
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let Ok(out) = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.trim().splitn(3, '|');
            let pid = it.next()?.trim().parse().ok()?;
            let alive = it.next()?.trim() == "1";
            let path = PathBuf::from(it.next()?.trim());
            Some((pid, alive, path))
        })
        .collect()
}

#[cfg(not(windows))]
fn running_xwins() -> Vec<(u32, bool, PathBuf)> {
    Vec::new()
}

#[cfg(test)]
#[path = "xserver_tests.rs"]
mod tests;
