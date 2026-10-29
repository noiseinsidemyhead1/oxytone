//! Binaire principal de ytmr : assemble le core, l'API, l'intégration OS et la GUI.

use anyhow::{Context, Result};
use ytmr_core::{AppPaths, Config};

fn main() -> Result<()> {
    let paths = AppPaths::discover()?;
    let config = Config::load_or_create(&paths.config_file())
        .with_context(|| format!("chargement de {}", paths.config_file().display()))?;
    let _log = ytmr_core::logging::init(&config.log.filter, Some(&paths.log_dir()));

    tracing::info!(
        config = %paths.config_file().display(),
        logs = %paths.log_dir().display(),
        "ytmr démarré"
    );
    Ok(())
}
