//! Session import / export.
//!
//! * OpenSSH client config (`~/.ssh/config`): `Host` blocks become profiles; `ProxyJump`
//!   becomes a jump-host link, `LocalForward` / `RemoteForward` / `DynamicForward` become
//!   tunnels. Wildcard hosts and `Match` blocks are skipped.
//! * PuTTY saved sessions: the registry on Windows (`reg query`, no extra dependency) or
//!   `~/.putty/sessions/*` elsewhere.
//! * Ruxt JSON: the session list without any secret (passwords and passphrases never leave
//!   the vault).

use crate::core::config::{new_id, AuthMethod, SessionProfile, TunnelKind, TunnelSpec};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// OpenSSH config
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct SshHost {
    alias: String,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    proxy_jump: Option<String>,
    forward_agent: bool,
    forward_x11: bool,
    compression: bool,
    keepalive: Option<u64>,
    tunnels: Vec<TunnelSpec>,
}

fn is_pattern(alias: &str) -> bool {
    alias.contains('*') || alias.contains('?') || alias.starts_with('!')
}

/// `[bind:]port host:hostport` (LocalForward / RemoteForward).
fn parse_forward(kind: TunnelKind, value: &str) -> Option<TunnelSpec> {
    let mut parts = value.split_whitespace();
    let listen = parts.next()?;
    let (bind_addr, bind_port) = split_bind(listen)?;
    let (target_host, target_port) = if kind == TunnelKind::Dynamic {
        (String::new(), 0)
    } else {
        let target = parts.next()?;
        let (h, p) = target.rsplit_once(':')?;
        (h.trim_matches(['[', ']']).to_string(), p.parse().ok()?)
    };
    Some(TunnelSpec {
        kind,
        bind_addr,
        bind_port,
        target_host,
        target_port,
        auto_start: true,
    })
}

fn split_bind(listen: &str) -> Option<(String, u16)> {
    match listen.rsplit_once(':') {
        Some((addr, port)) => {
            let addr = addr.trim_matches(['[', ']']);
            let addr = if addr.is_empty() || addr == "*" {
                "0.0.0.0".to_string()
            } else if addr == "localhost" {
                "127.0.0.1".to_string()
            } else {
                addr.to_string()
            };
            Some((addr, port.parse().ok()?))
        }
        None => Some(("127.0.0.1".to_string(), listen.parse().ok()?)),
    }
}

fn yes(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "yes" | "true" | "on" | "1")
}

/// `user@host:port` / `host:port` / `host` → (user, host, port).
pub fn parse_user_host_port(s: &str) -> (String, String, u16) {
    let s = s.trim();
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) => (u.to_string(), r),
        None => (String::new(), s),
    };
    if let Some(inner) = rest.strip_prefix('[') {
        // [v6]:port or [v6]
        if let Some((h, after)) = inner.split_once(']') {
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(22);
            return (user, h.to_string(), port);
        }
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() && !h.contains(':') => (h.to_string(), p.parse().unwrap_or(22)),
        _ => (rest.to_string(), 22),
    };
    (user, host, port)
}

