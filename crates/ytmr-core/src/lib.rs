//! Cœur de ytmr : état du lecteur, file d'attente et bus d'événements.
//!
//! En clair : c'est le chef d'orchestre. La GUI et l'API ne font qu'envoyer des
//! ordres ici et écouter ce qui s'y passe ; c'est la seule source de vérité.

pub mod config;
pub mod logging;

pub use config::{AppPaths, Config, ConfigError};
