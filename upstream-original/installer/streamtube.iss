; Stream Tube installer (Inno Setup 6)
; Build:  ISCC.exe /DAppVersion=0.7.2-alpha installer\streamtube.iss
; Needs a release build in target\release and libmpv-2.dll in third_party\mpv.

#ifndef AppVersion
  #define AppVersion "0.7.2-alpha"
#endif
#define AppName "Stream Tube"
#define AppExe "streamtube.exe"
#define Root ".."

[Setup]
AppId={{6F1D7C1E-5B7A-4C1D-9A55-2E7A0C3B9F41}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Avaneesh Inamdar
AppPublisherURL=https://github.com/Avaneesh-Inamdar/Stream-Tube-Desktop
AppSupportURL=https://github.com/Avaneesh-Inamdar/Stream-Tube-Desktop/issues
AppUpdatesURL=https://github.com/Avaneesh-Inamdar/Stream-Tube-Desktop/releases
; per-user install: no admin prompt, and the in-app updater can replace files
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
OutputDir={#Root}\dist
OutputBaseFilename=StreamTube-{#AppVersion}-setup
SetupIconFile={#Root}\assets\streamtube.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
LicenseFile={#Root}\LICENSE
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#Root}\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\third_party\mpv\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion
; Khronos Vulkan loader, used only when Windows has no vulkan-1.dll (see src/player/mpv.rs)
Source: "{#Root}\third_party\vulkan\vulkan-1.dll"; DestDir: "{app}\vulkan"; Flags: ignoreversion
Source: "{#Root}\third_party\vulkan\LICENSE.txt"; DestDir: "{app}\vulkan"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; streamtube:// links (used by the browser userscript) open the app
Root: HKCU; Subkey: "Software\Classes\streamtube"; ValueType: string; ValueName: ""; ValueData: "URL:Stream Tube"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\streamtube"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\streamtube\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\streamtube\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""

[Run]
; also runs after a silent in-app update, so the app comes back by itself
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall

[UninstallDelete]
Type: filesandordirs; Name: "{app}"
