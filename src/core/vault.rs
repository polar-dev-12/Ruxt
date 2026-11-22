//! Encrypted store for saved passwords and key passphrases.
//!
//! There is no separate master password: the store is encrypted with the screen-lock
//! password (`applock.rs`), opened when Ruxt is unlocked and dropped (wiping the key) when it
//! locks. Without a screen-lock password nothing is saved. A file written by older versions
//! under a separate master password is detected by `open_with` and moved over once (see
//! `apply_new_master`).
//!
//! Format (all big-endian):
//! ```text
//! magic "RUXTVAUL" (or legacy "RUSSHVLT") | version u8 | m_cost u32 | t_cost u32 | p_cost u32 | salt[32] | nonce[24] | ciphertext
//! ```
//! * KDF: Argon2id (default 64 MiB, 3 passes) → 256-bit key.
//! * AEAD: XChaCha20-Poly1305; the header is bound as associated data, so any tampering
//!   with KDF parameters or the salt is detected.
//! * Plaintext is JSON; every secret lives in a zeroize-on-drop container.

use anyhow::{anyhow, bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const MAGIC: &[u8; 8] = b"RUXTVAUL";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 8 + 1 + 4 + 4 + 4 + 32 + 24;

// Argon2id parameters (OWASP 2023+ recommendation tier, erring on the strong side).
const M_COST_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;

#[derive(Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop, Clone)]
pub struct SecretEntry {
    pub password: Option<String>,
    pub passphrase: Option<String>,
}

impl std::fmt::Debug for SecretEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretEntry")
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("passphrase", &self.passphrase.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct VaultData {
    /// keyed by session profile id
    entries: BTreeMap<String, SecretEntry>,
    /// Key of the encrypted command history (hex), created on first use.
    #[serde(default)]
    history_key: Option<String>,
}

impl Drop for VaultData {
    fn drop(&mut self) {
        // SecretEntry zeroizes itself; clear the map eagerly so nothing lingers.
        self.entries.clear();
        if let Some(k) = self.history_key.as_mut() {
            k.zeroize();
        }
    }
}

/// An unlocked vault held in memory. Dropping it (or calling `lock`) wipes the key material.
pub struct Vault {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    salt: [u8; 32],
    data: VaultData,
}

impl Vault {
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    /// Create a brand-new vault protected by `master`.
    pub fn create(path: &Path, master: &SecretString) -> Result<Self> {
        if path.exists() {
            bail!("vault already exists");
        }
        let mut salt = [0u8; 32];
        rand::fill(&mut salt);
        let key = derive_key(master, &salt, M_COST_KIB, T_COST, P_COST)?;
        let v = Self {
            path: path.to_path_buf(),
            key,
            salt,
            data: VaultData::default(),
        };
        v.save()?;
        Ok(v)
    }

    /// Open and decrypt an existing vault.
    pub fn unlock(path: &Path, master: &SecretString) -> Result<Self> {
        Self::try_unlock(path, master)?
            .ok_or_else(|| anyhow!("wrong password (or the saved-password file was tampered with)"))
    }

    /// Like `unlock`, but a password that does not decrypt the file is `Ok(None)` rather than
    /// an error, so callers can tell it apart from a missing or corrupt file.
    pub fn try_unlock(path: &Path, master: &SecretString) -> Result<Option<Self>> {
        let bytes = std::fs::read(path).context("reading vault file")?;
        if bytes.len() < HEADER_LEN + 16 || (&bytes[..8] != MAGIC && &bytes[..8] != b"RUSSHVLT") {
            bail!("vault file is corrupt or not a Ruxt vault");
        }
        let (header, ciphertext) = bytes.split_at(HEADER_LEN);
        let version = header[8];
        if version != VERSION {
            bail!("unsupported vault version {version}");
        }
        let m = u32::from_be_bytes(header[9..13].try_into().unwrap());
        let t = u32::from_be_bytes(header[13..17].try_into().unwrap());
        let p = u32::from_be_bytes(header[17..21].try_into().unwrap());
        // Reject absurd parameters (DoS via a planted file): at most 256 MiB / 10 passes.
        if m > 256 * 1024 || t > 10 || p > 8 || m < 8 * 1024 || t == 0 || p == 0 {
            bail!("vault KDF parameters out of range");
        }
        let mut salt = [0u8; 32];
        salt.copy_from_slice(&header[21..53]);
        let nonce = XNonce::try_from(&header[53..77]).map_err(|_| anyhow!("bad nonce"))?;

        let key = derive_key(master, &salt, m, t, p)?;
        let cipher = XChaCha20Poly1305::new((&*key).into());
        let Ok(plain) = cipher.decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: header,
            },
        ) else {
            return Ok(None);
        };
        let plain = Zeroizing::new(plain);
        let data: VaultData = serde_json::from_slice(&plain).context("vault payload")?;
        Ok(Some(Self {
            path: path.to_path_buf(),
            key,
            salt,
            data,
        }))
    }

    /// Open the store with the screen-lock password, creating an empty one if there is none.
    /// A file that this password does not decrypt was written under an older, separate master
    /// password: the result then carries a key derived from `password` for moving it over.
    /// Slow (Argon2id); call it off the UI thread.
    pub fn open_with(path: &Path, password: &SecretString) -> Result<Opened> {
        if !path.exists() {
            return Ok(Opened::Open(Self::create(path, password)?));
        }
        match Self::try_unlock(path, password)? {
            Some(v) => Ok(Opened::Open(v)),
            None => {
                let (key, salt) = Self::derive_new_master(password)?;
                Ok(Opened::Foreign { key, salt })
            }
        }
    }

    /// Replace whatever is at `path` with an empty store under an already derived key.
    pub fn create_with_key(path: &Path, key: Zeroizing<[u8; 32]>, salt: [u8; 32]) -> Result<Self> {
        let v = Self {
            path: path.to_path_buf(),
            key,
            salt,
            data: VaultData::default(),
        };
        v.save()?;
        Ok(v)
    }

    /// Delete the store file (screen lock removed: nothing may stay saved without it).
    pub fn delete(path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("removing saved-password file"),
        }
    }

    /// Encrypt and write atomically. A fresh random nonce is used for every write.
    pub fn save(&self) -> Result<()> {
        let plain = Zeroizing::new(serde_json::to_vec(&self.data)?);
        let mut nonce_bytes = [0u8; 24];
        rand::fill(&mut nonce_bytes);
        let nonce = XNonce::from(nonce_bytes);

        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(MAGIC);
        header.push(VERSION);
        header.extend_from_slice(&M_COST_KIB.to_be_bytes());
        header.extend_from_slice(&T_COST.to_be_bytes());
        header.extend_from_slice(&P_COST.to_be_bytes());
        header.extend_from_slice(&self.salt);
        header.extend_from_slice(&nonce_bytes);
        debug_assert_eq!(header.len(), HEADER_LEN);

        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        let ct = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &plain,
                    aad: &header,
                },
            )
            .map_err(|_| anyhow!("encryption failure"))?;
        let mut out = header;
        out.extend_from_slice(&ct);
        super::config::write_private_atomic(&self.path, &out)
    }

    /// Derive a new master key and salt (can be run off the UI thread via `spawn_blocking`).
    pub fn derive_new_master(new_master: &SecretString) -> Result<(Zeroizing<[u8; 32]>, [u8; 32])> {
        let mut salt = [0u8; 32];
        rand::fill(&mut salt);
        let key = derive_key(new_master, &salt, M_COST_KIB, T_COST, P_COST)?;
        Ok((key, salt))
    }

    /// Apply a pre-derived master key and salt, re-encrypting the vault on disk.
    pub fn apply_new_master(&mut self, key: Zeroizing<[u8; 32]>, salt: [u8; 32]) -> Result<()> {
        self.key = key;
        self.salt = salt;
        self.save()
    }

    /// The key that encrypts the command history (`history.rs`), created and saved on first
    /// use. It lives here so the history opens with the screen-lock password and survives a
    /// password change without re-encryption.
    pub fn history_key(&mut self) -> Result<super::history::HistoryKey> {
        if self.data.history_key.is_none() {
            let k = super::history::new_key();
            self.data.history_key = Some(hex::encode(k.as_slice()));
            self.save()?;
        }
        let mut k = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(self.data.history_key.as_deref().unwrap_or(""), &mut *k)
            .map_err(|_| anyhow!("command history key is corrupt"))?;
        Ok(k)
    }

    pub fn get(&self, session_id: &str) -> Option<&SecretEntry> {
        self.data.entries.get(session_id)
    }

    pub fn set_password(&mut self, session_id: &str, pw: Option<SecretString>) -> Result<()> {
        let e = self.data.entries.entry(session_id.to_string()).or_default();
        // Wipe the previous value before it is dropped; a plain assignment would leave it in
        // freed memory.
        if let Some(old) = e.password.as_mut() {
            old.zeroize();
        }
        e.password = pw.map(|s| s.expose_secret().to_string());
        self.prune(session_id);
        self.save()
    }

    pub fn set_passphrase(&mut self, session_id: &str, pp: Option<SecretString>) -> Result<()> {
        let e = self.data.entries.entry(session_id.to_string()).or_default();
        if let Some(old) = e.passphrase.as_mut() {
            old.zeroize();
        }
        e.passphrase = pp.map(|s| s.expose_secret().to_string());
        self.prune(session_id);
        self.save()
    }

    pub fn remove(&mut self, session_id: &str) -> Result<()> {
        self.data.entries.remove(session_id);
        self.save()
    }

    fn prune(&mut self, id: &str) {
        if let Some(e) = self.data.entries.get(id) {
            if e.password.is_none() && e.passphrase.is_none() {
                self.data.entries.remove(id);
            }
        }
    }
}

