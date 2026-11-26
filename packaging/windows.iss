#define AppName "Яндекс Музыка · Rust"

[Setup]
AppId={{CCE980F8-0FF3-4981-ACEF-5D9B8291DD18}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=YandexMusicRust
AppPublisherURL=https://github.com/v8816/yandex-music-rust
DefaultDirName={localappdata}\Programs\Yandex Music Rust
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
DisableProgramGroupPage=yes
UsePreviousAppDir=yes
SetupIconFile=..\assets\app.ico
UninstallDisplayIcon={app}\yandex-music-rust.exe
OutputBaseFilename=YandexMusicRust-{#AppVersion}-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
#ifdef ReleaseSigning
SignTool=release
SignedUninstaller=yes
#endif

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#PlayerExe}"; DestDir: "{app}"; DestName: "yandex-music-rust.exe"; Flags: ignoreversion
Source: "..\LICENSE-UPSTREAM"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\yandex-music-rust.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\yandex-music-rust.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