/// Parse an OpenSSH client config into session profiles. `existing` lets `ProxyJump` refer to
/// a session the user already has (matched by name, then by user@host:port).
pub fn parse_openssh_config(text: &str, existing: &[SessionProfile]) -> Vec<SessionProfile> {
    let mut hosts: Vec<SshHost> = Vec::new();
    let mut cur: Option<SshHost> = None;
    let mut skipping = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().trim_matches('"').to_string()),
            None => (line.to_ascii_lowercase(), String::new()),
        };
        match key.as_str() {
            "host" => {
                if let Some(h) = cur.take() {
                    hosts.push(h);
                }
                // `Host a b` defines two aliases; keep the first concrete one.
                let alias = value.split_whitespace().find(|a| !is_pattern(a)).map(str::to_string);
                match alias {
                    Some(a) => {
                        skipping = false;
                        cur = Some(SshHost {
                            alias: a,
                            ..Default::default()
                        });
                    }
                    None => skipping = true,
                }
            }
            "match" => {
                if let Some(h) = cur.take() {
                    hosts.push(h);
                }
                skipping = true;
            }
            _ if skipping => {}
            _ => {
                let Some(h) = cur.as_mut() else { continue };
                match key.as_str() {
                    "hostname" => h.hostname = Some(value),
                    "user" => h.user = Some(value),
                    "port" => h.port = value.parse().ok(),
                    "identityfile" => {
                        if h.identity.is_none() {
                            h.identity = Some(value);
                        }
                    }
                    "proxyjump" => {
                        // Only the first hop is a session here; the rest would be its own jump.
                        if h.proxy_jump.is_none() && !value.eq_ignore_ascii_case("none") {
                            h.proxy_jump = value.split(',').next().map(|s| s.trim().to_string());
                        }
                    }
                    "forwardagent" => h.forward_agent = yes(&value),
                    "forwardx11" | "forwardx11trusted" => h.forward_x11 |= yes(&value),
                    "compression" => h.compression = yes(&value),
                    "serveraliveinterval" => h.keepalive = value.parse().ok(),
                    "localforward" => h.tunnels.extend(parse_forward(TunnelKind::Local, &value)),
                    "remoteforward" => h.tunnels.extend(parse_forward(TunnelKind::Remote, &value)),
                    "dynamicforward" => h.tunnels.extend(parse_forward(TunnelKind::Dynamic, &value)),
                    _ => {}
                }
            }
        }
    }
    if let Some(h) = cur.take() {
        hosts.push(h);
    }

    // First pass: profiles keyed by alias.
    let mut profiles: Vec<SessionProfile> = hosts
        .iter()
        .map(|h| {
            let mut p = SessionProfile {
                name: h.alias.clone(),
                group: "OpenSSH".into(),
                host: h.hostname.clone().unwrap_or_else(|| h.alias.clone()),
                port: h.port.unwrap_or(22),
                username: h.user.clone().unwrap_or_default(),
                agent_forwarding: h.forward_agent,
                x11_forwarding: h.forward_x11,
                compression: h.compression,
                tunnels: h.tunnels.clone(),
                ..Default::default()
            };
            if let Some(k) = h.keepalive {
                p.keepalive_secs = k;
            }
            if let Some(id) = &h.identity {
                p.auth = AuthMethod::PublicKey { path: id.clone() };
            }
            p
        })
        .collect();

    // Second pass: resolve ProxyJump to a profile id (imported alias, existing session, or a
    // new profile for an ad-hoc `user@host:port`).
    let mut extra: Vec<SessionProfile> = Vec::new();
    for (i, h) in hosts.iter().enumerate() {
        let Some(jump) = &h.proxy_jump else { continue };
        let id = if let Some(p) = profiles.iter().find(|p| p.name == *jump) {
            p.id.clone()
        } else if let Some(p) = existing.iter().find(|p| p.name == *jump) {
            p.id.clone()
        } else {
            let (user, host, port) = parse_user_host_port(jump);
            let found = profiles
                .iter()
                .chain(existing.iter())
                .chain(extra.iter())
                .find(|p| p.host == host && p.port == port && (user.is_empty() || p.username == user))
                .map(|p| p.id.clone());
            found.unwrap_or_else(|| {
                let p = SessionProfile {
                    name: jump.clone(),
                    group: "OpenSSH".into(),
                    host,
                    port,
                    username: user,
                    ..Default::default()
                };
                let id = p.id.clone();
                extra.push(p);
                id
            })
        };
        if profiles[i].id != id {
            profiles[i].jump = id;
        }
    }
    profiles.extend(extra);
    profiles
}

pub fn openssh_config_path() -> Option<PathBuf> {
    crate::core::hostkeys::home_dir().map(|h| h.join(".ssh").join("config"))
}

pub fn import_openssh(path: &std::path::Path, existing: &[SessionProfile]) -> Result<Vec<SessionProfile>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(parse_openssh_config(&text, existing))
}

// ---------------------------------------------------------------------------
// PuTTY
// ---------------------------------------------------------------------------

