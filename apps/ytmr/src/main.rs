//! Binaire principal de ytmr : l'application avec son interface native.

// En release, pas de console noire derrière la fenêtre sous Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use ytmr_core::{AppPaths, Config};

fn main() -> Result<()> {
    configure_renderer();
    #[cfg(windows)]
    refresh_path();
    // Dossier de travail sûr : celui qu'on reçoit peut avoir disparu (dossier
    // temporaire de l'installateur), et Windows refuse alors de lancer mpv.
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Into::into))
        && let Err(e) = std::env::set_current_dir::<std::path::PathBuf>(dir)
    {
        eprintln!("dossier de travail : {e}");
    }
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

/// PATH à jour, relu dans le registre (Windows).
///
/// L'Explorateur garde le PATH de l'ouverture de session : ytmr lancé depuis
/// le menu Démarrer ou l'installateur ne voyait pas mpv, yt-dlp ni node
/// installés depuis (scoop), alors qu'un nouveau terminal les trouvait.
/// On reprend donc le PATH système et utilisateur actuels, puis ce que le
/// processus avait déjà en plus (lancement depuis un terminal).
#[cfg(windows)]
fn refresh_path() {
    use winreg::{HKCU, HKLM, RegKey};
    let read = |key: &RegKey, sub: &str| -> Option<String> {
        key.open_subkey(sub)
            .ok()?
            .get_value::<String, _>("Path")
            .ok()
    };
    let machine = read(
        HKLM,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    );
    let user = read(HKCU, "Environment");
    let current = std::env::var("PATH").unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let entries: Vec<String> = [
        machine.unwrap_or_default(),
        user.unwrap_or_default(),
        current,
    ]
    .iter()
    .flat_map(|p| p.split(';').map(expand).collect::<Vec<_>>())
    .filter(|e| !e.trim().is_empty())
    .filter(|e| seen.insert(e.trim_end_matches('\\').to_lowercase()))
    .collect();
    // SAFETY : appelé en tout début de `main`, avant la création du moindre
    // thread ; personne ne lit l'environnement en parallèle.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("PATH", entries.join(";"));
    }
}

/// Remplace les `%VARIABLE%` d'une entrée du registre (`%USERPROFILE%\…`).
#[cfg(windows)]
fn expand(entry: &str) -> String {
    let mut out = String::new();
    let mut rest = entry;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            break;
        };
        out.push_str(&rest[..start]);
        let name = &after[..end];
        match std::env::var(name) {
            Ok(value) => out.push_str(&value),
            Err(_) => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(all(test, windows))]
mod tests {
    use super::expand;

    #[test]
    fn reads_the_user_path_from_the_registry() {
        use winreg::HKCU;
        let path: std::io::Result<String> = HKCU
            .open_subkey("Environment")
            .and_then(|k| k.get_value("Path"));
        println!("PATH utilisateur : {path:?}");
        assert!(path.is_ok());
    }

    #[test]
    fn expands_registry_variables() {
        let home = std::env::var("USERPROFILE").unwrap();
        assert_eq!(
            expand(r"%USERPROFILE%\scoop\shims"),
            format!(r"{home}\scoop\shims")
        );
        assert_eq!(expand(r"C:\Outils"), r"C:\Outils");
        assert_eq!(expand("%INCONNUE_YTMR%"), "%INCONNUE_YTMR%");
    }
}
