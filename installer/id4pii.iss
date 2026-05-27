#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#ifndef ChromeExtId
  #define ChromeExtId ""
#endif
#ifndef GitHubRepo
  #define GitHubRepo "TBLgGamin/id4pii"
#endif
#ifndef SignToolCmd
  #define SignToolCmd ""
#endif
#ifndef SignUninstaller
  #define SignUninstaller "no"
#endif

#define MyAppName "id4pii"
#define MyAppPublisher "TBLgGamin"
#define MyAppURL "https://github.com/" + GitHubRepo
#define MyAppExeName "id4pii.exe"
#define MyAppGuardExeName "id4pii-guard.exe"
#define ChromeUpdateUrl "https://clients2.google.com/service/update2/crx"

[Setup]
AppId={{9F2B5C90-1C7B-4F2D-9C2B-1D7E5B6C0001}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableDirPage=no
OutputDir=dist
OutputBaseFilename=id4pii-setup
SetupIconFile=..\assets\icon.ico
WizardImageFile=wizard-image.bmp
WizardSmallImageFile=wizard-small.bmp
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
PrivilegesRequired=admin
UninstallDisplayIcon={app}\{#MyAppGuardExeName}
UsedUserAreasWarning=no
CloseApplications=force
RestartApplications=no
#if SignToolCmd != ""
SignTool={#SignToolCmd}
SignedUninstaller={#SignUninstaller}
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Run id4pii guard on Windows login"; GroupDescription: "Startup:"
Name: "downloadmodel"; Description: "Download the PII model now (~875 MB)"; GroupDescription: "Model:"
Name: "registerextension"; Description: "Pre-register Chrome extension so Chrome offers to enable it"; GroupDescription: "Browser:"; Check: ChromeExtensionConfigured
Name: "desktopicon"; Description: "Create a Desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\{#MyAppGuardExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{group}\id4pii"; Filename: "{app}\{#MyAppGuardExeName}"; IconFilename: "{app}\{#MyAppGuardExeName}"; Comment: "Start id4pii guard in the system tray"
Name: "{group}\Open id4pii log folder"; Filename: "{localappdata}\id4pii\logs"; Comment: "View id4pii guard logs"
Name: "{group}\{cm:UninstallProgram,id4pii}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\id4pii"; Filename: "{app}\{#MyAppGuardExeName}"; IconFilename: "{app}\{#MyAppGuardExeName}"; Tasks: desktopicon

[Registry]
Root: HKLM; Subkey: "Software\Wow6432Node\Google\Chrome\Extensions\{#ChromeExtId}"; ValueType: string; ValueName: "update_url"; ValueData: "{#ChromeUpdateUrl}"; Flags: uninsdeletekey; Tasks: registerextension
Root: HKLM; Subkey: "Software\Google\Chrome\Extensions\{#ChromeExtId}"; ValueType: string; ValueName: "update_url"; ValueData: "{#ChromeUpdateUrl}"; Flags: uninsdeletekey; Tasks: registerextension
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "id4pii"; ValueData: """{app}\{#MyAppGuardExeName}"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#MyAppExeName}"; Parameters: "install --with-model"; Description: "Download the PII model"; Flags: runhidden waituntilterminated; Tasks: downloadmodel
Filename: "{app}\{#MyAppGuardExeName}"; Description: "Launch id4pii guard now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\{#MyAppExeName}"; Parameters: "uninstall"; RunOnceId: "id4pii_uninstall"; Flags: runhidden

[Code]
function ChromeExtensionConfigured(): Boolean;
begin
  Result := Length(ExpandConstant('{#ChromeExtId}')) > 0;
end;
