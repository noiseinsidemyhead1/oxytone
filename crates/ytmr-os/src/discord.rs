//! Statut Discord « En train d'écouter » (Rich Presence).
//!
//! En clair : si le réglage est actif et Discord ouvert sur la machine, ton
//! profil Discord montre le titre, l'artiste, la pochette et le temps restant
//! du morceau, comme avec Spotify. En pause, le statut s'efface. Discord exige
//! l'identifiant d'une « application » créée sur discord.com/developers (son
//! nom est celui affiché : « Écoute ytmr »).
//!
//! Tout se passe sur un fil à part : Discord fermé ou absent ne ralentit rien,
//! on réessaie simplement plus tard.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{
    Activity, ActivityType, Assets, StatusDisplayType, Timestamps,
};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use ytmr_core::{PlaybackState, Status};

/// Discord absent : nouvel essai de connexion après ce délai.
const RETRY: Duration = Duration::from_secs(20);

/// Réglages partagés avec l'interface.
#[derive(Debug, Clone, Default)]
pub struct DiscordSettings {
    pub enabled: Arc<AtomicBool>,
    /// Identifiant d'application Discord (Application ID).
    pub client_id: Arc<Mutex<String>>,
}

impl DiscordSettings {
    fn wanted(&self) -> Option<String> {
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        let id = self
            .client_id
            .lock()
            .map(|g| g.trim().to_owned())
            .unwrap_or_default();
        (!id.is_empty()).then_some(id)
    }
}

/// Ce qu'on montre sur Discord.
#[derive(Debug, Clone, PartialEq)]
struct Presence {
    title: String,
    artist: String,
    album: Option<String>,
    cover: Option<String>,
    /// Début et fin (secondes Unix) pour le temps restant.
    start: i64,
    end: Option<i64>,
}

enum Msg {
    /// Nouvel état (None : rien en lecture ou en pause).
    State(Option<Presence>),
    /// Réglages modifiés : on réapplique.
    Settings,
}

static SENDER: OnceLock<Mutex<Sender<Msg>>> = OnceLock::new();

/// Démarre le fil Discord (une fois).
pub(crate) fn start(settings: DiscordSettings) {
    let (tx, rx) = channel();
    if SENDER.set(Mutex::new(tx)).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("ytmr-discord".into())
        .spawn(move || run(&settings, &rx));
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "fil Discord");
    }
}

fn send(msg: Msg) {
    if let Some(tx) = SENDER.get()
        && let Ok(tx) = tx.lock()
    {
        let _ = tx.send(msg);
    }
}

/// Réglage changé dans l'interface (activé, coupé, autre identifiant).
pub fn settings_changed() {
    send(Msg::Settings);
}

/// État du lecteur (appelé à chaque changement utile : morceau, pause, saut).
pub(crate) fn publish(state: &PlaybackState) {
    let presence = state
        .current
        .as_ref()
        .filter(|_| state.status == Status::Playing)
        .map(|entry| {
            let track = &entry.track;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let start = now - state.position.as_secs() as i64;
            Presence {
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: track.album.clone(),
                cover: cover_url(&track.cover_url()),
                start,
                end: state
                    .duration
                    .or(track.duration)
                    .map(|d| start + d.as_secs() as i64),
            }
        });
    send(Msg::State(presence));
}

/// Pochette assez grande pour Discord ; adresses courtes seulement (Discord
/// refuse au-delà de 256 caractères).
fn cover_url(url: &str) -> Option<String> {
    let url = if url.contains("googleusercontent.com") || url.contains("ggpht.com") {
        let base = url.split('=').next().unwrap_or(url);
        format!("{base}=w512-h512-l90-rj")
    } else {
        // Vignettes vidéo : sans les paramètres signés.
        url.split('?').next().unwrap_or(url).to_owned()
    };
    (url.starts_with("https://") && url.len() <= 256).then_some(url)
}

fn run(settings: &DiscordSettings, rx: &Receiver<Msg>) {
    let mut wanted: Option<Presence> = None;
    let mut client: Option<(String, DiscordIpcClient)> = None;
    let mut shown: Option<Option<Presence>> = None;
    loop {
        let timeout = if client.is_none() && settings.wanted().is_some() {
            RETRY
        } else {
            Duration::from_secs(3600)
        };
        match rx.recv_timeout(timeout) {
            Ok(Msg::State(p)) => wanted = p,
            Ok(Msg::Settings) => shown = None,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let Some(id) = settings.wanted() else {
            // Coupé : on efface et on se déconnecte.
            if let Some((_, mut c)) = client.take() {
                let _ = c.clear_activity();
                let _ = c.close();
            }
            shown = None;
            continue;
        };
        // Autre identifiant : nouvelle connexion.
        if client.as_ref().is_some_and(|(cid, _)| *cid != id) {
            if let Some((_, mut c)) = client.take() {
                let _ = c.close();
            }
            shown = None;
        }
        if client.is_none() {
            let mut c = DiscordIpcClient::new(&id);
            match c.connect() {
                Ok(()) => {
                    tracing::info!("Discord connecté");
                    client = Some((id, c));
                    shown = None;
                }
                // Discord fermé : on réessaiera.
                Err(e) => {
                    tracing::debug!(error = %e, "Discord indisponible");
                    continue;
                }
            }
        }
        if shown.as_ref() == Some(&wanted) {
            continue;
        }
        let Some((_, c)) = client.as_mut() else {
            continue;
        };
        let result = match &wanted {
            Some(p) => c.set_activity(activity(p)),
            None => c.clear_activity(),
        };
        match result {
            Ok(()) => shown = Some(wanted.clone()),
            Err(e) => {
                // Discord fermé entre-temps : reconnexion au prochain tour.
                tracing::debug!(error = %e, "statut Discord");
                client = None;
            }
        }
    }
}

fn activity(p: &Presence) -> Activity<'_> {
    let mut timestamps = Timestamps::new().start(p.start);
    if let Some(end) = p.end {
        timestamps = timestamps.end(end);
    }
    let mut assets = Assets::new();
    if let Some(cover) = &p.cover {
        assets = assets.large_image(cover.as_str());
    }
    if let Some(album) = &p.album {
        assets = assets.large_text(album.as_str());
    }
    Activity::new()
        .activity_type(ActivityType::Listening)
        .status_display_type(StatusDisplayType::Details)
        .details(p.title.as_str())
        .state(p.artist.as_str())
        .timestamps(timestamps)
        .assets(assets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_urls_are_short_and_large() {
        assert_eq!(
            cover_url("https://lh3.googleusercontent.com/abc=w120-h120-l90-rj").as_deref(),
            Some("https://lh3.googleusercontent.com/abc=w512-h512-l90-rj")
        );
        assert_eq!(
            cover_url("https://i.ytimg.com/vi/x/hqdefault.jpg?sqp=long&rs=sig").as_deref(),
            Some("https://i.ytimg.com/vi/x/hqdefault.jpg")
        );
        assert!(cover_url(&format!("https://e.com/{}", "a".repeat(300))).is_none());
    }
}
