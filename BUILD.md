# Ruxt 빌드 가이드

이 문서는 소스에서 `Ruxt`(`ruxt`)를 빌드·테스트·배포하는 절차를 OS별로 정리한 것입니다.
검증된 환경: Windows 11, Rust 1.99.1 (stable, `x86_64-pc-windows-msvc`), Visual Studio Build Tools 2026(MSVC 14.51) + Windows SDK 10.0.26100.

---

## 1. 요구 사항

| 항목 | 최소 | 비고 |
| --- | --- | --- |
| Rust | **1.95 이상** (stable) | egui/eframe 0.37 이 1.95 를 요구. `rustup`으로 설치 권장 |
| 디스크 | 약 4 GB | 의존성 소스 + `target/` (릴리스 1 회 빌드 기준) |
| 메모리 | 4 GB 이상 | LTO 링크 단계에서 2~3 GB 사용 |
| 네트워크 | crates.io 접근 | 첫 빌드 시 약 400 개 크레이트 다운로드 |

의존성 중 C/어셈블리 빌드가 필요한 것은 `ring`(암호 프리미티브) 하나뿐이며, MSVC/GCC/Clang 중 하나만 있으면 됩니다. CMake·NASM·Perl은 **필요 없습니다**(`russh`를 `aws-lc-rs` 대신 `ring` 백엔드로 고정해 두었습니다).

---

## 2. Windows

### 2-1. 툴체인 설치

1. **Visual Studio Build Tools** (C++ 워크로드)
   ```powershell
   winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
   이미 Visual Studio(2019 이상)가 설치되어 있고 "C++를 사용한 데스크톱 개발" 워크로드가 포함되어 있으면 생략합니다. 확인:
   ```powershell
   Get-ChildItem "C:\Program Files*\Microsoft Visual Studio\*\*\VC\Tools\MSVC" -ErrorAction SilentlyContinue
   ```
2. **Rust** (rustup)
   ```powershell
   Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile $env:TEMP\rustup-init.exe
   & $env:TEMP\rustup-init.exe -y --default-toolchain stable --profile minimal --default-host x86_64-pc-windows-msvc
   ```
   새 터미널을 열거나 `$env:Path += ";$env:USERPROFILE\.cargo\bin"` 으로 PATH를 갱신합니다.
3. 확인
   ```powershell
   cargo --version   # cargo 1.95.0 이상
   rustc --version
   ```

### 2-2. 빌드

```powershell
cargo build --release
```

* 결과물: `target\release\ruxt.exe` (단일 파일, 약 22 MB(OpenGL·wgpu 두 렌더러 포함), 추가 DLL 불필요)
* 첫 빌드 약 5~10 분(의존성 컴파일), 이후 증분 빌드는 수십 초.
* 디버그 빌드(`cargo build`)는 콘솔 창이 함께 뜨고 로그가 보입니다. 릴리스 빌드는 `windows_subsystem = "windows"` 로 콘솔 없이 실행됩니다.

### 2-3. 실행

```powershell
.\target\release\ruxt.exe
```

처음 실행 시 설정 폴더 `%APPDATA%\ruxt\config\` 와 로그 폴더 `%LOCALAPPDATA%\ruxt\data\` 가 생성됩니다.

### 2-4. GNU 툴체인(MinGW)으로 빌드하려면

MSVC 없이 MSYS2/UCRT64 GCC로도 빌드됩니다.
```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu build --release
```
`ring` 이 `gcc`를 요구하므로 `pacman -S mingw-w64-ucrt-x86_64-gcc` 로 설치하고 PATH에 넣어 두어야 합니다.

---

## 3. Linux (Debian/Ubuntu 기준)

### 3-1. 시스템 패키지

```bash
sudo apt update
sudo apt install -y build-essential pkg-config curl \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libgl1-mesa-dev libegl1-mesa-dev libfontconfig1-dev \
  libssl-dev
