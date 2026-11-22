//! The Cygwin/X tree embedded in the executable (Windows builds made with the tree present, see
//! `build.rs`).
//!
//! XWin.exe cannot run from memory: it needs `cygwin1.dll` beside it and reads fonts, keymaps
//! and `/tmp` as real files. So the tree is unpacked to `%LOCALAPPDATA%\ruxt\data\xserver`
//! and checked against the embedded SHA-256 index before *every* start, because that directory
//! is writable by the user (unlike `Program Files`). A mismatch, a missing file or an unknown
//! file in `bin\` (where XWin would load a planted DLL from) unpacks a fresh copy. The path is
//! the same for every Ruxt version, so the firewall rule for its `XWin.exe` survives updates.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/xserver_index.rs"));

/// The contents of [`FILES`] back to back, as one deflate stream.
static ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/xserver.bin"));

/// Empty directories XWin and xkbcomp write into (not in the index, which lists files only).
const WRITABLE_DIRS: &[&str] = &["tmp", "var/log/xwin", "var/lib/xkb"];

/// Where the tree is unpacked.
pub fn dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "ruxt").map(|d| d.data_local_dir().join("xserver"))
}

/// Make sure `dir` holds exactly the embedded tree, unpacking it when it does not.
pub fn ensure(dir: &Path) -> Result<()> {
    if verify(dir) {
        return Ok(());
    }
    let parent = dir.parent().context("X server directory has no parent")?;
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(format!("xserver.new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let unpacked = extract(&staging).and_then(|()| {
        if verify(&staging) {
            Ok(())
        } else {
            bail!("the unpacked X server does not match its index")
        }
    });
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e).context("unpacking the embedded X server");
    }
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e).with_context(|| {
                format!(
                    "replacing {} (is an X server from another Ruxt still running?)",
                    dir.display()
                )
            });
        }
    }
    if let Err(e) = std::fs::rename(&staging, dir) {
        let _ = std::fs::remove_dir_all(&staging);
        // Another Ruxt may have put the same tree in place first.
        if !verify(dir) {
            return Err(e).with_context(|| format!("moving the X server into {}", dir.display()));
        }
    }
    super::audit::log(
        "xserver.unpacked",
        serde_json::json!({"dir": dir.display().to_string()}),
    );
    Ok(())
}

fn join(dir: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(dir.to_path_buf(), |p, c| p.join(c))
}

/// Every indexed file present with its size and hash, nothing else in `bin\`.
fn verify(dir: &Path) -> bool {
    for (rel, hash, size) in FILES {
        let path = join(dir, rel);
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() == *size => {}
            _ => return false,
        }
        let Ok(data) = std::fs::read(&path) else { return false };
        if hex(&Sha256::digest(&data)) != *hash {
            return false;
        }
    }
    let known: HashSet<String> = FILES
        .iter()
        .filter_map(|(rel, _, _)| rel.strip_prefix("bin/"))
        .map(|f| f.to_ascii_lowercase())
        .collect();
    let Ok(entries) = std::fs::read_dir(dir.join("bin")) else {
        return false;
    };
    for e in entries {
        let Ok(e) = e else { return false };
        if !known.contains(&e.file_name().to_string_lossy().to_ascii_lowercase()) {
            return false;
        }
    }
    WRITABLE_DIRS.iter().all(|d| join(dir, d).is_dir())
}

fn extract(dest: &Path) -> Result<()> {
    let mut z = flate2::read::DeflateDecoder::new(ARCHIVE);
    for (rel, _, size) in FILES {
        let path = join(dest, rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut f = std::fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        let n = std::io::copy(&mut (&mut z).take(*size), &mut f)?;
        if n != *size {
            bail!("embedded X server archive is truncated at {rel}");
        }
    }
    for d in WRITABLE_DIRS {
        std::fs::create_dir_all(join(dest, d))?;
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_verifies_and_repairs() {
        let root = std::env::temp_dir().join(format!("ruxt-xbundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("xserver");
        ensure(&dir).unwrap();
        assert!(dir.join("bin").join("XWin.exe").is_file());
        assert!(verify(&dir));

        // A changed file is replaced.
        let xwin = dir.join("bin").join("XWin.exe");
        std::fs::write(&xwin, b"not xwin").unwrap();
        assert!(!verify(&dir));
        ensure(&dir).unwrap();
        assert!(verify(&dir));

        // So is a DLL planted beside XWin.exe.
        std::fs::write(dir.join("bin").join("version.dll"), b"").unwrap();
        assert!(!verify(&dir));
        ensure(&dir).unwrap();
        assert!(!dir.join("bin").join("version.dll").exists());

        // Files XWin writes elsewhere at run time do not count.
        std::fs::write(dir.join("tmp").join(".X0-lock"), b"1234").unwrap();
        assert!(verify(&dir));
        let _ = std::fs::remove_dir_all(&root);
    }
}
