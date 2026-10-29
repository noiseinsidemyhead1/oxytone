//! Implémentation de [`Player`] avec mpv lancé en processus fils.
//!
//! En clair : on démarre mpv en arrière-plan, sans fenêtre, en lui demandant
//! d'ouvrir un canal de télécommande. On lui donne des adresses de flux déjà
//! résolues (voir `resolve`) ; le morceau suivant est placé dans sa playlist
//! pour qu'il le précharge et l'enchaîne sans blanc. Si mpv plante, seul ce
//! lecteur s'arrête, pas l'application.

mod ipc;

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Child;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::deps::{Dependencies, JsRuntime};
use crate::{EndReason, EntryId, MediaSource, Player, PlayerError, PlayerEvent};
use ipc::IpcClient;

/// Écart minimal entre deux [`PlayerEvent::Position`] : mpv en envoie des dizaines par seconde.
const POSITION_STEP: Duration = Duration::from_millis(250);
const QUIT_TIMEOUT: Duration = Duration::from_secs(3);

/// Propriétés mpv suivies, avec leur identifiant d'observation.
const OBSERVED: [(u64, &str); 4] = [
    (1, "pause"),
    (2, "time-pos"),
    (3, "duration"),
    (4, "volume"),
];

pub struct MpvPlayer {
    ipc: IpcClient,
    child: Mutex<Child>,
    events: broadcast::Sender<PlayerEvent>,
}

impl MpvPlayer {
    /// Lance mpv et attend que son canal IPC réponde.
    ///
    /// `fallback_client` et `ytdl_raw_options` ne servent qu'au repli, quand mpv
    /// doit résoudre lui-même une URL YouTube via son hook yt-dlp.
    pub async fn spawn(
        deps: &Dependencies,
        volume: u8,
        fallback_client: Option<&str>,
        ytdl_raw_options: &[String],
    ) -> Result<Self, PlayerError> {
        let volume = check_volume(volume)?;
        let ipc_path = ipc::ipc_path();

        let mut cmd = crate::process::command(&deps.mpv.path);
        cmd.args([
            "--idle=yes",
            "--no-video",
            "--no-terminal",
            "--keep-open=no",
            // Ouvre l'entrée suivante de la playlist avant la fin de la courante.
            "--prefetch-playlist=yes",
            "--gapless-audio=yes",
            "--ytdl-format=bestaudio/best",
        ])
        .arg(format!("--input-ipc-server={}", ipc_path.display()))
        .arg(format!("--volume={volume}"))
        .arg(format!(
            "--script-opts-append=ytdl_hook-ytdl_path={}",
            deps.ytdlp.path.display()
        ));
        if deps.js_runtime == Some(JsRuntime::Node) {
            cmd.arg("--ytdl-raw-options-append=js-runtimes=node");
        }
        if let Some(client) = fallback_client {
            cmd.arg(format!(
                "--ytdl-raw-options-append=extractor-args=youtube:player_client={client}"
            ));
        }
        for option in ytdl_raw_options {
            cmd.arg(format!("--ytdl-raw-options-append={option}"));
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        tracing::debug!(?cmd, "lancement de mpv");
        let mut child = cmd.spawn().map_err(PlayerError::Spawn)?;

        let (raw_tx, raw_rx) = mpsc::unbounded_channel();
        let ipc = tokio::select! {
            res = IpcClient::connect(&ipc_path, raw_tx) => res?,
            status = child.wait() => {
                return Err(PlayerError::ExitedEarly(
                    status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string()),
                ));
            }
        };

        let (events, _) = broadcast::channel(256);
        tokio::spawn(translate_events(raw_rx, events.clone()));

        for (id, name) in OBSERVED {
            ipc.command(json!(["observe_property", id, name])).await?;
        }
        tracing::info!(mpv = %deps.mpv.version, ytdlp = %deps.ytdlp.version, "mpv prêt");

        Ok(Self {
            ipc,
            child: Mutex::new(child),
            events,
        })
    }

    async fn set(&self, name: &str, value: Value) -> Result<(), PlayerError> {
        self.ipc
            .command(json!(["set_property", name, value]))
            .await
            .map(drop)
    }

    async fn loadfile(&self, source: &MediaSource, flag: &str) -> Result<EntryId, PlayerError> {
        let options = source
            .user_agent
            .as_deref()
            .map(|ua| format!("user-agent={}", escape_option(ua)))
            .unwrap_or_default();
        // Depuis mpv 0.38 : loadfile <url> <flags> <index> <options>.
        let data = self
            .ipc
            .command(json!(["loadfile", source.url, flag, -1, options]))
            .await?;
        data.get("playlist_entry_id")
            .and_then(Value::as_u64)
            .map(EntryId)
            .ok_or_else(|| PlayerError::Command {
                command: "loadfile".into(),
                message: format!("réponse sans playlist_entry_id : {data}"),
            })
    }
}

impl Player for MpvPlayer {
    async fn load(&self, source: &MediaSource) -> Result<EntryId, PlayerError> {
        let entry = self.loadfile(source, "replace").await?;
        // Un morceau choisi doit démarrer, même si le précédent était en pause.
        self.set("pause", json!(false)).await?;
        Ok(entry)
    }

    async fn enqueue(&self, source: &MediaSource) -> Result<EntryId, PlayerError> {
        self.loadfile(source, "append").await
    }

    async fn clear_pending(&self) -> Result<(), PlayerError> {
        self.ipc.command(json!(["playlist-clear"])).await.map(drop)
    }

    async fn skip_to_pending(&self) -> Result<(), PlayerError> {
        self.ipc
            .command(json!(["playlist-next", "force"]))
            .await
            .map(drop)
    }

