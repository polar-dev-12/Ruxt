//! Embeds the bundled Cygwin/X tree into Windows builds (unpacked at run time by
//! `src/core/xbundle.rs`).
//!
//! The tree comes from `installer/fetch-xserver.ps1`: `$RUXT_XSERVER_EMBED_DIR`, else `xserver/`
//! in the profile directory (`target[/<triple>]/<profile>/xserver`, the script's default and the
//! release workflow's `-Dest`). Only the files listed in its `MANIFEST.txt` are taken, and each
//! must match the SHA-256 recorded there, so whatever XWin wrote into the tree while it ran from
//! there (`tmp/.X0-lock`, ...) stays out.
//!
//! Without a tree the build goes on with no X server inside (a warning says so), unless
//! `RUXT_REQUIRE_XSERVER=1` (release builds) makes that an error. After fetching the tree into a
//! profile directory that has been built before, run `cargo clean -p ruxt` (or touch build.rs):
//! a missing tree is not watched, so that ordinary builds do not rerun this script every time.

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

#[path = "embed_paths.rs"]
mod embed_paths;
use embed_paths::profile_dir;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=RUXT_XSERVER_EMBED_DIR");
    println!("cargo::rerun-if-env-changed=RUXT_REQUIRE_XSERVER");
    println!("cargo::rustc-check-cfg=cfg(ruxt_embedded_xserver)");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    embed_icon();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let src = match std::env::var_os("RUXT_XSERVER_EMBED_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => profile_dir(&out).join("xserver"),
    };
    let manifest = src.join("MANIFEST.txt");
    if !manifest.is_file() {
        let msg = format!(
            "no X server tree at {} (run installer/fetch-xserver.ps1): building without the embedded X server",
            src.display()
        );
        if std::env::var("RUXT_REQUIRE_XSERVER").as_deref() == Ok("1") {
            panic!("{msg}");
        }
        println!("cargo::warning={msg}");
        return;
    }
    println!("cargo::rerun-if-changed={}", manifest.display());

    let files = read_manifest(&manifest);
    assert!(
        files.iter().any(|(rel, _)| rel == "bin/XWin.exe"),
        "{} does not list bin\\XWin.exe",
        manifest.display()
    );

    // Archive: the file contents back to back in index order, deflated as one stream. The index
    // (path, SHA-256, size) is generated Rust code, so the runtime needs no archive format.
    let mut z = flate2::write::DeflateEncoder::new(
        std::fs::File::create(out.join("xserver.bin")).expect("create xserver.bin"),
        flate2::Compression::best(),
    );
    let mut index = String::from("pub(super) const FILES: &[(&str, &str, u64)] = &[\n");
    for (rel, want) in files
        .iter()
        .map(|(r, h)| (r.as_str(), Some(h.as_str())))
        .chain([("MANIFEST.txt", None)])
    {
        let path = rel.split('/').fold(src.clone(), |p, c| p.join(c));
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let got = hex(&Sha256::digest(&data));
        if let Some(want) = want {
            assert!(
                got == want,
                "{} does not match the SHA-256 in MANIFEST.txt (re-run installer/fetch-xserver.ps1)",
                path.display()
            );
        }
        z.write_all(&data).expect("write xserver.bin");
        index.push_str(&format!("    ({rel:?}, {got:?}, {}),\n", data.len()));
    }
    index.push_str("];\n");
    z.finish().expect("finish xserver.bin");
    std::fs::write(out.join("xserver_index.rs"), index).expect("write xserver_index.rs");
    println!("cargo::rustc-cfg=ruxt_embedded_xserver");
}

/// `(relative path with '/', lowercase SHA-256)` for each "  <sha256>  <path>" line.
fn read_manifest(path: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut files = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let Some((h, rel)) = line.trim().split_once("  ") else {
            continue;
        };
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let rel = rel.trim().replace('\\', "/");
        assert!(
            !rel.starts_with('/')
                && !rel.contains(':')
                && rel.split('/').all(|c| !c.is_empty() && c != "." && c != ".."),
            "unsafe path in MANIFEST.txt: {rel}"
        );
        files.push((rel, h.to_ascii_lowercase()));
    }
    files
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Gives `ruxt.exe` its icon (Explorer, shortcuts, taskbar, Apps & features). The window icon
/// itself is set at run time from `assets/icon-256.png` (see `window_icon` in main.rs).
#[cfg(windows)]
fn embed_icon() {
    println!("cargo::rerun-if-changed=assets/icon.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    if let Err(e) = res.compile() {
        panic!("embedding assets/icon.ico failed: {e}");
    }
}

/// Windows targets are built on Windows hosts, so there is nothing to embed elsewhere.
#[cfg(not(windows))]
fn embed_icon() {}
