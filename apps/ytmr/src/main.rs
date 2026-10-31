//! Binaire principal de ytmr : l'application avec son interface native.

// En release, pas de console noire derrière la fenêtre sous Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use ytmr_core::{AppPaths, Config};

fn main() -> Result<()> {
    configure_renderer();
    // ytmr tourne déjà (peut-être caché près de l'horloge) : on le ramène.
    if !ytmr_gui::claim_instance() {
        return Ok(());
    }
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
/// - Rendu GPU (wgpu) avec synchronisation verticale : l'affichage suit la
///   fréquence de l'écran (60, 120, 144 Hz…). Le rendu logiciel (`tiny-skia`)
///   prenait ~90 Mo de moins mais saccadait au survol et au défilement : il
///   reste disponible avec `ICED_BACKEND=tiny-skia`.
/// - Carte basse consommation (GPU intégré) : largement suffisante pour cette
///   interface, et elle ne réveille pas la carte dédiée d'un portable.
/// - Sous Windows, DirectX 12 seul : par défaut wgpu essaie aussi OpenGL, dont
///   l'initialisation plante avec certains pilotes (0xC0000005 au démarrage,
///   constaté le 2026-10-29). DX12 coûte aussi moins de mémoire que Vulkan
///   (109 Mo contre 160 Mo mesurés sur un Radeon 780M).
///
/// Une valeur déjà définie par l'utilisateur est respectée.
fn configure_renderer() {
    let defaults: &[(&str, &str)] = if cfg!(windows) {
        &[("WGPU_BACKEND", "dx12"), ("WGPU_POWER_PREF", "low")]
    } else {
        &[("WGPU_POWER_PREF", "low")]
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
