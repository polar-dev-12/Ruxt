//! Ed25519 key-pair generation (OpenSSH format, passphrase-protected with bcrypt-pbkdf/AES-256-CTR).

use anyhow::{bail, Context, Result};
use russh::keys::ssh_key::{Algorithm, LineEnding, PrivateKey};
use secrecy::{ExposeSecret, SecretString};
use std::path::Path;

pub fn generate_ed25519(path: &Path, comment: &str, passphrase: Option<&SecretString>) -> Result<String> {
    if path.exists() {
        bail!("{} already exists; refusing to overwrite", path.display());
    }
    let mut rng = rand::rng();
    let mut key = PrivateKey::random(&mut rng, Algorithm::Ed25519).context("keygen")?;
    key.set_comment(comment);
    let key = match passphrase {
        Some(pp) if !pp.expose_secret().is_empty() => key
            .encrypt(&mut rng, pp.expose_secret().as_bytes())
            .context("encrypting key")?,
        _ => key,
    };
    let pem = key.to_openssh(LineEnding::LF).context("encode")?;
    // Owner-only from the first byte (matters most for an unencrypted key).
    super::config::write_private_atomic(path, pem.as_bytes())?;
    let pubkey = key.public_key().to_openssh().context("encode public key")?;
    let pub_path = path.with_extension("pub");
    std::fs::write(&pub_path, format!("{pubkey}\n"))?;
    Ok(pubkey)
}
