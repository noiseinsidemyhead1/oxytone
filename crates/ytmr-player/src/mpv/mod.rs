//! Implémentation de [`Player`] avec mpv lancé en processus fils.
//!
//! En clair : on démarre mpv en arrière-plan, sans fenêtre, en lui demandant
//! d'ouvrir un canal de télécommande. Quand on lui donne une URL YouTube, mpv
//! appelle yt-dlp tout seul pour trouver le flux audio. Si mpv plante, seul ce
//! lecteur s'arrête, pas l'application.

mod ipc;

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc};

use crate::deps::{Dependencies, JsRuntime};
use crate::{EndReason, Player, PlayerError, PlayerEvent};
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
    child: Child,
    events: broadcast::Sender<PlayerEvent>,
}

impl MpvPlayer {
    /// Lance mpv et attend que son canal IPC réponde.
    /// `ytdl_raw_options` : options `clé=valeur` transmises telles quelles à yt-dlp.
    pub async fn spawn(
        deps: &Dependencies,
        volume: u8,
        ytdl_raw_options: &[String],
    ) -> Result<Self, PlayerError> {
        let volume = check_volume(volume)?;
        let ipc_path = ipc::ipc_path();

        let mut cmd = Command::new(&deps.mpv.path);
        cmd.args([
            "--idle=yes",
            "--no-video",
            "--no-terminal",
            "--keep-open=no",
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

        Ok(Self { ipc, child, events })
    }

    /// Demande à mpv de quitter proprement, et le tue s'il traîne.
    pub async fn shutdown(mut self) {
        let _ = self.ipc.command(json!(["quit"])).await;
        if tokio::time::timeout(QUIT_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            tracing::warn!("mpv ne quitte pas, arrêt forcé");
            let _ = self.child.kill().await;
        }
    }

    async fn set(&self, name: &str, value: Value) -> Result<(), PlayerError> {
        self.ipc
            .command(json!(["set_property", name, value]))
            .await
            .map(drop)
    }
}

impl Player for MpvPlayer {
    async fn load(&self, url: &str) -> Result<(), PlayerError> {
        self.ipc
            .command(json!(["loadfile", url, "replace"]))
            .await?;
        // Une piste chargée doit démarrer, même si la précédente était en pause.
        self.set("pause", json!(false)).await
    }

    async fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
        self.set("pause", json!(paused)).await
    }

    async fn toggle_pause(&self) -> Result<(), PlayerError> {
        self.ipc.command(json!(["cycle", "pause"])).await.map(drop)
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

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }
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
        let event = match msg.get("event").and_then(Value::as_str) {
            Some("file-loaded") => {
                last_position = None;
                Some(PlayerEvent::Loaded)
            }
            Some("end-file") => Some(PlayerEvent::Ended(end_reason(&msg))),
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
}
