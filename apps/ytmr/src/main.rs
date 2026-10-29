//! Binaire principal de ytmr : l'application avec son interface native.

// En release, pas de console noire derrière la fenêtre sous Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use ytmr_core::{AppPaths, Config};

fn main() -> Result<()> {
    configure_renderer();
    let paths = AppPaths::discover()?;
    let config_file = paths.config_file();
    let mut config = Config::load_or_create(&config_file)
        .with_context(|| format!("chargement de {}", config_file.display()))?;
    let _log = ytmr_core::logging::init(&config.log.filter, Some(&paths.log_dir()));
    if config.api.ensure_token()? {
        config.save(&config_file)?;
        tracing::info!("token d'API généré dans la config (écoute réseau)");
    }
    ytmr_gui::run(config, paths).context("interface graphique")
}

/// Choix du rendu, qu'iced et wgpu ne lisent que dans l'environnement.
///
/// - Rendu logiciel (`tiny-skia`) par défaut : mesuré le 2026-10-29, l'appli
///   occupe ~21 Mo de mémoire privée contre 109 à 207 Mo avec le GPU (le pilote
///   graphique coûte à lui seul ~90 Mo), pour 0,1 % de CPU en lecture. Une
///   interface de lecteur de musique se redessine peu : le GPU n'apporte rien.
/// - Si l'utilisateur choisit le GPU (`ICED_BACKEND=wgpu`) : sous Windows, sans
///   OpenGL, dont l'initialisation plante avec certains pilotes (accès mémoire
///   invalide au démarrage), et la carte basse consommation.
///
/// Une valeur déjà définie par l'utilisateur est respectée.
fn configure_renderer() {
    let defaults: &[(&str, &str)] = if cfg!(windows) {
        &[
            ("ICED_BACKEND", "tiny-skia"),
            ("WGPU_BACKEND", "dx12"),
            ("WGPU_POWER_PREF", "low"),
        ]
    } else {
        &[("ICED_BACKEND", "tiny-skia"), ("WGPU_POWER_PREF", "low")]
    };
    for (key, value) in defaults {
        if std::env::var_os(key).is_none() {
            // SAFETY : appelé en tout début de `main`, avant la création du
            // moindre thread ; personne ne lit l'environnement en parallèle.
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }
}
