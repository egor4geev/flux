; Flux's installer for Windows (part 10.9), for Inno Setup 6.3 or newer. Per user, without
; administrator rights: Flux goes to %LOCALAPPDATA%\Programs\Flux, with a Start menu shortcut, an
; uninstaller (Flux's settings, projects and plugins in %LOCALAPPDATA%\Flux stay) and, if chosen,
; "Open with Flux" in the menu of folders. Built by CI next to the zip
; (.github/workflows/release.yml):
;
;   iscc /DVersion=0.3.0 /DArch=x64 scripts\windows\flux.iss
;     dist\app\{Flux.exe,LICENSE.txt} → dist\Flux-0.3.0-windows-x86_64-setup.exe
;
; Arch is the processor Flux.exe was built for: x64 (installs on Windows on ARM too, emulated) or
; arm64. AppDir (what the zip holds) and OutputDir may be given too: absolute, or relative to this
; file's folder. Unsigned: Windows SmartScreen asks once (More info → Run anyway). Updates come over
; the air afterwards (crates/flux-update); a newer installer also installs over an older Flux.
;
; This file is UTF-8 with a byte order mark: the compiler reads the Russian texts right only so.

#ifndef Version
  #error Pass the version: iscc /DVersion=X.Y.Z
#endif
#ifndef Arch
  #define Arch "x64"
#endif
#ifndef AppDir
  #define AppDir "..\..\dist\app"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#if Arch == "arm64"
  #define ArchName "aarch64"
  #define Architectures "arm64"
#elif Arch == "x64"
  #define ArchName "x86_64"
  #define Architectures "x64compatible"
#else
  #error Arch is x64 or arm64
#endif

[Setup]
; The same for every version and processor: a newer installer updates the installed Flux.
AppId={{83E3E79D-88ED-4703-9005-0B506848A564}
AppName=Flux
AppVersion={#Version}
AppVerName=Flux {#Version}
AppPublisher=Egor Ageev
AppPublisherURL=https://github.com/egor4geev/flux
AppSupportURL=https://github.com/egor4geev/flux/issues
AppUpdatesURL=https://github.com/egor4geev/flux/releases
AppCopyright=Copyright (c) 2026 Egor Ageev
; No administrator: {autopf} is the user's own %LOCALAPPDATA%\Programs, {autoprograms} their
; Start menu, HKA their part of the registry (HKCU).
PrivilegesRequired=lowest
DefaultDirName={autopf}\Flux
DisableProgramGroupPage=yes
ArchitecturesAllowed={#Architectures}
ArchitecturesInstallIn64BitMode={#Architectures}
; Windows 10 1903: the UTF-8 code page of Flux.exe's manifest.
MinVersion=10.0.18362
OutputDir={#OutputDir}
OutputBaseFilename=Flux-{#Version}-windows-{#ArchName}-setup
SetupIconFile=..\..\crates\flux-app\resources\windows\flux.ico
UninstallDisplayIcon={app}\Flux.exe
UninstallDisplayName=Flux
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; A running Flux is asked to quit before its files are replaced, and started again after.
CloseApplications=yes
RestartApplications=yes
; "Open with Flux": Explorer re-reads the menus of folders.
ChangesAssociations=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[CustomMessages]
en.OpenWithFlux=Open with Flux
ru.OpenWithFlux=Открыть в Flux
en.OpenWithFluxTask=Add "Open with Flux" to the menu of folders
ru.OpenWithFluxTask=Добавить «Открыть в Flux» в меню папок
en.Explorer=Explorer:
ru.Explorer=Проводник:

[Tasks]
Name: "openwith"; Description: "{cm:OpenWithFluxTask}"; GroupDescription: "{cm:Explorer}"; Flags: unchecked

[Files]
Source: "{#AppDir}\Flux.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#AppDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The id Flux sets on its process (crates/flux-app/src/main.rs): its windows group under the
; pinned shortcut, its notifications are Flux's.
Name: "{autoprograms}\Flux"; Filename: "{app}\Flux.exe"; AppUserModelID: "Flux.Flux"

[Registry]
; A folder's menu, and the menu of a folder's background (inside an open folder).
Root: HKA; Subkey: "Software\Classes\Directory\shell\Flux"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithFlux}"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\shell\Flux"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\Flux.exe"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\shell\Flux\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Flux.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Flux"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithFlux}"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Flux"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\Flux.exe"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Flux\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Flux.exe"" ""%V"""; Tasks: openwith

[Run]
Filename: "{app}\Flux.exe"; Description: "{cm:LaunchProgram,Flux}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; What updates over the air leave in the folder: the previous version's files, moved aside while
; it ran (crates/flux-update, install.rs); they are deleted at Flux's next start anyway.
Type: filesandordirs; Name: "{app}\.flux-old-*"
Type: filesandordirs; Name: "{app}\.flux-new-*"
Type: dirifempty; Name: "{app}"
