#ifndef AppVersion
  #define AppVersion "0.1.1"
#endif
[Setup]
AppId={{CEAC2846-C9D8-46D7-93C7-8099A312C1E7}
AppName=Poetrhymer
AppVersion={#AppVersion}
AppPublisher=SargeMadovsky
AppPublisherURL=https://github.com/SargeMadovsky/poetrhymer
AppUpdatesURL=https://github.com/SargeMadovsky/poetrhymer/releases
DefaultDirName={localappdata}\Programs\Poetrhymer
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=Poetrhymer-Setup-{#AppVersion}-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\Poetrhymer.exe
CloseApplications=yes
RestartApplications=no
SetupMutex=PoetrhymerSetup
SetupIconFile=..\assets\icon\poetrhymer.ico
[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
[Files]
Source: "..\dist\Poetrhymer.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "THIRD_PARTY_NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
[Icons]
Name: "{autoprograms}\Poetrhymer"; Filename: "{app}\Poetrhymer.exe"
Name: "{autodesktop}\Poetrhymer"; Filename: "{app}\Poetrhymer.exe"; Tasks: desktopicon
[Run]
Filename: "{app}\Poetrhymer.exe"; Description: "{cm:LaunchProgram,Poetrhymer}"; Flags: nowait postinstall skipifsilent
