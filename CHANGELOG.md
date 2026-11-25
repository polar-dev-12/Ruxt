# Changelog

English | [한국어](CHANGELOG.ko.md)

This project follows [Semantic Versioning](https://semver.org/). The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [1.2.0] - 2026-11-25

### Added
- **Update check**: checks GitHub Releases on startup (Settings → "Check for updates on startup", on by default) and on demand in Help & about, notifying you in the status bar when a newer version is available without automatically downloading or running binaries.
- **SFTP directory sync**: automatically updates the SFTP panel's current directory when running `cd` or `pwd` in the terminal.

## [1.1.0] - 2026-11-22

### Added
- **App icon**: `ruxt.exe`, the window and taskbar, shortcuts and the installer now show the Ruxt icon (`assets/`). Previously Windows showed a blank executable icon and the window used the default egui icon.

### Security
- Updated `url` to 2.5.9 (RUSTSEC-2026-0592: stack overflow in `Url::origin()` on nested `blob:` URLs). It is pulled in through `eframe` → `egui-winit` → `webbrowser`.

### Changed
- **The X server no longer starts automatically with Ruxt** (Settings → X server → "On startup", off by default). It starts only when an X11-forwarding session connects or when you turn it on from the status bar. Right before it launches, Ruxt asks once to add a Windows Firewall inbound block rule (UAC). If you decline, the server still starts with cookie authentication only, and a warning stays in the status menu. If "On startup" is already on in your saved settings, that value is kept.

### Fixed
- **Windows release build**: with Cargo 1.100's build directory layout (`target/<triple>/<profile>/build/<package>/<hash>/out`), `build.rs` looked for the embedded X server in `...\release\build\xserver`, so builds with `RUXT_REQUIRE_XSERVER=1` failed. It now finds the profile directory (`target/<triple>/release/xserver`) again.

## [1.0.0] - 2026-11-21

First stable public release. The source is now public, and this version is meant to be ready for everyday use. It has not had an independent security audit yet.

### Added
- **Documentation page** on the project site (`site/docs.html`) covering installation, sessions, the terminal, Tail Viewer, SFTP, tunnels, X11, screen lock, themes and settings. It shows English unless you pick a language; use the same toggle as the landing page to switch to Korean.
- **10 new built-in themes, 12 in total**: dark (Matcha, Purple Green, Harbor, Ember, Ink, Pine) and light (Light, Porcelain, Sand, Mint, Rose, Lavender). Pick one from the theme list in Settings → Appearance. Purple Green has a purple background with green accents. The same list is also available as starting points in the theme editor.
- **Agent Guard** (Windows; toolbar toggle or Settings → Security; off by default): hides the Ruxt window from screenshots, recordings and screen sharing, and drops synthetic keyboard and mouse input injected by programs such as AI agents and macros. If a screen lock password is set, the window also locks as soon as input is blocked. The input hook is in a separate crate, `crates/ruxt-inputguard`, so the main crate keeps `forbid(unsafe_code)`.
- `Ctrl+Enter` in the multi-line paste confirmation dialog pastes right away.
- **External editor dropdown** in Settings. It auto-detects installed editors such as VS Code, Cursor, VSCodium, Zed, Sublime Text, Notepad++ and Notepad. You can also type a path or browse for one.

### Changed
- **Compact Material 3 chrome for desktop**: areas are separated by surface tone instead of button borders, and secondary buttons are ghost buttons with no background until you hover over them. The toolbar now has actions on the left and Lock/Settings/Help on the right, and the close button sits inside each tab. CPU, MEM and disk in the status bar use fixed-width numbers with 36×5px mini bars.
- **Settings and data folders have moved to `%APPDATA%\ruxt\` and `%LOCALAPPDATA%\ruxt\`.** Existing data is not migrated automatically. To keep your previous settings, copy the contents of the old `config` folder into the new one. The installer's publisher name is now `Ruxt`.
- **Shortcut help**: fixed key names being split vertically one character per line in narrow windows, and added alternating row backgrounds to make the list easier to read.
- **`Ctrl+C`/`Ctrl+V` copy and paste in the terminal**: when text is selected, `Ctrl+C` copies it and clears the selection. With no selection it still sends an interrupt (^C) to the remote program. `Ctrl+V` pastes, so you can no longer send ^V to the shell. `Ctrl+Shift+C/V` still work.
- **X server bundled into `ruxt.exe`**: the Cygwin/X tree (about 27 MB) is now embedded in the executable. This removes the "X server" installer component and the `xserver\` folder in the portable zip; `ruxt.exe` is now about 38 MB.
  - The first time the X server starts, it is extracted to `%LOCALAPPDATA%\ruxt\data\xserver`.
  - On every start, Ruxt checks the SHA-256 of every file and makes sure `bin\` has no unknown files. If anything differs, it extracts the files again.
  - The installer's firewall block rule now points to this path, and the `{app}\xserver` folder installed by earlier versions is removed on upgrade.
  - An `xserver\` folder next to `ruxt.exe`, or `RUXT_XSERVER_DIR`, still takes precedence.

### Fixed
- Mouse drag selection in the terminal started a few pixels after the point you clicked, so the first character was left out.

## [0.5.0] - 2026-10-30

### Added
- **Terminal drawing characters are rendered directly**: box drawing (─│╭╮╔═), blocks (█▀▄░▒▓, eighths, quadrants), Braille (⣿) and 2×3 sextants (the 🬀 range) are drawn to fit the cell instead of coming from the font. CLI animations and TUI borders now join up with no gaps in any font, and characters missing from Windows fonts, such as Braille, no longer show up as a replacement glyph (a ? in a box).
- **Fallback fonts for symbols and emoji**: symbols (★ ✓ ⚡ ☀ ⇒ and so on) and emoji that are missing from the chosen font and the Korean font are drawn with the OS symbol fonts (Windows: Segoe UI Symbol, Segoe UI Emoji; macOS: Apple Symbols; Linux: Noto Sans Symbols, DejaVu Sans). Emoji are shown in a single color.
- **First-run tour**: on first launch (after the screen lock setup dialog), six slides introduce connecting and sessions, files/tunnels/X11, terminal tools, security settings and shortcuts. Move between slides with `←`/`→` or the page dots, and reopen the tour from `?` → "Show the tour again". The shortcut table in the help window now also lists opening links, reconnecting and the SFTP keys.
- **Tail Viewer**: a plain `tail` command typed in the terminal (targeting a file, with no pipes or redirection) is intercepted and shown in a separate window, leaving the prompt free. In SSH tabs it runs over a separate channel on the same connection, in the terminal's current folder; in local shell tabs it follows the file directly. You can also open it by right-clicking a file in SFTP → "Open in Tail Viewer".
  - Pause (keeps buffering in the background), auto-scroll, line wrap, filter, copy all and restart. Keeps the last 50,000 lines.
  - Keyword watch: several comma-separated keywords, AND/OR, match count, window flash and line highlight, jump to matching lines, and a matches-only view.
  - Write matching lines to a `.txt` file in real time (with timestamps or as-is).
  - Use it as a separate OS window or pin it inside the Ruxt window (Pin to window / Open in new window). You can turn it off in Settings → Terminal → Tail Viewer (on by default).
- **SFTP `..` row**: the remote list and the local pane always show a `..` row at the top that goes to the parent folder (double-click). The existing ↑ button and Backspace still work.
- **Project site** (`site/`): a static React + Vite page and a GitHub Pages deployment workflow.
- **SFTP checkbox selection**: every row has a checkbox, so you can select several items without Ctrl, and the header checkbox selects or clears every visible item.
- **Folder transfer progress**: when downloading, uploading or copying a folder, Ruxt first counts the files and their size ("Counting… n items") and then shows overall progress such as `37/120 · 700 MB / 2.0 GB` (previously each file went from 0 to 100% on its own). Counting stops at 50,000 files.

### Security
- **Bundled X server upgraded to X.Org 21.1.25** (from 21.1.21): includes the security fixes in 21.1.22–21.1.25 (bounds checks in Xi barriers and GLX RenderLarge, use-after-free fixes in glyphsets and Present, XKB memory errors, and more). These cover attacks on the X server by forwarded remote X clients. The Cygwin mirror still only has 21.1.21, so only `XWin.exe` is built from the Cygwin/X source tag `xserver-cygwin-21.1.25-1`; data files and DLLs come from the mirror packages.

### Changed
- **Encrypted command history**: the command history used for autocomplete (`history.db`) is now kept only when a screen lock password is set, and each line is encrypted with XChaCha20-Poly1305. The key lives in the same store as the saved passwords (`vault.bin`), so it can only be read while unlocked and is wiped from memory on lock. Plaintext history from earlier versions is encrypted and migrated on the first unlock, and the plaintext is deleted. Removing the screen lock also deletes the history.
- **Audit log is off by default**: Settings → Security has a new "Keep an audit log" option, off on new installs (turn it on if you need it). Turning it off only stops new entries, and toggling it is itself logged. The threat model now states that the audit log, command history and settings files are plaintext metadata, and that protecting them on disk relies on OS disk encryption.
- **Name standardized as "Ruxt"**: the window title, installer and docs now say `Ruxt` instead of `ruXt`. The executable (`ruxt.exe`) and the settings folder path are unchanged. The X server firewall rule is now named `Ruxt X server (block inbound)`, so if you have a rule with the old name, the X menu will say the rule is missing (just add it again).
- **Server info and SFTP view settings are saved per session**: status monitoring and its polling interval set in the server info window, and the "Hidden items" toggle in SFTP, are saved to that session and applied on the next connection. Previously the monitoring checkbox only lasted for that tab, the polling interval changed a global value shared by every session, and hidden items always started off. The session editor can also set the polling interval (0 = the default from Settings). Also fixed monitoring that was turned off for a session turning back on when the monitoring switch in Settings changed.
- The "Multi-exec" toolbar button is now **Broadcast input** in the English UI (the Korean label "동시 입력" is unchanged).
- The README and package description now introduce Ruxt on its own; the MobaXterm comparison moved to the README's "Coming from MobaXterm" section.
- **The credential vault is merged into screen lock**: the separate vault master password and vault open/lock are gone. Session passwords and key passphrases are encrypted with the screen lock (formerly "app lock") password and used only while unlocked. Previously you could still connect with key/agent authentication or by typing a password while the vault was locked, so locking it had unclear meaning.
  - If a screen lock password is set, Ruxt starts locked. Without a screen lock password, passwords are not saved.
  - Changing the screen lock password re-encrypts saved passwords with the new one, and removing the screen lock deletes saved passwords.
  - A vault from an earlier version is migrated on the first unlock by asking once for the old master password (you can also do this later in Settings → Security, or delete it and start over if you forgot it). If you used only the vault without a screen lock password, Ruxt offers once more at startup to set one.
  - Removed the vault button from the toolbar and "Credential vault" and "Vault auto-lock" from Settings (the `auto_lock_minutes` setting is ignored).
- **On/off buttons change appearance with their state**: Sessions, Find, SFTP, Tunnels, Broadcast input, X server, Vault, Settings and `?` in the toolbar, and Pause in the log viewer, are filled with the accent color and show a check mark when on, and only outlined when off (Material filter chips). Previously the on state was just a faint background and hard to tell apart. Text on filled buttons uses whichever of black or white has the higher WCAG contrast.
- Removed the host key fingerprint and `Auth: publickey` from the bottom status bar. Connected SSH tabs always show a server status summary (`Server info` when monitoring is off), which opens the server info window when clicked.
- Rebuilt the **Help and About window** (`?`) with Material 3 components: a version chip, documentation link buttons (filled, outlined, text), a card listing security properties (title + description) and a shortcuts card (keycap table). Added shortcuts for terminal find, scrolling and autocomplete.

### Fixed
- **X server becoming unresponsive right after starting**: if XWin receives a connection right after opening its port (about one second, before its internal window manager attaches), it stops responding to every client afterward (reproduced on both 21.1.21 and 21.1.25). Ruxt's startup check and display discovery were connecting to the port at exactly that moment. The startup check now watches XWin's log for the ready line (up to 10 seconds), nothing connects to that port in the meantime, and X11 forwarding and untrusted cookie generation wait until it is ready. Real X server tests went from 2/5 to 8/8 passing.
- `installer\fetch-xserver.ps1`: gitlab.freedesktop.org returned an Anubis bot-check page to PowerShell's default User-Agent (`Mozilla/…`), so the source hash check failed. It now downloads with a non-browser User-Agent and says so if it gets HTML back.
- **Vault lock/unlock**: on first launch the vault unlock dialog and the app lock setup dialog appeared at the same time, and the vault password field stole focus every frame, so typing went into the wrong dialog. The app lock dialog now appears first and the vault dialog after it, and focus is grabbed only once (or after a wrong password).
- If app lock also locked the vault, unlocking now shows the vault unlock dialog again. If the app locks during key derivation, the result is discarded and the vault stays locked.
- Locking the vault from the toolbar or Settings shows a "Vault locked" notification and closes the master password change dialog if it is open (the same applies to auto-lock).
- Progress bars and cancel buttons in the transfer list shifted sideways depending on name length: the name column is now a fixed width and truncated (full name in a tooltip), and the bar and button stay in place.
- Redrew the SFTP refresh icon (a 270° arc with a tangent arrowhead).

## [0.3.0] - 2026-10-29

### Added
- **Server status monitor**: every few seconds, Ruxt runs a read-only script (hostname, uptime, `/proc/stat`, `/proc/meminfo`, `df`) on the connected server over an exec channel on the existing SSH session, and summarizes CPU, memory, swap, disk, load, users and process count in the bottom status bar (`CPU 12%  MEM 61%  load 0.42  disk 46%`). Clicking it opens the **Server info** window (connection info: address, auth method, host key fingerprint, security profile, jump hosts, forwards/tunnels; server status: sparklines and bars). Clicking the host key fingerprint opens the same window. Turn it on or off and change the interval (2–120 seconds) in Settings → Server status. Servers that refuse exec are detected and monitoring stops for that session. Server output is parsed defensively, with a size limit and control characters stripped.
- **Command autocomplete**: commands you run are saved to SQLite (`history.db`, owner-only permissions), loaded into memory at startup, and suggested as you type with ghost text and a candidate list. `→` accepts, `Ctrl+↑/↓` picks a candidate, `Esc` hides. Commands used on the same host come first, then by use count and recency. Whether to record is decided from **the characters actually shown on the terminal grid**, so input that is not echoed, such as at password prompts, is never saved. Lines that look like secrets (`password=`, `token=`, `Authorization:`, `*_SECRET=` and so on) and lines starting with a space are skipped (same as `HISTCONTROL=ignorespace`). It does not work in the alternate screen (vim and so on). Up to 20,000 entries; you can turn it off or clear everything in Settings.
- **Untrusted X11 forwarding is now the default**: a session's X11 forwarding now gives remote clients an **untrusted cookie** issued through the X SECURITY extension, like `ssh -X`, which stops them from reading other windows or capturing keystrokes. Turning on **Trusted mode (-Y)** in the session editor allows full access as before (with a warning). If the X server has no SECURITY extension, forwarding is refused and the terminal explains why and how to fix it. Untrusted authorizations are revoked when the session ends.
- **X server firewall rule**: X.org cannot listen on loopback only (`-listen tcp` binds every interface), so TCP port 6000+n is exposed to the LAN. The installer (admin install) adds a Windows Firewall inbound **block** rule for `XWin.exe` (loopback is not filtered, so Ruxt's bridge still works), and Ruxt's X menu warns when the rule is missing and can add it after a UAC prompt.
- **"Legacy" security profile**: for servers older than OpenSSH 7.2 that need `ssh-rsa` (RSA/SHA-1) host key signatures.

- **Custom themes**: in Settings → Appearance, **New theme…** opens the theme editor, where you pick UI colors (panels, windows, inputs, buttons, text, accent, selection, links, warnings, errors, borders) and terminal colors (text, background, cursor, the 16 ANSI colors) with color pickers, then name and save the theme. Changes preview live, and canceling goes back to the previous theme. Themes are saved with your settings (up to 32), and **Export/Import** exchanges them as `*.ruxt-theme.json` (`#rrggbb` colors, hand-editable, you can list only some colors). The built-in Matcha and Light themes use the same format, so you can use them as starting points.
- **UI/UX cleanup (Material 3 principles)**: no window or modal **grows larger than the screen** (maximum size fits the viewport, resizable, scrolling body), so buttons stay visible in small windows. Settings, the session editor and server info are split into titled **section cards** (tonal surfaces, rounded corners, striped rows). Dialog buttons follow a **filled (confirm) / outlined (secondary) / text (cancel)** hierarchy, aligned right, and dangerous confirmations use an error-colored filled button. The 8dp spacing grid, 8/12 corner radii, shadows and dividers are consistent throughout. Custom theme colors apply to all of these surfaces.
- **Bundled Cygwin/X built with the SECURITY extension**: Cygwin's `xorg-server` package is built with `-Dxcsecurity` off, so untrusted X11 forwarding was impossible. `installer\fetch-xserver.ps1` now downloads the Cygwin build tools and bundles an `XWin.exe` rebuilt from the `xserver-cygwin-21.1.21-1` source with `-Dxcsecurity=true` (the build string shows `XCSECURITY`). Untrusted authorizations expire 7 days after the last client exits (we found that the X server's timer overflows past 49 days and crashes the server).

