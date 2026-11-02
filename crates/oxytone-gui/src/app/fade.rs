//! Fondu d'une couleur à l'autre (teintes de l'interface).
//!
//! En clair : quand le morceau change, la teinte glisse de l'ancienne
//! couleur à la nouvelle en 0,6 s au lieu de changer d'un coup.

use std::time::Duration;

use iced::Color;

/// Durée du fondu entre deux teintes.
const FADE: Duration = Duration::from_millis(600);

/// Teinte qui glisse d'une couleur à l'autre (pas de changement brutal).
#[derive(Debug, Clone, Copy)]
pub(super) struct Fade {
    from: Color,
    pub(super) to: Color,
    since: Option<std::time::Instant>,
}

impl Fade {
    pub(super) fn new(color: Color) -> Self {
        Self {
            from: color,
            to: color,
            since: None,
        }
    }

    /// Nouvelle cible ; le fondu part de la couleur affichée `current`.
    pub(super) fn set(&mut self, current: Color, to: Color) {
        if to == self.to {
            return;
        }
        self.from = current;
        self.to = to;
        self.since = Some(std::time::Instant::now());
    }

    pub(super) fn is_running(&self) -> bool {
        self.since.is_some()
    }

    pub(super) fn at(&mut self, now: std::time::Instant) -> Color {
        let Some(since) = self.since else {
            return self.to;
        };
        let t = (now.saturating_duration_since(since).as_secs_f32() / FADE.as_secs_f32()).min(1.0);
        if t >= 1.0 {
            self.since = None;
            return self.to;
        }
        // Douce au départ et à l'arrivée.
        let t = t * t * (3.0 - 2.0 * t);
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Color::from_rgb(
            mix(self.from.r, self.to.r),
            mix(self.from.g, self.to.g),
            mix(self.from.b, self.to.b),
        )
    }
}
