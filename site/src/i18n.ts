export type Lang = "ko" | "en";

export const REPO = "https://github.com/polarbear138/ruxt";

type Row = { threat: string; defense: string };
type Group = { title: string; items: string[] };
type Doc = { title: string; desc: string; href: string };

export interface Copy {
  langLabel: string;
  tagline: string;
  lede: string;
  ctaDownload: string;
  ctaSource: string;
  statusNote: string;
  terminalTitle: string;
  terminalCaption: string;
  defendsTitle: string;
  defendsIntro: string;
  rows: Row[];
  featuresTitle: string;
  groups: Group[];
  screensTitle: string;
  screenExplorer: string;
  downloadTitle: string;
  downloadIntro: string;
  latest: string;
  latestUnknown: string;
  assets: { name: string; note: string; match: RegExp }[];
  checksums: string;
  smartscreen: string;
  buildFromSource: string;
  docsTitle: string;
  docs: Doc[];
  footerLicense: string;
  footerBundle: string;
  footerHonest: string;
}

export const copy: Record<Lang, Copy> = {
  ko: {
    langLabel: "English",
    tagline: "보안을 먼저 설계한 SSH·SFTP 워크스테이션",
    lede:
      "호스트 키 고정, 비신뢰 X11 포워딩, 화면 잠금 비밀번호로 암호화하는 비밀번호 저장, 루프백 전용 터널이 기본값입니다. 느슨하게 쓰려면 직접 꺼야 합니다. 순수 Rust 한 개의 실행 파일로 Windows, Linux, macOS에서 돕니다.",
    ctaDownload: "Windows 설치 파일 받기",
    ctaSource: "GitHub에서 소스 보기",
    statusNote:
      "1.0.0은 첫 안정 공개 버전입니다. 핵심 경로는 인프로세스 SSH 서버 테스트와 세 OS의 CI로 검증했지만 독립 보안 리뷰는 아직 받지 않았습니다. 운영 자격증명으로 쓰기 전에 위협 모델을 읽어 주세요.",
    terminalTitle: "db-01.corp — Ruxt",
    terminalCaption:
      "실제 세션 기록입니다. 처음 보는 호스트 키는 지문을 확인한 뒤에만 저장되고, 포워딩된 X 프로그램은 다른 창을 읽을 수 없습니다.",
    defendsTitle: "기본값이 지키는 것",
    defendsIntro:
      "설정을 바꾸지 않은 상태에서 Ruxt가 막는 것들입니다. 각 줄은 위협 모델 문서의 한 행에 대응합니다.",
    rows: [
      {
        threat: "가짜 서버로 유도하는 중간자",
        defense:
          "호스트 키를 처음 접속 때 지문·randomart와 함께 확인받아 고정합니다. 키가 바뀌면 연결을 끊고, 새 지문을 확인했다고 체크한 뒤에만 교체합니다. 점프 호스트도 홉마다 따로 고정합니다.",
      },
      {
        threat: "오래된 알고리즘으로의 다운그레이드",
        defense:
          "ML-KEM-768+X25519 / X25519 키 교환, ChaCha20-Poly1305와 AES-GCM, SHA-2 MAC만 씁니다. SHA-1, CBC, DSA, RC4, 3DES는 어떤 프로필에도 없고, ssh-rsa는 명시적인 '레거시' 프로필에서만 허용됩니다.",
      },
      {
        threat: "디스크에서 비밀번호 유출",
        defense:
          "비밀번호와 패스프레이즈는 화면 잠금 비밀번호로 암호화해(Argon2id + XChaCha20-Poly1305) 저장하고, 인증이 끝나면 메모리에서 지우며, Ruxt가 잠기면 함께 닫힙니다. 설정 파일과 세션 내보내기에는 비밀이 들어가지 않습니다.",
      },
      {
        threat: "포워딩된 X 프로그램의 화면·키 입력 엿보기",
        defense:
          "ssh -X와 같은 비신뢰 모드가 기본입니다. 동봉 Cygwin/X(X.Org 21.1.25)는 X SECURITY 확장을 켜서 직접 빌드했고, 원격 프로그램은 확장이 발급한 비신뢰 쿠키로만 접속합니다. 신뢰 모드(-Y)는 세션마다 켜야 합니다.",
      },
      {
        threat: "클립보드 인젝션과 터미널 이스케이프",
        defense:
          "여러 줄 붙여넣기는 미리보기와 함께 확인을 받고, 붙여넣는 텍스트의 제어 문자는 제거됩니다. OSC 52 클립보드 쓰기는 기본 차단이며, 창 제목·배너·파일명의 제어·bidi 문자도 걸러냅니다.",
      },
      {
        threat: "터널이 네트워크에 노출",
        defense:
          "로컬·SOCKS 리스너는 루프백에만 바인드되고 SOCKS5는 CONNECT만 받습니다. 서버가 여는 채널은 요청한 포워딩만 수락합니다. 에이전트 포워딩은 기본 꺼짐입니다.",
      },
      {
        threat: "악의적인 파일명으로 로컬 파일 덮어쓰기",
        defense:
          "원격 파일명이 로컬 경로가 되기 전에 경로 구분자, '..', 드라이브 접두사, NTFS 스트림, 예약 장치명을 거부합니다. 디렉터리 심볼릭 링크는 따라가지 않고, 전송은 임시 파일에 쓴 뒤 원자적으로 교체합니다.",
      },
      {
        threat: "내 PC에서 새는 흔적",
        defense:
          "크래시 덤프(Windows 오류 보고)에서 제외되고, 복사한 내용은 N초 뒤 클립보드에서 지울 수 있습니다. 자동 완성용 명령어 기록은 화면 잠금 비밀번호가 있을 때만 남고 줄마다 암호화되며, 감사 로그는 기본 꺼짐입니다. 텔레메트리와 자동 업데이트는 없습니다.",
      },
    ],
    featuresTitle: "하는 일",
    groups: [
      {
        title: "터미널",
        items: [
          "Alacritty 엔진 기반 xterm-256color 에뮬레이션, 스크롤백 검색, URL Ctrl+클릭",
          "상자·블록·점자 문자를 칸에 맞춰 직접 그려 TUI 테두리가 어떤 글꼴에서도 이어짐",
          "한글·CJK IME 조합 표시, 마우스 리포팅, 브래킷 페이스트, Ctrl+C/V 복사·붙여넣기",
          "이전에 실행한 명령을 흐린 글씨로 제안하는 자동 완성 (→로 수락, 기록은 암호화)",
          "모든 SSH 탭에 동시에 입력하는 동시 입력",
          "tail 명령을 키워드 감시·필터·파일 기록이 되는 Tail Viewer 창으로 열기",
        ],
      },
      {
        title: "세션",
        items: [
          "그룹·태그·최근 목록, 빠른 연결, 최대 4홉의 점프 호스트",
          "OpenSSH config, PuTTY 저장 세션, JSON 가져오기와 비밀 없는 내보내기",
          "비밀번호, 키 파일(OpenSSH·PKCS#8·PPK), SSH 에이전트, keyboard-interactive",
        ],
      },
      {
        title: "SFTP",
        items: [
          "탐색기 스타일 브라우저(체크박스 다중 선택)와 내 PC 폴더를 나란히 보는 로컬 창",
          "드래그 업로드·다운로드, 잘라내기·복사·붙여넣기, 서버 간 복사, 폴더 전체 기준 진행률",
          "원격 파일을 로컬 편집기(VS Code·Notepad++ 등 자동 감지)로 열고 저장하면 자동 업로드",
        ],
      },
      {
        title: "터널과 X11",
        items: [
          "-L, -R, -D(SOCKS5) 포워딩과 세션별 자동 시작",
          "ruxt.exe에 내장된 Cygwin/X 서버를 창 없이 무작위 쿠키로 띄우고, 켤 때마다 파일 무결성(SHA-256) 확인",
          "비신뢰(-X) 기본, 신뢰(-Y)는 세션별 선택",
        ],
      },
      {
        title: "잠금과 에이전트 가드",
        items: [
          "쓰지 않으면 창 전체를 잠그고 저장된 비밀을 메모리에서 지우는 화면 잠금",
          "에이전트 가드 (Windows, 기본 꺼짐): 창을 스크린샷·녹화·화면 공유에서 숨기고, 프로그램이 주입한 키보드·마우스 입력(AI 에이전트, 매크로)을 버림",
          "선택형 감사 로그, 명령어 기록·감사 로그 보관 기간",
        ],
      },
      {
        title: "서버 상태와 테마",
        items: [
          "CPU·메모리·디스크를 상태 표시줄의 미니 막대와 고정 폭 수치로 표시, 클릭하면 상세 창 (세션별 동의)",
          "기본 테마 12개(퍼플 그린 포함)와 색 편집기, 테마 JSON 내보내기·가져오기",
          "한국어·English UI, 처음 실행 둘러보기와 단축키 도움말",
        ],
      },
    ],
    screensTitle: "화면",
    screenExplorer: "SFTP 탐색기 (말차 테마, 한국어)",
    downloadTitle: "다운로드",
    downloadIntro:
      "GitHub 릴리스에서 받습니다. Windows용 ruxt.exe에는 X 서버가 내장되어 있어 따로 설치할 것이 없고, 모든 파일의 SHA-256이 SHA256SUMS에 있습니다.",
    latest: "최신 버전",
    latestUnknown: "릴리스 페이지에서 최신 버전을 확인하세요",
    assets: [
      { name: "Windows 설치 파일", note: "x64, X 서버 내장, Inno Setup", match: /-setup\.exe$/i },
      { name: "Windows 포터블", note: "x64 zip, X 서버 내장", match: /windows-x86_64\.zip$/i },
      { name: "Linux", note: "x86_64 tar.gz", match: /linux-x86_64\.tar\.gz$/i },
      { name: "macOS Apple Silicon", note: "arm64 tar.gz", match: /macos-aarch64\.tar\.gz$/i },
      { name: "macOS Intel", note: "x86_64 tar.gz", match: /macos-x86_64\.tar\.gz$/i },
    ],
    checksums: "SHA256SUMS",
    smartscreen:
      "코드 서명 인증서가 아직 없어 Windows SmartScreen 경고가 뜰 수 있습니다. SHA-256을 대조한 뒤 '추가 정보 → 실행'으로 진행하세요.",
    buildFromSource: "소스에서 빌드하기 (Rust 1.95+)",
    docsTitle: "문서",
    docs: [
      {
        title: "기능 설명",
        desc: "이 사이트의 문서. 설치, 세션, 터미널, SFTP, 터널, X11, 화면 잠금.",
        href: `${import.meta.env.BASE_URL}docs.html`,
      },
      { title: "사용자 가이드", desc: "저장소에 있는 같은 내용의 한국어 원문.", href: `${REPO}/blob/main/docs/USER_GUIDE.md` },
      { title: "위협 모델", desc: "무엇으로부터 무엇을 지키고, 무엇은 지키지 못하는지.", href: `${REPO}/blob/main/docs/THREAT_MODEL.md` },
      { title: "보안 정책", desc: "취약점을 비공개로 신고하는 방법과 응답 기한.", href: `${REPO}/blob/main/SECURITY.md` },
      { title: "제3자 구성 요소", desc: "동봉 Cygwin/X의 LGPL 고지와 소스 입수 방법.", href: `${REPO}/blob/main/docs/THIRD_PARTY.md` },
      { title: "변경 이력", desc: "버전별로 무엇이 바뀌었는지.", href: `${REPO}/blob/main/CHANGELOG.md` },
      { title: "빌드 가이드", desc: "OS별 툴체인과 빌드 절차.", href: `${REPO}/blob/main/BUILD.md` },
    ],
    footerLicense: "MIT 또는 Apache-2.0 중 선택.",
    footerBundle:
      "Windows용 ruxt.exe에 내장된 X 서버는 Cygwin/X(런타임 LGPLv3, X.Org MIT)입니다. 소스와 재빌드 절차는 제3자 구성 요소 문서에 있습니다.",
    footerHonest: "텔레메트리 없음. 외부로 나가는 연결은 사용자가 연 SSH 세션뿐입니다.",
  },
  en: {
    langLabel: "한국어",
    tagline: "The SSH and SFTP workstation that was designed security-first",
    lede:
      "Pinned host keys, untrusted X11 forwarding, passwords saved only under the screen-lock password and loopback-only tunnels are the defaults; you have to switch them off to be less careful. One pure-Rust binary for Windows, Linux and macOS.",
    ctaDownload: "Download for Windows",
    ctaSource: "View source on GitHub",
    statusNote:
      "1.0.0 is the first stable public release. The core paths are tested against an in-process SSH server and on CI for three operating systems, but the code has not had an independent security review yet. Read the threat model before using production credentials.",
    terminalTitle: "db-01.corp — Ruxt",
    terminalCaption:
      "A real session transcript. An unknown host key is stored only after you confirm its fingerprint, and a forwarded X program cannot read other windows.",
    defendsTitle: "What the defaults protect",
    defendsIntro:
      "What Ruxt blocks without any configuration. Each row maps to a row of the threat model document.",
    rows: [
      {
        threat: "A man in the middle posing as your server",
        defense:
          "Host keys are pinned on first use after you see the fingerprint and randomart. A changed key blocks the connection; it is replaced only after you confirm you verified the new one. Jump hosts are pinned per hop.",
      },
      {
        threat: "Downgrade to weak algorithms",
        defense:
          "Only ML-KEM-768+X25519 / X25519 key exchange, ChaCha20-Poly1305 and AES-GCM, SHA-2 MACs. No SHA-1, CBC, DSA, RC4 or 3DES in any profile; ssh-rsa only in an explicit Legacy profile.",
      },
      {
        threat: "Passwords leaking from disk",
        defense:
          "Passwords and passphrases are saved encrypted with the screen-lock password (Argon2id + XChaCha20-Poly1305), wiped from memory after authentication, and closed whenever Ruxt locks. Config files and session exports never contain secrets.",
      },
      {
        threat: "Forwarded X programs snooping on windows and keystrokes",
        defense:
          "Untrusted mode (ssh -X) is the default. The bundled Cygwin/X (X.Org 21.1.25) is rebuilt with the X SECURITY extension and forwarded programs connect with a confined cookie it issues. Trusted mode (-Y) is a per-session choice.",
      },
      {
        threat: "Clipboard injection and terminal escapes",
        defense:
          "Multi-line pastes ask first with a preview, control characters are stripped from pasted text, OSC 52 clipboard writes are off by default, and titles, banners and file names lose their control and bidi characters.",
      },
      {
        threat: "Tunnels exposed to the network",
        defense:
          "Local and SOCKS listeners bind loopback only and SOCKS5 accepts CONNECT alone. The server may open only the channels you asked for. Agent forwarding is off by default.",
      },
      {
        threat: "Hostile file names overwriting local files",
        defense:
          "Remote names are rejected before they become local paths if they carry separators, '..', drive prefixes, NTFS streams or reserved device names. Directory symlinks are never followed; transfers land in a temp file and are swapped in atomically.",
      },
      {
        threat: "Traces left on your own machine",
        defense:
          "Excluded from Windows Error Reporting crash dumps, optional clipboard auto-clear. Command history for completion is kept only with a screen-lock password and encrypted line by line; the audit log is off by default. No telemetry, no auto-update.",
      },
    ],
    featuresTitle: "What it does",
    groups: [
      {
        title: "Terminal",
        items: [
          "Alacritty engine, xterm-256color, scrollback search, URL Ctrl+click",
          "Box, block and Braille characters drawn to the cell, so TUI borders join in any font",
          "CJK IME composition, mouse reporting, bracketed paste, Ctrl+C/V copy and paste",
          "Completion of previously run commands as ghost text (→ to accept; history encrypted)",
          "Broadcast input: type into every SSH tab at once",
          "tail commands open in Tail Viewer with keyword watch, filter and logging to a file",
        ],
      },
      {
        title: "Sessions",
        items: [
          "Groups, tags, recent list, quick connect, jump hosts up to 4 hops",
          "Import from OpenSSH config, PuTTY and JSON; export without secrets",
          "Password, key files (OpenSSH, PKCS#8, PPK), SSH agent, keyboard-interactive",
        ],
      },
      {
        title: "SFTP",
        items: [
          "Explorer-style browser with checkbox selection and a local pane beside the remote one",
          "Drag to upload and download, cut/copy/paste, server-side copy, whole-folder progress",
          "Edit remote files in a local editor (VS Code, Notepad++ and others detected) with auto-upload on save",
        ],
      },
      {
        title: "Tunnels and X11",
        items: [
          "-L, -R and -D (SOCKS5) with per-session auto start",
          "Cygwin/X built into ruxt.exe, started without a window with a random cookie, file integrity (SHA-256) checked on every start",
          "Untrusted (-X) by default, trusted (-Y) per session",
        ],
      },
      {
        title: "Lock and agent guard",
        items: [
          "Screen lock after idle time that also wipes saved secrets from memory",
          "Agent guard (Windows, off by default): hides the window from screenshots, recording and screen sharing, and drops keyboard and mouse input injected by programs (AI agents, macros)",
          "Optional audit log; retention limits for command history and the audit log",
        ],
      },
      {
        title: "Server status and themes",
        items: [
          "CPU, memory and disk as mini bars and fixed-width percents in the status bar; details on click (per-session opt-in)",
          "12 built-in themes, including Purple Green, plus a colour editor; themes export and import as JSON",
          "Korean and English UI, a first-run tour and a shortcut reference",
        ],
      },
    ],
    screensTitle: "Screens",
    screenExplorer: "SFTP explorer (Matcha theme, Korean UI)",
    downloadTitle: "Download",
    downloadIntro:
      "Releases are published on GitHub. On Windows the X server is built into ruxt.exe, so there is nothing else to install, and SHA256SUMS lists every file.",
    latest: "Latest release",
    latestUnknown: "See the releases page for the latest version",
    assets: [
      { name: "Windows installer", note: "x64, X server built in, Inno Setup", match: /-setup\.exe$/i },
      { name: "Windows portable", note: "x64 zip, X server built in", match: /windows-x86_64\.zip$/i },
      { name: "Linux", note: "x86_64 tar.gz", match: /linux-x86_64\.tar\.gz$/i },
      { name: "macOS Apple Silicon", note: "arm64 tar.gz", match: /macos-aarch64\.tar\.gz$/i },
      { name: "macOS Intel", note: "x86_64 tar.gz", match: /macos-x86_64\.tar\.gz$/i },
    ],
    checksums: "SHA256SUMS",
    smartscreen:
      "There is no code-signing certificate yet, so Windows SmartScreen may warn. Compare the SHA-256, then choose More info → Run anyway.",
    buildFromSource: "Build from source (Rust 1.95+)",
    docsTitle: "Documentation",
    docs: [
      {
        title: "Documentation",
        desc: "On this site, in English by default: install, sessions, the terminal, SFTP, tunnels, X11, the screen lock.",
        href: `${import.meta.env.BASE_URL}docs.html`,
      },
      { title: "User guide (Korean)", desc: "The same material as a Markdown file in the repository.", href: `${REPO}/blob/main/docs/USER_GUIDE.md` },
      { title: "Threat model (Korean)", desc: "What is protected from what, and what is not.", href: `${REPO}/blob/main/docs/THREAT_MODEL.md` },
      { title: "Security policy", desc: "How to report a vulnerability privately, and response times.", href: `${REPO}/blob/main/SECURITY.md` },
      { title: "Third-party components", desc: "LGPL notice for the bundled Cygwin/X and where its source is.", href: `${REPO}/blob/main/docs/THIRD_PARTY.md` },
      { title: "Changelog", desc: "What changed in each version.", href: `${REPO}/blob/main/CHANGELOG.md` },
      { title: "Build guide (Korean)", desc: "Toolchains and build steps per OS.", href: `${REPO}/blob/main/BUILD.md` },
    ],
    footerLicense: "MIT or Apache-2.0, at your option.",
    footerBundle:
      "The X server built into ruxt.exe on Windows is Cygwin/X (runtime under LGPLv3, X.Org under MIT). Source and the rebuild recipe are in the third-party components document.",
    footerHonest: "No telemetry. The only outbound connections are the SSH sessions you open.",
  },
};

