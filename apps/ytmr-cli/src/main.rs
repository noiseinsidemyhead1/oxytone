//! Binaire headless de ytmr : debug et usage sans interface graphique.

use anyhow::{Context, Result};
use ytmr_core::{AppPaths, Config};

fn main() -> Result<()> {
    let paths = AppPaths::discover()?;
    let config = Config::load_or_create(&paths.config_file())
        .with_context(|| format!("chargement de {}", paths.config_file().display()))?;
    let _log = ytmr_core::logging::init(&config.log.filter, None);

    tracing::info!(config = %paths.config_file().display(), "ytmr-cli démarré");
    tracing::debug!(?config, "config chargée");
    Ok(())
}
