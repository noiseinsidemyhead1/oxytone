//! Animations des curseurs et interrupteurs.
//!
//! En clair : quand une valeur change d'un coup (clic sur un curseur, réglage
//! tout prêt de l'égaliseur, interrupteur), ce qu'on voit glisse jusqu'à la
//! nouvelle valeur au lieu de sauter. L'application dit à chaque mise à jour
//! où chaque valeur *doit* être ([`Anims::aim`]) ; la valeur affichée la
//! rejoint image par image ([`Anims::frame`]), seulement le temps du mouvement.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Vitesse de rattrapage (plus grand = plus vif) : ~150 ms pour l'essentiel.
const SPEED: f32 = 22.0;

/// Valeurs animées.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// Bande de l'égaliseur.
    Band(usize),
    Volume,
    /// Position de lecture (seuls les sauts sont animés).
    Seek,
    Normalize,
    Notifications,
}

#[derive(Debug, Clone, Copy)]
struct Value {
    shown: f32,
    target: f32,
}

#[derive(Debug, Default)]
pub struct Anims {
    values: HashMap<Key, Value>,
    last_frame: Option<Instant>,
}

impl Anims {
    /// Où la valeur doit aller. Une nouvelle valeur part d'emblée de là ;
    /// un écart inférieur à `snap` est rattrapé sans animation (la position de
    /// lecture qui avance toute seule, par exemple).
    pub fn aim(&mut self, key: Key, target: f32, snap: f32) {
        let value = self.values.entry(key).or_insert(Value {
            shown: target,
            target,
        });
        value.target = target;
        if (value.shown - target).abs() <= snap {
            value.shown = target;
        }
    }

    /// Valeur affichée (`fallback` si elle n'a jamais été visée).
    pub fn get(&self, key: Key, fallback: f32) -> f32 {
        self.values.get(&key).map_or(fallback, |v| v.shown)
    }

    pub fn is_running(&self) -> bool {
        self.values
            .values()
            .any(|v| (v.shown - v.target).abs() > f32::EPSILON)
    }

    /// Avance d'une image.
    pub fn frame(&mut self, now: Instant) {
        let dt = self
            .last_frame
            .map_or(Duration::from_millis(16), |last| {
                now.saturating_duration_since(last)
            })
            .min(Duration::from_millis(50))
            .as_secs_f32();
        self.last_frame = Some(now);
        let blend = 1.0 - (-dt * SPEED).exp();
        for v in self.values.values_mut() {
            v.shown += (v.target - v.shown) * blend;
            // Assez près : on se pose (sinon l'animation ne finirait jamais).
            if (v.target - v.shown).abs() < 0.002 * (1.0 + v.target.abs()) {
                v.shown = v.target;
            }
        }
        if !self.is_running() {
            self.last_frame = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glides_then_settles() {
        let mut a = Anims::default();
        a.aim(Key::Volume, 20.0, 0.0);
        assert_eq!(
            a.get(Key::Volume, 0.0),
            20.0,
            "première valeur : pas d'animation"
        );
        a.aim(Key::Volume, 80.0, 0.0);
        assert!(a.is_running());
        let start = Instant::now();
        a.frame(start);
        let first = a.get(Key::Volume, 0.0);
        assert!(first > 20.0 && first < 80.0);
        for i in 1..200 {
            a.frame(start + Duration::from_millis(i * 8));
        }
        assert!(!a.is_running());
        assert_eq!(a.get(Key::Volume, 0.0), 80.0);
    }

    #[test]
    fn small_moves_snap() {
        let mut a = Anims::default();
        a.aim(Key::Seek, 10.0, 1.5);
        a.aim(Key::Seek, 10.25, 1.5);
        assert!(!a.is_running());
        a.aim(Key::Seek, 60.0, 1.5);
        assert!(a.is_running());
    }
}