/** Lines of the hero transcript. `kind` picks the colour; `delay` is the pause before the line. */
export const transcript: { kind: "local" | "prompt" | "shell" | "err" | "warn"; text: string; delay: number }[] = [
  { kind: "local", text: "[Ruxt] Connecting to deploy@db-01.corp:22 ...", delay: 300 },
  { kind: "prompt", text: "The authenticity of host db-01.corp:22 can't be established.", delay: 700 },
  { kind: "prompt", text: "ssh-ed25519 key fingerprint: SHA256:9Xq2bqNUsnl5Ul5ZK7F0p3KkR8uVfQ2mE1xAo6Sg0bE", delay: 250 },
  { kind: "prompt", text: "▸ Trust and connect   ▸ Cancel", delay: 900 },
  { kind: "local", text: "[Ruxt] Authenticated (publickey (id_ed25519)). Opening shell...", delay: 500 },
  { kind: "local", text: "[Ruxt] X11: untrusted forwarding (-X): remote X clients are confined by the X SECURITY extension.", delay: 350 },
  { kind: "shell", text: "deploy@db-01:~$ xwd -root -out /tmp/shot.xwd", delay: 900 },
  { kind: "err", text: "X Error of failed request:  BadAccess (attempt to access private resource denied)", delay: 400 },
  { kind: "shell", text: "deploy@db-01:~$ ", delay: 500 },
];

export const statusLine = "connected   135×41   CPU 7%   MEM 41%   disk 46%";
