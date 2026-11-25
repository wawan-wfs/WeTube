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
OutputBaseFilename=WeTube-Desktop-Native-x64-online-setup
SetupIconFile=..\assets\streamtube.ico
LicenseFile=..\LICENSE
InfoBeforeFile=runtime-notice.txt
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
UninstallDisplayIcon={app}\wetube.exe
[Files]
Source: "..\target\release\wetube.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{tmp}\wetube-runtime\libmpv-2.dll"; DestDir: "{app}"; Flags: external ignoreversion; ExternalSize: 0
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "UPSTREAM-LICENSE.txt"; Flags: ignoreversion
Source: "runtime-notice.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
[Icons]
Name: "{group}\WeTube Desktop"; Filename: "{app}\wetube.exe"
Name: "{userdesktop}\WeTube Desktop"; Filename: "{app}\wetube.exe"; Tasks: desktopicon
[Tasks]
Name: "desktopicon"; Description: "Create desktop shortcut"

[Code]
const
  RuntimeURL = 'https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261111/mpv-dev-x86_64-20261111-git-bcde6b93cc.7z';
  ArchiveSHA = '273c079b11ea5575822f972756f4c9a71149a5c10130353abeb8586738938452';
  DllSHA = '88504ee5560942a955405e99dfb5971368e4cf851321f36565bf2a667271db1b';

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ArchivePath, RuntimeDir, DllPath, TarPath: String;
  ExitCode: Integer;
begin
  Result := '';
  ArchivePath := ExpandConstant('{tmp}\wetube-mpv.7z');
  RuntimeDir := ExpandConstant('{tmp}\wetube-runtime');
  DllPath := RuntimeDir + '\libmpv-2.dll';
  TarPath := ExpandConstant('{sys}\tar.exe');
  try
    if not FileExists(TarPath) then
      RaiseException('Windows tar.exe is missing. Windows 10 version 1803 or later is required.');
    ForceDirectories(RuntimeDir);
    if FileExists(DllPath) then
      DeleteFile(DllPath);
    DownloadTemporaryFile(RuntimeURL, 'wetube-mpv.7z', ArchiveSHA, nil);
    if CompareText(GetSHA256OfFile(ArchivePath), ArchiveSHA) <> 0 then
      RaiseException('Downloaded libmpv archive SHA-256 mismatch.');
    if not Exec(TarPath, '-xf "' + ArchivePath + '" -C "' + RuntimeDir + '" libmpv-2.dll',
      RuntimeDir, SW_HIDE, ewWaitUntilTerminated, ExitCode) then
      RaiseException('Windows could not start tar.exe.');
    if ExitCode <> 0 then
      RaiseException('Windows could not extract the verified libmpv archive. tar exit: ' + IntToStr(ExitCode));
    if not FileExists(DllPath) then
      RaiseException('Verified archive did not contain the required libmpv-2.dll.');
    if CompareText(GetSHA256OfFile(DllPath), DllSHA) <> 0 then
      RaiseException('Extracted libmpv DLL SHA-256 mismatch.');
    Log('WeTube: upstream runtime archive and DLL both verified.');
  except
    Result := 'WeTube runtime installation stopped: ' + GetExceptionMessage;
  end;
end;
