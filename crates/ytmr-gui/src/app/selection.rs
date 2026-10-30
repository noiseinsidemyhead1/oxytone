//! Sélection de morceaux, glisser-déposer, raccourci « L ».
//!
//! En clair : un clic sélectionne (Ctrl ajoute, Maj prend une plage) ; un
//! appui maintenu puis déplacé emporte la sélection vers la file ou une
//! playlist ; dans la file, on réordonne les morceaux à la souris.

use super::*;

impl App {
    pub(super) fn queue_drag_step(&mut self, step: QueueDrag) -> Task<Message> {
        match step {
            QueueDrag::Press(i) => {
                self.queue_selected = self.queue.entries.get(i).map(|e| e.id);
                self.queue_drag = Some(QueueDragState {
                    from: i,
                    over: i,
                    moved: false,
                });
                Task::none()
            }
            QueueDrag::Over(i) => {
                if let Some(d) = &mut self.queue_drag {
                    d.moved |= i != d.from;
                    d.over = i;
                }
                Task::none()
            }
            QueueDrag::Release => {
                let Some(d) = self.queue_drag.take() else {
                    return Task::none();
                };
                let Some(entry) = self.queue.entries.get(d.from) else {
                    return Task::none();
                };
                let id = entry.id;
                // Un clic simple ne lance rien (double-clic pour cela).
                if !d.moved {
                    return Task::none();
                }
                // Le morceau en cours reste en tête : rien ne passe devant lui.
                let first = usize::from(self.queue.current.is_some());
                if d.from < first || d.over == d.from {
                    return Task::none();
                }
                let to = d.over.max(first);
                // Affiché tout de suite ; le core confirme juste après.
                let queue = Arc::make_mut(&mut self.queue);
                let moved = queue.entries.remove(d.from);
                let at = to.min(queue.entries.len());
                queue.entries.insert(at, moved);
                self.send(Command::Move { id, to })
            }
        }
    }

    /// Morceaux de la liste affichée (pour la sélection et le glisser-déposer).
    pub(crate) fn current_list(&self) -> Vec<&Track> {
        match &self.route {
            // Dans l'ordre affiché (tri et recherche), comme les rangs des lignes.
            Route::Playlist(_) => match &self.lib.playlist {
                Some((_, Load::Ready(p))) => crate::library::view_order(self, p)
                    .into_iter()
                    .map(|i| &p.tracks[i])
                    .collect(),
                _ => Vec::new(),
            },
            Route::Album(_) => match &self.album {
                Some((_, Load::Ready(a))) => a.tracks.iter().collect(),
                _ => Vec::new(),
            },
            Route::Artist(_) => match &self.artist {
                Some((_, Load::Ready(a))) => a.songs.iter().collect(),
                _ => Vec::new(),
            },
            Route::History => match &self.lib.history {
                Some(Load::Ready(t)) => t.iter().collect(),
                _ => Vec::new(),
            },
            Route::Search => match &self.search {
                Some(Load::Ready(r)) => r
                    .songs
                    .iter()
                    .take(crate::views::search_songs_shown(self))
                    .collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Morceaux sélectionnés, dans l'ordre de la liste affichée.
    pub(crate) fn selected_tracks(&self) -> Vec<Track> {
        self.current_list()
            .into_iter()
            .filter(|t| self.selected.contains(&t.video_id))
            .cloned()
            .collect()
    }

    /// Nom d'une playlist de la bibliothèque (étiquette de glisser-déposer).
    pub(crate) fn playlist_name(&self, id: &str) -> String {
        match &self.lib.playlists {
            Some(Load::Ready(list)) => list
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.title.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub(super) fn select(&mut self, video_id: String, index: usize) -> Task<Message> {
        let list: Vec<String> = self
            .current_list()
            .iter()
            .map(|t| t.video_id.clone())
            .collect();
        let mut collapse_to = None;
        if self.modifiers.shift()
            && let Some(anchor) = self.select_anchor
            && !list.is_empty()
        {
            let last = list.len() - 1;
            let (a, b) = (anchor.min(index).min(last), anchor.max(index).min(last));
            let range = list[a..=b].to_vec();
            if self.modifiers.control() {
                for id in range {
                    if !self.selected.contains(&id) {
                        self.selected.push(id);
                    }
                }
            } else {
                self.selected = range;
            }
        } else if self.modifiers.control() {
            match self.selected.iter().position(|v| *v == video_id) {
                Some(at) => {
                    self.selected.remove(at);
                }
                None => self.selected.push(video_id.clone()),
            }
            self.select_anchor = Some(index);
        } else {
            // Ligne d'une sélection multiple : elle reste entière (on va peut-être
            // la glisser) et se réduit à cette ligne si ce n'était qu'un clic.
            if self.selected.len() > 1 && self.selected.contains(&video_id) {
                collapse_to = Some(video_id.clone());
            } else {
                self.selected = vec![video_id.clone()];
            }
            self.select_anchor = Some(index);
        }
        // Prêt à glisser : la sélection si la ligne en fait partie, sinon elle seule.
        let tracks = if self.selected.contains(&video_id) {
            self.selected_tracks()
        } else {
            self.current_list()
                .into_iter()
                .filter(|t| t.video_id == video_id)
                .take(1)
                .cloned()
                .collect()
        };
        self.track_drag = (!tracks.is_empty()).then_some(TrackDrag {
            tracks,
            origin: None,
            pos: iced::Point::ORIGIN,
            active: false,
            collapse_to,
        });
        Task::none()
    }

    pub(super) fn drag_tracks(&mut self, step: DragStep) -> Task<Message> {
        const THRESHOLD: f32 = 8.0;
        match step {
            DragStep::Move(position) => {
                if let Some(drag) = &mut self.track_drag {
                    let origin = *drag.origin.get_or_insert(position);
                    drag.pos = position;
                    if !drag.active && origin.distance(position) > THRESHOLD {
                        drag.active = true;
                        self.drop_target = None;
                    }
                }
                Task::none()
            }
            DragStep::Release => {
                let Some(drag) = self.track_drag.take() else {
                    return Task::none();
                };
                let target = self.drop_target.take();
                if !drag.active {
                    if let Some(id) = drag.collapse_to {
                        self.selected = vec![id];
                    }
                    return Task::none();
                }
                let count = drag.tracks.len();
                match target {
                    Some(DropTarget::Queue) => Task::batch([
                        self.send(Command::Enqueue(drag.tracks)),
                        self.show_toast(format!("{count} titre(s) ajouté(s) à la file")),
                    ]),
                    Some(DropTarget::Playlist(id)) => {
                        self.update(Message::Lib(crate::library::Lib::AddTo(id, drag.tracks)))
                    }
                    None => Task::none(),
                }
            }
        }
    }

    /// L : like de la sélection, sinon du morceau en cours.
    pub(super) fn like_shortcut(&mut self) -> Task<Message> {
        let mut tracks = self.selected_tracks();
        if tracks.is_empty()
            && let Some(entry) = &self.state.current
        {
            tracks.push(entry.track.clone());
        }
        let tasks: Vec<Task<Message>> = tracks
            .into_iter()
            .map(|t| self.update(Message::Lib(crate::library::Lib::ToggleLike(t))))
            .collect();
        Task::batch(tasks)
    }
}
