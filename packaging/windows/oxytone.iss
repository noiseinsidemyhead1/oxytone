; Installateur Windows d'oxytone (Inno Setup).
;
; En clair : installe oxytone.exe (dans Programmes, ou pour toi seul sans
; droits administrateur), crée le raccourci du menu Démarrer, propose un
; raccourci sur le Bureau et le lancement au démarrage de Windows, et ajoute
; la désinstallation dans « Applications installées ». mpv, yt-dlp et node ne
; sont pas inclus : oxytone se sert de ceux déjà installés et dit au premier
; lancement ce qui manque.
;
; ytmr (l'ancien nom) déjà installé : il est fermé puis désinstallé, sans
; toucher à ses données (désinstallation silencieuse : elle ne propose pas de
; les effacer). oxytone les reprend à son premier lancement (déplacement des
; dossiers, voir `oxytone_core::migrate`). Le lancement avec Windows est
; gardé s'il était actif pour ytmr.
;
; Construit par build.ps1 : iscc /DAppVersion=0.1.0 /DExePath=…\oxytone.exe oxytone.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef ExePath
  #define ExePath "..\..\target\release\oxytone.exe"
#endif

[Setup]
; Nouvel identifiant : celui de ytmr était {3FC4D580-A74D-4897-AD35-3C2B0F76430A}.
AppId={{2424E58F-1DDC-4438-9DBD-BD0681C24221}
AppName=oxytone
AppVersion={#AppVersion}
AppVerName=oxytone {#AppVersion}
AppPublisher=oxytone
DefaultDirName={autopf}\oxytone
DefaultGroupName=oxytone
DisableProgramGroupPage=yes
; Pour toi seul par défaut (aucun droit administrateur) ; « pour tous » au choix.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\..\dist
OutputBaseFilename=oxytone-setup-{#AppVersion}
SetupIconFile=oxytone.ico
UninstallDisplayIcon={app}\oxytone.exe
UninstallDisplayName=oxytone
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; oxytone ouvert pendant une mise à jour : Windows lui demande de se fermer.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "desktopicon"; Description: "Créer un raccourci sur le Bureau"; Flags: unchecked
Name: "autostart"; Description: "Lancer oxytone au démarrage de Windows"; Flags: unchecked

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\oxytone"; Filename: "{app}\oxytone.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\oxytone"; Filename: "{app}\oxytone.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
; Démarrage avec Windows (retiré à la désinstallation).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "oxytone"; ValueData: """{app}\oxytone.exe"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
; Dossier de travail = celui d'oxytone : celui de l'installateur (temporaire)
; est supprimé à sa fermeture, et Windows refusait alors de lancer mpv et yt-dlp.
Filename: "{app}\oxytone.exe"; WorkingDir: "{app}"; Description: "Lancer oxytone"; Flags: nowait postinstall skipifsilent

[Code]
const
  YtmrUninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{3FC4D580-A74D-4897-AD35-3C2B0F76430A}_is1';
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

var
  // ytmr se lançait avec Windows : oxytone reprend le réglage.
  YtmrAutostart: Boolean;

function InitializeSetup(): Boolean;
begin
  YtmrAutostart := RegValueExists(HKCU, RunKey, 'ytmr');
  Result := True;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpSelectTasks) and YtmrAutostart then
    WizardSelectTasks('autostart');
end;

// Commande de désinstallation de ytmr, s'il est installé (pour soi ou pour tous).
function YtmrUninstaller(var Cmd: String): Boolean;
begin
  Result := RegQueryStringValue(HKCU, YtmrUninstallKey, 'UninstallString', Cmd)
    or RegQueryStringValue(HKLM, YtmrUninstallKey, 'UninstallString', Cmd);
end;

function YtmrRunning(): Boolean;
var
  Locator, Service, Procs: Variant;
begin
  Result := False;
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Procs := Service.ExecQuery('SELECT ProcessId FROM Win32_Process WHERE Name = ''ytmr.exe''');
    Result := Procs.Count > 0;
  except
    Log('Recherche de ytmr.exe impossible : ' + GetExceptionMessage);
  end;
end;

// ytmr n'enregistre sa session (file, position) qu'en quittant normalement :
// on lui demande d'abord de se fermer, puis on demande à l'utilisateur de le
// quitter (son icône près de l'horloge le garde ouvert quand on ferme la
// fenêtre). La fermeture forcée (processus et ses enfants : mpv, yt-dlp)
// n'est qu'un dernier recours.
procedure CloseYtmr();
var
  Code, Tries: Integer;
begin
  if not YtmrRunning() then
    Exit;
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM ytmr.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Tries := 0;
  while YtmrRunning() and (Tries < 10) do
  begin
    Sleep(500);
    Tries := Tries + 1;
  end;
  while YtmrRunning() and not WizardSilent() do
  begin
    if MsgBox('ytmr, l''ancienne version d''oxytone, est encore ouvert.'
        + #13#10#13#10 + 'Quitte-le (clic droit sur son icône près de l''horloge → Quitter), puis clique sur OK.'
        + #13#10#13#10 + 'Annuler : il sera fermé de force (la position de lecture en cours peut être perdue).',
        mbInformation, MB_OKCANCEL) = IDCANCEL then
      Break;
  end;
  if YtmrRunning() then
  begin
    Log('Fermeture forcée de ytmr');
    Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM ytmr.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
    Sleep(1000);
  end;
end;

// Avant l'installation : ytmr fermé, puis désinstallé en silence (ses données
// restent : seule sa désinstallation interactive propose de les effacer).
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Cmd: String;
  Code: Integer;
begin
  Result := '';
  if not YtmrUninstaller(Cmd) then
    Exit;
  CloseYtmr();
  if Exec(RemoveQuotes(Cmd), '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('ytmr désinstallé (code %d)', [Code]))
  else
    Log('Désinstallation de ytmr impossible : ' + SysErrorMessage(Code));
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // Installation silencieuse (pas de page des tâches) : le lancement avec
  // Windows de ytmr est repris tel quel.
  if (CurStep = ssPostInstall) and WizardSilent() and YtmrAutostart
      and not WizardIsTaskSelected('autostart') then
    RegWriteStringValue(HKCU, RunKey, 'oxytone', '"' + ExpandConstant('{app}\oxytone.exe') + '"');
end;

// Désinstallation : oxytone est retiré, réglages et téléchargements restent,
// sauf si l'on choisit de tout effacer.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    // Valeur posée par la migration ou l'installation silencieuse.
    RegDeleteValue(HKCU, RunKey, 'oxytone');
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent then
  begin
    if MsgBox('Effacer aussi tes réglages, ton cache et tes morceaux téléchargés ?'
        + #13#10#13#10 + 'Non : ils restent pour une prochaine installation.',
        mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\oxytone'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\oxytone'), True, True, True);
    end;
  end;
end;
