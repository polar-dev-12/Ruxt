<#
.SYNOPSIS
  Build the Cygwin/X tree that Ruxt bundles as its X server.

.DESCRIPTION
  build.rs embeds this tree into ruxt.exe (only the files MANIFEST.txt lists, hash-checked),
  and Ruxt unpacks it to %LOCALAPPDATA%\ruxt\data\xserver on first use. A tree left
  next to ruxt.exe (or in $env:RUXT_XSERVER_DIR) is used directly instead. Run this script
  *before* `cargo build`. This script produces that tree from the official Cygwin distribution:

    1. downloads Cygwin's signed `setup-x86_64.exe` and checks its Authenticode signature;
    2. runs it headless into a scratch root with `xorg-server` (+ dependencies) from a mirror;
    3. copies the minimal subset XWin needs at run time (about 26 MiB) into -Dest:
         bin\      XWin.exe, xkbcomp.exe, sh.exe (dash; XWin runs xkbcomp through /bin/sh),
                   cygnativeGLthunk.dll (-wgl hardware GL), cygwin1.dll and the DLLs that
                   `cygcheck` reports for them
         etc\X11\  system.XWinrc
         usr\share\X11\fonts\{misc,encodings}, usr\share\X11\xkb (xkeyboard-config), XErrorDB
         tmp\, var\log\xwin\, var\lib\xkb\   (writable dirs XWin and xkbcomp expect)
         licenses\ COPYING files of the bundled packages (Cygwin: LGPLv3; X.org: MIT/X11)
    4. writes MANIFEST.txt (package versions and SHA-256 of every file).

  Ruxt starts the result as
    XWin :N -multiwindow [-clipboard|-noclipboard] -wgl -listen tcp -nolisten inet6
         -silent-dup-error -fp /usr/share/X11/fonts/misc,built-ins -logfile <log> -auth <cookie>
  (see src\core\xserver.rs). Re-run this script to pick up new xorg-server point releases.

.PARAMETER Dest
  Output directory. Default: target\release\xserver (what build.rs embeds into a release build).
.PARAMETER Mirror
  Cygwin mirror URL. Any entry from https://cygwin.com/mirrors.html works.
.PARAMETER Scratch
  Working directory for the full Cygwin download/install (~0.5 GiB). Default: %TEMP%\ruxt-cygwin.
.PARAMETER KeepScratch
  Do not delete the scratch directory afterwards (useful to inspect the full install).
.PARAMETER NoSecurityBuild
  Keep Cygwin's stock XWin.exe. By default step 2b rebuilds XWin.exe from the Cygwin
  xserver source with the X SECURITY extension enabled (-Dxcsecurity=true), which the stock
  package leaves out and which Ruxt's default *untrusted* X11 forwarding (ssh -X) needs.
  The rebuild installs gcc/meson/ninja and the -devel packages into the scratch root
  (another ~400 MiB) and takes 5-15 minutes.
.PARAMETER XserverSource
  Cygwin xserver branch tag to build. It may be newer than the xorg-server package on the
  mirror (only XWin.exe is rebuilt; data files and DLLs come from the package), which is how
  X.Org security point releases reach the bundle before Cygwin ships them.

.EXAMPLE
  .\installer\fetch-xserver.ps1
  .\installer\fetch-xserver.ps1 -Dest target\debug\xserver