fn derive_key(master: &SecretString, salt: &[u8; 32], m: u32, t: u32, p: u32) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(m, t, p, Some(32)).map_err(|e| anyhow!("argon2 params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(master.expose_secret().as_bytes(), salt, &mut *key)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(key)
}

/// Result of `Vault::open_with`.
pub enum Opened {
    Open(Vault),
    /// The file is encrypted under another password (an old separate master password). The
    /// key and salt are derived from the screen-lock password, ready for `apply_new_master`
    /// once the old password unlocks the file, or for `create_with_key` to start over.
    Foreign {
        key: Zeroizing<[u8; 32]>,
        salt: [u8; 32],
    },
}

/// Compare two secrets without stopping at the first different byte.
pub fn secrets_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

/// Estimate password strength in bits of entropy (rough, for UI hints only).
pub fn password_entropy_bits(pw: &str) -> f64 {
    let mut pool = 0u32;
    if pw.chars().any(|c| c.is_ascii_lowercase()) {
        pool += 26;
    }
    if pw.chars().any(|c| c.is_ascii_uppercase()) {
        pool += 26;
    }
    if pw.chars().any(|c| c.is_ascii_digit()) {
        pool += 10;
    }
    if pw.chars().any(|c| !c.is_ascii_alphanumeric()) {
        pool += 33;
    }
    if pool == 0 {
        return 0.0;
    }
    (pw.chars().count() as f64) * (pool as f64).log2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_match_only_when_equal() {
        assert!(secrets_eq("correct horse", "correct horse"));
        assert!(!secrets_eq("correct horse", "correct horsE"));
        assert!(!secrets_eq("short", "longer-secret"));
        assert!(!secrets_eq("", "x"));
        assert!(secrets_eq("", ""));
    }

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("ruxt-vault-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("v.bin");
        let _ = std::fs::remove_file(&p);
        let master = SecretString::from("correct horse battery staple".to_string());
        {
            let mut v = Vault::create(&p, &master).unwrap();
            v.set_password("s1", Some(SecretString::from("pw".to_string())))
                .unwrap();
        }
        let v = Vault::unlock(&p, &master).unwrap();
        assert_eq!(v.get("s1").unwrap().password.as_deref(), Some("pw"));
        assert!(Vault::unlock(&p, &SecretString::from("wrong".to_string())).is_err());
        // tamper with the ciphertext
        let original = std::fs::read(&p).unwrap();
        let mut bytes = original.clone();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&p, &bytes).unwrap();
        assert!(Vault::unlock(&p, &master).is_err());
        // tamper with the header (salt byte): must be caught through the AEAD associated data
        let mut bytes = original.clone();
        bytes[30] ^= 1;
        std::fs::write(&p, &bytes).unwrap();
        assert!(Vault::unlock(&p, &master).is_err());
        // KDF parameters pushed out of range: rejected before any key derivation happens
        let mut bytes = original.clone();
        bytes[9..13].copy_from_slice(&(1024u32 * 1024).to_be_bytes());
        std::fs::write(&p, &bytes).unwrap();
        let err = match Vault::unlock(&p, &master) {
            Ok(_) => panic!("absurd KDF parameters must be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("out of range"), "{err}");
        // untouched file still opens
        std::fs::write(&p, &original).unwrap();
        assert!(Vault::unlock(&p, &master).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_with_creates_opens_and_detects_a_foreign_file() {
        let dir = std::env::temp_dir().join(format!("ruxt-vault3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("v.bin");
        let _ = std::fs::remove_file(&p);
        let lock = SecretString::from("screen lock pw".to_string());
        let old = SecretString::from("old master pw".to_string());
        // absent: created under the lock password
        match Vault::open_with(&p, &lock).unwrap() {
            Opened::Open(mut v) => v.set_password("s1", Some(SecretString::from("x".to_string()))).unwrap(),
            Opened::Foreign { .. } => panic!("a missing file is created, not foreign"),
        }
        assert!(matches!(Vault::open_with(&p, &lock).unwrap(), Opened::Open(_)));
        // a file under another password: foreign, and moving it keeps the secrets
        std::fs::remove_file(&p).unwrap();
        {
            let mut v = Vault::create(&p, &old).unwrap();
            v.set_password("s1", Some(SecretString::from("kept".to_string())))
                .unwrap();
        }
        let Opened::Foreign { key, salt } = Vault::open_with(&p, &lock).unwrap() else {
            panic!("file under another password must be foreign");
        };
        let mut v = Vault::unlock(&p, &old).unwrap();
        v.apply_new_master(key, salt).unwrap();
        drop(v);
        let Opened::Open(v) = Vault::open_with(&p, &lock).unwrap() else {
            panic!("moved file opens with the lock password");
        };
        assert_eq!(v.get("s1").unwrap().password.as_deref(), Some("kept"));
        Vault::delete(&p).unwrap();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_a_secret_keeps_only_the_new_value() {
        let dir = std::env::temp_dir().join(format!("ruxt-vault2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("v.bin");
        let _ = std::fs::remove_file(&p);
        let master = SecretString::from("another master password".to_string());
        let mut v = Vault::create(&p, &master).unwrap();
        v.set_password("s1", Some(SecretString::from("first".to_string())))
            .unwrap();
        v.set_password("s1", Some(SecretString::from("second".to_string())))
            .unwrap();
        assert_eq!(v.get("s1").unwrap().password.as_deref(), Some("second"));
        v.set_password("s1", None).unwrap();
        assert!(v.get("s1").is_none(), "entry without secrets is pruned");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
