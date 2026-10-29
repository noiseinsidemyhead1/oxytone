//! Lecture audio derrière un trait [`Player`], implémenté par mpv piloté en JSON IPC.
//!
//! En clair : la télécommande de mpv. mpv (aidé de yt-dlp) fait le vrai travail
//! de lecture ; on ne touche jamais à l'extraction des flux YouTube. Le trait
//! permet de changer de moteur plus tard (libmpv, rodio) sans toucher au reste.

pub mod deps;
mod mpv;

use std::time::Duration;

use thiserror::Error;
use tokio::sync::broadcast;

pub use deps::Dependencies;
pub use mpv::MpvPlayer;

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("{name} introuvable : {hint}")]
    MissingTool { name: &'static str, hint: String },
    #[error("impossible de lancer mpv : {0}")]
    Spawn(#[source] std::io::Error),
    #[error("mpv s'est arrêté au démarrage ({0})")]
    ExitedEarly(String),
    #[error("mpv n'a pas ouvert son canal IPC à temps")]
    ConnectTimeout,
    #[error("canal IPC mpv : {0}")]
    Ipc(#[source] std::io::Error),
    #[error("mpv ne répond pas à {0}")]
    CommandTimeout(String),
    #[error("mpv a refusé {command} : {message}")]
    Command { command: String, message: String },
    #[error("mpv n'est plus joignable")]
    Closed,
    #[error("argument invalide : {0}")]
    InvalidArgument(String),
}

/// Pourquoi un morceau s'est arrêté.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// Fin naturelle du morceau : c'est le signal pour passer au suivant.
    Eof,
    /// Arrêt demandé, ou remplacé par un autre morceau.
    Stopped,
    /// Lecture impossible (réseau, yt-dlp, format…).
    Error(String),
}

/// Ce que le lecteur signale de lui-même.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// Le morceau est ouvert et va démarrer.
    Loaded,
    Paused(bool),
    /// Position courante, envoyée au plus toutes les 250 ms.
    Position(Duration),
    Duration(Duration),
    Volume(u8),
    Ended(EndReason),
    /// Plus rien à jouer.
    Idle,
    /// Le moteur de lecture a disparu (crash ou arrêt).
    Exited,
}

/// Contrat commun à tous les moteurs de lecture.
pub trait Player: Send + Sync {
    /// Remplace le morceau courant par `url` et lance la lecture.
    fn load(&self, url: &str) -> impl Future<Output = Result<(), PlayerError>> + Send;
    fn set_paused(&self, paused: bool) -> impl Future<Output = Result<(), PlayerError>> + Send;
    fn toggle_pause(&self) -> impl Future<Output = Result<(), PlayerError>> + Send;
    fn stop(&self) -> impl Future<Output = Result<(), PlayerError>> + Send;
    /// Position absolue depuis le début du morceau.
    fn seek(&self, position: Duration) -> impl Future<Output = Result<(), PlayerError>> + Send;
    /// 0 à 100 ; au-delà, [`PlayerError::InvalidArgument`].
    fn set_volume(&self, volume: u8) -> impl Future<Output = Result<(), PlayerError>> + Send;
    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent>;
}
