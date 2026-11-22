# Ruxt Build Guide

English | [한국어](BUILD.ko.md)

This document describes how to build, test and package `Ruxt` (`ruxt`) from source on each OS.
Tested environment: Windows 11, Rust 1.99.1 (stable, `x86_64-pc-windows-msvc`), Visual Studio Build Tools 2026 (MSVC 14.51) + Windows SDK 10.0.26100.

---

## 1. Requirements

| Item | Minimum | Notes |
| --- | --- | --- |
| Rust | **1.95 or later** (stable) | egui/eframe 0.37 requires 1.95. Installing with `rustup` is recommended |
| Disk | About 4 GB | Dependency sources + `target/` (one release build) |
| Memory | 4 GB or more | The LTO link step uses 2–3 GB |
| Network | Access to crates.io | The first build downloads about 400 crates |

The only dependency that needs a C/assembly build is `ring` (crypto primitives), so any one of MSVC, GCC or Clang is enough. CMake, NASM and Perl are **not needed** (`russh` is pinned to the `ring` backend instead of `aws-lc-rs`).

---

## 2. Windows

### 2-1. Install the toolchain

1. **Visual Studio Build Tools** (C++ workload)
   ```powershell
   winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
   Skip this if Visual Studio (2019 or later) is already installed with the "Desktop development with C++" workload. To check:
   ```powershell
   Get-ChildItem "C:\Program Files*\Microsoft Visual Studio\*\*\VC\Tools\MSVC" -ErrorAction SilentlyContinue
   ```
2. **Rust** (rustup)
   ```powershell
   Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile $env:TEMP\rustup-init.exe
   & $env:TEMP\rustup-init.exe -y --default-toolchain stable --profile minimal --default-host x86_64-pc-windows-msvc
   ```
   Open a new terminal, or update PATH with `$env:Path += ";$env:USERPROFILE\.cargo\bin"`.
3. Check
   ```powershell
   cargo --version   # cargo 1.95.0 or later
   rustc --version
   ```

### 2-2. Build

```powershell
cargo build --release
```

* Output: `target\release\ruxt.exe` (a single file, about 22 MB with both the OpenGL and wgpu renderers, no extra DLLs needed)
* The first build takes about 5–10 minutes (compiling dependencies); incremental builds after that take tens of seconds.
* A debug build (`cargo build`) opens a console window that shows logs. A release build uses `windows_subsystem = "windows"` and runs without a console.

### 2-3. Run

```powershell
.\target\release\ruxt.exe
```

On first run, the settings folder `%APPDATA%\ruxt\config\` and the log folder `%LOCALAPPDATA%\ruxt\data\` are created.

### 2-4. Building with the GNU toolchain (MinGW)

You can also build with MSYS2/UCRT64 GCC instead of MSVC.
```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu build --release
```
`ring` needs `gcc`, so install it with `pacman -S mingw-w64-ucrt-x86_64-gcc` and add it to PATH.

---

## 3. Linux (Debian/Ubuntu)

### 3-1. System packages

```bash
sudo apt update
sudo apt install -y build-essential pkg-config curl \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libgl1-mesa-dev libegl1-mesa-dev libfontconfig1-dev \
  libssl-dev
```

* Windowing/input: `winit` → xkbcommon, Wayland and X11 headers
* Rendering: `glow` (OpenGL) → Mesa GL/EGL
* File dialogs: `rfd` uses the **XDG Desktop Portal** by default, so GTK development packages are not needed. On minimal systems without a portal (servers and so on), install `xdg-desktop-portal-gtk` or `-kde`.
* Clipboard: `arboard` supports both X11 and Wayland.

Fedora/RHEL:
```bash
sudo dnf install -y gcc gcc-c++ pkg-config libxkbcommon-devel libxkbcommon-x11-devel wayland-devel \
  libX11-devel libXcursor-devel libXrandr-devel libXi-devel mesa-libGL-devel mesa-libEGL-devel fontconfig-devel
```

Arch:
```bash
sudo pacman -S --needed base-devel pkgconf libxkbcommon wayland libx11 libxcursor libxrandr libxi mesa fontconfig
```

### 3-2. Install Rust and build

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
cargo build --release
./target/release/ruxt
```

Settings are stored in `~/.config/ruxt/` and logs in `~/.local/share/ruxt/`, with file permissions restricted to 0600/0700.

---

## 4. macOS

