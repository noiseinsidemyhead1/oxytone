//! Intégration OS : MPRIS (Linux) et SMTC (Windows) via souvlaki.
//!
//! En clair : fait apparaître le morceau dans l'overlay média du système
//! (Windows : le panneau du volume et l'écran de verrouillage ; Linux : la
//! zone média du bureau) et branche les touches multimédia du clavier. Il
//! ajoute aussi les boutons du mini-lecteur de la barre des tâches (Windows)
//! et une notification à chaque nouveau morceau. Comme
//! la GUI et l'API, ce module ne fait qu'envoyer des commandes au moteur et
//! afficher son état.

pub mod discord;
#[cfg(windows)]
pub mod fullscreen;
mod notify;
pub mod secret;
#[cfg(windows)]
pub mod taskbar;
#[cfg(windows)]
pub mod tray;

pub use discord::DiscordSettings;
pub use notify::NotifySettings;

use std::ffi::c_void;
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use thiserror::Error;
use tokio::runtime::Handle;
use ytmr_core::{Command, CoreHandle, PlaybackState, Status};

#[derive(Debug, Error)]
pub enum OsError {
    #[error("contrôles média du système : {0}")]
    Controls(String),
}

/// Pas d'avance/recul quand le système ne précise pas la durée.
const SEEK_STEP: Duration = Duration::from_secs(10);

/// Lance l'intégration sur un fil dédié, pour toute la vie de l'appli.
///
/// `window` : handle natif de la fenêtre (HWND), exigé sous Windows.
pub fn start(
    core: CoreHandle,
    window: Option<usize>,
    runtime: Handle,
    notify: NotifySettings,
) -> Result<(), OsError> {
    #[cfg(windows)]
    taskbar::connect(core.clone(), runtime.clone());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("ytmr-os".into())
        .spawn(move || run(core, window, runtime, notify, ready_tx))
        .map_err(|e| OsError::Controls(e.to_string()))?;
    ready_rx
        .recv()
        .map_err(|_| OsError::Controls("fil arrêté au démarrage".into()))?
}

fn run(
    core: CoreHandle,
    window: Option<usize>,
    runtime: Handle,
    notify: NotifySettings,
    ready: std::sync::mpsc::Sender<Result<(), OsError>>,
) {
    let config = PlatformConfig {
        display_name: "ytmr",
        dbus_name: "ytmr",
        hwnd: window.map(|w| w as *mut c_void),
    };
    let mut controls = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(OsError::Controls(format!("{e:?}"))));
            return;
        }
    };
    let handler_core = core.clone();
    let handler_runtime = runtime.clone();
    let attached = controls.attach(move |event| {
        let state = handler_core.state();
        let Some(command) = command_for(event, &state) else {
            return;
        };
        let core = handler_core.clone();
        handler_runtime.spawn(async move {
            if let Err(e) = core.send(command).await {
                tracing::debug!(error = %e, "commande des touches média");
            }
        });
    });
    if let Err(e) = attached {
        let _ = ready.send(Err(OsError::Controls(format!("{e:?}"))));
        return;
    }
    let _ = ready.send(Ok(()));
    discord::start(notify.discord.clone());

    // On recopie l'état du moteur à chaque changement utile.
    let mut rx = core.watch_state();
    let mut shown = Shown {
        notify,
        runtime: Some(runtime.clone()),
        ..Shown::default()
    };
    loop {
        let state = rx.borrow_and_update().clone();
        shown.update(&mut controls, &state);
        if runtime.block_on(rx.changed()).is_err() {
            break; // moteur arrêté
        }
    }
}

/// Ce qui est déjà affiché par le système (pour ne rien renvoyer d'inutile).
#[derive(Default)]
struct Shown {
    video_id: Option<String>,
    status: Option<Status>,
    /// Instant et position du dernier envoi, pour repérer un saut (seek).
    anchor: Option<(std::time::Instant, Duration)>,
    notify: NotifySettings,
    runtime: Option<Handle>,
    /// Dernier morceau notifié (une bulle par morceau, même après une pause).
    notified: Option<String>,
}

