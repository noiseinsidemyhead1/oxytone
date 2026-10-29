//! Défilement animé à la molette.
//!
//! En clair : iced fait sauter une liste de 60 px d'un coup à chaque cran de
//! molette, ce qui donne une impression de saccade. Ce widget enveloppe une
//! liste défilante, intercepte les crans de molette et les transmet à
//! l'application, qui fait glisser la liste image par image (à la fréquence de
//! l'écran). Le pavé tactile, déjà fluide, n'est pas intercepté, ni la molette
//! avec Shift : elle revient aux rangées défilant horizontalement.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::widget::scrollable::{AbsoluteOffset, Viewport};
use iced::{Element, Event, Length, Rectangle, Size, Task, Vector};

/// Distance parcourue par cran de molette.
const WHEEL_STEP: f32 = 110.0;
/// Vitesse de rattrapage (plus grand = plus vif).
const RESPONSIVENESS: f32 = 16.0;

pub struct WheelCatcher<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_wheel: Box<dyn Fn(f32) -> Message + 'a>,
    /// Rangée horizontale : n'intercepte que la molette avec Shift.
    shift_only: bool,
}

/// Enveloppe `content` (une liste défilante) : `on_wheel` reçoit les crans.
pub fn wheel<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_wheel: impl Fn(f32) -> Message + 'a,
) -> WheelCatcher<'a, Message, Theme, Renderer> {
    WheelCatcher {
        content: content.into(),
        on_wheel: Box::new(on_wheel),
        shift_only: false,
    }
}

/// Enveloppe une rangée défilant horizontalement : Shift + molette la fait
/// glisser (animé), la molette seule continue de faire défiler la page.
pub fn wheel_horizontal<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_wheel: impl Fn(f32) -> Message + 'a,
) -> WheelCatcher<'a, Message, Theme, Renderer> {
    WheelCatcher {
        content: content.into(),
        on_wheel: Box::new(on_wheel),
        shift_only: true,
    }
}

/// Shift enfoncé ? (suivi via les changements de touches de modification)
#[derive(Debug, Default)]
struct Keys {
    shift: bool,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for WheelCatcher<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Keys>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Keys::default())
    }

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
        if let Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) = event {
            tree.state.downcast_mut::<Keys>().shift = modifiers.shift();
        }
        let shift = tree.state.downcast_ref::<Keys>().shift;
        if let Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x, y },
        }) = event
            && shift == self.shift_only
            && cursor.is_over(layout.bounds())
        {
            // Avec Shift, certaines souris envoient le cran sur l'axe horizontal.
            let lines = if *y == 0.0 { *x } else { *y };
            shell.publish((self.on_wheel)(lines));
            shell.capture_event();
            return;
        }
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
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
    From<WheelCatcher<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
{
    fn from(widget: WheelCatcher<'a, Message, Theme, Renderer>) -> Self {
        Element::new(widget)
    }
}

/// Position animée d'une liste défilante.
#[derive(Debug, Default, Clone, Copy)]
struct Motion {
    offset: f32,
    target: f32,
    max: f32,
    animating: bool,
    horizontal: bool,
}

/// Animations de défilement, par identifiant de liste.
#[derive(Debug, Default)]
pub struct Scrolling {
    lists: HashMap<&'static str, Motion>,
    last_frame: Option<Instant>,
}

impl Scrolling {
    pub fn is_animating(&self) -> bool {
        self.lists.values().any(|m| m.animating)
    }

    /// Un cran de molette (`lines` > 0 vers le haut).
    pub fn wheel(&mut self, id: &'static str, lines: f32) {
        let motion = self.lists.entry(id).or_default();
        if !motion.animating {
            motion.target = motion.offset;
            self.last_frame = None;
        }
        motion.target = (motion.target - lines * WHEEL_STEP).clamp(0.0, motion.max);
        motion.animating = (motion.target - motion.offset).abs() > 0.5;
    }

    /// Cran de Shift + molette sur une rangée horizontale.
    pub fn wheel_horizontal(&mut self, id: &'static str, lines: f32) {
        self.lists.entry(id).or_default().horizontal = true;
        self.wheel(id, lines);
    }

    pub fn scrolled_horizontal(&mut self, id: &'static str, viewport: &Viewport) {
        let motion = self.lists.entry(id).or_default();
        motion.horizontal = true;
        motion.max = (viewport.content_bounds().width - viewport.bounds().width).max(0.0);
        if !motion.animating {
            motion.offset = viewport.absolute_offset().x;
            motion.target = motion.offset;
        }
    }

    /// La liste a bougé (barre de défilement, pavé tactile, taille) : on se recale.
    pub fn scrolled(&mut self, id: &'static str, viewport: &Viewport) {
        let motion = self.lists.entry(id).or_default();
        motion.max = (viewport.content_bounds().height - viewport.bounds().height).max(0.0);
        if !motion.animating {
            motion.offset = viewport.absolute_offset().y;
            motion.target = motion.offset;
        }
    }

    /// Retour en haut immédiat (changement de page).
    pub fn reset(&mut self, id: &'static str) {
        let motion = self.lists.entry(id).or_default();
        *motion = Motion {
            max: motion.max,
            ..Motion::default()
        };
    }

    /// Avance les animations d'une image et rend les déplacements à appliquer.
    pub fn frame<Message: Send + 'static>(&mut self, now: Instant) -> Task<Message> {
        let dt = self
            .last_frame
            .map_or(Duration::from_millis(16), |last| {
                now.saturating_duration_since(last)
            })
            .min(Duration::from_millis(50))
            .as_secs_f32();
        self.last_frame = Some(now);
        let blend = 1.0 - (-dt * RESPONSIVENESS).exp();
        let mut tasks = Vec::new();
        for (id, motion) in &mut self.lists {
            if !motion.animating {
                continue;
            }
            motion.offset += (motion.target - motion.offset) * blend;
            if (motion.target - motion.offset).abs() < 0.5 {
                motion.offset = motion.target;
                motion.animating = false;
            }
            let offset = if motion.horizontal {
                AbsoluteOffset {
                    x: Some(motion.offset),
                    y: None,
                }
            } else {
                AbsoluteOffset {
                    x: None,
                    y: Some(motion.offset),
                }
            };
            tasks.push(iced::widget::operation::scroll_to(*id, offset));
        }
        if !self.is_animating() {
            self.last_frame = None;
        }
        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_moves_target_within_bounds() {
        let mut s = Scrolling::default();
        s.lists.insert(
            "p",
            Motion {
                max: 300.0,
                ..Motion::default()
            },
        );
        s.wheel("p", -1.0);
        assert!(s.is_animating());
        assert_eq!(s.lists["p"].target, WHEEL_STEP);
        s.wheel("p", -5.0);
        assert_eq!(s.lists["p"].target, 300.0, "borné en bas");
        s.wheel("p", 10.0);
        assert_eq!(s.lists["p"].target, 0.0, "borné en haut");
    }

    #[test]
    fn animation_converges() {
        let mut s = Scrolling::default();
        s.lists.insert(
            "p",
            Motion {
                max: 1000.0,
                ..Motion::default()
            },
        );
        s.wheel("p", -2.0);
        let start = Instant::now();
        for i in 1..=120 {
            let _: Task<()> = s.frame(start + Duration::from_millis(i * 8));
        }
        assert!(!s.is_animating());
        assert_eq!(s.lists["p"].offset, 2.0 * WHEEL_STEP);
    }
}
