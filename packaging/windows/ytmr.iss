; Installateur Windows de ytmr (Inno Setup).
;
; En clair : installe ytmr.exe (dans Programmes, ou pour toi seul sans droits
; administrateur), crée le raccourci du menu Démarrer, propose un raccourci
; sur le Bureau et le lancement au démarrage de Windows, et ajoute la
; désinstallation dans « Applications installées ». mpv, yt-dlp et node ne
; sont pas inclus : ytmr se sert de ceux déjà installés et dit au premier
; lancement ce qui manque.
;
; Construit par build.ps1 : iscc /DAppVersion=0.1.0 /DExePath=…\ytmr.exe ytmr.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef ExePath
  #define ExePath "..\..\target\release\ytmr.exe"
#endif

[Setup]
AppId={{3FC4D580-A74D-4897-AD35-3C2B0F76430A}
AppName=ytmr
AppVersion={#AppVersion}
AppVerName=ytmr {#AppVersion}
AppPublisher=ytmr
DefaultDirName={autopf}\ytmr
DefaultGroupName=ytmr
DisableProgramGroupPage=yes
; Pour toi seul par défaut (aucun droit administrateur) ; « pour tous » au choix.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\..\dist
OutputBaseFilename=ytmr-setup-{#AppVersion}
SetupIconFile=ytmr.ico
UninstallDisplayIcon={app}\ytmr.exe
UninstallDisplayName=ytmr
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; ytmr ouvert pendant une mise à jour : Windows lui demande de se fermer.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "desktopicon"; Description: "Créer un raccourci sur le Bureau"; Flags: unchecked
Name: "autostart"; Description: "Lancer ytmr au démarrage de Windows"; Flags: unchecked

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\ytmr"; Filename: "{app}\ytmr.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\ytmr"; Filename: "{app}\ytmr.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
; Démarrage avec Windows (retiré à la désinstallation).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ytmr"; ValueData: """{app}\ytmr.exe"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
; Dossier de travail = celui de ytmr : celui de l'installateur (temporaire) est
; supprimé à sa fermeture, et Windows refusait alors de lancer mpv et yt-dlp.
Filename: "{app}\ytmr.exe"; WorkingDir: "{app}"; Description: "Lancer ytmr"; Flags: nowait postinstall skipifsilent

[Code]
// Désinstallation : ytmr est retiré, réglages et téléchargements restent,
// sauf si l'on choisit de tout effacer.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent then
  begin
    if MsgBox('Effacer aussi tes réglages, ton cache et tes morceaux téléchargés ?'
        + #13#10#13#10 + 'Non : ils restent pour une prochaine installation.',
        mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\ytmr'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\ytmr'), True, True, True);
    end;
  end;
end;
