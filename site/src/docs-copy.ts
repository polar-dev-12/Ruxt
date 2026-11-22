import { REPO, type Lang } from "./i18n";

/** A short run of prose. Backticks become code; `[[label|url]]` becomes a link. */
export type Block =
  | { kind: "p"; text: string }
  | { kind: "ul"; items: string[] }
  | { kind: "table"; headers: string[]; rows: string[][] }
  | { kind: "note"; text: string };

export type DocsSection = { id: string; title: string; blocks: Block[] };

export interface DocsCopy {
  pageTitle: string;
  lede: string;
  onThisPage: string;
  langLabel: string;
  home: string;
  download: string;
  alsoTitle: string;
  also: { title: string; href: string }[];
  sections: DocsSection[];
}

const THREAT = `${REPO}/blob/main/docs/THREAT_MODEL.md`;
const SECURITY = `${REPO}/blob/main/SECURITY.md`;
const THIRD = `${REPO}/blob/main/docs/THIRD_PARTY.md`;
const CHANGELOG = `${REPO}/blob/main/CHANGELOG.md`;
const BUILD = `${REPO}/blob/main/BUILD.md`;
const GUIDE = `${REPO}/blob/main/docs/USER_GUIDE.md`;

const en: DocsCopy = {
  pageTitle: "Documentation",
  lede:
    "How Ruxt works, from the first launch through sessions, the terminal, files, tunnels and the screen lock. The English text is the default. The same page is available in Korean from the language button.",
  onThisPage: "On this page",
  langLabel: "한국어",
  home: "Home",
  download: "Download",
  alsoTitle: "Other documents",
  also: [
    { title: "Threat model (Korean)", href: THREAT },
    { title: "Security policy", href: SECURITY },
    { title: "Third-party components", href: THIRD },
    { title: "Changelog", href: CHANGELOG },
    { title: "Build guide (Korean)", href: BUILD },
    { title: "User guide in the repository (Korean)", href: GUIDE },
  ],
  sections: [
    {
      id: "overview",
      title: "Overview",
      blocks: [
        {
          kind: "p",
          text: "Ruxt is an SSH and SFTP workstation in one pure-Rust binary. It runs on Windows, Linux and macOS. The window is drawn with egui, the terminal is the Alacritty engine, and SSH is russh.",
        },
        {
          kind: "p",
          text: "The defaults are the strict ones. Host keys are pinned, only modern algorithms are offered, X11 forwarding is untrusted, tunnels listen on loopback, and a password is saved only when a screen-lock password encrypts it. You turn a protection off when you want the looser behaviour. The binary is `#![forbid(unsafe_code)]`. There is no telemetry and no auto-update. The only outbound connections are the SSH sessions you open.",
        },
        {
          kind: "note",
          text: "1.0.0 is the first stable public release. Core paths are tested against an in-process SSH server and on CI for three operating systems. There has not been an independent security review. Read the [[threat model|{{threat}}]] before using production credentials, and report vulnerabilities through the [[security policy|{{security}}]].",
        },
      ],
    },
    {
      id: "install",
      title: "Install and first launch",
      blocks: [
        {
          kind: "p",
          text: "Downloads are on the GitHub releases page, with a `SHA256SUMS` file for every archive. On Windows the X server is inside `ruxt.exe`, so there is no separate X package to install.",
        },
        {
          kind: "table",
          headers: ["Platform", "What to run"],
          rows: [
            ["Windows", "The installer `ruxt-<version>-setup.exe`, or the portable zip `ruxt-<version>-windows-x86_64.zip`. No extra DLLs."],
            ["Linux", "`ruxt-<version>-linux-x86_64.tar.gz`, then `./ruxt`. X11 and Wayland both work."],
            ["macOS", "`ruxt-<version>-macos-aarch64.tar.gz` on Apple silicon, or `…-macos-x86_64.tar.gz` on Intel."],
            ["From source", "`cargo build --release` with Rust 1.95 or newer. See the [[build guide|{{build}}]]."],
          ],
        },
        {
          kind: "p",
          text: "There is no code-signing certificate yet, so Windows SmartScreen may warn. Compare the SHA-256, then choose More info → Run anyway.",
        },
        {
          kind: "p",
          text: "The first launch asks whether to protect Ruxt with a password. That password is the screen lock and the key for saved secrets. A six-slide tour follows: connecting, files and tunnels, terminal tools, security settings, and shortcuts. Close it and it stays closed. The toolbar `?` menu opens it again with Show the tour again. Right-click the toolbar to show or hide each button. Hiding every button leaves that same menu, which can turn them back on. The interface language is Korean or English, under Settings → Appearance.",
        },
      ],
    },
    {
      id: "sessions",
      title: "Sessions and connecting",
      blocks: [
        {
          kind: "p",
          text: "The toolbar field takes `user@host:port` and connects immediately. A quick-connect session is not saved, and its password cannot be saved either. Keep a server you use often as a session.",
        },
        {
          kind: "p",
          text: "+ New session opens the editor.",
        },
        {
          kind: "table",
          headers: ["Field", "What it does"],
          rows: [
            ["Name, group, tags", "Shown in the sidebar and used by search. Tags are comma-separated."],
            ["Host, port, username", "The target. An empty username uses the account name of this computer."],
            ["Authentication", "Auto (agent, then default key files, then password, then keyboard-interactive), password, a public-key file (OpenSSH, PKCS#8 or PuTTY PPK), the SSH agent, or keyboard-interactive for OTP and 2FA."],
            ["Jump host", "Connect through another saved session, as OpenSSH `ProxyJump` does. Each hop uses that session's host key, authentication and saved password. Chains stop at four hops."],
            ["Security profile", "Strict (the default) offers a post-quantum hybrid key exchange and AEAD ciphers only. Compatible adds Diffie–Hellman group exchange and CTR ciphers for older servers. Legacy is the only profile that allows `ssh-rsa` (SHA-1 signatures). SHA-1 and CBC are absent from every profile."],
            ["Startup command", "Run once the shell is open."],
            ["Forwarding", "X11, agent forwarding (shown with a warning), and tunnels (`-L`, `-R`, `-D`)."],
            ["Saved password", "Stores the password or key passphrase when a screen-lock password exists."],
          ],
        },
        {
          kind: "p",
          text: "Open a session by double-clicking it, or by right-click → Connect. Sessions you opened recently collect under Recent. The search field matches name, host, group and tags.",
        },
        {
          kind: "p",
          text: "Import, above the session list, reads an OpenSSH config (`~/.ssh/config`), PuTTY saved sessions (the Windows registry, or `~/.putty/sessions`), or a Ruxt JSON export from another computer. OpenSSH `ProxyJump`, `IdentityFile`, local, remote and dynamic forwards, `ForwardAgent`, `ForwardX11`, `Compression` and `ServerAliveInterval` are copied. Wildcard hosts and `Match` blocks are skipped. PuTTY does not store passwords, so none are imported. A Ruxt export never contains passwords or passphrases. A session that already has the same name and address is skipped. A colliding id is replaced with a new one.",
        },
      ],
    },
    {
      id: "host-keys",
      title: "Host keys",
      blocks: [
        {
          kind: "p",
          text: "The first connection to a server opens an unknown-host-key dialog. Compare the SHA-256 fingerprint and the randomart with the value your administrator published, then choose Trust and connect. The key is stored and later connections do not ask again.",
        },
        {
          kind: "p",
          text: "A changed key blocks the connection. Replace it only when you know the server was reinstalled, and only after you tick the confirmation. If the change might be a man-in-the-middle, cancel and ask the administrator. Jump hosts are pinned separately, one key per hop.",
        },
      ],
    },
    {
      id: "terminal",
      title: "Terminal",
      blocks: [
        {
          kind: "p",
          text: "Each SSH session and the local shell is an `xterm-256color` terminal. Box-drawing, block and Braille characters are painted to the cell, so TUI borders join in any font. Symbols missing from the chosen font fall back to the operating system's symbol font. CJK input shows the composition string. Mouse reporting and bracketed paste are enabled.",
        },
        {
          kind: "table",
          headers: ["Action", "Keys"],
          rows: [
            ["Copy", "`Ctrl+C` when text is selected, `Ctrl+Shift+C`, `Ctrl+Insert`"],
            ["Paste", "`Ctrl+V`, `Ctrl+Shift+V`, `Shift+Insert`, right or middle click"],
            ["Interrupt", "`Ctrl+C` with nothing selected. The key goes to the remote program."],
            ["Find in scrollback", "`Ctrl+Shift+F`. `Enter` and `Shift+Enter` move between hits."],
            ["Open a link", "`Ctrl`+click"],
            ["Scrollback", "`Shift+PgUp`, `PgDn`, `Home`, `End`, and the wheel"],
            ["Font size", "`Ctrl`+wheel, `Ctrl` plus `+` or `-`"],
            ["Reconnect", "`Enter` after the session has closed"],
            ["Command completion", "`→` accepts, `Ctrl+↑` / `Ctrl+↓` chooses, `Esc` hides"],
          ],
        },
        {
          kind: "p",
          text: "A paste that contains a newline asks for confirmation and shows a preview. That prompt can be turned off in Settings. Control characters such as ESC are removed from pasted text. Text fields outside the terminal (the session editor, quick connect, the SFTP path) keep ordinary `Ctrl+C`, `Ctrl+V` and `Ctrl+X`.",
        },
        {
          kind: "p",
          text: "Broadcast input, on the toolbar, sends what you type to every open SSH tab. The local shell is a separate tab. Its program is chosen in Settings → Terminal, or detected automatically when that field is empty.",
        },
      ],
    },
    {
      id: "completion",
      title: "Command completion",
      blocks: [
        {
          kind: "p",
          text: "Completion runs only after you have set a screen-lock password. Each command you submit with Enter is encrypted into `history.db`. The next time you type the same prefix, the rest of the line appears in faint type after the cursor. `→` fills it in, which types those characters into the shell. Several matches open a list under the cursor. Commands used on this server come first, then frequent and recent ones.",
        },
        {
          kind: "ul",
          items: [
            "Only characters that were actually echoed are stored. A password prompt that does not echo is not recorded. Lines that look like secrets (`password=`, `token=`, `Authorization:`, `XXX_SECRET=`) are skipped.",
            "A command that starts with a space is not stored, as with `HISTCONTROL=ignorespace`.",
            "Completion is inactive inside a full-screen program such as vim.",
            "Each line is encrypted with a key that the screen-lock password opens. History is recorded and suggested only while Ruxt is unlocked, and it is wiped from memory on lock. The file keeps a random row id and a last-used time in the clear, so old rows can be expired.",
            "Plaintext history left by an older version is encrypted the first time you unlock, and the plaintext is deleted. If you never set a screen-lock password, that plaintext remains until you use Clear command history.",
            "Settings → Command completion turns suggestions off, caps how many commands are kept, or deletes the history.",
          ],
        },
      ],
    },
    {
      id: "server-status",
      title: "Server status",
      blocks: [
        {
          kind: "p",
          text: "A connected SSH tab can show CPU, memory and the first disk in the status bar. Each one is a dim label, a thin bar and a fixed-width percent. The bar stays in the theme colour below 60%, turns orange from 60% and red from 85%. Click a meter, or Server info when the monitor is off, to open the details window. Load, swap, users and processes are in that window, not on the bar.",
        },
        {
          kind: "ul",
          items: [
            "Connection: address, security profile, jump host, forwarding (X11 trusted or untrusted, agent, tunnel count), state, authentication method, and the host-key fingerprint with a copy button.",
            "Status: hostname, OS and kernel, uptime, load and CPU count, bars and sparklines for CPU, memory and swap, per-disk usage, and user and process counts.",
          ],
        },
        {
          kind: "p",
          text: "The monitor runs read-only commands on the existing SSH session (`hostname`, `uptime`, `/proc/stat`, `/proc/meminfo`, `df`, and similar). It does not log in again. macOS and BSD, which have no `/proc`, show only some of the fields. A server that refuses a shell is detected and that session's monitor stops. The commands can appear in the server's own audit log, so the per-session switch in the session editor is off by default. The window can turn monitoring off or change the interval for that session. Settings → Server status is the master switch and the default interval (2 to 120 seconds) used when a session does not set its own.",
        },
      ],
    },
    {
      id: "tailviewer",
      title: "Tail Viewer",
      blocks: [
        {
          kind: "p",
          text: "Typing a standalone `tail` of a file, such as `tail -f -n 100 app.log`, opens a log window and leaves the terminal prompt free for the next command. A pipe, a redirection, `;`, `&&` or command substitution is not intercepted. The command then runs in the shell as usual.",
        },
        {
          kind: "p",
          text: "On an SSH tab the follow runs on a separate channel of the same connection, in the terminal's current directory. On a local-shell tab Ruxt reads the file itself. Closing the window or pressing Stop ends the remote `tail`. The SFTP browser can open the same window from a file's right-click menu, Open in Tail Viewer.",
        },
        {
          kind: "ul",
          items: [
            "Pause freezes the view and queues lines that arrive meanwhile. Follow, wrap, filter (optional case match, matching lines only), copy all, clear and restart are on the toolbar. The window keeps the latest 50,000 lines.",
            "Watch takes several keywords, separated by commas or semicolons (or by spaces when there is no comma). Match any keyword (OR) or all of them (AND). A hit flashes the window for three seconds, highlights the line, and counts on the button. Jump on hit scrolls to the new line even when Follow is off. Watched lines only hides the rest.",
            "Matched lines can be appended to a `.txt` file as they arrive, with a timestamp prefix or as the raw line. Ruxt does not manage that file. Choose the folder carefully if the log is sensitive.",
            "The window opens as its own OS window. Dock puts it inside the Ruxt window, and New window takes it back out. Settings → Terminal turns the interception off, and `tail` then stays in the terminal.",
          ],
        },
      ],
    },
    {
      id: "sftp",
      title: "SFTP",
      blocks: [
        {
          kind: "p",
          text: "SFTP on the toolbar opens an explorer on the right. It follows the same habits as a file manager.",
        },
        {
          kind: "ul",
          items: [
            "Move with back, forward and up, by double-clicking a folder, from the folder tree, or by clicking a segment of the path. Click the empty part of the path bar to type a path. `~` is accepted. A `..` row at the top of the list goes to the parent.",
            "View → Follow the terminal's working directory tracks `cd` (from OSC 7 or the prompt title). View → Hidden items shows names that start with a dot. That choice is saved per session.",
            "Select with a click, `Ctrl`+click, `Shift`+click, `Ctrl+A`, or the checkbox on each row. The header checkbox selects everything visible. Click empty space to clear.",
            "Keys: `Backspace` up, `Enter` open, `F2` rename, `Delete` delete, `F5` refresh, `Alt+Left` / `Alt+Right` for history, arrows to move.",
            "Click a column header to sort by name, modified time, type or size. Folders stay first.",
            "Upload from the Upload menu or by dropping files from the desktop. A name collision asks before overwrite. The transfer is written under a temporary name and swapped in, so a failure leaves the original.",
            "Download asks for a folder unless the local pane is open, in which case files land in that pane's current folder.",
            "View → Local pane shows a folder on this computer beside the remote list. Drag from the local pane to the remote list to upload, and the other way to download. The Drives menu jumps to Desktop, Documents, Downloads or a drive.",
            "Drag a remote item onto a folder row or a tree node to move it. Cut, copy and paste are on the right-click menu and on `Ctrl+X`, `Ctrl+C`, `Ctrl+V`. A copy in the same folder is named `name - copy`. SFTP has no copy command, so a copy travels through the client even though it stays on the server.",
            "Double-click or Edit opens the file in the external editor (Settings → Terminal; empty means the operating system's default). Each save uploads it again. If the server copy changed, Ruxt asks before overwriting. With no editor configured, executables and scripts are not opened, because opening them might run them.",
            "Permissions on a single item takes an octal mode such as `755`. Delete is permanent and asks first. A directory symlink is removed as a link. The target is not followed.",
            "The transfer list under the browser starts collapsed, showing how many of the transfers have finished. Click that line, or drag its top edge up, to open the list, where each transfer shows progress and can be cancelled. At most four transfers run at once. A folder transfer counts files first (shown as counting, and capped at 50,000 files) and then reports files and bytes against that total.",
          ],
        },
      ],
    },
    {
      id: "tunnels",
      title: "Tunnels",
      blocks: [
        {
          kind: "p",
          text: "Add a tunnel in the session editor or in the Tunnels panel. A session can start its tunnels when it connects.",
        },
        {
          kind: "table",
          headers: ["Kind", "Meaning", "Example"],
          rows: [
            ["Local (`-L`)", "A port on this computer, through the server, to a target host and port", "`127.0.0.1:8080 -> 10.0.0.5:80`"],
            ["Remote (`-R`)", "A port on the server, through this computer, to a target", "`127.0.0.1:9000 <- 127.0.0.1:3000`"],
            ["Dynamic (`-D`)", "A SOCKS5 proxy on this computer", "`127.0.0.1:1080`"],
          ],
        },
        {
          kind: "p",
          text: "Local and dynamic listeners bind to loopback unless you allow other addresses under Settings → Security and change the bind address. SOCKS5 accepts CONNECT only. The server may open only the forwarded channels you asked for. Agent forwarding is off unless you enable it on that session.",
        },
      ],
    },
    {
      id: "x11",
      title: "X11 forwarding",
      blocks: [
        {
          kind: "p",
          text: "A remote graphical program needs a local X server. On Windows, Cygwin/X (`XWin.exe`, X.Org 21.1.25) is packed inside `ruxt.exe`. The first start unpacks it under `%LOCALAPPDATA%\\ruxt\\data\\xserver` and later starts check that the files still match (a change is unpacked again). An `XWin.exe` path in Settings, or an `xserver` folder beside `ruxt.exe`, is used first. VcXsrv and Xming are not supported. Linux and macOS use the X server already on the machine.",
        },
        {
          kind: "p",
          text: "Ruxt starts the server with a random authentication cookie, and only when an X11 session connects or you turn it on. It does not start just because Ruxt starts (that switch is off by default). Just before it listens, Ruxt asks once, with a UAC prompt, for a Windows Firewall inbound block rule. Declining still starts the server, and the status menu keeps the warning. The status bar shows the state: a click starts or stops the server Ruxt owns, and a right-click opens refresh, the firewall rule and the download link. The real cookie is not given to the remote program.",
        },
        {
          kind: "p",
          text: "The default is untrusted mode, the same idea as `ssh -X`. Ruxt asks the X SECURITY extension for an untrusted cookie, so the remote program can touch its own windows and cannot read other windows or grab the keyboard. The bundled `XWin.exe` is built with that extension enabled. The stock Cygwin package does not include it. If a toolkit fails in untrusted mode (a BadAccess error), turn on trusted mode (`-Y`) in the session editor. That is `ssh -Y`: the remote program can see the whole local display, so use it only on a server you trust. If an untrusted cookie cannot be created, for example because a different `XWin.exe` has no SECURITY extension, forwarding is refused and the terminal says why. An untrusted authorisation expires seven days after the last X program exits. A very long-lived session that starts being refused should reconnect.",
        },
        {
          kind: "p",
          text: "The X.Org server has no loopback-only option, so TCP port 6000+n is visible on the network. The cookie is still required. The Windows installer adds an inbound block rule for `XWin.exe` when it is installed as administrator. The Windows firewall does not filter loopback, so forwarding keeps working and the port is closed to the LAN. Without the rule, right-click the status-bar label: it warns and offers Add firewall block rule.",
        },
      ],
    },
    {
      id: "lock",
      title: "Screen lock and saved passwords",
      blocks: [
        {
          kind: "p",
          text: "One password locks the window and encrypts saved session passwords and key passphrases. There is no separate master password.",
        },
        {
          kind: "p",
          text: "If you set it on first launch, the window locks after 10 minutes of idle. Later asks for the password before you can see the sessions again. Choosing Later leaves the lock off and does not ask again. Nothing can be saved until you set a password under Settings → Security → Screen lock.",
        },
        {
          kind: "ul",
          items: [
            "With a screen-lock password, Ruxt starts locked. Unlocking opens the saved passwords as well.",
            "Remember (encrypted with the screen-lock password) on a password prompt stores it, for a saved session only. The session editor can set or clear the saved password too.",
            "A saved password is tried quietly on connect. A server that authenticates with a key file or the agent has nothing to store unless the key itself has a passphrase.",
            "The idle time is set in steps of 10 minutes. Zero disables automatic locking. The toolbar Lock button still locks immediately.",
            "SSH sessions and transfers keep running while the window is locked. They are only hidden. Saved passwords are wiped from memory and opened again on the next unlock.",
            "Disk holds an Argon2id verifier (`applock.bin`) and ciphertext (`vault.bin`, Argon2id + XChaCha20-Poly1305). Each wrong password delays the next check by one more second, up to five.",
            "The same password encrypts command history.",
            "Changing the password re-encrypts saved passwords. Removing the lock deletes saved passwords and command history.",
            "A forgotten password cannot be recovered. Quit Ruxt and delete `applock.bin` and `vault.bin` in the config folder. The lock goes away and the saved passwords are gone.",
          ],
        },
        {
          kind: "p",
          text: "An older version stored passwords under a separate vault master password. The first time you unlock with a screen-lock password, or the first time you set one, Ruxt asks for that old password once and re-encrypts. Later does not discard it. Settings → Security → Move saved passwords… asks again. If the old password is forgotten, Delete previous passwords starts clean.",
        },
      ],
    },
    {
      id: "agent-guard",
      title: "Agent guard",
      blocks: [
        {
          kind: "p",
          text: "On Windows, Agent guard is a toolbar toggle and a switch under Settings → Security. It is off by default. While it is on, the Ruxt windows are hidden from screenshots, recording and screen sharing, and keyboard or mouse input injected by another program (an AI agent, a macro) is dropped. If a screen-lock password exists, that injected input also locks the window. The input hook lives in a separate crate so the Ruxt binary itself stays `forbid(unsafe_code)`.",
        },
      ],
    },
    {
      id: "themes",
      title: "Themes",
      blocks: [
        {
          kind: "p",
          text: "Settings → Appearance lists twelve built-in themes. Dark: Matcha (the default), Purple Green, Harbor, Ember, Ink, Pine. Light: Light, Porcelain, Sand, Mint, Rose, Lavender. Purple Green is deep purple surfaces with a green accent.",
        },
        {
          kind: "p",
          text: "New theme… opens an editor that starts from the colours in use. The left column is the interface (panels, windows, fields, striped rows, three button states, text, secondary text, accent, selection, links, warning, error, borders). The right column is the terminal (text, background, cursor, and the 16 ANSI colours). A change is previewed immediately. Cancel restores the previous theme. A named theme is stored with the settings, up to 32, and chosen from the custom list.",
        },
        {
          kind: "ul",
          items: [
            "Export writes `name.ruxt-theme.json`. Colours are `#rrggbb` strings, so the file can be edited by hand. A file that sets only some colours is valid. The rest come from Matcha.",
            "Import adds the file to the list and applies it. A name that is already used gets a ` (2)` suffix.",
            "Start from, inside the editor, loads any built-in. The theme's name stays and the colours change.",
          ],
        },
        {
          kind: "p",
          text: "UI scale runs from 75% to 200%. Menus and dialogs have their own font size. The terminal font is chosen separately and can be any monospace family installed on the computer.",
        },
      ],
    },
    {
      id: "settings",
      title: "Settings",
      blocks: [
        {
          kind: "table",
          headers: ["Area", "What is there"],
          rows: [
            ["Graphics", "Renderer (OpenGL or wgpu), hardware acceleration (preferred, required, or off), VSync. Applied on the next start. Try wgpu when a remote desktop or a virtual machine draws slowly or incorrectly. `RUXT_RENDERER=wgpu` selects it too."],
            ["Terminal", "Font and size, local shell, copy on select, open `tail` in Tail Viewer (on by default), external editor. The editor list detects VS Code, Cursor, VSCodium, Zed, Sublime Text, Notepad++ and Notepad, and still accepts a path."],
            ["X server", "Executable, start when a forwarding session connects, start the bundled server when Ruxt starts (off by default, no console window), stop it on exit, clipboard sharing (off by default). A firewall block rule is requested the first time the server starts. Trusted mode is per session, in the session editor."],
            ["Security", "Confirm a multi-line paste, allow OSC 52, allow a bind address other than loopback, confirm closing a live session, clear the clipboard after N seconds, disconnect every session after N idle minutes, write an audit log (off by default), retention for command history and the audit log, and the screen lock."],
          ],
        },
        {
          kind: "p",
          text: "Dialogs stay inside the Ruxt window. A long one scrolls and keeps its buttons in view, and the corner resizes it. The confirming action is a filled pill, a secondary action is a ghost button (no border; a tint appears on hover), and cancel is a text button. A confirmation that cannot be undone uses the error colour. The main window separates the terminal, the bars and the active tab by surface tone rather than a 1 px border.",
        },
      ],
    },
    {
      id: "files",
      title: "Files on disk",
      blocks: [
        {
          kind: "table",
          headers: ["File", "Windows", "Linux and macOS"],
          rows: [
            ["Settings and sessions", "`%APPDATA%\\ruxt\\config\\config.json`", "`~/.config/ruxt/config.json`"],
            ["Saved passwords", "`%APPDATA%\\ruxt\\config\\vault.bin`", "`~/.config/ruxt/vault.bin`"],
            ["Screen-lock verifier", "`%APPDATA%\\ruxt\\config\\applock.bin`", "`~/.config/ruxt/applock.bin`"],
            ["Known hosts", "`%APPDATA%\\ruxt\\config\\known_hosts`", "`~/.config/ruxt/known_hosts`"],
            ["Audit log", "`%LOCALAPPDATA%\\ruxt\\data\\audit.log`", "`~/.local/share/ruxt/audit.log`"],
            ["Command history", "`%LOCALAPPDATA%\\ruxt\\data\\history.db`", "`~/.local/share/ruxt/history.db`"],
            ["Remote-edit copies", "`%LOCALAPPDATA%\\ruxt\\data\\edit\\`", "`~/.local/share/ruxt/edit/`"],
          ],
        },
        {
          kind: "p",
          text: "Files are created so that only the owner can read them. A damaged config is kept aside as `config.json.bad-<time>`. The audit log is JSON Lines of connections, authentication, host keys, tunnels, transfers and lock events, and it contains no secrets. It is still plaintext, as is `config.json`, so a lost disk should be protected with BitLocker, FileVault or LUKS. Command history is encrypted with the screen-lock password. The audit log is off until you enable it.",
        },
      ],
    },
    {
      id: "troubleshooting",
      title: "Troubleshooting",
      blocks: [
        {
          kind: "table",
          headers: ["What you see", "What to check"],
          rows: [
            ["connection timed out", "Host, port and firewall. The timeout pauses while a host-key dialog is open."],
            ["Authentication failed", "Pick an authentication method explicitly. Key files must be OpenSSH, PKCS#8 or PPK. An encrypted key asks for its passphrase. The agent is the Windows OpenSSH agent pipe, or `SSH_AUTH_SOCK`."],
            ["Key-exchange failure on an old server", "Switch the security profile to Compatible. If the server only signs host keys with `ssh-rsa` (SHA-1), use Legacy. That is typical of OpenSSH before 7.2."],
            ["An X11 program dies with BadAccess, or untrusted mode is unavailable", "Turn on trusted mode (`-Y`) in the session editor, and only for a server you trust. An X server without the SECURITY extension reports the same thing."],
            ["No command suggestions", "Command completion must be on, you must be outside a full-screen program, and the command must have been run before."],
            ["No server status in the status bar", "The server refused exec (a restricted shell, or a Windows server), or Settings → Server status is off. The details window has Retry."],
            ["Korean text is a box", "Settings → Terminal → Font, and pick a monospace font that includes Hangul. The rest of the UI picks a system Korean font on its own."],
            ["The window is slow or torn", "Settings → Graphics, set the renderer to wgpu or hardware acceleration to off, then restart."],
            ["An X11 program does not appear", "Check the X server label on the status bar. If the SSH server has `X11Forwarding no`, the terminal says the request was refused."],
          ],
        },
      ],
    },
    {
      id: "limits",
      title: "What Ruxt does not do",
      blocks: [
        {
          kind: "p",
          text: "There is no RDP, VNC, Telnet, serial console or bundled Unix toolbox. In exchange the same binary runs on Windows, Linux and macOS, and the source is open. Ruxt is an independent project and is not affiliated with MobaXterm. MobaXterm is a trademark of Mobatek.",
        },
        {
          kind: "p",
          text: "MobaXterm session files are not imported. Import an OpenSSH config or PuTTY saved sessions instead. MultiExec in MobaXterm corresponds to Broadcast input. MobaKeyGen corresponds to Keygen on the toolbar, which writes an Ed25519 key pair.",
        },
      ],
    },
  ],
};

