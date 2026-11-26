; Zest's Windows installer (Inno Setup 6). Built by the release workflow:
;   iscc /DAppVersion=2026.11.1 /DDistDir=<folder with Zest.exe and zest-login.exe> package\zest.iss
; Output: package\dist\Zest-Setup.exe.
; Zest keeps its library next to Zest.exe (default_data_dir in grid-own/src/main.rs), so it installs per user into %USERPROFILE%\Zest, a folder the
; user can write to, and needs no administrator rights. Zest is a separate app from PlayDate: its own AppId, so it installs beside PlayDate and
; never over it.

#ifndef AppVersion
#error "AppVersion must be supplied: iscc /DAppVersion=X.Y.Z"
#endif
; the numbers-only version for Windows' file properties (2026.11.1 for 2026.11.1-beta.1)
#ifndef NumVersion
#define NumVersion AppVersion
#endif
#ifndef DistDir
#define DistDir "..\dist\windows"
#endif
#define AppName      "Zest"
#define AppPublisher "Zest"
#define AppURL       "https://github.com/RobbyRatpoison/zest-library-manager"
#define AppExeName   "Zest.exe"

[Setup]
; never change: it is how Windows and later installers recognise an existing Zest install
AppId={{6F3B2C1E-8A4D-4E7F-9B2A-5D1C0E9F7A36}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
AppUpdatesURL={#AppURL}

DefaultDirName={%USERPROFILE}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes

OutputDir=dist
OutputBaseFilename=Zest-Setup
SetupIconFile=..\grid-own\assets\zest.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}

Compression=lzma2/ultra64
SolidCompression=yes

WizardStyle=modern
DisableWelcomePage=no
DisableDirPage=no
DisableReadyPage=no

PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; a running Zest is asked to close (the in-app updater has already quit it)
CloseApplications=yes
RestartApplications=no
RestartIfNeededByRun=no

ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

VersionInfoVersion={#NumVersion}
VersionInfoCompany={#AppPublisher}
VersionInfoDescription={#AppName} Installer
VersionInfoProductName={#AppName}
VersionInfoProductVersion={#NumVersion}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "{#DistDir}\Zest.exe";       DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\zest-login.exe"; DestDir: "{app}"; Flags: ignoreversion

[UninstallDelete]
Type: filesandordirs; Name: "{app}\.rollback"

[Icons]
Name: "{group}\{#AppName}";     FileName: "{app}\{#AppExeName}"
Name: "{group}\Uninstall Zest"; FileName: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; FileName: "{app}\{#AppExeName}"; Tasks: desktopicon

[Run]
; after a normal install, the last page offers to start Zest; after a silent install (the in-app updater) Zest is started again by itself
Filename: "{app}\{#AppExeName}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\{#AppExeName}"; Flags: nowait; Check: WizardSilent

[Code]
{ Deletes every file in Dir matching Pattern. }
procedure DeleteMatching(Dir: String; Pattern: String);
var
  FindRec: TFindRec;
begin
  if FindFirst(Dir + '\' + Pattern, FindRec) then
  begin
    try
      repeat
        if (FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) = 0 then
          DeleteFile(Dir + '\' + FindRec.Name);
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;

{ Everything Zest saves next to the program: settings, the library databases, caches and state files, cover art (static), store data (plugins).
  In a folder named Zest (the default) everything left is Zest's, so the folder goes. In a folder the user chose with another name, only Zest's
  known files go, so nothing else in that folder is touched (a rare cache file may be left behind). }
procedure DeleteUserData(Dir: String);
begin
  if CompareText(ExtractFileName(RemoveBackslash(Dir)), 'Zest') = 0 then
  begin
    DelTree(Dir, True, True, True);
    exit;
  end;
  DeleteFile(Dir + '\config.json');
  DeleteFile(Dir + '\state.json');
  DeleteFile(Dir + '\theme.json');
  DeleteFile(Dir + '\santa_gifts.json');
  DeleteFile(Dir + '\steam_hltb_map.json');
  DeleteFile(Dir + '\humble_cache.json');
  DeleteFile(Dir + '\xbox_nexus_pending.json');
  DeleteFile(Dir + '\emulators.json');
  DeleteFile(Dir + '\zest.lock');
  DeleteMatching(Dir, 'steamgifts_wins_*.json');
  DeleteMatching(Dir, 'group_sources_*.json');
  DeleteMatching(Dir, 'games*.db*');
  DeleteMatching(Dir, 'zest.log*');
  DelTree(Dir + '\static', True, True, True);
  DelTree(Dir + '\plugins', True, True, True);
end;

var
  DeleteData: Boolean;

function InitializeUninstall(): Boolean;
var
  NL: String;
begin
  Result := True;
  DeleteData := False;
  NL := Chr(13) + Chr(10);
  { a silent uninstall never deletes the library }
  if UninstallSilent then
    exit;
  DeleteData := MsgBox('Do you also want to delete your Zest library and settings?' + NL + NL +
    'This includes your games list, settings, sign-ins, saved filters, shelves and cover art.' + NL + NL +
    'Click YES to delete everything.' + NL +
    'Click NO to keep them (Zest finds them again if you reinstall into the same folder).',
    mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  { after the program files are gone, so the folder can be removed whole }
  if (CurUninstallStep = usPostUninstall) and DeleteData then
    DeleteUserData(ExpandConstant('{app}'));
end;