impl Shown {
    fn update(&mut self, controls: &mut MediaControls, state: &PlaybackState) {
        let video_id = state.current.as_ref().map(|e| e.track.video_id.clone());
        if video_id != self.video_id {
            let result = match &state.current {
                Some(entry) => {
                    let track = &entry.track;
                    let cover = track.cover_url();
                    controls.set_metadata(MediaMetadata {
                        title: Some(&track.title),
                        artist: Some(&track.artist),
                        album: track.album.as_deref(),
                        cover_url: Some(&cover),
                        duration: state.duration.or(track.duration),
                    })
                }
                None => controls.set_metadata(MediaMetadata::default()),
            };
            if let Err(e) = result {
                tracing::debug!(error = ?e, "métadonnées pour le système");
            }
            self.video_id = video_id;
            self.status = None;
        }
        // La position avance seule pendant la lecture : on ne la renvoie qu'au
        // changement d'état ou après un saut.
        let expected = self.anchor.map(|(at, pos)| match self.status {
            Some(Status::Playing) => pos + at.elapsed(),
            _ => pos,
        });
        let jumped = expected.is_none_or(|e| e.abs_diff(state.position) > Duration::from_secs(2));
        if state.status == Status::Playing
            && self.video_id.is_some()
            && self.notified != self.video_id
        {
            if let Some(entry) = &state.current
                && self.notify.wanted()
                && let Some(runtime) = &self.runtime
            {
                notify::track_started(&entry.track, &self.notify, runtime);
            }
            self.notified = self.video_id.clone();
        }
        if self.status == Some(state.status) && !jumped {
            return;
        }
        #[cfg(windows)]
        taskbar::set_playing(state.status == Status::Playing);
        let progress = Some(MediaPosition(state.position));
        let playback = match state.status {
            Status::Playing => MediaPlayback::Playing { progress },
            Status::Idle => MediaPlayback::Stopped,
            _ => MediaPlayback::Paused { progress },
        };
        if let Err(e) = controls.set_playback(playback) {
            tracing::debug!(error = ?e, "état de lecture pour le système");
        }
        self.status = Some(state.status);
        self.anchor = Some((std::time::Instant::now(), state.position));
        discord::publish(state);
    }
}

fn command_for(event: MediaControlEvent, state: &PlaybackState) -> Option<Command> {
    let seek_by = |forward: bool, step: Duration| {
        let target = if forward {
            state.position + step
        } else {
            state.position.saturating_sub(step)
        };
        let target = match state.duration {
            Some(d) => target.min(d.saturating_sub(Duration::from_secs(1))),
            None => target,
        };
        Command::Seek(target)
    };
    Some(match event {
        MediaControlEvent::Play => Command::Play,
        MediaControlEvent::Pause | MediaControlEvent::Stop => Command::Pause,
        MediaControlEvent::Toggle => Command::TogglePause,
        MediaControlEvent::Next => Command::Next,
        MediaControlEvent::Previous => Command::Previous,
        MediaControlEvent::Seek(dir) => seek_by(dir == SeekDirection::Forward, SEEK_STEP),
        MediaControlEvent::SeekBy(dir, step) => seek_by(dir == SeekDirection::Forward, step),
        MediaControlEvent::SetPosition(MediaPosition(position)) => Command::Seek(position),
        MediaControlEvent::SetVolume(v) => {
            Command::SetVolume((v.clamp(0.0, 1.0) * 100.0).round() as u8)
        }
        MediaControlEvent::SetShuffle(on) => Command::SetShuffle(on),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_system_events_to_core_commands() {
        let state = PlaybackState {
            position: Duration::from_secs(5),
            duration: Some(Duration::from_secs(60)),
            ..PlaybackState::default()
        };
        assert!(matches!(
            command_for(MediaControlEvent::Toggle, &state),
            Some(Command::TogglePause)
        ));
        assert!(matches!(
            command_for(MediaControlEvent::Seek(SeekDirection::Backward), &state),
            Some(Command::Seek(d)) if d == Duration::ZERO
        ));
        assert!(matches!(
            command_for(MediaControlEvent::SetVolume(2.0), &state),
            Some(Command::SetVolume(100))
        ));
        assert!(command_for(MediaControlEvent::Quit, &state).is_none());
    }
}
