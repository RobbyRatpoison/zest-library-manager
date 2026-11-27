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
; no "Install for all users" choice: an all-users install goes to Program Files, which a normal user cannot write to, and Zest keeps its
; library next to Zest.exe. (Found on the test VM, 2026-11-26.)
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

[Messages]
; shown when something is left in the install folder: normally the library and settings the user chose to keep
UninstalledMost=%1 was removed.%n%nYour library and settings were kept in its folder, so reinstalling Zest finds them again.

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "{#DistDir}\Zest.exe";       DestDir: "{app}"; Flags: ignoreversion
Source: "{#DistDir}\zest-login.exe"; DestDir: "{app}"; Flags: ignoreversion
; only a MinGW build (the Linux cross-builds used for testing) needs the WebView2 loader as a separate file; the release build (MSVC) links it
; into zest-login.exe, so the file is not there and nothing is installed
Source: "{#DistDir}\WebView2Loader.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

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

{ True for the install's own program files, which the uninstaller removes itself (and the uninstaller's own files). }
function IsProgramFile(Name: String): Boolean;
begin
  Result := (CompareText(Name, 'Zest.exe') = 0) or (CompareText(Name, 'zest-login.exe') = 0) or
    (CompareText(Name, 'WebView2Loader.dll') = 0) or (CompareText(Copy(Name, 1, 5), 'unins') = 0);
end;

{ Everything Zest saves next to the program: settings, the library databases, caches and state files, cover art (static), store data (plugins),
  the sign-in window's browser data (zest-login.exe.WebView2). In a folder named Zest (the default) everything except the program files is
  Zest's, so all of it goes and the uninstaller then removes the emptied folder. In a folder the user chose with another name, only Zest's known
  files go, so nothing else in that folder is touched (a rare cache file may be left behind). Runs before the uninstaller removes the program
  files, so its final message reflects what is really left. }
procedure DeleteUserData(Dir: String);
var
  FindRec: TFindRec;
begin
  if CompareText(ExtractFileName(RemoveBackslash(Dir)), 'Zest') = 0 then
  begin
    if FindFirst(Dir + '\*', FindRec) then
    begin
      try
        repeat
          if (FindRec.Name <> '.') and (FindRec.Name <> '..') and not IsProgramFile(FindRec.Name) then
          begin
            if (FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then
              DelTree(Dir + '\' + FindRec.Name, True, True, True)
            else
              DeleteFile(Dir + '\' + FindRec.Name);
          end;
        until not FindNext(FindRec);
      finally
        FindClose(FindRec);
      end;
    end;
    exit;
  end;
  DeleteFile(Dir + '\owned_checked.json');
  DeleteFile(Dir + '\zest_sync_status.json');
  DeleteFile(Dir + '\zest_secrets.json');
  DelTree(Dir + '\.rollback', True, True, True);
  DelTree(Dir + '\zest-login.exe.WebView2', True, True, True);
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

{ Is the Zest installed here running? A running Zest.exe cannot be deleted, and it would keep writing to the library. Asked through WMI by the
  program's full path, so another Zest elsewhere (a portable copy) does not count. }
function ZestRunning(): Boolean;
var
  Locator, Service, Procs, P: Variant;
  I: Integer;
  Exe: String;
begin
  Result := False;
  Exe := ExpandConstant('{app}\Zest.exe');
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Procs := Service.ExecQuery('SELECT ExecutablePath FROM Win32_Process WHERE Name = ''Zest.exe''');
    for I := 0 to Procs.Count - 1 do
    begin
      P := Procs.ItemIndex(I);
      if not VarIsNull(P.ExecutablePath) then
        if CompareText(P.ExecutablePath, Exe) = 0 then
        begin
          Result := True;
          exit;
        end;
    end;
  except
    Result := False;
  end;
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
  { Zest must be closed first: a running Zest.exe cannot be removed }
  while ZestRunning() do
  begin
    if UninstallSilent or (MsgBox('Zest is running. Close Zest, then click Retry.', mbError, MB_RETRYCANCEL) <> IDRETRY) then
    begin
      Result := False;
      exit;
    end;
  end;
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
  { before the program files go: the uninstaller then removes the emptied folder, and its last message reflects what is really left }
  if (CurUninstallStep = usUninstall) and DeleteData then
    DeleteUserData(ExpandConstant('{app}'));
  { the uninstaller only removes folders it created; a reinstall into a folder that held kept data did not create it, so the emptied folder
    would stay. RemoveDir only removes an empty folder. (Found on the test VM, 2026-11-26.) }
  if (CurUninstallStep = usPostUninstall) and DeleteData then
    RemoveDir(ExpandConstant('{app}'));
end;
