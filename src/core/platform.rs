//! Small OS-level hardening that needs no unsafe code.

/// Keep Windows Error Reporting from taking (and uploading) a memory dump of Ruxt: a crash
/// dump would contain the open vault, passwords being typed and every terminal scrollback.
/// This is what `WerAddExcludedApplication(…, FALSE)` writes, done through `reg.exe` so the
/// crate stays `forbid(unsafe_code)`. Per-user, no elevation needed.
#[cfg(windows)]
pub fn exclude_from_error_reporting() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return;
    };
    let key = r"HKCU\Software\Microsoft\Windows\Windows Error Reporting\ExcludedApplications";
    let status = std::process::Command::new("reg")
        .args(["add", key, "/v", &name, "/t", "REG_DWORD", "/d", "1", "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => super::audit::log("wer.excluded", serde_json::json!({"exe": name})),
        Ok(s) => tracing::warn!("could not exclude {name} from Windows Error Reporting: {s}"),
        Err(e) => tracing::warn!("could not run reg.exe: {e}"),
    }
}

#[cfg(not(windows))]
pub fn exclude_from_error_reporting() {
    // Core dumps are governed by `ulimit -c` / systemd-coredump; nothing portable to do here
    // without libc (`prctl(PR_SET_DUMPABLE)`), which would need unsafe.
}

/// A text editor found on this machine, for the "External editor" choice in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorChoice {
    /// Shown in the drop-down.
    pub name: &'static str,
    /// Stored in `AppConfig::external_editor` (see `sftp::split_command`).
    pub command: String,
}

/// GUI editors: display name, Windows install paths (`%VAR%` expanded), program names looked
/// up on PATH elsewhere, and arguments placed before the file.
const KNOWN_EDITORS: &[(&str, &[&str], &[&str], &str)] = &[
    (
        "Visual Studio Code",
        &[
            r"%LOCALAPPDATA%\Programs\Microsoft VS Code\Code.exe",
            r"%ProgramFiles%\Microsoft VS Code\Code.exe",
        ],
        &["code"],
        "-w",
    ),
    (
        "Cursor",
        &[r"%LOCALAPPDATA%\Programs\cursor\Cursor.exe"],
        &["cursor"],
        "-w",
    ),
    (
        "VSCodium",
        &[
            r"%LOCALAPPDATA%\Programs\VSCodium\VSCodium.exe",
            r"%ProgramFiles%\VSCodium\VSCodium.exe",
        ],
        &["codium"],
        "-w",
    ),
    ("Zed", &[r"%LOCALAPPDATA%\Programs\Zed\Zed.exe"], &["zed"], ""),
    (
        "Sublime Text",
        &[
            r"%ProgramFiles%\Sublime Text\sublime_text.exe",
            r"%ProgramFiles%\Sublime Text 3\sublime_text.exe",
        ],
        &["subl"],
        "",
    ),
    (
        "Notepad++",
        &[
            r"%ProgramFiles%\Notepad++\notepad++.exe",
            r"%ProgramFiles(x86)%\Notepad++\notepad++.exe",
        ],
        &[],
        "",
    ),
    ("Notepad", &[r"%SystemRoot%\System32\notepad.exe"], &[], ""),
    ("Kate", &[r"%ProgramFiles%\Kate\bin\kate.exe"], &["kate"], ""),
    ("gedit", &[], &["gedit"], ""),
    ("GNOME Text Editor", &[], &["gnome-text-editor"], ""),
    ("Mousepad", &[], &["mousepad"], ""),
];

fn expand_env(path: &str) -> Option<std::path::PathBuf> {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        let end = start + 1 + rest[start + 1..].find('%')?;
        out.push_str(&rest[..start]);
        out.push_str(&std::env::var(&rest[start + 1..end]).ok()?);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out.into())
}

fn on_path(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

/// Editors installed here, detected once per run.
pub fn installed_editors() -> &'static [EditorChoice] {
    static FOUND: std::sync::OnceLock<Vec<EditorChoice>> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        KNOWN_EDITORS
            .iter()
            .filter_map(|(name, win_paths, programs, args)| {
                let exe = if cfg!(windows) {
                    win_paths.iter().filter_map(|p| expand_env(p)).find(|p| p.is_file())
                } else {
                    programs.iter().find_map(|p| on_path(p))
                }?;
                let mut command = format!("\"{}\"", exe.display());
                if !args.is_empty() {
                    command.push(' ');
                    command.push_str(args);
                }
                Some(EditorChoice { name, command })
            })
            .collect()
    })
}

#[cfg(all(test, windows))]
mod tests {
    /// Writes the real per-user registry value (what the app does at every start):
    /// `cargo test wer_exclusion -- --ignored`.
    #[test]
    #[ignore]
    fn wer_exclusion_is_registered() {
        super::exclude_from_error_reporting();
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\Windows Error Reporting\ExcludedApplications",
            ])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(text.contains("ruxt"), "{text}");
    }
}
