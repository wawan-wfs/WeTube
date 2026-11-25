#define AppName "WeTube Desktop"
#define AppVersion "0.8.0"
[Setup]
AppId={{0F9A10D4-4D59-4CB5-9299-A6984FCF4AC0}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=WFS Project
DefaultDirName={localappdata}\Programs\WeTube Desktop Native
DefaultGroupName=WeTube Desktop
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename=WeTube-Desktop-Native-0.8.0-x64-setup
SetupIconFile=..\assets\streamtube.ico
LicenseFile=..\LICENSE
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
UninstallDisplayIcon={app}\wetube.exe
[Files]
Source: "..\target\release\wetube.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\third_party\mpv\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "UPSTREAM-LICENSE.txt"; Flags: ignoreversion
Source: "..\third_party\runtime-notices\*"; DestDir: "{app}\licenses"; Flags: recursesubdirs createallsubdirs
[Icons]
Name: "{group}\WeTube Desktop"; Filename: "{app}\wetube.exe"
Name: "{userdesktop}\WeTube Desktop"; Filename: "{app}\wetube.exe"; Tasks: desktopicon
[Tasks]
Name: "desktopicon"; Description: "Create desktop shortcut"
