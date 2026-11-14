; Zest installer (Inno Setup 6).
;
; Built by the release workflow:  ISCC /DAppVersion=1.2.3 installer\zest.iss
; Installs for the current user by default, so no administrator rights are
; needed and the built-in updater can replace the files later.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6B1E7C52-3F0B-4B7E-9E0A-5A2D7C1F8E41}
AppName=Zest
AppVersion={#AppVersion}
AppVerName=Zest {#AppVersion}
AppPublisher=Pseudo Nymm
AppPublisherURL=https://skippynet.github.io/zest/
AppSupportURL=https://github.com/SkippyNet/zest/issues
AppUpdatesURL=https://github.com/SkippyNet/zest/releases
AppCopyright=Copyright 2026 Pseudo Nymm and the Zest contributors
VersionInfoVersion={#AppVersion}
VersionInfoDescription=Zest setup
DefaultDirName={autopf}\Zest
DefaultGroupName=Zest
DisableProgramGroupPage=yes
DisableWelcomePage=no
LicenseFile=..\LICENSE
; The privacy policy is shown before anything is installed. Its one automatic
; network use, the daily update check, can be unticked on the next page.
InfoBeforeFile=..\PRIVACY.md
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
OutputDir=..\dist
OutputBaseFilename=Zest-{#AppVersion}-x64-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\zest.exe
UninstallDisplayName=Zest
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
; The tray agent keeps zest.exe open; close it so the files can be replaced.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
; The only thing Zest does on the network by itself (see PRIVACY.md).
Name: "updatecheck"; Description: "Check for new versions of Zest once a day"; GroupDescription: "Updates:"

[Files]
Source: "..\target\release\zest.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\zest-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\NOTICE"; DestDir: "{app}"; DestName: "NOTICE.txt"; Flags: ignoreversion
Source: "..\PRIVACY.md"; DestDir: "{app}"; DestName: "PRIVACY.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Zest"; Filename: "{app}\zest.exe"
Name: "{autodesktop}\Zest"; Filename: "{app}\zest.exe"; Tasks: desktopicon

[Run]
; Carry out the choice made for "Check for new versions of Zest once a day",
; before Zest is started below. See AppliesUpdateChoice for when this runs.
Filename: "{app}\zest-cli.exe"; Parameters: "update auto on"; Tasks: updatecheck; Flags: runhidden runasoriginaluser; Check: AppliesUpdateChoice
Filename: "{app}\zest-cli.exe"; Parameters: "update auto off"; Tasks: not updatecheck; Flags: runhidden runasoriginaluser; Check: AppliesUpdateChoice
; After a normal install: offer to start Zest.
Filename: "{app}\zest.exe"; Description: "{cm:LaunchProgram,Zest}"; Flags: nowait postinstall skipifsilent
; After a silent install (that is how Zest updates itself): start it again.
; The agent exits by itself when smart cleaning is switched off.
; "--restarted" tells the new copy to wait for the old window to finish closing.
Filename: "{app}\zest.exe"; Parameters: "--restarted"; Flags: nowait; Check: ShouldRelaunch
Filename: "{app}\zest.exe"; Parameters: "--agent"; Flags: nowait; Check: ShouldRelaunch

[UninstallRun]
; Zest may have registered a scheduled clean; remove it with the program.
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /TN ""Zest\Scheduled Clean"" /F"; Flags: runhidden; RunOnceId: "ZestScheduledClean"

[Registry]
; The "start with Windows" entry of the agent, if the user switched it on.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Zest"; Flags: dontcreatekey uninsdeletevalue

[Code]
function ShouldRelaunch(): Boolean;
begin
  // /NOLAUNCH lets automated installs (and tests) skip the restart.
  Result := WizardSilent() and (Pos('/NOLAUNCH', Uppercase(GetCmdTail())) = 0);
end;

function AppliesUpdateChoice(): Boolean;
begin
  // With the wizard on screen the tick box is a choice the user just made.
  // A silent install has no tick box: Zest updates itself that way, and such
  // a run must leave the setting the user chose in the app alone. A silent
  // install only changes it when the command line names the task, as in
  //   /MERGETASKS="!updatecheck"   (off)   or   /MERGETASKS="updatecheck"   (on)
  Result := (not WizardSilent()) or (Pos('UPDATECHECK', Uppercase(GetCmdTail())) > 0);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    DataDir := ExpandConstant('{userappdata}\Zest');
    if DirExists(DataDir) and not UninstallSilent() then
    begin
      if MsgBox('Remove your Zest settings, history and registry backups as well?' + #13#10 + #13#10 + DataDir,
        mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(DataDir, True, True, True);
    end;
  end;
end;