/// Decode PuTTY's session-name escaping (`%20` style).
pub fn putty_unescape(name: &str) -> String {
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 && i + 2 <= b.len() - 1 + 1 {
            if let (Some(h), Some(l)) = (hex_val(b.get(i + 1).copied()), hex_val(b.get(i + 2).copied())) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: Option<u8>) -> Option<u8> {
    match c? {
        c @ b'0'..=b'9' => Some(c - b'0'),
        c @ b'a'..=b'f' => Some(c - b'a' + 10),
        c @ b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Parse the output of `reg query HKCU\Software\SimonTatham\PuTTY\Sessions /s` into
/// (session name, values). Only `import_putty` (Windows) calls it; tests run everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_reg_query(text: &str) -> Vec<(String, HashMap<String, String>)> {
    let mut out: Vec<(String, HashMap<String, String>)> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if !raw.starts_with(' ') && !raw.starts_with('\t') {
            // A key path line.
            if let Some(name) = line.rsplit('\\').next() {
                if name.eq_ignore_ascii_case("Sessions") {
                    continue;
                }
                out.push((putty_unescape(name), HashMap::new()));
            }
            continue;
        }
        let Some((_, fields)) = out.last_mut() else { continue };
        // `    Name    REG_SZ    value with spaces`
        let t = line.trim();
        let Some((k, rest)) = t.split_once(char::is_whitespace) else {
            continue;
        };
        let rest = rest.trim_start();
        let (ty, v) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let v = v.trim().to_string();
        let v = if ty.eq_ignore_ascii_case("REG_DWORD") {
            // reg.exe prints DWORDs as hex (`0x16` = 22).
            let hex = v.trim_start_matches("0x").trim_start_matches("0X");
            u64::from_str_radix(hex, 16).map(|n| n.to_string()).unwrap_or(v)
        } else {
            v
        };
        fields.insert(k.to_string(), v);
    }
    out
}

/// PuTTY `PortForwardings`: `L8080=localhost:80,D1080,R9000=host:9000` (optional 4/6 prefix).
pub fn parse_putty_forwardings(s: &str) -> Vec<TunnelSpec> {
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let item = item.trim_start_matches(['4', '6']);
        let (kind, rest) = match item.chars().next() {
            Some('L') => (TunnelKind::Local, &item[1..]),
            Some('R') => (TunnelKind::Remote, &item[1..]),
            Some('D') => (TunnelKind::Dynamic, &item[1..]),
            _ => continue,
        };
        let (listen, target) = match rest.split_once('=') {
            Some((l, t)) => (l, Some(t)),
            None => (rest, None),
        };
        let Some((bind_addr, bind_port)) = split_bind(listen) else {
            continue;
        };
        let (target_host, target_port) = match (kind, target) {
            (TunnelKind::Dynamic, _) => (String::new(), 0),
            (_, Some(t)) => match t.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().unwrap_or(0)),
                None => continue,
            },
            (_, None) => continue,
        };
        out.push(TunnelSpec {
            kind,
            bind_addr,
            bind_port,
            target_host,
            target_port,
            auto_start: true,
        });
    }
    out
}

/// Build a profile from one PuTTY session's values (`None` for non-SSH sessions).
pub fn putty_profile(name: &str, f: &HashMap<String, String>) -> Option<SessionProfile> {
    let proto = f.get("Protocol").map(String::as_str).unwrap_or("ssh");
    if !proto.eq_ignore_ascii_case("ssh") {
        return None;
    }
    let host_raw = f.get("HostName").cloned().unwrap_or_default();
    if host_raw.trim().is_empty() || name == "Default Settings" {
        return None;
    }
    let (user_in_host, host, _) = parse_user_host_port(&host_raw);
    let username = f
        .get("UserName")
        .cloned()
        .filter(|u| !u.is_empty())
        .unwrap_or(user_in_host);
    let port = f.get("PortNumber").and_then(|p| p.parse().ok()).unwrap_or(22);
    let mut p = SessionProfile {
        name: name.to_string(),
        group: "PuTTY".into(),
        host,
        port,
        username,
        x11_forwarding: f.get("X11Forward").map(|v| v == "1").unwrap_or(false),
        agent_forwarding: f.get("AgentFwd").map(|v| v == "1").unwrap_or(false),
        compression: f.get("Compression").map(|v| v == "1").unwrap_or(false),
        tunnels: f
            .get("PortForwardings")
            .map(|s| parse_putty_forwardings(s))
            .unwrap_or_default(),
        ..Default::default()
    };
    if let Some(k) = f.get("PublicKeyFile").filter(|k| !k.trim().is_empty()) {
        p.auth = AuthMethod::PublicKey { path: k.clone() };
    }
    if let Some(t) = f.get("TermType").filter(|t| !t.is_empty()) {
        p.term_type = t.clone();
    }
    if let Some(k) = f.get("PingIntervalSecs").and_then(|v| v.parse::<u64>().ok()) {
        if k > 0 {
            p.keepalive_secs = k;
        }
    }
    Some(p)
}

