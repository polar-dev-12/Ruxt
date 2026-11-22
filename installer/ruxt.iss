; Inno Setup script for Ruxt (built by .github/workflows/release.yml).
; Usage: ISCC.exe /DAppVersion=0.2.0 /DBinDir=target\release installer\ruxt.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\target\release"
#endif

[Setup]
AppId={{7E2C1F0A-5B7D-4F6A-9C3E-RUXT00000001}
AppName=Ruxt
AppVersion={#AppVersion}
AppPublisher=Ruxt
AppPublisherURL=https://github.com/polarbear138/ruxt
AppSupportURL=https://github.com/polarbear138/ruxt/issues
DefaultDirName={autopf}\Ruxt
DefaultGroupName=Ruxt
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE-MIT
OutputDir=..\dist
OutputBaseFilename=ruxt-{#AppVersion}-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
; The firewall rule and the uninstall cleanup name the X server that ruxt.exe unpacks into the
; installing user's %LOCALAPPDATA% (an elevated install keeps that user's profile).
UsedUserAreasWarning=no
UninstallDisplayIcon={app}\ruxt.exe
MinVersion=10.0

[Languages]
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; ruxt.exe carries the Cygwin/X tree (see build.rs) and unpacks it on first use.
Source: "{#BinDir}\ruxt.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\*"; DestDir: "{app}\docs"; Flags: ignoreversion recursesubdirs
Source: "..\README.ko.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\SECURITY.md"; DestDir: "{app}"; Flags: ignoreversion

[InstallDelete]
; Up to 0.5.0 the X server was installed beside ruxt.exe. Left behind, that old tree would take
; precedence over the one the new ruxt.exe carries.
Type: filesandordirs; Name: "{app}\xserver"

[Icons]
Name: "{group}\Ruxt"; Filename: "{app}\ruxt.exe"
Name: "{group}\{cm:UninstallProgram,Ruxt}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Ruxt"; Filename: "{app}\ruxt.exe"; Tasks: desktopicon

[Run]
; X.org listens on every interface (there is no loopback-only switch). Windows Firewall never
; filters loopback traffic, so an inbound *block* rule for XWin.exe closes the port to the LAN
; while Ruxt's own X11 bridge (127.0.0.1) keeps working. Needs an administrative install;
; otherwise Ruxt offers to add the rule from its X menu (UAC prompt).
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Ruxt X server (block inbound)"""; Flags: runhidden; Check: IsAdminInstallMode
Filename: "netsh"; Parameters: "advfirewall firewall add rule name=""Ruxt X server (block inbound)"" dir=in action=block program=""{localappdata}\ruxt\data\xserver\bin\XWin.exe"" enable=yes profile=any"; Flags: runhidden; Check: IsAdminInstallMode
Filename: "{app}\ruxt.exe"; Description: "{cm:LaunchProgram,Ruxt}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Ruxt X server (block inbound)"""; Flags: runhidden; RunOnceId: "ruxt-xserver-fwrule"; Check: IsAdminInstallMode

[UninstallDelete]
; Configuration, the credential vault and known_hosts stay in %APPDATA%\ruxt on purpose.
Type: filesandordirs; Name: "{app}\docs"
Type: filesandordirs; Name: "{app}\xserver"
; The unpacked X server is only a copy of what ruxt.exe carried.
Type: filesandordirs; Name: "{localappdata}\ruxt\data\xserver"
