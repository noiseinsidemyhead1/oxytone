//! Cœur d'oxytone : état du lecteur, file d'attente et bus d'événements.
//!
//! En clair : c'est le chef d'orchestre. La GUI et l'API ne font qu'envoyer des
//! ordres ici et écouter ce qui s'y passe ; c'est la seule source de vérité.

pub mod config;
pub mod engine;
pub mod logging;
pub mod queue;
pub mod stats;
pub mod track;

pub use config::{AppPaths, Config, ConfigError};
pub use engine::{
    Command, CoreError, CoreEvent, CoreHandle, CoreOptions, PlaybackState, QueueSnapshot, Radio,
    Status,
};
/// Variables d'environnement `OXYTONE_…` (repli sur `YTMR_…`).
pub use oxytone_player::env;
pub use queue::{QueueId, QueuedTrack, Repeat};
pub use track::Track;
