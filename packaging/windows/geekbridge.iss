; Inno Setup 6 script for the GeekBridge desktop installer. Built on CI only (windows-latest):
;   iscc /DAppVersion=0.1.0 /DBinDir=<dir with the .exe files> /DOutDir=<dir> packaging\windows\geekbridge.iss
; Per-user install (no admin rights). Not run on Windows by the authoring agent.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif
#ifndef OutDir
  #define OutDir "..\..\dist"
#endif

[Setup]
AppId={{6E0B1F1E-5B0D-4B52-9C55-6C1E6E2A7B10}
AppName=GeekBridge
AppVersion={#AppVersion}
AppPublisher=GeekBridge
AppPublisherURL=https://github.com/fspecii/geekbridge
DefaultDirName={autopf}\GeekBridge
DefaultGroupName=GeekBridge
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=geekbridge-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\geekbridge.exe
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "autostart"; Description: "Start the GeekBridge background service when I sign in"; GroupDescription: "Startup:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#BinDir}\geekbridge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-ctl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-calls.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-messages.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-clipboard.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-agenda.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-contacts.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\geekbridge-share.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{group}\GeekBridge"; Filename: "{app}\geekbridge.exe"
Name: "{group}\GeekBridge Share"; Filename: "{app}\geekbridge-share.exe"
Name: "{group}\Uninstall GeekBridge"; Filename: "{uninstallexe}"
Name: "{autodesktop}\GeekBridge"; Filename: "{app}\geekbridge.exe"; Tasks: desktopicon

[Registry]
; Run key: the per-user Startup mechanism. The service is a console program, so start it hidden.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "GeekBridgeService"; \
  ValueData: """{sys}\WindowsPowerShell\v1.0\powershell.exe"" -NoProfile -WindowStyle Hidden -Command ""Start-Process -WindowStyle Hidden -FilePath '{app}\geekbridge-service.exe'"""; \
  Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -WindowStyle Hidden -Command ""Start-Process -WindowStyle Hidden -FilePath '{app}\geekbridge-service.exe'"""; \
  Flags: runhidden nowait; Tasks: autostart
Filename: "{app}\geekbridge.exe"; Description: "Open GeekBridge"; Flags: postinstall nowait skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM geekbridge-service.exe"; Flags: runhidden; RunOnceId: "StopService"
