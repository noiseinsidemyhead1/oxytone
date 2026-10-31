//! Notification système au changement de morceau.
//!
//! En clair : quand un nouveau titre commence et que ytmr n'est pas au premier
//! plan, une petite bulle du système affiche sa pochette, le titre et l'artiste
//! (Windows : centre de notifications ; Linux : le démon de notifications du bureau).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::runtime::Handle;
use ytmr_core::Track;

/// Identifiant d'application des notifications Windows (enregistré au premier
/// envoi pour que la bulle porte le nom « ytmr », pas celui de PowerShell).
#[cfg(windows)]
const APP_ID: &str = "ytmr.player";

/// Réglages partagés avec l'interface.
#[derive(Clone, Default)]
pub struct NotifySettings {
    /// Notifications activées (Paramètres).
    pub enabled: Arc<AtomicBool>,
    /// Fenêtre au premier plan : inutile de notifier, le titre est déjà sous les yeux.
    pub focused: Arc<AtomicBool>,
    /// Cache de jaquettes (la pochette de la bulle), fourni une fois le backend prêt.
    pub covers: Arc<std::sync::OnceLock<Arc<ytmr_media::Covers>>>,
    /// Statut Discord « En train d'écouter ».
    pub discord: crate::DiscordSettings,
}

// À la main : le cache de jaquettes n'a pas de `Debug`.
impl std::fmt::Debug for NotifySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifySettings")
            .field("enabled", &self.enabled)
            .field("focused", &self.focused)
            .finish_non_exhaustive()
    }
}

impl NotifySettings {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            focused: Arc::new(AtomicBool::new(true)),
            covers: Arc::default(),
            discord: crate::DiscordSettings::default(),
        }
    }

    pub(crate) fn wanted(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && !self.focused.load(Ordering::Relaxed)
    }
}

/// Taille de la pochette de la bulle.
const COVER_PX: u32 = 192;

/// Affiche la bulle sans bloquer l'appelant.
pub(crate) fn track_started(track: &Track, settings: &NotifySettings, runtime: &Handle) {
    let title = track.title.clone();
    let covers = settings.covers.get().cloned();
    let cover_url = track.cover_url();
    let runtime = runtime.clone();
    let body = match &track.album {
        Some(album) => format!("{} · {album}", track.artist),
        None => track.artist.clone(),
    };
    let spawned = std::thread::Builder::new()
        .name("ytmr-notify".into())
        .spawn(move || {
            // Pochette : fichier du cache (téléchargé au besoin, 5 s au plus).
            let cover = covers.and_then(|covers| {
                let id = covers.register(&cover_url);
                let file = runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        covers.jpeg_file(&id, COVER_PX),
                    )
                    .await
                });
                file.ok()?.ok()
            });
            if let Err(e) = show(&title, &body, cover.as_deref()) {
                tracing::debug!(error = %e, "notification");
            }
        });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "notification");
    }
}

/// Bulle compacte façon Discord : petite pochette à gauche (`appLogoOverride`),
/// titre et artiste à droite, sans son (la musique joue déjà). notify-rust
/// posait l'image en grand au-dessus du texte.
#[cfg(windows)]
fn show(title: &str, body: &str, cover: Option<&std::path::Path>) -> Result<(), String> {
    use tauri_winrt_notification::{Duration, IconCrop, Toast};
    register_app_id();
    let mut toast = Toast::new(APP_ID)
        .title(title)
        .text1(body)
        .sound(None)
        .duration(Duration::Short);
    if let Some(cover) = cover {
        toast = toast.icon(cover, IconCrop::Square, "");
    }
    toast.show().map_err(|e| format!("{e:?}"))
}

#[cfg(not(windows))]
fn show(title: &str, body: &str, cover: Option<&std::path::Path>) -> Result<(), String> {
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body).appname("ytmr");
    if let Some(cover) = cover {
        n.image_path(&cover.to_string_lossy());
    }
    n.show().map(|_| ()).map_err(|e| e.to_string())
}

/// `HKCU\Software\Classes\AppUserModelId\ytmr.player` → `DisplayName = ytmr` :
/// c'est ainsi qu'une appli non empaquetée donne son nom à ses notifications.
#[cfg(windows)]
#[allow(unsafe_code)] // API du registre Win32.
fn register_app_id() {
    use std::sync::Once;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    use windows::core::{HSTRING, w};
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let key = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
        let name: Vec<u16> = "ytmr".encode_utf16().chain([0]).collect();
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &key,
                w!("DisplayName"),
                REG_SZ.0,
                Some(name.as_ptr().cast()),
                (name.len() * 2) as u32,
            )
        };
        if status.is_err() {
            tracing::debug!(?status, "nom des notifications");
        }
    });
}