/// Read PuTTY's saved sessions from this machine (the registry).
#[cfg(windows)]
pub fn import_putty() -> Result<Vec<SessionProfile>> {
    let out = std::process::Command::new("reg")
        .args(["query", r"HKCU\Software\SimonTatham\PuTTY\Sessions", "/s"])
        .output()
        .context("run reg query")?;
    if !out.status.success() {
        bail!("no PuTTY sessions found in the registry");
    }
    // reg.exe writes in the console code page; names are %-escaped ASCII anyway.
    let text = String::from_utf8_lossy(&out.stdout);
    let sessions = parse_reg_query(&text);
    Ok(sessions.iter().filter_map(|(name, f)| putty_profile(name, f)).collect())
}

/// Read PuTTY's saved sessions from this machine (`~/.putty/sessions`).
#[cfg(not(windows))]
pub fn import_putty() -> Result<Vec<SessionProfile>> {
    {
        let dir = crate::core::hostkeys::home_dir()
            .map(|h| h.join(".putty").join("sessions"))
            .context("no home directory")?;
        let rd = std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))?;
        let mut out = Vec::new();
        for e in rd.flatten() {
            let name = putty_unescape(&e.file_name().to_string_lossy());
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let mut f = HashMap::new();
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    f.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            if let Some(p) = putty_profile(&name, &f) {
                out.push(p);
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Ruxt JSON
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
struct ExportFile {
    ruxt_sessions: u32,
    exported_at: String,
    sessions: Vec<SessionProfile>,
}

const EXPORT_VERSION: u32 = 1;

/// Serialise the session list (no secrets are part of `SessionProfile`).
pub fn export_json(sessions: &[SessionProfile]) -> Result<Vec<u8>> {
    let f = ExportFile {
        ruxt_sessions: EXPORT_VERSION,
        exported_at: chrono::Local::now().to_rfc3339(),
        sessions: sessions.to_vec(),
    };
    Ok(serde_json::to_vec_pretty(&f)?)
}

pub fn import_json(bytes: &[u8]) -> Result<Vec<SessionProfile>> {
    let f: ExportFile = serde_json::from_slice(bytes).context("not a Ruxt session export")?;
    if f.ruxt_sessions > EXPORT_VERSION {
        bail!("export format version {} is newer than this Ruxt", f.ruxt_sessions);
    }
    Ok(f.sessions)
}

/// Prepare imported profiles for insertion: drop ones that duplicate an existing session
/// (same user@host:port and name), give colliding ids fresh ones, and keep jump links intact.
pub fn merge_new(existing: &[SessionProfile], imported: Vec<SessionProfile>) -> Vec<SessionProfile> {
    let mut remap: HashMap<String, String> = HashMap::new();
    let mut out: Vec<SessionProfile> = Vec::new();
    for mut p in imported {
        let dup = existing
            .iter()
            .find(|e| e.host == p.host && e.port == p.port && e.username == p.username && e.name == p.name);
        if let Some(d) = dup {
            remap.insert(p.id.clone(), d.id.clone());
            continue;
        }
        if existing.iter().any(|e| e.id == p.id) || out.iter().any(|o| o.id == p.id) {
            let fresh = new_id();
            remap.insert(p.id.clone(), fresh.clone());
            p.id = fresh;
        }
        p.scrollback = p.scrollback.clamp(100, crate::core::config::MAX_SCROLLBACK);
        out.push(p);
    }
    let ids: Vec<String> = out.iter().map(|o| o.id.clone()).collect();
    for p in &mut out {
        if let Some(n) = remap.get(&p.jump) {
            p.jump = n.clone();
        }
        let known = ids.contains(&p.jump) || existing.iter().any(|e| e.id == p.jump);
        if !p.jump.is_empty() && !known {
            p.jump.clear();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openssh_config_hosts_forwards_and_jump() {
        let cfg = r#"
# comment
Host *
    ServerAliveInterval 60

Host bastion
    HostName bastion.example.com
    User ops
    Port 2222

Host web
    HostName 10.0.0.5
    User deploy
    IdentityFile ~/.ssh/id_web
    ProxyJump bastion
    LocalForward 8080 localhost:80
    DynamicForward 1080
    RemoteForward 0.0.0.0:9000 127.0.0.1:9000
    ForwardAgent yes
    Compression yes

Host adhoc
    ProxyJump root@jump.example.org:2200

Match host web
    User other
"#;
        let p = parse_openssh_config(cfg, &[]);
        let bastion = p.iter().find(|p| p.name == "bastion").unwrap();
        assert_eq!(bastion.host, "bastion.example.com");
        assert_eq!(bastion.port, 2222);
        assert_eq!(bastion.username, "ops");
        let web = p.iter().find(|p| p.name == "web").unwrap();
        assert_eq!(web.jump, bastion.id);
        assert_eq!(
            web.auth,
            AuthMethod::PublicKey {
                path: "~/.ssh/id_web".into()
            }
        );
        assert_eq!(web.tunnels.len(), 3);
        assert_eq!(web.tunnels[0].kind, TunnelKind::Local);
        assert_eq!(web.tunnels[0].bind_port, 8080);
        assert_eq!(web.tunnels[0].target_host, "localhost");
        assert_eq!(web.tunnels[1].kind, TunnelKind::Dynamic);
        assert_eq!(web.tunnels[2].kind, TunnelKind::Remote);
        assert_eq!(web.tunnels[2].bind_addr, "0.0.0.0");
        assert!(web.agent_forwarding && web.compression);
        // `Match` is skipped; `User other` must not leak into anything.
        assert!(p.iter().all(|p| p.username != "other"));
        // ad-hoc jump target became its own profile
        let adhoc = p.iter().find(|p| p.name == "adhoc").unwrap();
        let jump = p.iter().find(|p| p.id == adhoc.jump).unwrap();
        assert_eq!(
            (jump.username.as_str(), jump.host.as_str(), jump.port),
            ("root", "jump.example.org", 2200)
        );
        assert!(!p.iter().any(|p| p.name == "*"));
    }

    #[test]
    fn putty_reg_query_parsing() {
        let text = "\r\nHKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\r\n\r\nHKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\my%20server\r\n    HostName    REG_SZ    admin@10.1.2.3\r\n    PortNumber    REG_DWORD    0x16\r\n    Protocol    REG_SZ    ssh\r\n    PublicKeyFile    REG_SZ    C:\\keys\\a.ppk\r\n    PortForwardings    REG_SZ    L8080=localhost:80,D1080,4R9000=host:9000\r\n    X11Forward    REG_DWORD    0x1\r\n\r\nHKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\serial%20thing\r\n    HostName    REG_SZ    COM3\r\n    Protocol    REG_SZ    serial\r\n";
        let sessions = parse_reg_query(text);
        assert_eq!(sessions.len(), 2);
        let (name, f) = &sessions[0];
        assert_eq!(name, "my server");
        let p = putty_profile(name, f).unwrap();
        assert_eq!(p.host, "10.1.2.3");
        assert_eq!(p.username, "admin");
        assert_eq!(p.port, 22);
        assert!(p.x11_forwarding);
        assert_eq!(
            p.auth,
            AuthMethod::PublicKey {
                path: "C:\\keys\\a.ppk".into()
            }
        );
        assert_eq!(p.tunnels.len(), 3);
        assert_eq!(p.tunnels[2].kind, TunnelKind::Remote);
        assert_eq!(p.tunnels[2].target_port, 9000);
        assert!(putty_profile(&sessions[1].0, &sessions[1].1).is_none());
    }

    #[test]
    fn json_roundtrip_and_merge() {
        let a = SessionProfile {
            name: "a".into(),
            host: "a.example".into(),
            ..Default::default()
        };
        let b = SessionProfile {
            name: "b".into(),
            host: "b.example".into(),
            jump: a.id.clone(),
            ..Default::default()
        };
        let bytes = export_json(&[a.clone(), b.clone()]).unwrap();
        let back = import_json(&bytes).unwrap();
        assert_eq!(back, vec![a.clone(), b.clone()]);
        // `a` already exists under the same id: skipped, and b's jump still resolves to it.
        let merged = merge_new(std::slice::from_ref(&a), back);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "b");
        assert_eq!(merged[0].jump, a.id);
        // id collision with a *different* session gets a fresh id
        let c = SessionProfile {
            id: a.id.clone(),
            name: "c".into(),
            host: "c.example".into(),
            ..Default::default()
        };
        let merged = merge_new(std::slice::from_ref(&a), vec![c]);
        assert_eq!(merged.len(), 1);
        assert_ne!(merged[0].id, a.id);
        assert!(import_json(b"{\"nope\":1}").is_err());
    }

    #[test]
    fn user_host_port_forms() {
        assert_eq!(parse_user_host_port("u@h:2200"), ("u".into(), "h".into(), 2200));
        assert_eq!(parse_user_host_port("h"), ("".into(), "h".into(), 22));
        assert_eq!(parse_user_host_port("[::1]:22"), ("".into(), "::1".into(), 22));
        assert_eq!(putty_unescape("a%20b%2Fc"), "a b/c");
    }
}