#>
[CmdletBinding()]
param(
    [string]$Dest = (Join-Path $PSScriptRoot '..\target\release\xserver'),
    [string]$Mirror = 'https://mirrors.kernel.org/sourceware/cygwin/',
    [string]$Scratch = (Join-Path $env:TEMP 'ruxt-cygwin'),
    [switch]$KeepScratch,
    [switch]$NoSecurityBuild,
    [string]$XserverSource = 'xserver-cygwin-21.1.25-1',
    # SHA-256 of the source tarball for $XserverSource (gitlab serves it unsigned); '' skips the check.
    [string]$XserverSha256 = '519712debc21205bdb4817e3d9f3fa621e3be1edbaa6f6fb96c2ac882b30dc58'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Dest = [System.IO.Path]::GetFullPath($Dest)
$Scratch = [System.IO.Path]::GetFullPath($Scratch)
$root = Join-Path $Scratch 'root'
$pkgs = Join-Path $Scratch 'pkgs'
$setup = Join-Path $Scratch 'setup-x86_64.exe'
New-Item -ItemType Directory -Force $Scratch | Out-Null

# ---------------------------------------------------------------- 1. setup.exe (signed)
Write-Host "==> downloading Cygwin setup-x86_64.exe"
Invoke-WebRequest -Uri 'https://cygwin.com/setup-x86_64.exe' -OutFile $setup -UseBasicParsing
$sig = Get-AuthenticodeSignature $setup
if ($sig.Status -ne 'Valid') {
    throw "setup-x86_64.exe signature is $($sig.Status); refusing to run it"
}
Write-Host "    signature OK: $($sig.SignerCertificate.Subject)"

# ---------------------------------------------------------------- 2. headless install
# xkeyboard-config/xkbcomp are hard dependencies of xorg-server, but naming them keeps the
# intent visible. xorg-x11-fonts-misc (the classic `misc` bitmap fonts incl. `fixed` and
# `cursor`) is *not* a dependency of the server, so it is requested explicitly. Base packages
# are installed too (setup has no way to skip them) and are simply not copied in step 3.
Write-Host "==> installing xorg-server into $root (this downloads ~100 MiB)"
$args = @(
    '--quiet-mode', '--no-admin', '--no-shortcuts', '--no-startmenu', '--no-desktop',
    '--no-write-registry', '--only-site', '--site', $Mirror,
    '--root', $root, '--local-package-dir', $pkgs,
    '--packages', 'xorg-server,xkeyboard-config,xkbcomp,xorg-x11-fonts-misc,dash'
)
$p = Start-Process -FilePath $setup -ArgumentList $args -Wait -PassThru -NoNewWindow
if ($p.ExitCode -ne 0) { throw "Cygwin setup exited with $($p.ExitCode)" }
$xwin = Join-Path $root 'bin\XWin.exe'
if (-not (Test-Path $xwin)) { throw "XWin.exe was not installed" }

# ---------------------------------------------------------------- 2b. XWin with SECURITY
# Cygwin builds xorg-server without the SECURITY extension (see the comment in its cygport
# file), so `xauth generate ... untrusted` and Ruxt's untrusted X11 forwarding cannot work
# with the stock binary. Rebuild just XWin.exe from the same source with -Dxcsecurity=true.
$securityBuilt = $false
if (-not $NoSecurityBuild) {
    Write-Host "==> rebuilding XWin.exe with the SECURITY extension (-Dxcsecurity=true)"
    $buildPkgs = 'gcc-core,gcc-g++,meson,ninja,pkg-config,bison,flex,make,ImageMagick,tar,bzip2,' +
        'xorgproto,windowsdriproto,libfontenc-devel,libfreetype-devel,libGL-devel,libnettle-devel,' +
        'libpixman1-devel,libtirpc-devel,libX11-devel,libXRes-devel,libXau-devel,libXaw-devel,' +
        'libXdmcp-devel,libXext-devel,libXfont2-devel,libXi-devel,libXinerama-devel,libXmu-devel,' +
        'libXpm-devel,libXrender-devel,libXtst-devel,libxcb-composite-devel,libxcb-ewmh-devel,' +
        'libxcb-icccm-devel,libxcb-image-devel,libxcb-keysyms-devel,libxcb-randr-devel,' +
        'libxcb-render-devel,libxcb-render-util-devel,libxcb-shape-devel,libxcb-util-devel,' +
        'libxcb-xfixes-devel,libxcb-xkb-devel,libxcvt-devel,libxkbfile-devel,font-util,' +
        'khronos-opengl-registry,python39-lxml,xkbcomp-devel,xtrans'
    $args = @(
        '--quiet-mode', '--no-admin', '--no-shortcuts', '--no-startmenu', '--no-desktop',
        '--no-write-registry', '--only-site', '--site', $Mirror,
        '--root', $root, '--local-package-dir', $pkgs, '--packages', $buildPkgs
    )
    # setup reports a failing postinstall script as exit code 1; the packages are installed
    # regardless, so the toolchain is verified below instead.
    $null = Start-Process -FilePath $setup -ArgumentList $args -Wait -PassThru -NoNewWindow
    foreach ($tool in 'bin\gcc.exe', 'bin\ninja.exe', 'bin\meson', 'bin\magick.exe') {
        if (-not (Test-Path (Join-Path $root $tool))) { throw "build tool missing: $tool" }
    }

    $srcDir = Join-Path $Scratch 'src'
    New-Item -ItemType Directory -Force $srcDir | Out-Null
    $tag = $XserverSource -replace '\.', '-'
    $tarball = Join-Path $srcDir "$XserverSource.tar.bz2"
    if (-not (Test-Path $tarball)) {
        $uri = "https://gitlab.freedesktop.org/jturney/xserver/-/archive/$tag/xserver-$tag.tar.bz2"
        Write-Host "    downloading $uri"
        # gitlab.freedesktop.org answers browser-like user agents (PowerShell's default starts
        # with "Mozilla/") with an Anubis bot-check page instead of the archive.
        Invoke-WebRequest -Uri $uri -OutFile $tarball -UseBasicParsing -UserAgent 'ruxt-fetch-xserver'
    }
    if ($XserverSha256) {
        $got = (Get-FileHash $tarball -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($got -ne $XserverSha256.ToLowerInvariant()) {
            $isHtml = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($tarball), 0, 15) -match '^<!doctype|^<html'
            Remove-Item $tarball -Force
            if ($isHtml) { throw "gitlab returned an HTML page (bot check?) instead of $XserverSource.tar.bz2" }
            throw "source tarball hash mismatch: got $got, expected $XserverSha256 (update -XserverSha256 together with -XserverSource)"
        }
        Write-Host "    tarball SHA-256 OK"
    }
    $srcTree = Join-Path $srcDir "xserver-$tag"
    if (-not (Test-Path $srcTree)) { tar -xjf $tarball -C $srcDir }
    if (-not (Test-Path $srcTree)) { throw "source tree $srcTree missing after extraction" }

    # Cygwin ships python3-lxml for python3.9 while /usr/bin/python3 is newer; point the GLX
    # wrapper generator at the python on PATH and put a 3.9 shim first on it.
    $glxMeson = Join-Path $srcTree 'hw\xwin\glx\meson.build'
    (Get-Content $glxMeson -Raw) -replace 'py3 = python3\.find_python\(\)', "py3 = find_program('python3')" |
        Set-Content $glxMeson -NoNewline -Encoding ascii
    $pybin = Join-Path $Scratch 'pybin'
    New-Item -ItemType Directory -Force $pybin | Out-Null
    foreach ($n in 'python3', 'python') {
        [System.IO.File]::WriteAllText((Join-Path $pybin $n), "#!/bin/sh`nexec /usr/bin/python3.9 `"`$@`"`n")
    }

    function ConvertTo-CygPath([string]$p) {
        $full = [System.IO.Path]::GetFullPath($p)
        '/cygdrive/' + $full.Substring(0, 1).ToLower() + ($full.Substring(2) -replace '\\', '/')
    }
    $srcCyg = ConvertTo-CygPath $srcTree
    $pybinCyg = ConvertTo-CygPath $pybin
    $buildSh = Join-Path $Scratch 'build-xwin.sh'
    $script = @"
set -euo pipefail
export PATH=${pybinCyg}:/usr/local/bin:/usr/bin:/bin
export LANG=C.UTF-8
chmod +x ${pybinCyg}/python3 ${pybinCyg}/python
cd '$srcCyg'
rm -rf _build
meson setup _build --prefix=/usr --buildtype=release \
  -Dpciaccess=false -Dint10=false -Dsystemd_logind=false \
  -Dxephyr=false -Dxnest=false -Dxvfb=false -Dxwin=true -Dxorg=false \
  -Dhal=false -Dudev=false -Ddri1=false -Ddri2=false -Ddri3=false \
  -Dxf86bigfont=true -Dxv=false -Dxvmc=false -Dglamor=false \
  -Dsha1=CryptoAPI -Dlog_dir=/var/log/xwin \
  -Ddefault_font_path=catalogue:/etc/X11/fontpath.d,built-ins \
  -Dxkb_default_rules=base -Dxkb_output_dir=/var/lib/xkb \
  -Dbuilder_addr=ruxt@localhost \
  "-Dbuilder_string=Ruxt build of $XserverSource with XCSECURITY, `$(date +%Y-%m-%d)" \
  "-Dvendor_name=The Cygwin/X Project" -Dvendor_name_short=Cygwin/X -Dvendor_web=https://x.cygwin.com/ \
  -Dxcsecurity=true
ninja -C _build hw/xwin/XWin.exe
grep -q '#define XCSECURITY 1' _build/include/dix-config.h
"@
    [System.IO.File]::WriteAllText($buildSh, ($script -replace "`r`n", "`n"))
    $bash = Join-Path $root 'bin\bash.exe'
    # Clear MSYS variables so a Git-for-Windows shell cannot leak its own mount table in.
    foreach ($v in 'MSYSTEM', 'MSYS', 'MINGW_PREFIX', 'MSYSTEM_PREFIX') { Remove-Item "Env:$v" -ErrorAction SilentlyContinue }
    & $bash --norc --noprofile ($buildSh -replace '\\', '/')
    if ($LASTEXITCODE -ne 0) { throw "XWin build failed (exit $LASTEXITCODE)" }
    $built = Join-Path $srcTree '_build\hw\xwin\XWin.exe'
    if (-not (Test-Path $built)) { throw "XWin.exe was not produced" }
    Copy-Item $built $xwin -Force
    $securityBuilt = $true
    Write-Host "    XWin.exe rebuilt with XCSECURITY"
}

# ---------------------------------------------------------------- 3. minimal tree
Write-Host "==> assembling minimal tree in $Dest"
if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
foreach ($d in 'bin', 'etc\X11', 'usr\share\X11\fonts', 'tmp', 'var\log\xwin', 'var\lib\xkb', 'licenses') {
    New-Item -ItemType Directory -Force (Join-Path $Dest $d) | Out-Null
}

# Executables XWin needs, plus every DLL cygcheck can trace from them. cygcheck is run with
# the root's bin on PATH so it can follow the whole chain.
$roots = @('XWin.exe', 'xkbcomp.exe', 'dash.exe', 'cygnativeGLthunk.dll')
$env:PATH = "$root\bin;$env:PATH"
$cygcheck = Join-Path $root 'bin\cygcheck.exe'
$binRoot = (Join-Path $root 'bin').ToLowerInvariant()
$needed = New-Object System.Collections.Generic.HashSet[string]
foreach ($r in $roots) { [void]$needed.Add($r) }
& $cygcheck ($roots | ForEach-Object { Join-Path $root "bin\$_" }) 2>$null |
    ForEach-Object { $_.Trim() } |
    Where-Object { $_.ToLowerInvariant().StartsWith($binRoot) } |
    ForEach-Object { [void]$needed.Add([System.IO.Path]::GetFileName($_)) }
foreach ($f in $needed) {
    $target = if ($f -eq 'dash.exe') { 'sh.exe' } else { $f }   # /bin/sh for xkbcomp's wrapper
    Copy-Item (Join-Path $root "bin\$f") (Join-Path $Dest "bin\$target")
}
Write-Host "    $($needed.Count) files in bin\"

Copy-Item (Join-Path $root 'etc\X11\system.XWinrc') (Join-Path $Dest 'etc\X11\')
Copy-Item -Recurse (Join-Path $root 'usr\share\X11\fonts\misc') (Join-Path $Dest 'usr\share\X11\fonts\misc')
Copy-Item -Recurse (Join-Path $root 'usr\share\X11\fonts\encodings') (Join-Path $Dest 'usr\share\X11\fonts\encodings')
Copy-Item (Join-Path $root 'usr\share\X11\XErrorDB') (Join-Path $Dest 'usr\share\X11\')
# /usr/share/X11/xkb is a Cygwin symlink to the versioned xkeyboard-config dir; materialise it.
$xkbSrc = Get-ChildItem (Join-Path $root 'usr\share') -Directory -Filter 'xkeyboard-config*' | Select-Object -First 1
if (-not $xkbSrc) { throw "xkeyboard-config data not found" }
Copy-Item -Recurse $xkbSrc.FullName (Join-Path $Dest 'usr\share\X11\xkb')

# Licences of what we ship.
foreach ($pkg in 'Cygwin', 'xorg-server', 'xkeyboard-config', 'xkbcomp', 'libX11', 'libfreetype6', 'libpixman1_0', 'libxcb1', 'libXfont2_2') {
    $doc = Join-Path $root "usr\share\doc\$pkg"
    if (Test-Path $doc) {
        $out = Join-Path $Dest "licenses\$pkg"
        New-Item -ItemType Directory -Force $out | Out-Null
        Get-ChildItem $doc -File | Where-Object { $_.Name -match '^(COPYING|LICENSE|COPYRIGHT)' } |
            Copy-Item -Destination $out
    }
}

# ---------------------------------------------------------------- 4. manifest
# Owning package (with version) of every file we copied, via `cygcheck -f`; plus the data
# packages whose files are not binaries.
$owned = & $cygcheck -f ($needed | ForEach-Object { Join-Path $root "bin\$_" }) 2>$null |
    ForEach-Object { $_.Trim() } | Where-Object { $_ }
$data = Get-Content (Join-Path $root 'etc\setup\installed.db') |
    Where-Object { $_ -match '^(xkeyboard-config|xorg-x11-fonts-misc|xorg-server-common) ' } |
    ForEach-Object { ($_ -split ' ')[1] -replace '\.tar\.(bz2|xz|zst)$', '' }
$installed = @($owned) + @($data) | Sort-Object -Unique
$lines = @(
    "Ruxt bundled X server (Cygwin/X), assembled $(Get-Date -Format 'yyyy-MM-dd HH:mm') from $Mirror",
    $(if ($securityBuilt) { "XWin.exe rebuilt from $XserverSource with -Dxcsecurity=true (X SECURITY extension for untrusted X11 forwarding)" } else { "XWin.exe is Cygwin's stock build (no SECURITY extension: untrusted X11 forwarding unavailable)" }),
    "",
    "Packages:",
    ($installed | Sort-Object | ForEach-Object { "  $_" }),
    "",
    "Files (SHA-256):"
)
Get-ChildItem $Dest -Recurse -File | Where-Object { $_.Name -ne 'MANIFEST.txt' } | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($Dest.Length + 1)
    $lines += "  $((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $rel"
}
Set-Content -Path (Join-Path $Dest 'MANIFEST.txt') -Value $lines -Encoding utf8

$size = (Get-ChildItem $Dest -Recurse -File | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("==> done: {0} ({1:N1} MiB)" -f $Dest, $size)
Write-Host "    xorg-server: $($installed | Where-Object { $_ -like 'xorg-server-2*' })"

if (-not $KeepScratch) {
    Write-Host "==> removing scratch $Scratch"
    Remove-Item -Recurse -Force $Scratch
}
