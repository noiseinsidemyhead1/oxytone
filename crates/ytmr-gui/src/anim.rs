//! Animations des curseurs et interrupteurs.
//!
//! En clair : quand une valeur change d'un coup (clic sur un curseur, réglage
//! tout prêt de l'égaliseur, interrupteur), ce qu'on voit glisse jusqu'à la
//! nouvelle valeur au lieu de sauter. L'application dit à chaque mise à jour
//! où chaque valeur *doit* être ([`Anims::aim`]) ; la valeur affichée la
//! rejoint image par image ([`Anims::frame`]), seulement le temps du mouvement.
//!
//! La file d'attente a sa propre animation : quand l'aléatoire change, chaque
//! morceau glisse de son ancienne place à la nouvelle ([`Reorder`], [`Shift`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector};
use ytmr_core::QueueId;

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

/// Durée du glissement des morceaux de la file.
const REORDER: Duration = Duration::from_millis(520);
/// Écart (en lignes) d'où arrive un morceau qui n'était pas dans la file.
const NEW_FROM: f32 = 6.0;

/// Réorganisation animée de la file : pour chaque morceau, l'écart (en
/// lignes) entre son ancienne place et la nouvelle, qui se résorbe.
#[derive(Debug)]
pub struct Reorder {
    since: Instant,
    rows: HashMap<QueueId, f32>,
}

impl Reorder {
    /// `before` : ordre avant le mélange ; `after` : ordre reçu du moteur.
    pub fn new(before: &[QueueId], after: &[QueueId], now: Instant) -> Option<Self> {
        let old: HashMap<QueueId, usize> =
            before.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let rows: HashMap<QueueId, f32> = after
            .iter()
            .enumerate()
            .filter_map(|(new, id)| {
                let delta = match old.get(id) {
                    Some(&old) => old as f32 - new as f32,
                    None => NEW_FROM,
                };
                (delta != 0.0).then_some((*id, delta))
            })
            .collect();
        (!rows.is_empty()).then_some(Self { since: now, rows })
    }

    /// Décalage restant (en lignes) du morceau `id` à l'instant `now`.
    pub fn offset(&self, id: QueueId, now: Instant) -> f32 {
        let Some(delta) = self.rows.get(&id) else {
            return 0.0;
        };
        let t = (now.saturating_duration_since(self.since).as_secs_f32() / REORDER.as_secs_f32())
            .min(1.0);
        // Départ vif, arrivée douce (cubique).
        let eased = 1.0 - (1.0 - t).powi(3);
        delta * (1.0 - eased)
    }

    pub fn is_done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= REORDER
    }
}

/// Dessine son contenu décalé verticalement (sans changer la mise en page).
/// Le décalage est purement visuel : il ne sert qu'aux animations courtes.
pub struct Shift<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    dy: f32,
}

pub fn shift<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    dy: f32,
) -> Shift<'a, Message, Theme, Renderer> {
    Shift {
        content: content.into(),
        dy,
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Shift<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if self.dy.abs() < 0.5 {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
            return;
        }
        let offset = Vector::new(0.0, self.dy);
        // La zone visible est décalée à l'inverse : ce qui glisse depuis
        // l'extérieur de la liste est bien dessiné en entrant.
        let shifted = *viewport - offset;
        renderer.with_translation(offset, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                &shifted,
            );
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer: renderer::Renderer + 'a>
    From<Shift<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
{
    fn from(widget: Shift<'a, Message, Theme, Renderer>) -> Self {
        Element::new(widget)
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
    fn reorder_offsets_shrink_to_zero() {
        let ids: Vec<QueueId> = (1..=4).map(QueueId).collect();
        let shuffled = vec![ids[0], ids[3], ids[1], ids[2]];
        let now = Instant::now();
        let r = Reorder::new(&ids, &shuffled, now).unwrap();
        assert_eq!(
            r.offset(ids[0], now),
            0.0,
            "le morceau en cours ne bouge pas"
        );
        assert_eq!(r.offset(ids[3], now), 2.0, "il remonte de deux lignes");
        assert_eq!(r.offset(ids[1], now), -1.0);
        let later = now + REORDER;
        assert_eq!(r.offset(ids[3], later), 0.0);
        assert!(r.is_done(later));
        assert!(Reorder::new(&ids, &ids, now).is_none(), "rien n'a bougé");
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