```

* 창/입력: `winit` → xkbcommon, Wayland, X11 헤더
* 렌더링: `glow`(OpenGL) → Mesa GL/EGL
* 파일 대화상자: `rfd` 는 기본적으로 **XDG Desktop Portal** 을 사용하므로 GTK 개발 패키지는 필요 없습니다. 포털이 없는 최소 환경(서버 등)에서는 `xdg-desktop-portal-gtk` 또는 `-kde` 패키지를 설치하세요.
* 클립보드: `arboard` 는 X11/Wayland 모두 지원합니다.

Fedora/RHEL 계열:
```bash
sudo dnf install -y gcc gcc-c++ pkg-config libxkbcommon-devel libxkbcommon-x11-devel wayland-devel \
  libX11-devel libXcursor-devel libXrandr-devel libXi-devel mesa-libGL-devel mesa-libEGL-devel fontconfig-devel
```

Arch:
```bash
sudo pacman -S --needed base-devel pkgconf libxkbcommon wayland libx11 libxcursor libxrandr libxi mesa fontconfig
```

### 3-2. Rust 설치 및 빌드

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
cargo build --release
./target/release/ruxt
```

설정은 `~/.config/ruxt/`, 로그는 `~/.local/share/ruxt/` 에 저장되며 파일 권한은 0600/0700 으로 제한됩니다.

---

## 4. macOS

```bash
xcode-select --install            # Apple Clang + SDK
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
cargo build --release
./target/release/ruxt
```

Apple Silicon/Intel 모두 네이티브 빌드됩니다. 폰트는 `SF Mono`/`Menlo`, CJK 폴백은 `AppleSDGothicNeo` 를 자동으로 찾습니다.

---

## 5. 테스트

```bash
cargo test
```

| 테스트 | 내용 |
| --- | --- |
| `core::vault::tests` | Argon2id + XChaCha20-Poly1305 금고 암복호화, 잘못된 비밀번호·본문/헤더 변조 감지, KDF 파라미터 범위, 비밀 교체·정리 |
| `core::hostkeys::tests` | 학습 멱등성, 변경 감지, forget 후 재학습 |
| `core::sftp::tests` | 원격 파일명 검증(경로 탈출·드라이브/스트림 접두사·예약 장치명), 실행 파일 확장자 판별, 경로 헬퍼, 편집기 명령 파싱 |
| `core::forward::tests` | 루프백 정책·바인드 주소 파싱(IPv6 포함), SOCKS5 핸드셰이크 파싱과 거부 응답 |
| `core::x11::tests` | 가짜 쿠키 → 실제 쿠키 교체, 잘못된 쿠키·로컬 쿠키 부재 시 거부, `.Xauthority` 항목 선택 |
| `core::ssh::tests::e2e_*` | 인프로세스 russh 서버: 호스트키 TOFU, 변조 키 거부 및 명시적 교체, 비밀번호 인증 실패/취소, 셸 I/O, -L 로컬 포워딩, -D SOCKS5, 요청하지 않은 forwarded-tcpip 채널 거부 |
| `core::local_pty::tests` | ConPTY/pty 로컬 셸 왕복, 종료 감지, `shutdown()`에 의한 프로세스 종료 |
| `core::xserver::tests` | X authority 파일 생성·쿠키 읽기, 디스플레이 리스너 탐색 |
| `app::terminal_widget::tests` | 키 매핑(xterm 시퀀스, AltGr, Alt+구두점), 마우스 리포팅(SGR/X10), 붙여넣기 위생, 검색·URL 감지, OSC 7 분할 수신, bidi 문자 제거 |
| `app::fonts::tests` | 시스템 글꼴 스캔(name/post 테이블만 읽음)과 패밀리 검색 |

네트워크는 127.0.0.1 의 임시 포트만 사용하므로 외부 접속 없이 실행됩니다. 총 소요 약 2~3 초(컴파일 제외).

---

## 6. 빌드 프로필 및 보안 관련 설정

`Cargo.toml` 의 릴리스 프로필:

```toml
[profile.release]
opt-level = 3
lto = "fat"            # 전체 LTO
codegen-units = 1
overflow-checks = true # 릴리스에서도 정수 오버플로 패닉(보안 상 의도적)
strip = true           # 심볼 제거
```