- **"Don't ask again" on exit confirmation**: the confirmation shown when closing a connected tab or quitting with live sessions now has a checkbox. Check it and confirm to stop asking; you can turn it back on with "Confirm when closing or … quitting with connected sessions" in Settings → Security.

- **Security review follow-ups**:
  - `ruxt.exe` is excluded from Windows Error Reporting (WER) (registered under `HKCU` at startup, no admin rights needed). This keeps crash dumps containing an open vault, passwords or scrollback from being uploaded.
  - Temporary files for remote editing are kept in `%LOCALAPPDATA%\ruxt\data\edit\` instead of `%TEMP%` and excluded from Windows Search indexing.
  - Settings → Security: **Auto-clear clipboard** (deletes content Ruxt copied after N seconds if it is still on the clipboard), **Disconnect all sessions after N idle minutes**, and **retention periods for command history and the audit log** (90/180 days by default, cleaned up at startup).
  - The server status monitor now requires **per-session consent** ("Server status monitoring" in the session editor, off by default). The global setting acts only as a master switch.
  - `fetch-xserver.ps1` pins and verifies the SHA-256 of the X server source tarball.
- **X server UX**: Ruxt starts the bundled X server automatically at launch (Settings → X server → "On startup", on by default). The X server now runs **without a window**: XWin is a GUI-subsystem program, so creation flags did not hide it, and the Cygwin runtime opened a new console (a Windows Terminal window on Windows 11). It is now launched through a windowless `cmd.exe` console. On exit Ruxt ends the **whole process tree**, including forked children left by XWin, and a server left behind by a crash is found on the next launch via `xserver.pid` and whether its parent is alive, cleaned up, and replaced (previously it was mistaken for an external server and a second one was started next to it). The X server status in the toolbar and status bar now uses translated text ("X server: running (:0)").
- **Key generation**: a passphrase is now required (at least 8 characters), and the **Generate random passphrase** button creates a 24-character one (4×6, without easily confused characters) and shows it once.

### Changed
- **VcXsrv / Xming support removed**: only the bundled Cygwin/X (`XWin.exe`) is used (the executable path in Settings also accepts only `XWin.exe`). This guarantees, with a single server, the SECURITY extension needed for untrusted X11 forwarding and tracking of the latest X.org releases.
- Removed `ssh-rsa` (RSA/SHA-1) host key signatures from the **Compatible** security profile (they were included in the russh defaults). Use the new **Legacy** profile for servers that require SHA-1.
- **App lock**: Ruxt offers to set a password on first launch. Once set, the whole window locks after 10 idle minutes (default; set 0 = off or steps of 10 minutes in Settings → Security). You can also lock right away with the toolbar `Lock` button. Locking also locks the vault, and sessions keep running in the background. The password is stored in `applock.bin` only as an Argon2id verifier, and each failure delays the next attempt. Changing or removing the password requires the current password.
- **Settings → Appearance**: sliders for UI scale (75–200%) and menu/window font size (9–24 pt). These are separate from the terminal font size and apply when you release the mouse.

### Changed
- **X server**: instead of VcXsrv (last release 2025-03), Ruxt now **bundles Cygwin/X (`XWin.exe`)**, which keeps up with X.org point releases. `installer\fetch-xserver.ps1` downloads a minimal set (`xorg-server` and its dependencies) from a Cygwin mirror into `target\release\xserver\`, and the installer includes it as the optional "X server" component. Ruxt looks for the bundled tree first, then a custom path, then an installed VcXsrv/Xming (`RUXT_XSERVER_DIR` sets the bundled location). XWin starts with `-listen tcp -nolisten inet6 -auth <random cookie>` and never uses `-ac`.

## [0.2.0] - 2026-10-29

### Added
- **UI language**: switch between Korean (default) and English (Settings → Appearance → Language). Every window, dialog and menu uses the same language.
- **SFTP explorer**: rewritten in the style of Windows File Explorer: back/forward/up, a path bar you can click to type in, a folder tree, sortable details columns (name, date modified, type, size, permissions), drawn icons, Ctrl/Shift multi-select, and keyboard navigation (Backspace, Enter, F2, Delete, F5, Alt+←/→, Ctrl+A).
- **SFTP local pane**: view a folder on your PC side by side and drag to upload or download. Remote **cut, copy and paste**, **drag to move** onto folder rows or tree nodes, and server-side copy (`Copy`) and bulk move (`Move`) commands.
- **Jump hosts (ProxyJump)**: connect through another saved session from the session editor. Host key pinning, authentication and vault credentials apply separately to each hop, up to 4 hops.
- **Session import/export**: OpenSSH `~/.ssh/config` (Host/HostName/User/Port/IdentityFile/ProxyJump/LocalForward/RemoteForward/DynamicForward), PuTTY saved sessions (Windows registry / `~/.putty/sessions`) and Ruxt JSON. Exports never include secrets.
- **Session list**: a Recent connections group, a tags field, and search by tag or group.
- **Graphics options**: renderer (OpenGL / wgpu: DirectX 12, Vulkan, Metal), hardware acceleration (preferred/required/off) and VSync. Can also be set with the `RUXT_RENDERER=wgpu|opengl` environment variable.
- **Clipboard in input fields**: Ctrl+C / Ctrl+V / Ctrl+X work in every input field outside the terminal (session editor, quick connect, SFTP path and so on).
- **Accessibility**: drawn buttons, list rows, tree nodes and column headers have accessible names and roles.
- **UI smoke tests**: headless tests based on `egui_kittest` (SFTP explorer interactions, session editor validation, settings rendering, missing translation checks) run in CI.
- **Distribution**: a release workflow that, on tag push, builds Windows/Linux/macOS (arm64, x86_64) binaries, a Windows installer (Inno Setup) and SHA256SUMS. Builds are Authenticode-signed when the certificate secret is available.
- **Docs**: a user guide (`docs/USER_GUIDE.md`) and threat model (`docs/THREAT_MODEL.md`), which can be opened directly from the About window.
- Added macOS to CI.

### Fixed
- Input fields outside the terminal did not accept Ctrl+V because `clipboard_shortcuts` was off.
- Glyphs missing from the bundled font (`⏎`, `▾` and so on) showed as □; they are now drawn instead.

## [0.1.0] - 2026-10-01

### Added
- First public release: multi-tab SSH terminal, SFTP, -L/-R/-D tunnels, X11 forwarding, local shell, session manager, encrypted credential vault and audit log.
