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
DisableProgramGroupPage=yes
DisableDirPage=no
OutputDir=dist
OutputBaseFilename=id4pii-setup
SetupIconFile=..\assets\icon.ico
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
PrivilegesRequired=admin
UninstallDisplayIcon={app}\{#MyAppExeName}
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

[Files]
Source: "..\target\release\id4pii.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Registry]
Root: HKLM; Subkey: "Software\Wow6432Node\Google\Chrome\Extensions\{#ChromeExtId}"; ValueType: string; ValueName: "update_url"; ValueData: "{#ChromeUpdateUrl}"; Flags: uninsdeletekey; Tasks: registerextension
Root: HKLM; Subkey: "Software\Google\Chrome\Extensions\{#ChromeExtId}"; ValueType: string; ValueName: "update_url"; ValueData: "{#ChromeUpdateUrl}"; Flags: uninsdeletekey; Tasks: registerextension
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "id4pii"; ValueData: """{app}\{#MyAppExeName}"" guard"; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#MyAppExeName}"; Parameters: "install --with-model"; Description: "Download the PII model"; Flags: runhidden waituntilterminated; Tasks: downloadmodel
Filename: "{app}\{#MyAppExeName}"; Parameters: "guard"; Description: "Launch id4pii guard now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\{#MyAppExeName}"; Parameters: "uninstall"; Flags: runhidden

[Code]
function ChromeExtensionConfigured(): Boolean;
begin
  Result := Length(ExpandConstant('{#ChromeExtId}')) > 0;
end;