```bash
xcode-select --install            # Apple Clang + SDK
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
cargo build --release
./target/release/ruxt
```

Builds natively on both Apple Silicon and Intel. Fonts default to `SF Mono`/`Menlo`, and `AppleSDGothicNeo` is found automatically as the CJK fallback.

---

## 5. Tests

```bash
cargo test
```

| Test | What it covers |
| --- | --- |
| `core::vault::tests` | Argon2id + XChaCha20-Poly1305 vault encryption/decryption, detection of wrong passwords and tampered body/header, KDF parameter ranges, secret rotation and cleanup |
| `core::hostkeys::tests` | Idempotent learning, change detection, relearning after forget |
| `core::sftp::tests` | Remote file name validation (path escapes, drive/stream prefixes, reserved device names), executable extension detection, path helpers, editor command parsing |
| `core::forward::tests` | Loopback policy and bind address parsing (including IPv6), SOCKS5 handshake parsing and rejection replies |
| `core::x11::tests` | Replacing the fake cookie with the real one, rejection on a wrong cookie or missing local cookie, `.Xauthority` entry selection |
| `core::ssh::tests::e2e_*` | In-process russh server: host key TOFU, rejection and explicit replacement of a changed key, password auth failure/cancel, shell I/O, -L local forwarding, -D SOCKS5, rejection of unrequested forwarded-tcpip channels |
| `core::local_pty::tests` | ConPTY/pty local shell round trip, exit detection, process termination via `shutdown()` |
| `core::xserver::tests` | X authority file creation and cookie reading, display listener discovery |
| `app::terminal_widget::tests` | Key mapping (xterm sequences, AltGr, Alt+punctuation), mouse reporting (SGR/X10), paste sanitizing, search and URL detection, OSC 7 split across reads, bidi character stripping |
| `app::fonts::tests` | System font scan (reads only the name/post tables) and family lookup |

Tests use only temporary ports on 127.0.0.1, so they run without any external connection. They take about 2–3 seconds in total (excluding compilation).

---

## 6. Build profile and security settings

Release profile in `Cargo.toml`:

```toml
[profile.release]
opt-level = 3
lto = "fat"            # full LTO
codegen-units = 1
overflow-checks = true # panic on integer overflow in release too (intentional, for security)
strip = true           # strip symbols
```

* `#![forbid(unsafe_code)]` is set at the crate root, so any `unsafe` block fails to compile.
* `russh` is pinned to `default-features = false, features = ["ring", "flate2", "rsa"]`. Switching to `aws-lc-rs` also requires CMake/NASM.
* `Cargo.lock` is committed, so builds reproduce with the same dependency versions. After updating dependencies with `cargo update`, run the tests again.

---

## 7. Optional tools

```bash
rustup component add clippy rustfmt
cargo clippy --all-targets -- -D warnings   # static analysis (same as CI, keep zero warnings)
cargo fmt                                   # formatting (rustfmt.toml: max_width = 120)
cargo install cargo-audit && cargo audit    # check for vulnerable dependencies (RustSec)
```

`rust-version = "1.95"` in `Cargo.toml` is the minimum supported version. Older toolchains refuse to build without `--ignore-rust-version`.

---

## 8. Packaging

