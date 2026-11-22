# Third-party components and licence notices

Ruxt itself is licensed under **MIT OR Apache-2.0** (`LICENSE-MIT`, `LICENSE-APACHE`).
Binary releases also contain the components below.

## Rust crates (statically linked)

All dependencies are permissively licensed (MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0,
Unicode, OFL for egui's default fonts, and the `ring` crate's MIT/ISC/OpenSSL notice).
`deny.toml` restricts the allowed licences and CI runs `cargo deny check` on every change.
The full list with versions and licences for any build:

```sh
cargo deny list -f tsv
```

Fonts: egui's default font set (Ubuntu-Light under the Ubuntu Font Licence 1.0, Hack under
MIT, NotoEmoji under OFL 1.1, emoji-icon-font under MIT) is compiled in; terminal fonts are
loaded from the system.

## Bundled Cygwin/X server (embedded in the Windows `ruxt.exe`)

The Windows `ruxt.exe` carries a minimal **Cygwin/X** tree (unpacked to
`%LOCALAPPDATA%\ruxt\data\xserver` on first use)
assembled by `installer/fetch-xserver.ps1` from the official Cygwin distribution:

| Component | Licence | Source |
| --- | --- | --- |
| `cygwin1.dll` and the other `cyg*.dll` runtime libraries | **LGPL v3** (Cygwin) and the respective library licences (MIT/X11, zlib, …) | Cygwin mirrors, `x86_64/release/<package>/<package>-<version>-src.tar.xz` |
| `XWin.exe`, `cygnativeGLthunk.dll` | MIT/X11 (X.Org) | Built from `https://gitlab.freedesktop.org/jturney/xserver`, tag `xserver-cygwin-21.1.25-1` (SHA-256 pinned in the script), with `-Dxcsecurity=true` |
| `xkbcomp.exe`, `sh.exe` (dash) | MIT/X11, BSD | Cygwin packages |
| `usr\share\X11\xkb`, fonts `misc`/`encodings` | MIT/X11 and public-domain bitmap fonts | Cygwin packages |

Every bundle ships a `MANIFEST.txt` listing the exact Cygwin package versions and the
SHA-256 of every file, and a `licenses\` folder with the COPYING files of the packages.

### LGPL compliance statement

The Cygwin runtime is used **unmodified** and is linked **dynamically** (`XWin.exe` loads
`cygwin1.dll` at run time), so you can run it with your own builds of the same Cygwin
version: put a copy of the tree with your DLLs in `bin\` into an `xserver\` folder next to
`ruxt.exe` or point `RUXT_XSERVER_DIR` at it, and Ruxt uses that tree instead of the
embedded one. (Files changed inside the unpacked copy are replaced by the embedded ones,
which Ruxt checks before every start.) Complete corresponding source for every Cygwin package
in a bundle is available from any Cygwin mirror under the package name and version recorded
in that bundle's `MANIFEST.txt`; the Ruxt project will also provide it on request (open an
issue). The rebuild recipe for `XWin.exe` is `installer/fetch-xserver.ps1`.

## Not bundled

Ruxt has no telemetry, auto-update or crash-reporting component. The only outbound
connections are the SSH sessions you open and the links you click.
