#define RedistName "vc_redist." + AppArch + ".exe"
#define RedistVersion GetVersionNumbersString(RedistPath)
#if RedistVersion == ""
  #error Could not read the Visual C++ Redistributable version
#endif

#if AppArch == "x64"
  #define TargetArchitecture "x64os"
#elif AppArch == "arm64"
  #define TargetArchitecture "arm64"
#else
  #error hearlier supports x64 and ARM64
#endif

[Setup]
AppId=hearlier
AppName=hearlier
AppVersion={#AppVersion}
AppPublisherURL=https://github.com/newbialywhodis/hearlier
AppSupportURL=https://github.com/newbialywhodis/hearlier/issues
AppUpdatesURL=https://github.com/newbialywhodis/hearlier/releases/latest
DefaultDirName={localappdata}\Programs\hearlier
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
MinVersion=10.0.22000
ArchitecturesAllowed={#TargetArchitecture}
ArchitecturesInstallIn64BitMode={#TargetArchitecture}
SetupIconFile=..\assets\hearlier.ico
UninstallDisplayIcon={app}\hearlier.exe
OutputBaseFilename=hearlier-v{#AppVersion}-win-{#AppArch}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
AppMutex=Local\RecallAudio.Session
SetupMutex=Local\Hearlier.Setup
CloseApplications=no

[Files]
Source: "{#RedistPath}"; DestName: "{#RedistName}"; Flags: dontcopy noencryption
Source: "{#AppDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\hearlier"; Filename: "{app}\hearlier.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\hearlier.exe"; Description: "Launch hearlier"; Flags: nowait postinstall skipifsilent

[Code]
const
  RedistKey = 'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\{#AppArch}';
  StartupKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  StartupName = 'Recall Audio';

function HasVisualCppRuntime: Boolean;
var
  Installed: Cardinal;
  Version: String;
  InstalledVersion, RequiredVersion: Int64;
begin
  Result := False;
  if not RegQueryDWordValue(HKLM64, RedistKey, 'Installed', Installed) or
     (Installed <> 1) or
     not RegQueryStringValue(HKLM64, RedistKey, 'Version', Version) then
    Exit;

  if (Length(Version) > 0) and (Version[1] = 'v') then
    Delete(Version, 1, 1);
  if StrToVersion(Version, InstalledVersion) and
     StrToVersion('{#RedistVersion}', RequiredVersion) then
    Result := ComparePackedVersion(InstalledVersion, RequiredVersion) >= 0;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Redist, Params: String;
  ExitCode: Integer;
begin
  Result := '';
  if HasVisualCppRuntime then
  begin
    Log('A compatible Microsoft Visual C++ Redistributable is already installed.');
    Exit;
  end;

  ExtractTemporaryFile('{#RedistName}');
  Redist := ExpandConstant('{tmp}\{#RedistName}');
  Params := '/install /passive /norestart';
  if WizardSilent then
    Params := '/install /quiet /norestart';
  if not ShellExec('runas', Redist, Params, '', SW_SHOWNORMAL,
                   ewWaitUntilTerminated, ExitCode) then
    Result := 'Could not start the Microsoft Visual C++ Redistributable. ' +
      SysErrorMessage(ExitCode)
  else if (ExitCode = 3010) or (ExitCode = 1641) then
  begin
    NeedsRestart := True;
    Result := 'Restart Windows and run hearlier Setup again to finish installation.';
  end
  else if (ExitCode <> 0) and not HasVisualCppRuntime then
    Result := Format('Microsoft Visual C++ Redistributable installation failed (code %d).', [ExitCode]);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command: String;
begin
  if (CurUninstallStep = usPostUninstall) and
     RegQueryStringValue(HKCU, StartupKey, StartupName, Command) and
     (CompareText(Command, ExpandConstant('"{app}\hearlier.exe"')) = 0) then
    RegDeleteValue(HKCU, StartupKey, StartupName);
end;