* `#![forbid(unsafe_code)]` 가 크레이트 루트에 있어 `unsafe` 블록이 들어가면 컴파일이 실패합니다.
* `russh` 는 `default-features = false, features = ["ring", "flate2", "rsa"]` 로 고정되어 있습니다. `aws-lc-rs` 로 바꾸면 CMake/NASM 이 추가로 필요합니다.
* `Cargo.lock` 이 저장소에 포함되어 있어 동일한 의존성 버전으로 재현 빌드됩니다. 의존성 갱신은 `cargo update` 후 테스트를 다시 실행하세요.

---

## 7. 선택 도구

```bash
rustup component add clippy rustfmt
cargo clippy --all-targets -- -D warnings   # 정적 분석 (CI와 동일, 경고 0 유지)
cargo fmt                                   # 포맷 (rustfmt.toml: max_width = 120)
cargo install cargo-audit && cargo audit    # 취약 의존성 점검(RustSec)
```

`Cargo.toml`의 `rust-version = "1.95"`가 최소 지원 버전입니다. 더 낮은 툴체인에서는 `--ignore-rust-version` 없이 빌드가 거부됩니다.

---

## 8. 배포 패키징

### Windows
* `ruxt.exe` 자체는 런타임 의존 DLL 이 없습니다. 복사만 하면 됩니다.
* **X 서버**: X11 포워딩에 쓰는 Cygwin/X 는 `ruxt.exe` 안에 내장되고, 처음 쓸 때 `%LOCALAPPDATA%\ruxt\data\xserver` 에 풀립니다(켤 때마다 해시 확인). **`cargo build` 전에** 트리를 받아 두어야 `build.rs` 가 넣습니다. 다음 스크립트가 Cygwin 공식 `setup-x86_64.exe`(Authenticode 서명 검증)로 `xorg-server` 를 받아 XWin 이 실행에 필요한 최소 구성(약 26 MiB)만 골라 넣고 `MANIFEST.txt`(패키지 버전·SHA-256)를 씁니다.
  ```powershell
  .\installer\fetch-xserver.ps1                              # → target\release\xserver (릴리스 빌드에 내장)
  .\installer\fetch-xserver.ps1 -Dest target\debug\xserver   # 개발용 (디버그 빌드에 내장)
  cargo build --release
  ```
  * `build.rs` 는 `MANIFEST.txt` 에 적힌 파일만 넣고 각 파일의 SHA-256 을 대조합니다(실행 중 생긴 `tmp\.X0-lock` 등은 빠짐). 다른 곳의 트리를 넣으려면 `RUXT_XSERVER_EMBED_DIR` 로 지정합니다.
  * 트리가 없으면 경고만 내고 X 서버 없이 빌드합니다. `RUXT_REQUIRE_XSERVER=1` 이면 빌드가 실패합니다(릴리스 워크플로).
  * 이미 빌드한 뒤에 트리를 받았다면 `cargo clean -p ruxt --release` 후 다시 빌드하세요(트리가 없을 때는 감시하지 않음).
  * 실행 시 `ruxt.exe` 옆 `xserver\` 폴더나 `RUXT_XSERVER_DIR` 가 있으면 내장본 대신 그것을 씁니다(개발·디버깅용). X.org 보안 수정은 이 스크립트를 다시 실행해 새 `xorg-server` 포인트 릴리스를 받는 것으로 반영합니다.
* 서명: `signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /a target\release\ruxt.exe`
* SmartScreen 경고를 피하려면 코드 서명 인증서가 필요합니다.

### Linux
* 바이너리는 glibc 에 동적 링크됩니다. 오래된 배포판을 지원하려면 가장 낮은 glibc 환경(예: Ubuntu 20.04 컨테이너)에서 빌드하세요.
* `.desktop` 파일 예시:
  ```ini
  [Desktop Entry]
  Name=Ruxt
  Exec=/usr/local/bin/ruxt
  Type=Application
  Categories=Network;TerminalEmulator;
  ```

### macOS
* `.app` 번들이 필요하면 `cargo install cargo-bundle` 후 `cargo bundle --release`.

---

## 9. 문제 해결

| 증상 | 원인 / 해결 |
| --- | --- |
| `error: linker 'link.exe' not found` | MSVC C++ 워크로드 미설치. 2-1 의 Build Tools 설치 후 새 터미널에서 재시도 |
| `ring` 빌드 실패 (`cl.exe`/`gcc` 없음) | C 컴파일러 필요. Windows: Build Tools, Linux: `build-essential`, macOS: `xcode-select --install` |
| Linux에서 `xkbcommon`/`wayland` 관련 링크 오류 | 3-1 의 `-dev` 패키지 누락 |
| 실행 시 창이 검게 나오거나 바로 종료 | OpenGL 드라이버 문제. `RUXT_LOG=debug` 로 디버그 빌드(`cargo run`)를 실행해 로그 확인. 원격 데스크톱/VM 에서는 Mesa(llvmpipe) 설치 |
| 파일 대화상자가 열리지 않음 (Linux) | `xdg-desktop-portal` + 데스크톱용 백엔드(`-gtk`/`-kde`) 설치 |
| 한글/CJK 가 □ 로 보임 | 시스템에 CJK 폰트가 없음. Windows `malgun.ttf`, Linux `fonts-nanum`/`fonts-noto-cjk`, macOS 기본 폰트를 자동 탐색 |
| 빌드가 메모리 부족으로 중단 | `lto = "thin"` 으로 낮추거나 `CARGO_BUILD_JOBS=2 cargo build --release` |
| 첫 빌드가 매우 느림 | 정상입니다. 개발 중에는 `cargo build`(dev 프로필, 의존성은 `opt-level=2`) 를 사용하세요 |

---

## 10. 진단용 환경변수

| 변수 | 효과 |
| --- | --- |
| `RUXT_LOG=debug` (또는 `trace`, `russh=debug`) | 로그 상세도. 비밀번호·키는 절대 기록되지 않음 |
| `RUXT_THEME=light` / `matcha` / `purple-green` / `harbor` / `ember` / `ink` / `pine` / `porcelain` / `sand` / `mint` / `rose` / `lavender` | 이번 실행에만 테마 강제(설정 파일 변경 없음). `dark`는 말차 |
| `RUXT_OPEN_LOCAL=1` | 시작 시 로컬 셸 탭 자동 오픈 |

디버그 빌드에서는 로그가 콘솔로 출력됩니다. 릴리스 빌드에서 로그를 보려면 PowerShell 에서 리디렉션하세요:
```powershell
$env:RUXT_LOG="debug"; .\target\debug\ruxt.exe 2> ruxt.log
```

## 릴리스 만들기

`vX.Y.Z` 태그를 푸시하면 `.github/workflows/release.yml`이 Windows(x86_64)·Linux(x86_64)·macOS(arm64, x86_64) 바이너리를 빌드하고, Windows 설치 파일(`installer/ruxt.iss`, Inno Setup 6)과 `SHA256SUMS`를 만들어 GitHub 릴리스에 올립니다. 릴리스 노트는 `CHANGELOG.md`의 해당 버전 절에서 가져옵니다.

```powershell
# 1. CHANGELOG.md 에 [X.Y.Z] 절을 쓰고 Cargo.toml 의 version 을 올린다
# 2. 태그를 푸시한다
git tag vX.Y.Z
git push origin vX.Y.Z
```

Windows 바이너리와 설치 파일의 Authenticode 서명은 저장소 시크릿 `WINDOWS_CERT_PFX_BASE64`(PFX 파일의 base64)와 `WINDOWS_CERT_PASSWORD`가 있을 때만 수행됩니다. 로컬에서 설치 파일만 만들려면:

```powershell
.\installer\fetch-xserver.ps1
cargo build --release
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DAppVersion=0.2.0 /DBinDir=target\release installer\ruxt.iss
# 결과: dist\ruxt-0.2.0-setup.exe
```

### 그래픽 백엔드

기본 렌더러는 OpenGL(glow)이고, 설정 → 그래픽에서 wgpu(DirectX 12 / Vulkan / Metal)로 바꿀 수 있습니다. 둘 다 바이너리에 포함되므로 별도 빌드 옵션은 없습니다. 환경 변수 `RUXT_RENDERER=wgpu` 또는 `opengl` 로 설정을 무시하고 지정할 수 있습니다(드라이버 문제 진단용).