    async fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
        self.set("pause", json!(paused)).await
    }

    async fn stop(&self) -> Result<(), PlayerError> {
        self.ipc.command(json!(["stop"])).await.map(drop)
    }

    async fn seek(&self, position: Duration) -> Result<(), PlayerError> {
        self.ipc
            .command(json!(["seek", position.as_secs_f64(), "absolute"]))
            .await
            .map(drop)
    }

    async fn set_volume(&self, volume: u8) -> Result<(), PlayerError> {
        self.set("volume", json!(check_volume(volume)?)).await
    }

    async fn set_effects(&self, effects: &crate::AudioEffects) -> Result<(), PlayerError> {
        self.set("af", json!(effects.to_mpv_af())).await
    }

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    /// Demande à mpv de quitter proprement, et le tue s'il traîne.
    async fn shutdown(&self) {
        let _ = self.ipc.command(json!(["quit"])).await;
        let mut child = self.child.lock().await;
        if tokio::time::timeout(QUIT_TIMEOUT, child.wait())
            .await
            .is_err()
        {
            tracing::warn!("mpv ne quitte pas, arrêt forcé");
            let _ = child.kill().await;
        }
    }
}

/// Valeur d'option mpv protégée : `%<octets>%<valeur>` (le User-Agent contient des virgules).
fn escape_option(value: &str) -> String {
    format!("%{}%{value}", value.len())
}

fn check_volume(volume: u8) -> Result<u8, PlayerError> {
    if volume > 100 {
        return Err(PlayerError::InvalidArgument(format!(
            "volume {volume} hors de 0 à 100"
        )));
    }
    Ok(volume)
}

/// Convertit les événements bruts de mpv en [`PlayerEvent`].
async fn translate_events(
    mut raw: mpsc::UnboundedReceiver<Value>,
    events: broadcast::Sender<PlayerEvent>,
) {
    let mut last_position: Option<Duration> = None;
    while let Some(msg) = raw.recv().await {
        let entry = msg
            .get("playlist_entry_id")
            .and_then(Value::as_u64)
            .map(EntryId);
        let event = match msg.get("event").and_then(Value::as_str) {
            Some("start-file") => {
                last_position = None;
                entry.map(PlayerEvent::Started)
            }
            Some("file-loaded") => Some(PlayerEvent::Loaded),
            Some("end-file") => Some(PlayerEvent::Ended {
                entry,
                reason: end_reason(&msg),
            }),
            Some("idle") => Some(PlayerEvent::Idle),
            Some("property-change") => property_event(&msg, &mut last_position),
            _ => None,
        };
        if let Some(event) = event {
            // Aucun abonné n'est pas une erreur.
            let _ = events.send(event);
        }
    }
    let _ = events.send(PlayerEvent::Exited);
}

fn end_reason(msg: &Value) -> EndReason {
    match msg.get("reason").and_then(Value::as_str) {
        Some("eof") => EndReason::Eof,
        Some("stop" | "quit" | "redirect") => EndReason::Stopped,
        Some("error") => EndReason::Error(
            msg.get("file_error")
                .and_then(Value::as_str)
                .unwrap_or("erreur inconnue")
                .to_owned(),
        ),
        other => EndReason::Error(format!("raison inattendue : {other:?}")),
    }
}

fn property_event(msg: &Value, last_position: &mut Option<Duration>) -> Option<PlayerEvent> {
    let data = msg.get("data")?;
    match msg.get("name").and_then(Value::as_str)? {
        "pause" => data.as_bool().map(PlayerEvent::Paused),
        "duration" => seconds(data).map(PlayerEvent::Duration),
        "volume" => data
            .as_f64()
            .map(|v| PlayerEvent::Volume(v.round().clamp(0.0, 100.0) as u8)),
        "time-pos" => {
            let pos = seconds(data)?;
            let moved_enough = last_position.is_none_or(|last| pos.abs_diff(last) >= POSITION_STEP);
            if !moved_enough {
                return None;
            }
            *last_position = Some(pos);
            Some(PlayerEvent::Position(pos))
        }
        _ => None,
    }
}

fn seconds(v: &Value) -> Option<Duration> {
    v.as_f64()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(Duration::from_secs_f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_reasons() {
        assert_eq!(end_reason(&json!({"reason": "eof"})), EndReason::Eof);
        assert_eq!(end_reason(&json!({"reason": "stop"})), EndReason::Stopped);
        assert_eq!(
            end_reason(&json!({"reason": "error", "file_error": "loading failed"})),
            EndReason::Error("loading failed".into())
        );
    }

    #[test]
    fn position_events_are_throttled() {
        let mut last = None;
        let pos = |s: f64| json!({"event": "property-change", "name": "time-pos", "data": s});

        assert_eq!(
            property_event(&pos(1.0), &mut last),
            Some(PlayerEvent::Position(Duration::from_secs(1)))
        );
        assert_eq!(property_event(&pos(1.1), &mut last), None);
        assert!(property_event(&pos(1.3), &mut last).is_some());
        // Un seek en arrière passe aussi.
        assert!(property_event(&pos(0.2), &mut last).is_some());
    }

    #[test]
    fn ignores_null_property_values() {
        let mut last = None;
        let msg = json!({"event": "property-change", "name": "duration", "data": null});
        assert_eq!(property_event(&msg, &mut last), None);
    }

    #[test]
    fn escapes_option_values_with_commas() {
        assert_eq!(escape_option("a, b"), "%4%a, b");
        assert_eq!(escape_option("é"), "%2%é");
    }
}
