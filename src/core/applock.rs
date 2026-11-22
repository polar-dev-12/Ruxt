//! Application lock: a password that gates the whole UI after a period of inactivity.
//!
//! This is a *screen lock*, not encryption: sessions keep running behind the lock screen and
//! nothing on disk is protected by it (the credential vault has its own master password, see
//! `vault.rs`). What it does guard against is someone using an unattended Ruxt window — its
//! open shells, SFTP panes and saved sessions.
//!
//! The password is never stored. `applock.bin` holds a random salt and an Argon2id verifier
//! (same parameters as the vault), so an attacker with the file still has to brute-force the
//! password; the verifier is compared in constant time.
//!
//! File layout: magic "RUXTLOCK" | version u8 | m_cost u32 | t_cost u32 | p_cost u32 | salt[32] | verifier[32]

use anyhow::{anyhow, bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use secrecy::{ExposeSecret, SecretString};
use std::path::Path;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"RUXTLOCK";
const VERSION: u8 = 1;
const M_COST_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;
const LEN: usize = 8 + 1 + 4 + 4 + 4 + 32 + 32;

/// Shortest password accepted when setting the lock.
pub const MIN_LEN: usize = 6;
/// Idle timeout granularity and ceiling (minutes). 0 disables the lock.
pub const STEP_MINUTES: u64 = 10;
pub const MAX_MINUTES: u64 = 240;

/// Snap a configured timeout to the allowed grid: 0 stays 0, anything else becomes a
/// multiple of `STEP_MINUTES` between `STEP_MINUTES` and `MAX_MINUTES`.
pub fn normalize_minutes(m: u64) -> u64 {
    if m == 0 {
        0
    } else {
        ((m + STEP_MINUTES / 2) / STEP_MINUTES * STEP_MINUTES).clamp(STEP_MINUTES, MAX_MINUTES)
    }
}

pub fn exists(path: &Path) -> bool {
    path.is_file()
}

/// Set (or replace) the lock password.
pub fn set_password(path: &Path, password: &SecretString) -> Result<()> {
    if password.expose_secret().chars().count() < MIN_LEN {
        bail!("password too short");
    }
    let mut salt = [0u8; 32];
    rand::fill(&mut salt);
    let verifier = derive(password, &salt, M_COST_KIB, T_COST, P_COST)?;
    let mut out = Vec::with_capacity(LEN);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&M_COST_KIB.to_be_bytes());
    out.extend_from_slice(&T_COST.to_be_bytes());
    out.extend_from_slice(&P_COST.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&*verifier);
    super::config::write_private_atomic(path, &out).context("writing app lock file")
}

/// Check `password` against the stored verifier. Slow by design (Argon2id); call it off the
/// UI thread. Returns `Ok(false)` for a wrong password and `Err` for a missing or corrupt file.
pub fn verify(path: &Path, password: &SecretString) -> Result<bool> {
    let bytes = std::fs::read(path).context("reading app lock file")?;
    if bytes.len() != LEN || &bytes[..8] != MAGIC {
        bail!("app lock file is corrupt");
    }
    if bytes[8] != VERSION {
        bail!("unsupported app lock version {}", bytes[8]);
    }
    let m = u32::from_be_bytes(bytes[9..13].try_into().unwrap());
    let t = u32::from_be_bytes(bytes[13..17].try_into().unwrap());
    let p = u32::from_be_bytes(bytes[17..21].try_into().unwrap());
    // Same sanity bounds as the vault: a planted file must not turn unlocking into a DoS.
    if m > 256 * 1024 || t > 10 || p > 8 || m < 8 * 1024 || t == 0 || p == 0 {
        bail!("app lock KDF parameters out of range");
    }
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&bytes[21..53]);
    let stored = &bytes[53..85];
    let computed = derive(password, &salt, m, t, p)?;
    Ok(ct_eq(&*computed, stored))
}

/// Remove the lock (the caller has already verified the current password).
pub fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("removing app lock file"),
    }
}

fn derive(pw: &SecretString, salt: &[u8; 32], m: u32, t: u32, p: u32) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(m, t, p, Some(32)).map_err(|e| anyhow!("argon2 params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(pw.expose_secret().as_bytes(), salt, &mut *out)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(out)
}

/// Constant-time equality for equal-length byte strings.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ruxt-applock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn set_verify_remove_roundtrip() {
        let p = tmp("lock-a");
        let _ = std::fs::remove_file(&p);
        assert!(!exists(&p));
        set_password(&p, &SecretString::from("correct horse")).unwrap();
        assert!(exists(&p));
        assert!(verify(&p, &SecretString::from("correct horse")).unwrap());
        assert!(!verify(&p, &SecretString::from("correct horsf")).unwrap());
        assert!(!verify(&p, &SecretString::from("")).unwrap());
        remove(&p).unwrap();
        assert!(!exists(&p));
        remove(&p).unwrap(); // idempotent
    }

    #[test]
    fn short_password_is_refused() {
        let p = tmp("lock-b");
        assert!(set_password(&p, &SecretString::from("abc")).is_err());
        assert!(!exists(&p));
    }

    #[test]
    fn tampered_file_is_rejected_not_accepted() {
        let p = tmp("lock-c");
        set_password(&p, &SecretString::from("secret-pw")).unwrap();
        let mut bytes = std::fs::read(&p).unwrap();
        // flip a verifier byte: must be a clean "wrong password", never a panic or a pass
        bytes[LEN - 1] ^= 0xff;
        std::fs::write(&p, &bytes).unwrap();
        assert!(!verify(&p, &SecretString::from("secret-pw")).unwrap());
        // truncated / foreign file: error
        std::fs::write(&p, b"RUXTLOCK junk").unwrap();
        assert!(verify(&p, &SecretString::from("secret-pw")).is_err());
        // absurd KDF cost in the header: refused before any hashing
        let mut bytes = bytes.clone();
        bytes[9..13].copy_from_slice(&(4 * 1024 * 1024u32).to_be_bytes());
        std::fs::write(&p, &bytes).unwrap();
        assert!(verify(&p, &SecretString::from("secret-pw")).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn minutes_snap_to_ten_minute_grid() {
        assert_eq!(normalize_minutes(0), 0);
        assert_eq!(normalize_minutes(1), 10);
        assert_eq!(normalize_minutes(14), 10);
        assert_eq!(normalize_minutes(15), 20);
        assert_eq!(normalize_minutes(25), 30);
        assert_eq!(normalize_minutes(10_000), MAX_MINUTES);
    }
}
