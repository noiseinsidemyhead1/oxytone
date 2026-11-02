//! Compteur des données téléchargées pour le son.
//!
//! En clair : on additionne ce qui arrive par Internet pour la musique — les
//! morceaux lus en direct par mpv (taille du fichier, quand il ne vient pas du
//! disque) et les fichiers téléchargés pour le cache et le hors ligne. Les
//! Paramètres affichent le total.

use std::sync::atomic::{AtomicU64, Ordering};

static AUDIO_BYTES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn add(bytes: u64) {
    AUDIO_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// Octets de son reçus depuis le lancement.
pub fn audio_bytes() -> u64 {
    AUDIO_BYTES.load(Ordering::Relaxed)
}