### Windows
* `ruxt.exe` itself has no runtime DLL dependencies. Just copy it.
* **X server**: the Cygwin/X used for X11 forwarding is embedded in `ruxt.exe` and extracted to `%LOCALAPPDATA%\ruxt\data\xserver` on first use (hashes are checked on every start). The tree has to be downloaded **before `cargo build`** for `build.rs` to embed it. The script below uses Cygwin's official `setup-x86_64.exe` (Authenticode signature verified) to download `xorg-server`, keeps only the minimal set XWin needs to run (about 26 MiB) and writes `MANIFEST.txt` (package versions and SHA-256).
  ```powershell
  .\installer\fetch-xserver.ps1                              # → target\release\xserver (embedded in release builds)
  .\installer\fetch-xserver.ps1 -Dest target\debug\xserver   # for development (embedded in debug builds)
  cargo build --release
  ```
  * `build.rs` embeds only the files listed in `MANIFEST.txt` and checks the SHA-256 of each one (files created at runtime, such as `tmp\.X0-lock`, are left out). To embed a tree from somewhere else, set `RUXT_XSERVER_EMBED_DIR`.
  * If the tree is missing, the build only prints a warning and builds without the X server. With `RUXT_REQUIRE_XSERVER=1` the build fails instead (release workflow).
  * If you downloaded the tree after building, run `cargo clean -p ruxt --release` and build again (the tree is not watched while it is missing).
  * At runtime, an `xserver\` folder next to `ruxt.exe` or `RUXT_XSERVER_DIR` is used instead of the embedded copy (for development and debugging). To pick up X.org security fixes, run this script again to get the new `xorg-server` point release.
* Signing: `signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /a target\release\ruxt.exe`
* Avoiding the SmartScreen warning requires a code signing certificate.

### Linux
* The binary is dynamically linked against glibc. To support older distributions, build in the environment with the oldest glibc you target (for example, an Ubuntu 20.04 container).
* Example `.desktop` file:
  ```ini
  [Desktop Entry]
  Name=Ruxt
  Exec=/usr/local/bin/ruxt
  Type=Application
  Categories=Network;TerminalEmulator;
  ```

### macOS
* For an `.app` bundle, run `cargo install cargo-bundle` and then `cargo bundle --release`.

---

## 9. Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `error: linker 'link.exe' not found` | The MSVC C++ workload is not installed. Install Build Tools from 2-1 and retry in a new terminal |
| `ring` fails to build (no `cl.exe`/`gcc`) | A C compiler is required. Windows: Build Tools, Linux: `build-essential`, macOS: `xcode-select --install` |
| Link errors about `xkbcommon`/`wayland` on Linux | Missing `-dev` packages from 3-1 |
| The window is black or closes right away | OpenGL driver issue. Run a debug build (`cargo run`) with `RUXT_LOG=debug` and check the log. On remote desktops/VMs, install Mesa (llvmpipe) |
| File dialogs do not open (Linux) | Install `xdg-desktop-portal` and a desktop backend (`-gtk`/`-kde`) |
| Korean/CJK text shows as □ | No CJK font on the system. Ruxt looks for Windows `malgun.ttf`, Linux `fonts-nanum`/`fonts-noto-cjk` and the macOS default fonts automatically |
| The build stops from running out of memory | Lower to `lto = "thin"` or run `CARGO_BUILD_JOBS=2 cargo build --release` |
| The first build is very slow | This is normal. During development, use `cargo build` (dev profile, dependencies at `opt-level=2`) |

---

## 10. Diagnostic environment variables

| Variable | Effect |
| --- | --- |
| `RUXT_LOG=debug` (or `trace`, `russh=debug`) | Log verbosity. Passwords and keys are never logged |
| `RUXT_THEME=light` / `matcha` / `purple-green` / `harbor` / `ember` / `ink` / `pine` / `porcelain` / `sand` / `mint` / `rose` / `lavender` | Forces a theme for this run only (does not change the settings file). `dark` means Matcha |
| `RUXT_OPEN_LOCAL=1` | Opens a local shell tab at startup |

Debug builds print logs to the console. To see logs from a release build, redirect them in PowerShell:
```powershell
$env:RUXT_LOG="debug"; .\target\debug\ruxt.exe 2> ruxt.log
```

## Making a release

Pushing a `vX.Y.Z` tag makes `.github/workflows/release.yml` build Windows (x86_64), Linux (x86_64) and macOS (arm64, x86_64) binaries, create the Windows installer (`installer/ruxt.iss`, Inno Setup 6) and `SHA256SUMS`, and upload them to a GitHub release. The release notes come from the matching version section of `CHANGELOG.md`.

```powershell
# 1. Write the [X.Y.Z] section in CHANGELOG.md (English) and CHANGELOG.ko.md, and bump version in Cargo.toml
# 2. Push the tag
git tag vX.Y.Z
git push origin vX.Y.Z
```

Authenticode signing of the Windows binary and installer runs only when the repository secrets `WINDOWS_CERT_PFX_BASE64` (the PFX file in base64) and `WINDOWS_CERT_PASSWORD` are set. To build only the installer locally:

```powershell
.\installer\fetch-xserver.ps1
cargo build --release
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DAppVersion=0.2.0 /DBinDir=target\release installer\ruxt.iss
# Output: dist\ruxt-0.2.0-setup.exe
```

### Graphics backends

The default renderer is OpenGL (glow), and you can switch to wgpu (DirectX 12 / Vulkan / Metal) in Settings → Graphics. Both are included in the binary, so there is no separate build option. The environment variable `RUXT_RENDERER=wgpu` or `opengl` overrides the setting (for diagnosing driver problems).