const ko: DocsCopy = {
  pageTitle: "문서",
  lede:
    "첫 실행부터 세션, 터미널, 파일, 터널, 화면 잠금까지 Ruxt가 하는 일을 설명합니다. 언어를 고르지 않으면 영어로 열리고, 이 버튼으로 한국어를 볼 수 있습니다.",
  onThisPage: "이 페이지에서",
  langLabel: "English",
  home: "홈",
  download: "다운로드",
  alsoTitle: "다른 문서",
  also: [
    { title: "위협 모델", href: THREAT },
    { title: "보안 정책", href: SECURITY },
    { title: "제3자 구성 요소", href: THIRD },
    { title: "변경 이력", href: CHANGELOG },
    { title: "빌드 가이드", href: BUILD },
    { title: "저장소의 사용자 가이드", href: GUIDE },
  ],
  sections: [
    {
      id: "overview",
      title: "개요",
      blocks: [
        {
          kind: "p",
          text: "Ruxt는 SSH와 SFTP를 한 창에서 다루는 순수 Rust 프로그램입니다. Windows, Linux, macOS에서 같은 실행 파일로 동작합니다. 창은 egui, 터미널은 Alacritty 엔진, SSH는 russh입니다.",
        },
        {
          kind: "p",
          text: "기본값은 엄격한 쪽입니다. 호스트 키를 고정하고, 최신 알고리즘만 제안하고, X11 포워딩은 비신뢰 모드이며, 터널은 루프백에만 열고, 비밀번호는 화면 잠금 비밀번호로 암호화할 수 있을 때만 저장합니다. 느슨하게 쓰려면 해당 보호를 직접 꺼야 합니다. 바이너리 전체는 `#![forbid(unsafe_code)]`입니다. 텔레메트리와 자동 업데이트는 없습니다. 밖으로 나가는 연결은 사용자가 연 SSH 세션뿐입니다.",
        },
        {
          kind: "note",
          text: "1.0.0은 첫 안정 공개 버전입니다. 핵심 경로는 인프로세스 SSH 서버 테스트와 세 운영체제의 CI로 확인했지만, 독립 보안 리뷰는 아직 없습니다. 운영 자격증명을 쓰기 전에 [[위협 모델|{{threat}}]]을 읽고, 취약점은 [[보안 정책|{{security}}]]으로 알려 주세요.",
        },
      ],
    },
    {
      id: "install",
      title: "설치와 첫 실행",
      blocks: [
        {
          kind: "p",
          text: "GitHub 릴리스 페이지에서 받고, 모든 압축 파일의 해시는 `SHA256SUMS`에 있습니다. Windows용 `ruxt.exe`에는 X 서버가 들어 있어 따로 설치할 패키지가 없습니다.",
        },
        {
          kind: "table",
          headers: ["플랫폼", "실행 방법"],
          rows: [
            ["Windows", "설치 파일 `ruxt-<버전>-setup.exe`, 또는 포터블 zip `ruxt-<버전>-windows-x86_64.zip`. 추가 DLL은 필요 없습니다."],
            ["Linux", "`ruxt-<버전>-linux-x86_64.tar.gz`를 풀고 `./ruxt`. X11과 Wayland를 모두 지원합니다."],
            ["macOS", "Apple Silicon은 `ruxt-<버전>-macos-aarch64.tar.gz`, Intel은 `…-macos-x86_64.tar.gz`."],
            ["소스 빌드", "Rust 1.95 이상에서 `cargo build --release`. [[빌드 가이드|{{build}}]]를 보세요."],
          ],
        },
        {
          kind: "p",
          text: "코드 서명 인증서가 아직 없어 Windows SmartScreen 경고가 뜰 수 있습니다. SHA-256을 대조한 뒤 추가 정보 → 실행으로 진행하세요.",
        },
        {
          kind: "p",
          text: "처음 실행하면 Ruxt를 비밀번호로 보호할지 묻습니다. 이 비밀번호가 화면 잠금이자 저장된 비밀의 키입니다. 이어서 여섯 장의 둘러보기가 나옵니다. 접속, 파일과 터널, 터미널 도구, 보안 설정, 단축키를 소개합니다. 한 번 닫으면 다시 뜨지 않고, 도구 모음의 `?`에서 둘러보기 다시 보기로 엽니다. 도구 모음을 우클릭하면 각 버튼을 보이거나 숨길 수 있습니다. 버튼을 모두 숨겨도 같은 메뉴에서 다시 켭니다. 인터페이스 언어는 설정 → 모양에서 한국어 또는 English입니다.",
        },
      ],
    },
    {
      id: "sessions",
      title: "세션과 접속",
      blocks: [
        {
          kind: "p",
          text: "도구 모음 입력 칸에 `user@host:port`를 적고 Enter를 누르면 바로 연결됩니다. 빠른 연결로 만든 세션은 저장되지 않고, 비밀번호도 저장할 수 없습니다. 자주 쓰는 서버는 세션으로 남기세요.",
        },
        {
          kind: "p",
          text: "+ 새 세션이 편집기를 엽니다.",
        },
        {
          kind: "table",
          headers: ["항목", "하는 일"],
          rows: [
            ["이름, 그룹, 태그", "왼쪽 목록과 검색에 쓰입니다. 태그는 쉼표로 구분합니다."],
            ["호스트, 포트, 사용자 이름", "접속 대상입니다. 사용자 이름을 비우면 이 PC의 계정 이름을 씁니다."],
            ["인증", "자동(에이전트 → 기본 키 파일 → 비밀번호 → 키보드 인터랙티브), 비밀번호, 공개키 파일(OpenSSH, PKCS#8, PuTTY PPK), SSH 에이전트, OTP·2FA용 키보드 인터랙티브."],
            ["점프 호스트", "다른 저장 세션을 먼저 거칩니다. OpenSSH의 `ProxyJump`와 같습니다. 각 홉은 그 세션의 호스트 키, 인증, 저장된 비밀번호를 따릅니다. 체인은 4홉에서 멈춥니다."],
            ["보안 프로필", "엄격(기본)은 포스트양자 하이브리드 키 교환과 AEAD 암호만 제안합니다. 호환은 오래된 서버를 위해 DH 그룹 교환과 CTR 암호를 더합니다. 레거시는 `ssh-rsa`(SHA-1 서명)를 허용하는 유일한 프로필입니다. SHA-1과 CBC는 어느 프로필에도 없습니다."],
            ["시작 명령", "셸이 열린 직후 한 번 실행합니다."],
            ["포워딩", "X11, 에이전트 포워딩(경고와 함께 표시), 터널(`-L`, `-R`, `-D`)."],
            ["저장된 비밀번호", "화면 잠금 비밀번호가 있을 때 비밀번호나 키 패스프레이즈를 저장합니다."],
          ],
        },
        {
          kind: "p",
          text: "세션은 더블클릭하거나 오른쪽 클릭 → 연결로 엽니다. 최근 연 세션은 최근 그룹에 모입니다. 검색은 이름, 호스트, 그룹, 태그를 찾습니다.",
        },
        {
          kind: "p",
          text: "세션 목록 위의 가져오기는 OpenSSH 설정(`~/.ssh/config`), PuTTY 저장 세션(Windows 레지스트리 또는 `~/.putty/sessions`), 다른 컴퓨터의 Ruxt JSON을 읽습니다. OpenSSH의 `ProxyJump`, `IdentityFile`, 로컬·원격·동적 포워딩, `ForwardAgent`, `ForwardX11`, `Compression`, `ServerAliveInterval`이 옮겨집니다. 와일드카드 호스트와 `Match` 블록은 건너뜁니다. PuTTY는 비밀번호를 저장하지 않으므로 가져올 비밀번호가 없습니다. Ruxt 내보내기에도 비밀번호와 패스프레이즈가 들어가지 않습니다. 이름과 주소가 같은 세션이 있으면 건너뛰고, id가 겹치면 새 id를 줍니다.",
        },
      ],
    },
    {
      id: "host-keys",
      title: "호스트 키",
      blocks: [
        {
          kind: "p",
          text: "서버에 처음 접속하면 알 수 없는 호스트 키 창이 열립니다. SHA-256 지문과 randomart를 관리자가 알려 준 값과 비교한 뒤 신뢰하고 연결을 누르세요. 키는 저장되고 다음 접속부터는 묻지 않습니다.",
        },
        {
          kind: "p",
          text: "키가 바뀌면 연결이 막힙니다. 서버를 재설치한 것이 확실할 때만, 확인란을 체크한 뒤에 교체하세요. 중간자 공격일 수 있으면 취소하고 관리자에게 확인하세요. 점프 호스트는 홉마다 따로 고정합니다.",
        },
      ],
    },
    {
      id: "terminal",
      title: "터미널",
      blocks: [
        {
          kind: "p",
          text: "SSH 세션과 로컬 셸은 `xterm-256color` 터미널입니다. 상자 그리기, 블록, 점자 문자는 칸에 맞춰 직접 그리므로 TUI 테두리가 어떤 글꼴에서도 이어집니다. 고른 글꼴에 없는 기호는 운영체제의 기호 글꼴로 대체됩니다. 한글 입력은 조합 중인 글자를 보여 줍니다. 마우스 리포팅과 브래킷 페이스트가 켜져 있습니다.",
        },
        {
          kind: "table",
          headers: ["동작", "키"],
          rows: [
            ["복사", "선택이 있을 때 `Ctrl+C`, `Ctrl+Shift+C`, `Ctrl+Insert`"],
            ["붙여넣기", "`Ctrl+V`, `Ctrl+Shift+V`, `Shift+Insert`, 오른쪽 또는 가운데 버튼"],
            ["인터럽트", "선택이 없으면 `Ctrl+C`가 원격 프로그램으로 갑니다."],
            ["스크롤백 검색", "`Ctrl+Shift+F`. `Enter`와 `Shift+Enter`로 다음·이전 일치."],
            ["링크 열기", "`Ctrl`+클릭"],
            ["스크롤백", "`Shift+PgUp`, `PgDn`, `Home`, `End`, 휠"],
            ["글꼴 크기", "`Ctrl`+휠, `Ctrl`과 `+` 또는 `-`"],
            ["다시 연결", "세션이 끝난 뒤 `Enter`"],
            ["명령어 자동 완성", "`→` 수락, `Ctrl+↑` / `Ctrl+↓` 선택, `Esc` 숨김"],
          ],
        },
        {
          kind: "p",
          text: "줄바꿈이 있는 붙여넣기는 미리보기와 함께 확인을 받습니다. 설정에서 끌 수 있습니다. ESC 같은 제어 문자는 붙여넣는 텍스트에서 제거됩니다. 터미널이 아닌 입력란(세션 편집기, 빠른 연결, SFTP 경로)에서는 일반적인 `Ctrl+C`, `Ctrl+V`, `Ctrl+X`가 그대로 동작합니다.",
        },
        {
          kind: "p",
          text: "도구 모음의 동시 입력은 열린 모든 SSH 탭에 키 입력을 보냅니다. 로컬 셸은 별도 탭입니다. 프로그램은 설정 → 터미널에서 고르고, 비워 두면 자동으로 찾습니다.",
        },
      ],
    },
    {
      id: "completion",
      title: "명령어 자동 완성",
      blocks: [
        {
          kind: "p",
          text: "자동 완성은 화면 잠금 비밀번호를 정한 뒤에만 동작합니다. Enter로 실행한 명령은 `history.db`에 암호화되어 저장됩니다. 같은 글자로 다시 입력하면 나머지가 커서 뒤에 흐린 글씨로 나타납니다. `→`는 그 글자를 셸에 입력합니다. 후보가 여러 개면 커서 아래에 목록이 열립니다. 이 서버에서 쓴 명령이 먼저이고, 그다음이 자주·최근에 쓴 명령입니다.",
        },
        {
          kind: "ul",
          items: [
            "화면에 실제로 표시된 글자만 저장됩니다. 에코되지 않는 비밀번호 프롬프트는 기록되지 않습니다. `password=`, `token=`, `Authorization:`, `XXX_SECRET=`처럼 비밀로 보이는 줄은 건너뜁니다.",
            "공백으로 시작하는 명령은 저장되지 않습니다. `HISTCONTROL=ignorespace`와 같습니다.",
            "vim 같은 전체 화면 프로그램 안에서는 동작하지 않습니다.",
            "각 줄은 화면 잠금 비밀번호로 여는 키로 암호화됩니다. 잠금이 풀려 있을 때만 기록하고 제안하며, 잠그면 메모리에서 지웁니다. 파일에 평문으로 남는 것은 무작위 줄 번호와 마지막 사용 시각뿐입니다.",
            "이전 버전이 남긴 평문 기록은 처음 잠금을 풀 때 암호화되고 평문은 지워집니다. 화면 잠금 비밀번호를 정하지 않았다면 평문이 남으므로, 명령어 기록 지우기로 지우세요.",
            "설정 → 명령어 자동 완성에서 제안을 끄거나, 보관 개수를 제한하거나, 기록을 지울 수 있습니다.",
          ],
        },
      ],
    },
    {
      id: "server-status",
      title: "서버 상태",
      blocks: [
        {
          kind: "p",
          text: "연결된 SSH 탭은 상태 표시줄에 CPU, 메모리, 첫 디스크를 보일 수 있습니다. 각각 흐린 라벨, 얇은 막대, 고정 폭 수치입니다. 막대는 60% 미만이면 테마 색, 60%부터 주황, 85%부터 빨강입니다. 막대를 클릭하거나, 모니터가 꺼져 있으면 서버 정보를 클릭하면 상세 창이 열립니다. 부하, 스왑, 사용자, 프로세스는 그 창에 있고 상태 표시줄에는 없습니다.",
        },
        {
          kind: "ul",
          items: [
            "연결: 주소, 보안 프로필, 점프 호스트, 포워딩(X11 신뢰/비신뢰, 에이전트, 터널 수), 상태, 인증 방법, 복사할 수 있는 호스트 키 지문.",
            "상태: 호스트 이름, OS와 커널, 가동 시간, 부하와 CPU 수, CPU·메모리·스왑 막대와 스파크라인, 디스크별 사용량, 사용자·프로세스 수.",
          ],
        },
        {
          kind: "p",
          text: "모니터는 이미 열린 SSH 세션에서 읽기 전용 명령(`hostname`, `uptime`, `/proc/stat`, `/proc/meminfo`, `df` 등)을 실행합니다. 다시 로그인하지 않습니다. `/proc`이 없는 macOS와 BSD는 일부 항목만 나옵니다. 셸 실행을 거부하는 서버는 감지되어 그 세션의 모니터가 멈춥니다. 이 명령은 서버의 감사 로그에 남을 수 있어, 세션 편집기의 세션별 스위치는 기본이 꺼짐입니다. 상세 창에서 그 세션의 모니터링을 끄거나 주기를 바꿀 수 있습니다. 설정 → 서버 상태는 마스터 스위치이고, 세션이 주기를 정하지 않았을 때의 기본 주기(2~120초)입니다.",
        },
      ],
    },
    {
      id: "tailviewer",
      title: "Tail Viewer",
      blocks: [
        {
          kind: "p",
          text: "`tail -f -n 100 app.log`처럼 파일만 대상으로 한 단독 `tail`을 치면 로그 창이 열리고 터미널 프롬프트는 다음 명령을 받을 수 있습니다. 파이프, 리디렉션, `;`, `&&`, 명령 치환이 있으면 가로채지 않고 셸에서 그대로 실행됩니다.",
        },
        {
          kind: "p",
          text: "SSH 탭에서는 같은 연결의 별도 채널로, 터미널의 현재 폴더에서 따라 읽습니다. 로컬 셸 탭에서는 Ruxt가 파일을 직접 읽습니다. 창을 닫거나 중지를 누르면 원격 `tail`도 끝납니다. SFTP에서 파일을 오른쪽 클릭해 Tail Viewer로 보기로도 엽니다.",
        },
        {
          kind: "ul",
          items: [
            "일시정지는 화면을 고정하고 그 사이 들어온 줄을 쌓아 둡니다. 자동 스크롤, 줄바꿈, 필터(대소문자 구분, 일치하는 줄만), 전체 복사, 지우기, 다시 시작이 도구 모음에 있습니다. 최근 5만 줄을 보관합니다.",
            "감시는 키워드를 쉼표나 세미콜론으로 여러 개 받습니다. 쉼표가 없으면 공백으로 나눕니다. 하나라도 포함(OR) 또는 모두 포함(AND)입니다. 일치하면 창이 3초간 깜빡이고 줄이 강조되며 횟수가 버튼에 표시됩니다. 감지 시 이동은 자동 스크롤이 꺼져 있어도 새 줄로 갑니다. 감시된 줄만은 나머지를 숨깁니다.",
            "일치한 줄을 `.txt` 파일에 도착하는 대로 덧붙일 수 있습니다. 시각을 앞에 붙이거나 원문 그대로입니다. 이 파일은 Ruxt가 관리하지 않습니다. 로그가 민감하면 저장 위치를 직접 정하세요.",
            "창은 별도의 OS 창으로 열립니다. 창 고정은 Ruxt 창 안에 붙이고, 새 창으로가 다시 뺍니다. 설정 → 터미널에서 가로채기를 끄면 `tail`은 터미널에 남습니다.",
          ],
        },
      ],
    },
    {
      id: "sftp",
      title: "SFTP",
      blocks: [
        {
          kind: "p",
          text: "도구 모음의 SFTP는 오른쪽에 탐색기를 엽니다. 파일 관리자를 쓰듯 다룹니다.",
        },
        {
          kind: "ul",
          items: [
            "뒤로, 앞으로, 상위로 이동하고, 폴더를 더블클릭하거나, 폴더 트리와 경로의 조각을 누릅니다. 경로 표시줄의 빈 곳을 누르면 경로를 직접 입력합니다. `~`를 쓸 수 있습니다. 목록 맨 위의 `..` 행은 상위 폴더로 갑니다.",
            "보기 → 터미널 작업 폴더 따라가기는 `cd`를 따라갑니다(OSC 7 또는 프롬프트 제목). 보기 → 숨김 항목은 점으로 시작하는 이름을 보여 줍니다. 이 선택은 세션마다 저장됩니다.",
            "클릭, `Ctrl`+클릭, `Shift`+클릭, `Ctrl+A`, 또는 각 행의 체크박스로 선택합니다. 머리글 체크박스는 보이는 항목 전체를 선택합니다. 빈 곳을 클릭하면 해제됩니다.",
            "키: `Backspace` 상위, `Enter` 열기, `F2` 이름 바꾸기, `Delete` 삭제, `F5` 새로 고침, `Alt+Left` / `Alt+Right` 기록, 방향키 이동.",
            "열 머리글을 누르면 이름, 수정한 시각, 유형, 크기로 정렬합니다. 폴더는 항상 먼저입니다.",
            "업로드 메뉴나 바탕화면에서 끌어 놓기로 올립니다. 이름이 같으면 덮어쓰기 전에 묻습니다. 전송은 임시 이름에 쓴 뒤 교체하므로, 실패해도 원본이 남습니다.",
            "다운로드는 저장 폴더를 묻습니다. 로컬 창이 열려 있으면 그 창의 현재 폴더로 받습니다.",
            "보기 → 로컬 창은 이 컴퓨터의 폴더를 원격 목록 옆에 보여 줍니다. 로컬 창에서 원격 목록으로 끌면 업로드, 반대는 다운로드입니다. 드라이브 메뉴는 바탕 화면, 문서, 다운로드, 드라이브로 갑니다.",
            "원격 항목을 폴더 행이나 트리 노드로 끌면 이동합니다. 잘라내기, 복사, 붙여넣기는 오른쪽 클릭 메뉴와 `Ctrl+X`, `Ctrl+C`, `Ctrl+V`입니다. 같은 폴더에 복사하면 `이름 - copy`가 됩니다. SFTP에는 복사 명령이 없어, 서버 안에서의 복사도 클라이언트를 거쳐 갑니다.",
            "더블클릭이나 편집은 외부 편집기에서 엽니다(설정 → 터미널, 비우면 운영체제 기본 프로그램). 저장할 때마다 다시 업로드됩니다. 서버 쪽 파일이 바뀌었으면 덮어쓰기 전에 묻습니다. 편집기를 지정하지 않으면 실행 파일과 스크립트는 열지 않습니다. 열면 실행될 수 있기 때문입니다.",
            "항목 하나의 권한은 `755` 같은 8진수를 받습니다. 삭제는 영구적이며 먼저 확인합니다. 디렉터리 심볼릭 링크는 링크만 지우고 대상을 따라가지 않습니다.",
            "탐색기 아래 전송 목록은 접힌 한 줄로 시작하고, 전체 중 몇 건이 끝났는지 보여 줍니다. 그 줄을 클릭하거나 위쪽 가장자리를 끌어 올리면 목록이 열리고, 각 전송의 진행률과 취소가 있습니다. 동시에 네 개까지 전송합니다. 폴더 전송은 파일을 먼저 세고(세는 중으로 표시, 5만 개에서 멈춤) 그 합계에 대해 파일 수와 바이트를 보여 줍니다.",
          ],
        },
      ],
    },
    {
      id: "tunnels",
      title: "터널",
      blocks: [
        {
          kind: "p",
          text: "터널은 세션 편집기나 터널 패널에서 추가합니다. 세션은 접속할 때 터널을 함께 열 수 있습니다.",
        },
        {
          kind: "table",
          headers: ["종류", "의미", "예"],
          rows: [
            ["로컬 (`-L`)", "이 컴퓨터의 포트에서 서버를 거쳐 대상 호스트와 포트로", "`127.0.0.1:8080 -> 10.0.0.5:80`"],
            ["원격 (`-R`)", "서버의 포트에서 이 컴퓨터를 거쳐 대상으로", "`127.0.0.1:9000 <- 127.0.0.1:3000`"],
            ["동적 (`-D`)", "이 컴퓨터의 SOCKS5 프록시", "`127.0.0.1:1080`"],
          ],
        },
        {
          kind: "p",
          text: "로컬과 동적 리스너는 루프백에만 바인드됩니다. 다른 주소를 허용하려면 설정 → 보안에서 켜고 바인드 주소를 바꾸세요. SOCKS5는 CONNECT만 받습니다. 서버는 요청한 포워딩 채널만 열 수 있습니다. 에이전트 포워딩은 그 세션에서 켜기 전에는 꺼져 있습니다.",
        },
      ],
    },
    {
      id: "x11",
      title: "X11 포워딩",
      blocks: [
        {
          kind: "p",
          text: "원격 그래픽 프로그램에는 로컬 X 서버가 필요합니다. Windows에서는 Cygwin/X(`XWin.exe`, X.Org 21.1.25)가 `ruxt.exe` 안에 들어 있습니다. 처음 켤 때 `%LOCALAPPDATA%\\ruxt\\data\\xserver`에 풀고, 이후에는 파일이 그대로인지 확인합니다. 바뀌었으면 다시 풉니다. 설정에 `XWin.exe` 경로를 적거나 `ruxt.exe` 옆에 `xserver` 폴더를 두면 그것을 먼저 씁니다. VcXsrv와 Xming은 지원하지 않습니다. Linux와 macOS는 그 컴퓨터에 있는 X 서버를 씁니다.",
        },
        {
          kind: "p",
          text: "Ruxt는 무작위 인증 쿠키로 서버를 시작합니다. 기본값으로는 Ruxt를 켤 때 같이 켜지지 않고, X11 세션이 접속하거나 직접 켤 때만 시작합니다. 포트를 열기 직전에 방화벽 인바운드 차단 규칙을 UAC로 한 번 묻습니다. 거부해도 서버는 켜지고 상태 메뉴에 경고가 남습니다. 상태 표시줄에 상태가 보이며, 클릭하면 Ruxt가 띄운 서버를 켜거나 끄고, 우클릭하면 새로고침, 방화벽 규칙, 다운로드가 열립니다. 실제 쿠키는 원격 프로그램에 전달되지 않습니다.",
        },
        {
          kind: "p",
          text: "기본은 비신뢰 모드이며 `ssh -X`와 같은 생각입니다. Ruxt가 X SECURITY 확장으로 비신뢰 쿠키를 받아, 원격 프로그램은 자기 창만 다루고 다른 창을 읽거나 키보드를 가로채지 못합니다. 동봉 `XWin.exe`는 이 확장을 켜고 빌드한 것입니다. Cygwin이 배포하는 기본 패키지에는 없습니다. 비신뢰 모드에서 툴킷이 실패하면(BadAccess) 세션 편집기에서 신뢰 모드(`-Y`)를 켜세요. 그것은 `ssh -Y`입니다. 원격 프로그램이 로컬 디스플레이 전체를 볼 수 있으므로 믿는 서버에만 쓰세요. 비신뢰 쿠키를 만들 수 없으면, 예를 들어 다른 `XWin.exe`에 SECURITY 확장이 없으면, 포워딩을 거부하고 터미널에 이유를 표시합니다. 비신뢰 인증은 마지막 X 프로그램이 끝난 뒤 7일이 지나면 만료됩니다. 아주 오래 연 세션에서 거부되기 시작하면 다시 접속하세요.",
        },
        {
          kind: "p",
          text: "X.Org 서버에는 루프백만 여는 옵션이 없어 TCP 포트 6000+n이 네트워크에 보입니다. 그래도 쿠키가 필요합니다. Windows 설치 파일은 관리자 설치 시 `XWin.exe`에 대한 인바운드 차단 규칙을 넣습니다. Windows 방화벽은 루프백을 거르지 않으므로 포워딩은 동작하고 LAN에서는 포트가 닫힙니다. 규칙이 없으면 상태 표시줄 항목을 우클릭하세요. 경고와 함께 방화벽 차단 규칙 추가를 제안합니다.",
        },
      ],
    },
    {
      id: "lock",
      title: "화면 잠금과 저장된 비밀번호",
      blocks: [
        {
          kind: "p",
          text: "비밀번호 하나가 창을 잠그고, 저장된 세션 비밀번호와 키 패스프레이즈를 암호화합니다. 별도의 마스터 비밀번호는 없습니다.",
        },
        {
          kind: "p",
          text: "첫 실행에서 정하면 10분 동안 사용하지 않을 때 창이 잠깁니다. 다시 세션을 보려면 비밀번호가 필요합니다. 나중을 고르면 잠금이 꺼진 채로 다시 묻지 않습니다. 설정 → 보안 → 화면 잠금에서 비밀번호를 정하기 전에는 아무것도 저장할 수 없습니다.",
        },
        {
          kind: "ul",
          items: [
            "화면 잠금 비밀번호가 있으면 Ruxt는 잠긴 상태로 시작합니다. 잠금을 풀면 저장된 비밀번호도 함께 열립니다.",
            "비밀번호 창의 기억하기(화면 잠금 비밀번호로 암호화)는 저장된 세션에서만 저장합니다. 세션 편집기에서도 저장된 비밀번호를 넣거나 지울 수 있습니다.",
            "저장된 비밀번호는 접속할 때 먼저 조용히 시도됩니다. 키 파일이나 에이전트로 인증하는 서버는, 키 자체에 패스프레이즈가 없으면 저장할 것이 없습니다.",
            "유휴 시간은 10분 단위입니다. 0이면 자동으로 잠기지 않습니다. 도구 모음의 잠금 버튼은 즉시 잠급니다.",
            "창이 잠겨 있는 동안에도 SSH 세션과 전송은 계속됩니다. 화면에 보이지 않을 뿐입니다. 저장된 비밀번호는 메모리에서 지워지고 다음 잠금 해제 때 다시 열립니다.",
            "디스크에는 Argon2id 검증값(`applock.bin`)과 암호문(`vault.bin`, Argon2id + XChaCha20-Poly1305)만 있습니다. 틀린 비밀번호를 넣을 때마다 다음 확인이 1초씩 늦어지고, 최대 5초입니다.",
            "같은 비밀번호가 명령어 기록을 암호화합니다.",
            "비밀번호를 바꾸면 저장된 비밀번호도 다시 암호화됩니다. 잠금을 해제하면 저장된 비밀번호와 명령어 기록이 삭제됩니다.",
            "잊은 비밀번호는 복구할 수 없습니다. Ruxt를 종료하고 설정 폴더의 `applock.bin`과 `vault.bin`을 지우면 잠금이 사라지고 저장된 비밀번호도 사라집니다.",
          ],
        },
        {
          kind: "p",
          text: "이전 버전은 별도의 금고 마스터 비밀번호로 비밀번호를 저장했습니다. 화면 잠금 비밀번호로 처음 잠금을 풀 때, 또는 잠금 비밀번호를 처음 정할 때, Ruxt가 예전 비밀번호를 한 번 묻고 다시 암호화합니다. 나중을 눌러도 버리지 않습니다. 설정 → 보안 → 저장된 비밀번호 옮기기…가 다시 묻습니다. 예전 비밀번호를 잊었다면 이전 비밀번호 삭제로 새로 시작합니다.",
        },
      ],
    },
    {
      id: "agent-guard",
      title: "에이전트 가드",
      blocks: [
        {
          kind: "p",
          text: "Windows에서 에이전트 가드는 도구 모음 토글이자 설정 → 보안의 스위치입니다. 기본은 꺼짐입니다. 켜면 Ruxt 창이 스크린샷, 녹화, 화면 공유에서 숨고, 다른 프로그램이 주입한 키보드·마우스 입력(AI 에이전트, 매크로)은 버려집니다. 화면 잠금 비밀번호가 있으면 그 주입된 입력은 창도 잠급니다. 입력 훅은 별도 crate에 있어 Ruxt 바이너리 자체는 `forbid(unsafe_code)`를 유지합니다.",
        },
      ],
    },
    {
      id: "themes",
      title: "테마",
      blocks: [
        {
          kind: "p",
          text: "설정 → 모양에 기본 테마 12개가 있습니다. 어두움: 말차(기본), 퍼플 그린, 항구, 잿불, 잉크, 소나무. 밝음: 밝음, 백자, 모래, 민트, 장미, 라벤더. 퍼플 그린은 보라색 바탕에 초록 강조색입니다.",
        },
        {
          kind: "p",
          text: "새 테마…는 지금 쓰는 색에서 시작하는 편집기를 엽니다. 왼쪽은 인터페이스(패널, 창, 입력란, 줄무늬 행, 버튼 세 상태, 글자, 보조 글자, 강조색, 선택, 링크, 경고, 오류, 테두리)입니다. 오른쪽은 터미널(글자, 배경, 커서, ANSI 16색)입니다. 바꾸는 즉시 미리 보입니다. 취소는 이전 테마로 되돌립니다. 이름을 붙인 테마는 설정과 함께 저장되고 최대 32개이며 사용자 지정 목록에서 고릅니다.",
        },
        {
          kind: "ul",
          items: [
            "내보내기는 `이름.ruxt-theme.json`을 씁니다. 색은 `#rrggbb` 문자열이라 손으로 고칠 수 있습니다. 일부 색만 적은 파일도 유효합니다. 나머지는 말차 기본값입니다.",
            "가져오기는 파일을 목록에 넣고 바로 적용합니다. 이미 있는 이름에는 ` (2)`가 붙습니다.",
            "편집기 안의 시작점은 기본 테마를 불러옵니다. 테마 이름은 유지되고 색만 바뀝니다.",
          ],
        },
        {
          kind: "p",
          text: "화면 배율은 75%에서 200%입니다. 메뉴와 대화상자에는 별도의 글꼴 크기가 있습니다. 터미널 글꼴은 따로 고르며, 이 컴퓨터에 설치된 고정폭 글꼴을 쓸 수 있습니다.",
        },
      ],
    },
    {
      id: "settings",
      title: "설정",
      blocks: [
        {
          kind: "table",
          headers: ["영역", "있는 것"],
          rows: [
            ["그래픽", "렌더러(OpenGL 또는 wgpu), 하드웨어 가속(선호, 필수, 끔), VSync. 다음 시작 때 적용됩니다. 원격 데스크톱이나 가상 머신에서 화면이 느리거나 깨지면 wgpu를 시도하세요. `RUXT_RENDERER=wgpu`로도 고릅니다."],
            ["터미널", "글꼴과 크기, 로컬 셸, 선택하면 복사, `tail`을 Tail Viewer에서 열기(기본 켬), 외부 편집기. 편집기 목록은 VS Code, Cursor, VSCodium, Zed, Sublime Text, Notepad++, 메모장을 찾고, 경로를 직접 넣을 수도 있습니다."],
            ["X 서버", "실행 파일, 포워딩 세션이 접속할 때 시작, Ruxt를 시작할 때 동봉 서버 시작(기본 꺼짐, 콘솔 창 없음), 종료 시 중지, 클립보드 공유(기본 끔). 서버를 처음 띄울 때 방화벽 차단 규칙을 묻습니다. 신뢰 모드는 세션 편집기에서 세션마다 고릅니다."],
            ["보안", "여러 줄 붙여넣기 확인, OSC 52 허용, 루프백이 아닌 바인드 허용, 연결된 세션을 닫을 때 확인, N초 뒤 클립보드 지우기, N분 유휴 뒤 모든 세션 끊기, 감사 로그 기록(기본 꺼짐), 명령어 기록과 감사 로그 보관 기간, 화면 잠금."],
          ],
        },
        {
          kind: "p",
          text: "대화상자는 Ruxt 창 안에 머뭅니다. 내용이 길면 본문이 스크롤되고 버튼은 보이며, 모서리로 크기를 바꿉니다. 확인은 채운 pill, 보조 동작은 테두리 없는 고스트 버튼(마우스를 올리면 옅은 배경), 취소는 글자 버튼입니다. 되돌릴 수 없는 확인은 오류색입니다. 본창은 1px 테두리 대신 면의 밝기로 터미널, 막대, 활성 탭을 나눕니다.",
        },
      ],
    },
    {
      id: "files",
      title: "디스크의 파일",
      blocks: [
        {
          kind: "table",
          headers: ["파일", "Windows", "Linux와 macOS"],
          rows: [
            ["설정과 세션", "`%APPDATA%\\ruxt\\config\\config.json`", "`~/.config/ruxt/config.json`"],
            ["저장된 비밀번호", "`%APPDATA%\\ruxt\\config\\vault.bin`", "`~/.config/ruxt/vault.bin`"],
            ["화면 잠금 검증값", "`%APPDATA%\\ruxt\\config\\applock.bin`", "`~/.config/ruxt/applock.bin`"],
            ["알려진 호스트", "`%APPDATA%\\ruxt\\config\\known_hosts`", "`~/.config/ruxt/known_hosts`"],
            ["감사 로그", "`%LOCALAPPDATA%\\ruxt\\data\\audit.log`", "`~/.local/share/ruxt/audit.log`"],
            ["명령어 기록", "`%LOCALAPPDATA%\\ruxt\\data\\history.db`", "`~/.local/share/ruxt/history.db`"],
            ["원격 편집 사본", "`%LOCALAPPDATA%\\ruxt\\data\\edit\\`", "`~/.local/share/ruxt/edit/`"],
          ],
        },
        {
          kind: "p",
          text: "파일은 소유자만 읽을 수 있게 만들어집니다. 손상된 설정은 `config.json.bad-<시각>`으로 따로 보관됩니다. 감사 로그는 접속, 인증, 호스트 키, 터널, 전송, 잠금 이벤트의 JSON Lines이며 비밀은 들어 있지 않습니다. 그래도 평문이고 `config.json`도 평문이므로, 디스크를 잃어버릴 때를 위해 BitLocker, FileVault, LUKS를 켜 두세요. 명령어 기록은 화면 잠금 비밀번호로 암호화됩니다. 감사 로그는 켜기 전에는 기록되지 않습니다.",
        },
      ],
    },
    {
      id: "troubleshooting",
      title: "문제 해결",
      blocks: [
        {
          kind: "table",
          headers: ["보이는 것", "확인할 것"],
          rows: [
            ["connection timed out", "호스트, 포트, 방화벽. 호스트 키 창이 열려 있는 동안은 시간 제한이 멈춥니다."],
            ["인증 실패", "인증 방식을 명시하세요. 키 파일은 OpenSSH, PKCS#8, PPK여야 합니다. 암호화된 키는 패스프레이즈를 묻습니다. 에이전트는 Windows OpenSSH 에이전트 파이프 또는 `SSH_AUTH_SOCK`입니다."],
            ["오래된 서버에서 키 교환 실패", "보안 프로필을 호환으로 바꾸세요. 서버가 `ssh-rsa`(SHA-1)로만 호스트 키에 서명하면 레거시를 쓰세요. OpenSSH 7.2 이전에서 흔합니다."],
            ["X11 프로그램이 BadAccess로 죽거나 비신뢰 모드를 쓸 수 없음", "세션 편집기에서 신뢰 모드(`-Y`)를 켜세요. 믿는 서버에만 쓰세요. SECURITY 확장이 없는 X 서버도 같은 메시지를 냅니다."],
            ["명령어 제안이 없음", "명령어 자동 완성이 켜져 있어야 하고, 전체 화면 프로그램 밖이어야 하며, 그 명령을 전에 실행한 적이 있어야 합니다."],
            ["상태 표시줄에 서버 상태가 없음", "서버가 exec를 거부했거나(제한된 셸, Windows 서버), 설정 → 서버 상태가 꺼져 있습니다. 상세 창에 다시 시도가 있습니다."],
            ["한글이 네모로 보임", "설정 → 터미널 → 글꼴에서 한글이 있는 고정폭 글꼴을 고르세요. UI의 나머지 글자는 시스템 한글 글꼴을 스스로 고릅니다."],
            ["창이 느리거나 찢어짐", "설정 → 그래픽에서 렌더러를 wgpu로, 또는 하드웨어 가속을 끔으로 하고 다시 시작하세요."],
            ["X11 프로그램이 나타나지 않음", "상태 표시줄의 X 서버 표시를 확인하세요. SSH 서버가 `X11Forwarding no`이면 터미널에 거부되었다고 나옵니다."],
          ],
        },
      ],
    },
    {
      id: "limits",
      title: "하지 않는 일",
      blocks: [
        {
          kind: "p",
          text: "RDP, VNC, Telnet, 시리얼 콘솔, 동봉 Unix 도구 모음은 없습니다. 그 대신 같은 실행 파일이 Windows, Linux, macOS에서 동작하고 소스가 공개되어 있습니다. Ruxt는 독립 프로젝트이며 MobaXterm과 관련이 없습니다. MobaXterm은 Mobatek의 상표입니다.",
        },
        {
          kind: "p",
          text: "MobaXterm 세션 파일은 가져오지 않습니다. OpenSSH 설정이나 PuTTY 저장 세션을 가져오세요. MobaXterm의 MultiExec는 동시 입력에 해당합니다. MobaKeyGen은 도구 모음의 키 생성에 해당하며 Ed25519 키 쌍을 만듭니다.",
        },
      ],
    },
  ],
};

function rewrite(text: string): string {
  return text
    .replace(/\{\{threat\}\}/g, THREAT)
    .replace(/\{\{security\}\}/g, SECURITY)
    .replace(/\{\{build\}\}/g, BUILD);
}

function clean(copy: DocsCopy): DocsCopy {
  return {
    ...copy,
    sections: copy.sections.map((section) => ({
      ...section,
      blocks: section.blocks.map((block) => {
        if (block.kind === "p" || block.kind === "note") return { ...block, text: rewrite(block.text) };
        if (block.kind === "ul") return { ...block, items: block.items.map(rewrite) };
        return {
          ...block,
          headers: block.headers.map(rewrite),
          rows: block.rows.map((row) => row.map(rewrite)),
        };
      }),
    })),
  };
}

export const docsCopy: Record<Lang, DocsCopy> = { en: clean(en), ko: clean(ko) };
