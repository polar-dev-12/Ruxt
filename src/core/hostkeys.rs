//! Host key trust store (Trust-On-First-Use with hard failure on change).
//!
//! Two sources are consulted:
//! 1. Ruxt's own `known_hosts` (writable, OpenSSH format).
//! 2. The user's `~/.ssh/known_hosts` (read-only, so existing OpenSSH trust carries over).
//!
//! A key that differs from a recorded one is reported as `Changed` and the connection is
//! refused unless the user explicitly removes the stale entry.

use anyhow::Result;
use russh::keys::known_hosts;
use russh::keys::ssh_key::{HashAlg, PublicKey};
use std::path::{Path, PathBuf};

/// Serialises every write to the app's known_hosts file: two tabs trusting the same new host
/// at once would otherwise append duplicate lines, and a `forget` could race a `learn`.
static WRITE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[derive(Debug, Clone)]
pub enum HostKeyStatus {
    Known,
    Unknown,
    Changed { line: usize, file: PathBuf },
}

#[derive(Debug, Clone)]
pub struct HostKeyStore {
    app_file: PathBuf,
    user_file: Option<PathBuf>,
}

impl HostKeyStore {
    pub fn new(app_file: PathBuf) -> Self {
        let user_file = home_dir().map(|h| h.join(".ssh").join("known_hosts"));
        Self { app_file, user_file }
    }

    pub fn app_file(&self) -> &Path {
        &self.app_file
    }

    pub fn check(&self, host: &str, port: u16, key: &PublicKey) -> HostKeyStatus {
        let mut files = vec![self.app_file.clone()];
        if let Some(u) = &self.user_file {
            files.push(u.clone());
        }
        for f in files {
            if !f.exists() {
                continue;
            }
            match known_hosts::check_known_hosts_path(host, port, key, &f) {
                Ok(true) => return HostKeyStatus::Known,
                Ok(false) => {}
                Err(russh::keys::Error::KeyChanged { line }) => {
                    return HostKeyStatus::Changed { line, file: f };
                }
                Err(e) => {
                    tracing::warn!("known_hosts {}: {e}", f.display());
                }
            }
        }
        HostKeyStatus::Unknown
    }

    /// Pin `key` for `host:port` in the app's own file (no-op if it is already recorded there).
    pub fn learn(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        let _g = WRITE_LOCK.lock();
        if let Some(parent) = self.app_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Create the file owner-only *before* russh appends to it with default permissions.
        super::config::ensure_private_file(&self.app_file)?;
        if matches!(
            known_hosts::check_known_hosts_path(host, port, key, &self.app_file),
            Ok(true)
        ) {
            return Ok(());
        }
        known_hosts::learn_known_hosts_path(host, port, key, &self.app_file)?;
        Ok(())
    }

    /// Remove every line that refers to `host:port` from the given file (used to "forget" a changed key).
    pub fn forget(&self, file: &Path, host: &str, port: u16) -> Result<usize> {
        let _g = WRITE_LOCK.lock();
        let content = std::fs::read_to_string(file)?;
        let needle_a = host_pattern(host, port);
        let needle_b = if port == 22 { host.to_string() } else { String::new() };
        let mut removed = 0;
        let kept: Vec<&str> = content
            .lines()
            .filter(|line| {
                let first = line.split_whitespace().next().unwrap_or("");
                let hit = first
                    .split(',')
                    .any(|h| h == needle_a || (!needle_b.is_empty() && h == needle_b));
                if hit {
                    removed += 1;
                }
                !hit
            })
            .collect();
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        super::config::write_private_atomic(file, out.as_bytes())?;
        Ok(removed)
    }
}

fn host_pattern(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

pub fn fingerprint_sha256(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

pub fn randomart(key: &PublicKey) -> String {
    let fp = key.fingerprint(HashAlg::Sha256);
    let header = format!("{} 256", key.algorithm().as_str().to_uppercase());
    fp.to_randomart(&header)
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::{Algorithm, PrivateKey};

    fn store() -> (HostKeyStore, PathBuf) {
        let mut r = [0u8; 4];
        rand::fill(&mut r);
        let dir = std::env::temp_dir().join(format!("ruxt-hk-{}-{}", std::process::id(), hex::encode(r)));
        std::fs::create_dir_all(&dir).unwrap();
        (
            HostKeyStore {
                app_file: dir.join("known_hosts"),
                user_file: None,
            },
            dir,
        )
    }

    #[test]
    fn learn_is_idempotent_and_forget_replaces() {
        let (s, dir) = store();
        let k1 = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let k2 = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        assert!(matches!(s.check("h", 2222, k1.public_key()), HostKeyStatus::Unknown));
        s.learn("h", 2222, k1.public_key()).unwrap();
        s.learn("h", 2222, k1.public_key()).unwrap();
        let lines = std::fs::read_to_string(&s.app_file)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert_eq!(lines, 1, "a second learn must not duplicate the entry");
        assert!(matches!(s.check("h", 2222, k1.public_key()), HostKeyStatus::Known));
        assert!(matches!(
            s.check("h", 2222, k2.public_key()),
            HostKeyStatus::Changed { .. }
        ));
        let removed = s.forget(&s.app_file.clone(), "h", 2222).unwrap();
        assert_eq!(removed, 1);
        s.learn("h", 2222, k2.public_key()).unwrap();
        assert!(matches!(s.check("h", 2222, k2.public_key()), HostKeyStatus::Known));
        assert!(matches!(
            s.check("h", 2222, k1.public_key()),
            HostKeyStatus::Changed { .. }
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
