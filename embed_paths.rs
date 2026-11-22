// Profile directory that holds the fetched Cygwin/X tree.
// Shared by `build.rs` and `tests/xserver_embed_dir.rs`.

use std::path::Path;

/// `target[/<triple>]/<profile>`, the parent of Cargo's `build` folder inside `OUT_DIR`.
///
/// Cargo before 1.100: `<profile>/build/<pkg>-<hash>/out`.
/// Cargo 1.100 and later: `<profile>/build/<pkg>/<hash>/out`.
/// `--target` inserts `<triple>` in front of `<profile>` in both layouts. A fixed ancestor
/// count lands on `build` after that change and misses `xserver/` fetched beside the profile.
pub(super) fn profile_dir(out_dir: &Path) -> &Path {
    out_dir
        .ancestors()
        .find(|p| p.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
        .expect("OUT_DIR has no profile directory above build/")
}

#[cfg(test)]
mod layout_tests {
    use super::profile_dir;
    use std::path::{Path, PathBuf};

    fn path(parts: &[&str]) -> PathBuf {
        parts.iter().collect()
    }

    #[test]
    fn cargo_before_1_100_host_release() {
        let out = path(&["target", "release", "build", "ruxt-dfa9aba8166ccfc7", "out"]);
        assert_eq!(profile_dir(&out), Path::new("target").join("release"));
    }

    #[test]
    fn cargo_before_1_100_explicit_target() {
        let out = path(&[
            "target",
            "x86_64-pc-windows-msvc",
            "release",
            "build",
            "ruxt-dfa9aba8166ccfc7",
            "out",
        ]);
        assert_eq!(
            profile_dir(&out).join("xserver"),
            path(&["target", "x86_64-pc-windows-msvc", "release", "xserver"])
        );
    }

    #[test]
    fn cargo_1_100_host_release() {
        let out = path(&["target", "release", "build", "ruxt", "dfa9aba8166ccfc7", "out"]);
        assert_eq!(
            profile_dir(&out).join("xserver"),
            path(&["target", "release", "xserver"])
        );
    }

    #[test]
    fn cargo_1_100_explicit_target_is_beside_build() {
        // Release workflow: fetch-xserver.ps1 -Dest target/<triple>/release/xserver
        let out = path(&[
            "target",
            "x86_64-pc-windows-msvc",
            "release",
            "build",
            "ruxt",
            "dfa9aba8166ccfc7",
            "out",
        ]);
        assert_eq!(
            profile_dir(&out).join("xserver"),
            path(&["target", "x86_64-pc-windows-msvc", "release", "xserver"])
        );
    }
}
