//! Variables d'environnement d'oxytone, avec repli sur l'ancien nom.
//!
//! En clair : l'appli s'appelait ytmr. Pendant une version, `YTMR_FPS` (et
//! les autres `YTMR_…`) marchent encore à la place de `OXYTONE_FPS`, avec un
//! avertissement au journal. À retirer à la version suivante.

use std::ffi::OsString;

/// Valeur de `OXYTONE_<name>`, sinon de l'ancien `YTMR_<name>` (avertissement).
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(format!("OXYTONE_{name}")).or_else(|| {
        let old = format!("YTMR_{name}");
        let value = std::env::var_os(&old)?;
        tracing::warn!("{old} est obsolète : utiliser OXYTONE_{name}");
        Some(value)
    })
}

/// Comme [`var_os`], en texte (`None` si absente ou pas en Unicode).
pub fn var(name: &str) -> Option<String> {
    var_os(name).and_then(|v| v.into_string().ok())
}
