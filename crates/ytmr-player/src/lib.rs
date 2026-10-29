//! Lecture audio derrière un trait [`Player`], implémenté par mpv piloté en JSON IPC.
//!
//! En clair : la télécommande de mpv. mpv (aidé de yt-dlp) fait le vrai travail
//! de lecture ; on ne touche jamais à l'extraction des flux YouTube. Le trait
//! permet de changer de moteur plus tard (libmpv, rodio) sans toucher au reste.

pub mod deps;
mod mpv;
mod process;
pub mod resolve;
pub mod worker;

use std::time::Duration;

use thiserror::Error;
use tokio::sync::broadcast;

pub use deps::Dependencies;
pub use mpv::MpvPlayer;
pub use resolve::{
    MediaSource, Priority, Resolver, ResolverOptions, StartMode, StreamQuality, YtDlpResolver,
};

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
    #[error("résolution du flux : {0}")]
    Resolve(String),
}

/// Identifiant d'une entrée chargée dans le lecteur. Permet de savoir à quel
/// morceau se rapporte un événement, même quand le lecteur enchaîne seul.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(pub u64);

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
    /// Une entrée commence (chargement demandé, ou enchaînement automatique).
    Started(EntryId),
    /// Le morceau est ouvert et le son va sortir.
    Loaded,
    Paused(bool),
    /// Position courante, envoyée au plus toutes les 250 ms.
    Position(Duration),
    Duration(Duration),
    Volume(u8),
    Ended {
        entry: Option<EntryId>,
        reason: EndReason,
    },
    /// Plus rien à jouer.
    Idle,
    /// Le moteur de lecture a disparu (crash ou arrêt).
    Exited,
}

/// Contrat commun à tous les moteurs de lecture.
///
/// Le lecteur tient au plus deux entrées : celle en cours et une « en attente »
/// qu'il enchaîne seul, sans blanc, à la fin de la première.
pub trait Player: Send + Sync + 'static {
    /// Remplace tout par `source` et lance la lecture.
    fn load(
        &self,
        source: &MediaSource,
    ) -> impl Future<Output = Result<EntryId, PlayerError>> + Send;
    /// Prépare `source` pour qu'elle démarre à la fin du morceau courant.
    fn enqueue(
        &self,
        source: &MediaSource,
    ) -> impl Future<Output = Result<EntryId, PlayerError>> + Send;
    /// Oublie les entrées en attente (garde le morceau courant).
    fn clear_pending(&self) -> impl Future<Output = Result<(), PlayerError>> + Send;
    /// Passe tout de suite à l'entrée en attente.
    fn skip_to_pending(&self) -> impl Future<Output = Result<(), PlayerError>> + Send;
    fn set_paused(&self, paused: bool) -> impl Future<Output = Result<(), PlayerError>> + Send;
    fn stop(&self) -> impl Future<Output = Result<(), PlayerError>> + Send;
    /// Position absolue depuis le début du morceau.
    fn seek(&self, position: Duration) -> impl Future<Output = Result<(), PlayerError>> + Send;
    /// 0 à 100 ; au-delà, [`PlayerError::InvalidArgument`].
    fn set_volume(&self, volume: u8) -> impl Future<Output = Result<(), PlayerError>> + Send;
    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent>;
    /// Arrête le moteur proprement. Plus aucune commande n'est possible ensuite.
    fn shutdown(&self) -> impl Future<Output = ()> + Send;
}
