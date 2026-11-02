//! Effets audio : égaliseur et volume homogène.
//!
//! En clair : les réglages de son de la page Paramètres. mpv les applique avec
//! ses filtres (ceux de FFmpeg) ; ici on ne fait que décrire ce qu'on veut et
//! le traduire en une ligne de filtres pour mpv.

use serde::{Deserialize, Serialize};

/// Fréquences centrales des 10 bandes (Hz), une octave chacune.
pub const EQ_BANDS: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
/// Gain maximal d'une bande, en dB (dans les deux sens).
pub const EQ_MAX_DB: i8 = 12;
/// Étiquette du filtre de gain par morceau (volume homogène), piloté par
/// `af-command` à chaque nouveau morceau.
pub const GAIN_LABEL: &str = "oxytonegain";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioEffects {
    /// Gain de chaque bande de [`EQ_BANDS`], en dB (−12 à +12).
    pub eq: [i8; 10],
    /// Volume homogène : chaque morceau trop fort est baissé au niveau prévu
    /// par YouTube (gain fixe par morceau, comme YouTube Music).
    pub normalize: bool,
}

impl AudioEffects {
    /// Gains ramenés dans les bornes (une valeur venue d'un client n'est jamais crue telle quelle).
    pub fn clamped(mut self) -> Self {
        for gain in &mut self.eq {
            *gain = (*gain).clamp(-EQ_MAX_DB, EQ_MAX_DB);
        }
        self
    }

    /// Valeur de la propriété `af` de mpv ; vide = aucun filtre.
    pub fn to_mpv_af(&self) -> String {
        let effects = self.clamped();
        let mut graph: Vec<String> = Vec::new();
        let boost = effects.eq.iter().copied().max().unwrap_or(0).max(0);
        if boost > 0 {
            // Les bandes poussées font saturer : on baisse d'autant avant.
            graph.push(format!("volume=-{boost}dB"));
        }
        for (freq, gain) in EQ_BANDS.iter().zip(effects.eq) {
            if gain != 0 {
                graph.push(format!("equalizer=f={freq}:t=o:w=1:g={gain}"));
            }
        }
        let mut chain: Vec<String> = Vec::new();
        if !graph.is_empty() {
            chain.push(format!("lavfi=[{}]", graph.join(",")));
        }
        if effects.normalize {
            // Gain du morceau en cours, réglé par le moteur (1 = inchangé).
            chain.push(format!("@{GAIN_LABEL}:lavfi=[volume=1]"));
        }
        chain.join(",")
    }
}

/// Réglages tout prêts de l'égaliseur.
pub const EQ_PRESETS: [(&str, [i8; 10]); 6] = [
    ("Plat", [0; 10]),
    ("Basses", [6, 5, 4, 2, 0, 0, 0, 0, 0, 0]),
    ("Électro", [5, 4, 1, 0, -2, 0, 1, 3, 4, 4]),
    ("Voix", [-2, -2, -1, 1, 3, 4, 3, 1, 0, -1]),
    ("Aigus", [0, 0, 0, 0, 0, 1, 2, 4, 5, 6]),
    ("Acoustique", [3, 2, 1, 0, 1, 1, 2, 3, 2, 1]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_means_no_filter() {
        assert_eq!(AudioEffects::default().to_mpv_af(), "");
    }

    #[test]
    fn builds_a_filter_graph() {
        let mut fx = AudioEffects::default();
        fx.eq[0] = 4;
        fx.eq[9] = -30;
        fx.normalize = true;
        assert_eq!(
            fx.to_mpv_af(),
            "lavfi=[volume=-4dB,equalizer=f=31:t=o:w=1:g=4,equalizer=f=16000:t=o:w=1:g=-12],@oxytonegain:lavfi=[volume=1]"
        );
    }
}
