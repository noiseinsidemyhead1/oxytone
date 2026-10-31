//! Raccourcis clavier.
//!
//! En clair : les touches que les champs de saisie ne prennent pas pour eux
//! (espace, flèches, L, M, /, ?, F11, Échap) pilotent la lecture et l'interface.

use super::*;

impl App {
    pub(super) fn on_key(&mut self, event: keyboard::Event) -> Task<Message> {
        // `listen` ne transmet que les touches ignorées par les widgets : taper
        // un espace dans la recherche n'arrive donc pas ici.
        if let keyboard::Event::ModifiersChanged(modifiers) = event {
            self.modifiers = modifiers;
            return Task::none();
        }
        let keyboard::Event::KeyPressed {
            key,
            modified_key,
            modifiers,
            ..
        } = event
        else {
            return Task::none();
        };
        // `key` est la touche *sans* Maj : en AZERTY, « / » et « ? » se tapent
        // avec Maj (« : » et « , » sans elle). Ces deux-là se lisent sur la
        // touche modifiée.
        match modified_key.as_ref() {
            Key::Character("?") => return self.update(Message::ToggleHelp),
            Key::Character("/") => return operation::focus(SEARCH_INPUT),
            _ => {}
        }
        match key.as_ref() {
            Key::Named(key::Named::Escape) if self.show_help => {
                self.show_help = false;
                Task::none()
            }
            Key::Character("l" | "L") if !modifiers.control() => self.like_shortcut(),
            Key::Character("z" | "Z") if modifiers.control() => self.update(Message::UndoRemove),
            Key::Character("m" | "M") if !modifiers.control() => self.update(Message::ToggleMute),
            Key::Named(key::Named::ArrowUp) => {
                let volume = self.state.volume.saturating_add(5).min(100);
                self.update(Message::Volume(volume))
            }
            Key::Named(key::Named::ArrowDown) => {
                let volume = self.state.volume.saturating_sub(5);
                self.update(Message::Volume(volume))
            }
            Key::Named(key::Named::Space) => self.send(Command::TogglePause),
            Key::Named(key::Named::ArrowRight) if modifiers.control() => self.send(Command::Next),
            Key::Named(key::Named::ArrowLeft) if modifiers.control() => {
                self.send(Command::Previous)
            }
            Key::Named(key::Named::ArrowRight) => self.seek_by(5.0),
            Key::Named(key::Named::ArrowLeft) => self.seek_by(-5.0),
            Key::Character("f" | "k") if modifiers.control() => operation::focus(SEARCH_INPUT),
            Key::Named(key::Named::F11) => self.update(Message::ToggleFullscreen),
            Key::Named(key::Named::Escape)
                if !self.selected.is_empty() && self.context_menu.is_none() =>
            {
                self.selected.clear();
                self.queue_selected = None;
                Task::none()
            }
            Key::Named(key::Named::Escape) if self.context_menu.is_some() => {
                self.context_menu = None;
                Task::none()
            }
            // Échap ne quitte plus une page : il remonte en haut (la vue « En
            // cours » se quitte toujours par Échap).
            Key::Named(key::Named::Escape) if self.route != Route::NowPlaying => {
                if self.page_view.offset > 0.5 {
                    self.scrolling.glide_to(PAGE_SCROLL, 0.0);
                }
                Task::none()
            }
            Key::Named(key::Named::Escape) | Key::Named(key::Named::BrowserBack) => {
                self.update(Message::Back)
            }
            _ => Task::none(),
        }
    }

    pub(super) fn seek_by(&mut self, delta: f32) -> Task<Message> {
        if self.state.current.is_none() || self.state.status == Status::Idle {
            return Task::none();
        }
        let max = self
            .state
            .duration
            .map_or(f32::MAX, |d| d.as_secs_f32() - 1.0);
        let target = (self.state.position.as_secs_f32() + delta).clamp(0.0, max.max(0.0));
        self.send(Command::Seek(Duration::from_secs_f32(target)))
    }
}
