//! Synchronisation avec le moteur de lecture.
//!
//! En clair : le moteur publie son état et sa file ; l'interface en garde une
//! copie et réagit aux changements (nouveau morceau, file modifiée, erreurs).

use super::*;

impl App {
    pub(super) fn on_core(&mut self, update: CoreUpdate) -> Task<Message> {
        match update {
            CoreUpdate::State(state) => {
                let track_changed = state.current.as_ref().map(|e| e.id)
                    != self.state.current.as_ref().map(|e| e.id);
                let previous = self
                    .state
                    .current
                    .as_ref()
                    .map(|e| e.track.video_id.clone());
                self.state = state;
                if self.muted_volume.is_some() && self.state.volume > 0 {
                    self.muted_volume = None;
                }
                let report = match &self.state.current {
                    Some(entry) if self.state.status == Status::Playing => {
                        self.report_play(entry.id, entry.track.video_id.clone())
                    }
                    _ => Task::none(),
                };
                if !track_changed {
                    self.follow_lyrics();
                    return report;
                }
                // Minuteur « fin du morceau » : le suivant s'arrête aussitôt.
                if self.sleep == Some(Sleep::EndOfTrack) && previous.is_some() {
                    self.sleep = None;
                    return Task::batch([
                        self.send(Command::Pause),
                        self.send(Command::Seek(Duration::ZERO)),
                        self.show_toast("Bonne nuit".to_owned()),
                    ]);
                }
                // Nouveau morceau : les anciennes paroles s'estompent (figées sur
                // leur dernière ligne), puis la liste remonte et les nouvelles
                // apparaissent en fondu. Avant : saut en haut et « Chargement… ».
                let now = std::time::Instant::now();
                self.track_since = now;
                self.lyrics_out = previous.map(|v| (v, now, self.lyrics_line));
                self.lyrics_line = None;
                self.lyrics_prev_line = None;
                let new_id = self
                    .state
                    .current
                    .as_ref()
                    .map(|e| e.track.video_id.clone())
                    .unwrap_or_default();
                self.lyrics_in = Some((new_id.clone(), now + LYRICS_OUT));
                let lyrics_top = Task::perform(tokio::time::sleep(LYRICS_OUT), move |()| {
                    Message::LyricsReset(new_id.clone())
                });
                let cover = self.current_cover();
                if let Some(color) = cover.as_ref().and_then(|id| self.accents.get(id)).copied() {
                    self.accent_fade.set(self.accent, color);
                }
                self.update_page_accent();
                Task::batch([
                    report,
                    lyrics_top,
                    self.want_lyrics(),
                    self.want_cover(cover.clone(), HERO),
                    if self.route == Route::NowPlaying {
                        self.want_cover(cover.clone(), self.big_size())
                    } else {
                        Task::none()
                    },
                    self.want_cover(cover.clone(), THUMB),
                    self.want_accent(cover),
                ])
            }
            CoreUpdate::Queue(queue) => {
                if std::mem::take(&mut self.shuffle_pending) {
                    let now = std::time::Instant::now();
                    self.frame_now = now;
                    self.queue_fade = Some(crate::anim::QueueFade::new(now));
                }
                self.queue = queue;
                // Morceau remis par Ctrl + Z : à sa place d'avant.
                let replace = self.undo_move.take().and_then(|(video_id, to)| {
                    let entry = self
                        .queue
                        .entries
                        .iter()
                        .rposition(|e| e.track.video_id == video_id)?;
                    let id = self.queue.entries[entry].id;
                    (entry != to).then(|| self.send(Command::Move { id, to: to.max(1) }))
                });
                if let Some(task) = replace {
                    return Task::batch([task, self.request_visible_covers()]);
                }
                // Seulement les lignes visibles : une file de 900 titres ne doit
                // pas déclencher 900 téléchargements.
                self.request_visible_covers()
            }
            CoreUpdate::Event(CoreEvent::Error(message)) => self.show_toast(message),
            CoreUpdate::Event(_) => Task::none(),
        }
    }
}

/// Flux des changements du moteur, converti en messages pour l'interface.
pub(super) fn core_updates(
    core: ytmr_core::CoreHandle,
) -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(64, async move |mut output| {
        let mut state = core.watch_state();
        let mut queue = core.watch_queue();
        let mut events = core.subscribe();
        let initial = Arc::clone(&queue.borrow_and_update());
        let _ = output.send(Message::Core(CoreUpdate::Queue(initial))).await;
        loop {
            let message = tokio::select! {
                changed = state.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    CoreUpdate::State(state.borrow_and_update().clone())
                }
                changed = queue.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    CoreUpdate::Queue(Arc::clone(&queue.borrow_and_update()))
                }
                event = events.recv() => match event {
                    Ok(event @ CoreEvent::Error(_)) => CoreUpdate::Event(event),
                    Ok(_) | Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
            };
            if output.send(Message::Core(message)).await.is_err() {
                break;
            }
        }
    })
}
