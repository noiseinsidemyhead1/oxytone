//! Traitement des messages (le cœur de l'architecture Elm d'iced).
//!
//! En clair : chaque clic, touche, réponse du réseau ou mise à jour du moteur
//! devient un message ; `update` le traite et rend les tâches à lancer
//! ensuite. Après chaque message, les valeurs animées visent leur nouvelle
//! place (`aim_anims`).

use super::*;

impl App {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.update_inner(message);
        self.aim_anims();
        task
    }

    /// Où doivent aller les curseurs et interrupteurs animés, d'après l'état.
    pub(super) fn aim_anims(&mut self) {
        use crate::anim::Key;
        let eq = self.settings.draft.unwrap_or(self.state.effects.eq);
        for (i, gain) in eq.iter().enumerate() {
            self.anims.aim(Key::Band(i), f32::from(*gain), 0.0);
        }
        self.anims
            .aim(Key::Volume, f32::from(self.state.volume), 0.0);
        // Position : seul le saut demandé (clic sur la barre) est animé ; la
        // lecture qui avance ou un nouveau morceau sont suivis tels quels.
        match self.seek_preview {
            Some(target) => self.anims.aim(Key::Seek, target, 0.0),
            None => self
                .anims
                .aim(Key::Seek, self.state.position.as_secs_f32(), f32::INFINITY),
        }
        // Mode focus : paroles synchronisées arrivées (ou parties) → glissement.
        // Rien sans réseau ni morceau : la pochette revient au centre.
        if self.route == Route::NowPlaying
            && (self.np_lyrics_known() || self.state.current.is_none())
        {
            let target = if self.np_synced() { 1.0 } else { 0.0 };
            if target != self.np_split_target {
                self.np_split_target = target;
                self.np_split_anim = Some((std::time::Instant::now(), self.np_split));
            }
        }
        // Mode focus : l'ancienne pochette reste affichée jusqu'à ce que la
        // nouvelle soit là en pleine définition, puis fondu. La 512 px ne sert
        // que si la grande a échoué, ou après NP_HD_WAIT.
        if self.route == Route::NowPlaying {
            let current = self.current_cover();
            if current == self.np_art {
                self.np_waiting = None;
            } else {
                let since = *self.np_waiting.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= NP_HD_WAIT {
                    self.np_waiting = None;
                }
            }
            let ready = current.as_deref().is_some_and(|id| {
                let big = self.big_size();
                let big_failed = !self.covers.contains_key(&(id.to_owned(), big));
                let waited = self.np_waiting.is_none();
                self.cover(Some(id), big).is_some()
                    || ((big_failed || waited) && self.cover(Some(id), HERO).is_some())
            });
            if ready && current != self.np_art {
                let previous = std::mem::replace(&mut self.np_art, current);
                self.np_fade = previous
                    .is_some()
                    .then(|| (std::time::Instant::now(), previous));
            }
        }
        // Pages quittées : leur mémoire est rendue (pochettes HD du mode focus,
        // journal des écoutes des statistiques).
        if self.route != Route::NowPlaying && self.np_art.is_some() {
            self.np_art = None;
            self.np_fade = None;
            self.covers.retain(|(_, size), _| *size <= HERO);
        }
        if self.route != Route::Stats {
            self.stats.release();
        }
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        self.anims
            .aim(Key::Normalize, on(self.state.effects.normalize), 0.0);
        self.anims
            .aim(Key::Notifications, on(self.notifications_on()), 0.0);
        self.anims
            .aim(Key::OfflineMode, on(self.lib.offline_mode), 0.0);
        self.anims.aim(Key::Tray, on(self.tray), 0.0);
    }

    pub(super) fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::BackendReady(result) => match result.map(|slot| slot.take()) {
                Ok(Some(backend)) => {
                    let core = backend.core.clone();
                    let account = crate::library::wait_for_account(backend.account.clone());
                    let cache_dir = backend.library_cache.clone();
                    if let Some(liked) = crate::library::cached_liked(&cache_dir) {
                        self.lib.liked = liked.tracks.iter().map(|t| t.video_id.clone()).collect();
                        self.lib.liked_count = Some(liked.tracks.len());
                    }
                    self.lib.cache_dir = Some(cache_dir);
                    self.load_recent_searches();
                    // Pochette des notifications : même cache que l'interface.
                    let _ = self.notify.covers.set(Arc::clone(&backend.covers));
                    self.phase = Phase::Ready(backend);
                    self.apply_offline_mode();
                    let offline_mode = self.send(Command::SetOffline(self.lib.offline_mode));
                    // Comme une mise à jour : charge aussi la jaquette d'une session reprise.
                    let initial = self.on_core(CoreUpdate::State(core.state()));
                    // Touches média et overlay du système : il faut la fenêtre native.
                    let native = window::oldest().and_then(|id| window::run(id, native_handle));
                    // Playlists gardées hors ligne : redonnées au moteur, cache relevé.
                    let offline = self.load_offline();
                    Task::batch([
                        initial,
                        account,
                        offline,
                        offline_mode,
                        Task::stream(core_updates(core)),
                        native.map(Message::NativeWindow),
                    ])
                }
                Ok(None) => Task::none(),
                Err(e) => {
                    tracing::error!("démarrage impossible : {e}");
                    self.phase = Phase::Failed(e);
                    Task::none()
                }
            },
            Message::Core(update) => self.on_core(update),
            Message::Navigate(route) => self.navigate(route),
            Message::Back => match self.history.pop() {
                Some(route) => {
                    self.route = route.clone();
                    self.scrolling.reset(PAGE_SCROLL);
                    self.page_view.offset = 0.0;
                    let open = self.open(route);
                    self.update_page_accent();
                    // Pochette d'en-tête peut-être libérée entre-temps.
                    let hero = self.want_cover(self.page_cover(), HERO);
                    Task::batch([open, scroll_top(), hero])
                }
                None => Task::none(),
            },
            Message::SearchInput(text) => {
                self.search_query = text;
                self.search_seq += 1;
                let seq = self.search_seq;
                if self.route != Route::Search {
                    let previous = std::mem::replace(&mut self.route, Route::Search);
                    self.history.push(previous);
                }
                if ytmr_catalog::normalize_query(&self.search_query).is_empty() {
                    self.suggestions.clear();
                }
                Task::batch([
                    Task::perform(tokio::time::sleep(SUGGEST_DEBOUNCE), move |()| {
                        Message::SuggestDebounced(seq)
                    }),
                    Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |()| {
                        Message::SearchDebounced(seq)
                    }),
                ])
            }
            Message::SearchDebounced(seq) => {
                if seq == self.search_seq {
                    self.run_search()
                } else {
                    Task::none()
                }
            }
            Message::SearchSubmit if self.lib.offline_mode => {
                self.suggestions.clear();
                self.show_toast("Recherche indisponible en mode hors ligne".into())
            }
            Message::SearchSubmit => {
                self.remember_search_query();
                self.suggestions.clear();
                if self.route != Route::Search {
                    let previous = std::mem::replace(&mut self.route, Route::Search);
                    self.history.push(previous);
                }
                self.run_search()
            }
            Message::SearchDone(seq, result) => {
                if seq != self.search_seq {
                    return Task::none();
                }
                match result {
                    Ok(results) => {
                        let mut tasks = vec![self.want_track_covers(&results.songs, THUMB)];
                        for album in results.albums.iter().take(12) {
                            let id = self.url_cover(album.thumbnail_url.as_deref());
                            tasks.push(self.want_cover(id, CARD));
                        }
                        for artist in results.artists.iter().take(8) {
                            let id = self.url_cover(artist.thumbnail_url.as_deref());
                            tasks.push(self.want_cover(id, CARD));
                        }
                        // Les premiers résultats sont les plus probables : on les prépare
                        // (dans l'ordre inverse, chaque indice passant devant le précédent).
                        for track in results.songs.iter().take(3).rev() {
                            tasks.push(self.send(Command::Prefetch(track.video_id.clone())));
                        }
                        // On attend les pochettes (0,7 s au plus) : les résultats
                        // s'affichent d'un coup, complets.
                        let query = ytmr_catalog::normalize_query(&self.search_query);
                        self.search_pending = Some((seq, query, results));
                        self.try_reveal_search(false);
                        tasks.push(Task::perform(
                            tokio::time::sleep(SEARCH_COVERS_WAIT),
                            move |()| Message::SearchReveal(seq),
                        ));
                        Task::batch(tasks)
                    }
                    Err(e) => {
                        self.search_inflight = false;
                        self.search = Some(Load::Failed(e));
                        Task::none()
                    }
                }
            }
            Message::AlbumLoaded(id, result) => {
                if !matches!(&self.album, Some((cur, _)) if *cur == id) {
                    return Task::none();
                }
                match result {
                    Ok(album) => {
                        let cover = self.url_cover(album.thumbnail_url.as_deref());
                        let mut tasks = vec![
                            self.want_cover(cover.clone(), HERO),
                            self.want_accent(cover),
                        ];
                        for track in album.tracks.iter().take(3).rev() {
                            tasks.push(self.send(Command::Prefetch(track.video_id.clone())));
                        }
                        self.album = Some((id.clone(), Load::Ready(album)));
                        tasks.push(self.arrived(&Route::Album(id)));
                        Task::batch(tasks)
                    }
                    Err(e) => {
                        let failed = self.arrival_failed(&Route::Album(id.clone()), &e);
                        self.album = Some((id, Load::Failed(e)));
                        failed
                    }
                }
            }
            Message::ArtistLoaded(id, result) => {
                if !matches!(&self.artist, Some((cur, _)) if *cur == id) {
                    return Task::none();
                }
                match result {
                    Ok(artist) => {
                        let cover = self.url_cover(artist.thumbnail_url.as_deref());
                        let mut tasks = vec![
                            self.want_cover(cover.clone(), HERO),
                            self.want_accent(cover),
                            self.want_track_covers(&artist.songs, THUMB),
                        ];
                        for album in artist.albums.iter().chain(&artist.singles) {
                            let id = self.url_cover(album.thumbnail_url.as_deref());
                            tasks.push(self.want_cover(id, CARD));
                        }
                        self.artist = Some((id.clone(), Load::Ready(artist)));
                        tasks.push(self.arrived(&Route::Artist(id)));
                        Task::batch(tasks)
                    }
                    Err(e) => {
                        let failed = self.arrival_failed(&Route::Artist(id.clone()), &e);
                        self.artist = Some((id, Load::Failed(e)));
                        failed
                    }
                }
            }
            Message::CoverLoaded(id, size, result) => {
                match result {
                    Ok(handle) => {
                        self.covers.insert((id, size), Some(handle));
                        self.trim_covers();
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "jaquette indisponible");
                        // Retirée : une prochaine demande retentera (réseau revenu…).
                        self.covers.remove(&(id.clone(), size));
                        self.cover_failed.insert((id, size));
                    }
                }
                if self.search_pending.is_some() {
                    self.try_reveal_search(false);
                }
                Task::none()
            }
            Message::SearchReveal(seq) => {
                if self
                    .search_pending
                    .as_ref()
                    .is_some_and(|(s, _, _)| *s == seq)
                {
                    self.try_reveal_search(true);
                }
                Task::none()
            }
            Message::PlayAlbum(shuffle) => match &self.album {
                Some((_, Load::Ready(album))) => {
                    let tracks = album.tracks.clone();
                    self.play_list(tracks, shuffle)
                }
                _ => Task::none(),
            },
            Message::PlayArtist(shuffle) => match &self.artist {
                Some((_, Load::Ready(artist))) => {
                    let tracks = artist.songs.clone();
                    self.play_list(tracks, shuffle)
                }
                _ => Task::none(),
            },
            Message::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                let on = self.fullscreen;
                // Windows : plein écran fait main (voir `ytmr_os::fullscreen`) ;
                // celui de winit ramenait un vieux cadre système à la sortie.
                #[cfg(windows)]
                {
                    window::oldest().and_then(move |id| {
                        window::run(id, move |w| {
                            if let Some(hwnd) = window_hwnd(w) {
                                ytmr_os::fullscreen::set(hwnd, on);
                            }
                        })
                        .discard()
                    })
                }
                #[cfg(not(windows))]
                {
                    let mode = if on {
                        window::Mode::Fullscreen
                    } else {
                        window::Mode::Windowed
                    };
                    window::oldest().and_then(move |id| window::set_mode(id, mode))
                }
            }
            Message::OpenArtist(id, name) => match id {
                Some(id) => self.navigate(Route::Artist(id)),
                // Pas d'identifiant (morceau de radio) : on cherche l'artiste par son nom.
                None => {
                    let Some(catalog) = self.catalog() else {
                        return Task::none();
                    };
                    self.loading_since
                        .get_or_insert_with(std::time::Instant::now);
                    self.search_inflight = true;
                    Task::perform(
                        async move { catalog.find_artist(&name).await.map_err(|e| e.to_string()) },
                        Message::ArtistFound,
                    )
                }
            },
            Message::NativeWindow(handle) => {
                let Some(core) = self.backend().map(|b| b.core.clone()) else {
                    return Task::none();
                };
                let notify = self.notify.clone();
                Task::perform(
                    async move {
                        let runtime = tokio::runtime::Handle::current();
                        tokio::task::spawn_blocking(move || {
                            ytmr_os::start(core, handle, runtime, notify).map_err(|e| e.to_string())
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    Message::MediaControls,
                )
            }
            Message::Focus(on) => {
                self.notify
                    .focused
                    .store(on, std::sync::atomic::Ordering::Relaxed);
                Task::none()
            }
            Message::ToggleControls => {
                self.controls_visible = !self.controls_visible;
                self.controls_anim = Some((std::time::Instant::now(), self.controls_level));
                Task::none()
            }
            Message::QueueHover(row) => {
                self.queue_hover = row;
                // Même préchargement qu'au survol d'une liste, sans toucher à
                // son état (sinon la ligne de même titre s'allumait ailleurs).
                let video = row
                    .and_then(|i| self.queue.entries.get(i))
                    .map(|e| e.track.video_id.clone());
                match video {
                    Some(video_id) => {
                        self.queue_hover_video = Some(video_id.clone());
                        Task::perform(tokio::time::sleep(HOVER_PREFETCH_DELAY), move |()| {
                            Message::QueueHoverSettled(video_id.clone())
                        })
                    }
                    None => {
                        self.queue_hover_video = None;
                        Task::none()
                    }
                }
            }
            Message::QueueHoverSettled(video_id) => {
                if self.queue_hover_video.as_deref() == Some(video_id.as_str()) {
                    self.send(Command::Prefetch(video_id))
                } else {
                    Task::none()
                }
            }
            Message::MediaControls(result) => {
                match result {
                    Ok(()) => tracing::info!("touches média et overlay du système branchés"),
                    Err(e) => tracing::warn!(error = %e, "touches média indisponibles"),
                }
                Task::none()
            }
            Message::OpenAlbumOf(track) => match track.album_id {
                Some(id) => self.navigate(Route::Album(id)),
                None => {
                    let Some(catalog) = self.catalog() else {
                        return Task::none();
                    };
                    self.loading_since
                        .get_or_insert_with(std::time::Instant::now);
                    self.search_inflight = true;
                    Task::perform(
                        async move { catalog.album_of(&track).await.map_err(|e| e.to_string()) },
                        Message::AlbumOfFound,
                    )
                }
            },
            Message::AlbumOfFound(result) => {
                self.search_inflight = false;
                match result {
                    Ok(Some(id)) => self.navigate(Route::Album(id)),
                    Ok(None) => self.show_toast("Album introuvable pour ce titre".to_owned()),
                    Err(e) => self.show_toast(format!("Recherche de l'album impossible : {e}")),
                }
            }
            Message::ArtistFound(result) => {
                self.search_inflight = false;
                match result {
                    Ok(Some(id)) => self.navigate(Route::Artist(id)),
                    Ok(None) => self.show_toast("Page de l'artiste introuvable".to_owned()),
                    Err(e) => self.show_toast(format!("Recherche de l'artiste impossible : {e}")),
                }
            }
            Message::AccentLoaded(id, result) => {
                let Ok(rgb) = result else {
                    return Task::none();
                };
                let color = style::accent_from_rgb(rgb);
                self.accents.insert(id.clone(), color);
                if self.current_cover().as_deref() == Some(id.as_str()) {
                    self.accent_fade.set(self.accent, color);
                }
                self.update_page_accent();
                Task::none()
            }
            Message::Play(tracks, start) => {
                if self.route == Route::Search {
                    self.remember_search();
                }
                self.send(Command::PlayTracks { tracks, start })
            }
            Message::SearchTab(tab) => {
                self.search_tab = tab;
                self.scroll_page_top()
            }
            Message::SearchRecent(query) => {
                self.search_query = query;
                self.search_tab = SearchTab::All;
                let submit = self.update(Message::SearchSubmit);
                self.remember_search_query();
                submit
            }
            Message::SearchForget(query) => {
                self.forget_search(Some(&query));
                Task::none()
            }
            Message::SearchClearRecent => {
                self.forget_search(None);
                Task::none()
            }
            Message::PlayShuffled(tracks) => {
                if tracks.is_empty() {
                    return Task::none();
                }
                // Le moteur mélange toute la liste et part du premier tiré.
                self.send(Command::PlayShuffled(tracks))
            }
            Message::PlayNext(track) => {
                let toast = self.show_toast(format!("« {} » sera lu ensuite", track.title));
                Task::batch([self.send(Command::PlayNext(vec![track])), toast])
            }
            Message::Enqueue(track) => {
                let toast = self.show_toast(format!("« {} » ajouté à la file", track.title));
                Task::batch([self.send(Command::Enqueue(vec![track])), toast])
            }
            Message::TogglePause => self.send(Command::TogglePause),
            Message::Next => self.send(Command::Next),
            Message::Previous => self.send(Command::Previous),
            Message::SeekPreview(seconds) => {
                self.seek_preview = Some(seconds);
                Task::none()
            }
            Message::SeekCommit => match self.seek_preview.take() {
                Some(seconds) => {
                    self.state.position = Duration::from_secs_f32(seconds.max(0.0));
                    self.send(Command::Seek(self.state.position))
                }
                None => Task::none(),
            },
            Message::Volume(volume) => {
                self.state.volume = volume;
                self.muted_volume = None;
                self.send(Command::SetVolume(volume))
            }
            Message::ToggleMute => match self.muted_volume.take() {
                Some(previous) => self.send(Command::SetVolume(previous)),
                None => {
                    self.muted_volume = Some(self.state.volume.max(1));
                    self.send(Command::SetVolume(0))
                }
            },
            Message::ToggleShuffle => {
                // La file va être remélangée : la sélection n'a plus de sens.
                self.queue_selected = None;
                self.shuffle_pending = true;
                self.send(Command::SetShuffle(!self.state.shuffle))
            }
            Message::CycleRepeat => self.send(Command::SetRepeat(self.state.repeat.cycle())),
            Message::ToggleQueue => {
                self.show_queue = !(self.show_queue && self.right_panel == RightPanel::Queue);
                self.right_panel = RightPanel::Queue;
                self.save_ui();
                self.request_visible_covers()
            }
            Message::ToggleLyrics => {
                self.show_queue = !(self.show_queue && self.right_panel == RightPanel::Lyrics);
                self.right_panel = RightPanel::Lyrics;
                self.lyrics_line = None;
                self.save_ui();
                self.want_lyrics()
            }
            Message::JumpTo(id) => self.send(Command::JumpTo(id)),
            Message::Select(video_id, index) => self.select(video_id, index),
            Message::PlayNextAll(tracks) => self.send(Command::PlayNext(tracks)),
            Message::EnqueueAll(tracks) => self.send(Command::Enqueue(tracks)),
            Message::DragTracks(step) => self.drag_tracks(step),
            Message::DropEnter(target) => {
                self.drop_target = Some(target);
                Task::none()
            }
            Message::DropLeave(target) => {
                // En passant d'une zone à l'autre, l'entrée arrive parfois avant la sortie.
                if self.drop_target.as_ref() == Some(&target) {
                    self.drop_target = None;
                }
                Task::none()
            }
            Message::ToggleHelp => {
                self.show_help = !self.show_help;
                Task::none()
            }
            Message::CardHover(key) => {
                self.card_hover = Some(key);
                Task::none()
            }
            Message::CardUnhover(key) => {
                if self.card_hover.as_ref() == Some(&key) {
                    self.card_hover = None;
                }
                Task::none()
            }
            Message::PlayItem(kind, id) => self.play_item(kind, id),
            Message::PlayFetched(result) => match result {
                Ok(tracks) if !tracks.is_empty() => {
                    self.send(Command::PlayTracks { tracks, start: 0 })
                }
                Ok(_) => self.show_toast("Rien à lire ici".into()),
                Err(e) => self.show_toast(format!("Lecture impossible : {e}")),
            },
            Message::QueueUnhover(row) => {
                // Seulement si c'est bien la ligne survolée : en remontant, la
                // nouvelle ligne est survolée avant que l'ancienne soit quittée.
                if self.queue_hover == Some(row) {
                    self.queue_hover = None;
                    self.queue_hover_video = None;
                }
                Task::none()
            }
            Message::Remove(id) => self.send(Command::Remove(id)),
            Message::ClearQueue => self.send(Command::ClearUpcoming),
            // Une fenêtre (ajout à une playlist, menu) couvre la page : ce qui
            // est dessous ne réagit pas au survol (ni préchargement inutile).
            Message::Hover(_) if self.lib.overlay.is_some() || self.context_menu.is_some() => {
                Task::none()
            }
            Message::Hover(video_id) => {
                self.hovered = Some(video_id.clone());
                Task::perform(tokio::time::sleep(HOVER_PREFETCH_DELAY), move |()| {
                    Message::HoverSettled(video_id.clone())
                })
            }
            Message::HoverSettled(video_id) => {
                if self.hovered.as_deref() == Some(video_id.as_str()) {
                    self.send(Command::Prefetch(video_id))
                } else {
                    Task::none()
                }
            }
            Message::Unhover(video_id) => {
                // Même raison que pour la file : la ligne quittée ne doit pas
                // effacer celle qu'on vient de survoler.
                if self.hovered.as_deref() == Some(video_id.as_str()) {
                    self.hovered = None;
                }
                Task::none()
            }
            Message::CommandDone(result) => match result {
                Ok(()) => Task::none(),
                Err(e) => self.show_toast(e),
            },
            Message::Key(event) => self.on_key(event),
            Message::DismissToast(seq) => {
                if self.toast.as_ref().is_some_and(|(s, _)| *s == seq) {
                    self.toast = None;
                }
                Task::none()
            }
            Message::CloseRequested => {
                if self.shutting_down {
                    return Task::none();
                }
                // Icône active : la fenêtre se cache, la musique continue
                // (« Quitter » dans le menu de l'icône ferme pour de bon).
                #[cfg(windows)]
                {
                    if self.tray && !ytmr_os::tray::quit_requested() {
                        ytmr_os::tray::hide_window();
                        return Task::none();
                    }
                    ytmr_os::tray::shutdown();
                }
                self.shutting_down = true;
                match std::mem::replace(&mut self.phase, Phase::Starting) {
                    Phase::Ready(backend) => {
                        Task::perform(backend.shutdown(), |()| Message::ShutdownDone)
                    }
                    _ => iced::exit(),
                }
            }
            // Mode focus : les paroles défilent seules, la molette n'y fait rien.
            Message::Wheel(NP_LYRICS_SCROLL, _) => Task::none(),
            Message::Wheel(id, lines) => {
                if id == LYRICS_SCROLL {
                    // On lit plus loin : le suivi reprend 4 s après le dernier cran.
                    self.lyrics_manual_until =
                        Some(std::time::Instant::now() + Duration::from_secs(4));
                }
                self.scrolling.wheel(id, lines);
                Task::none()
            }
            Message::Scrolled(id, viewport) => {
                self.scrolling.scrolled(id, &viewport);
                let view = View {
                    offset: viewport.absolute_offset().y,
                    height: viewport.bounds().height,
                };
                if id == PAGE_SCROLL {
                    self.page_view = view;
                    if self.route == Route::Home {
                        // Rangées de l'accueil qui arrivent à l'écran.
                        let covers = self.want_home_covers();
                        self.covers_seq += 1;
                        let seq = self.covers_seq;
                        return Task::batch([
                            covers,
                            Task::perform(tokio::time::sleep(COVERS_SETTLE), move |()| {
                                Message::CoversSettle(seq)
                            }),
                        ]);
                    }
                } else if id == QUEUE_SCROLL {
                    self.queue_view = view;
                } else if id == LYRICS_SCROLL {
                    self.lyrics_view = view;
                } else if id == NP_LYRICS_SCROLL {
                    self.np_lyrics_view = view;
                }
                self.covers_seq += 1;
                let seq = self.covers_seq;
                Task::perform(tokio::time::sleep(COVERS_SETTLE), move |()| {
                    Message::CoversSettle(seq)
                })
            }
            Message::CoversSettle(seq) => {
                if seq == self.covers_seq {
                    self.request_visible_covers()
                } else {
                    Task::none()
                }
            }
            Message::DragStart(panel) => {
                self.dragging = Some(panel);
                self.drag_anchor = None;
                Task::none()
            }
            Message::DragMove(x) => {
                // Relatif au point de départ : ne dépend pas de la largeur de la fenêtre.
                let Some(panel) = self.dragging else {
                    return Task::none();
                };
                let current = match panel {
                    Panel::Left => self.left_width,
                    Panel::Right => self.queue_width,
                };
                let (x0, w0) = *self.drag_anchor.get_or_insert((x, current));
                match panel {
                    Panel::Left => {
                        self.left_width = (w0 + x - x0).clamp(LEFT_RANGE.0, LEFT_RANGE.1)
                    }
                    Panel::Right => {
                        self.queue_width = (w0 + x0 - x).clamp(RIGHT_RANGE.0, RIGHT_RANGE.1);
                    }
                }
                Task::none()
            }
            Message::QueueDrag(step) => self.queue_drag_step(step),
            Message::DragEnd => {
                self.dragging = None;
                self.drag_anchor = None;
                self.save_ui();
                Task::none()
            }
            Message::WindowResized(size) => {
                self.window_width = size.width;
                self.window_size = size;
                // Changement d'écran possible : l'échelle est redemandée.
                let scale = window::oldest()
                    .and_then(window::scale_factor)
                    .map(Message::ScaleFactor);
                Task::batch([self.refresh_maximized(), scale])
            }
            Message::ScaleFactor(scale) => {
                self.scale_factor = scale;
                Task::none()
            }
            Message::Batch(messages) => Task::batch(
                messages
                    .into_iter()
                    .map(|m| self.update(m))
                    .collect::<Vec<_>>(),
            ),
            Message::Context(message) => self.update_context(message),
            Message::LyricsLoaded(video_id, result) => {
                let load = match result {
                    Ok(found) => Load::Ready(found),
                    Err(e) => {
                        tracing::debug!("paroles : {e}");
                        Load::Failed(e)
                    }
                };
                self.lyrics.insert(video_id, load);
                self.lyrics_line = None;
                self.follow_lyrics();
                Task::none()
            }
            Message::SeekTo(at) => self.send(Command::Seek(at)),
            Message::SuggestDebounced(seq) => {
                if seq != self.search_seq {
                    return Task::none();
                }
                let query = ytmr_catalog::normalize_query(&self.search_query);
                let Some(catalog) = self.catalog() else {
                    return Task::none();
                };
                Task::perform(
                    async move { catalog.suggestions(&query).await.map_err(|e| e.to_string()) },
                    move |r| Message::Suggestions(seq, r),
                )
            }
            Message::Suggestions(seq, result) => {
                if seq == self.search_seq {
                    let query = ytmr_catalog::normalize_query(&self.search_query).to_lowercase();
                    // Inutile de proposer exactement ce qui est déjà tapé.
                    self.suggestions = result
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|s| s.to_lowercase() != query)
                        .collect();
                }
                Task::none()
            }
            Message::PickSuggestion(query) => {
                self.search_query = query;
                self.suggestions.clear();
                self.search_seq += 1;
                self.run_search()
            }
            Message::Frame(now) => {
                self.anims.frame(now);
                self.frame_now = now;
                if self.queue_fade.is_some_and(|f| f.is_done(now)) {
                    self.queue_fade = None;
                }
                if self
                    .np_fade
                    .as_ref()
                    .is_some_and(|(since, _)| now.saturating_duration_since(*since) >= NP_FADE)
                {
                    self.np_fade = None;
                }
                if let Some((since, from)) = self.np_split_anim {
                    let t = (now.saturating_duration_since(since).as_secs_f32()
                        / NP_SPLIT.as_secs_f32())
                    .min(1.0);
                    let eased = t * t * (3.0 - 2.0 * t);
                    self.np_split = from + (self.np_split_target - from) * eased;
                    if t >= 1.0 {
                        self.np_split_anim = None;
                    }
                }
                if let Some((since, from)) = self.controls_anim {
                    let t = (now.saturating_duration_since(since).as_secs_f32()
                        / CONTROLS_FADE.as_secs_f32())
                    .min(1.0);
                    // Courbe douce (début et fin ralentis).
                    let eased = t * t * (3.0 - 2.0 * t);
                    let target = if self.controls_visible { 1.0 } else { 0.0 };
                    self.controls_level = from + (target - from) * eased;
                    if t >= 1.0 {
                        self.controls_anim = None;
                    }
                }
                if let Some((since, frames)) = &mut self.fps_meter {
                    *frames += 1;
                    let elapsed = now.saturating_duration_since(*since);
                    if elapsed >= Duration::from_secs(1) {
                        tracing::info!(
                            fps = *frames as f32 / elapsed.as_secs_f32(),
                            fullscreen = self.fullscreen,
                            route = ?self.route,
                            "images par seconde"
                        );
                        *since = now;
                        *frames = 0;
                    }
                }
                if self.is_loading() {
                    let since = *self.loading_since.get_or_insert(now);
                    self.loading_phase = now.saturating_duration_since(since).as_secs_f32();
                } else {
                    self.loading_since = None;
                }
                self.accent = self.accent_fade.at(now);
                self.page_accent = self.page_fade.at(now);
                self.scrolling.frame(now)
            }
            Message::WheelH(id, lines) => {
                self.scrolling.wheel_horizontal(id, lines);
                Task::none()
            }
            Message::ShelfScrolled(id, viewport) => {
                self.scrolling.scrolled_horizontal(id, &viewport);
                Task::none()
            }
            Message::BacklogCover(id, size, result) => {
                self.backlog_inflight = self.backlog_inflight.saturating_sub(1);
                let key = (id, size);
                match result {
                    Ok(handle) => {
                        self.touch_cover(&key);
                        // Demandée entre-temps pour être affichée : on la garde.
                        if !self.visible_covers.contains(&key) {
                            self.cover_evictable.insert(key.clone());
                        }
                        self.covers.insert(key, Some(handle));
                        self.trim_covers();
                    }
                    Err(_) => {
                        if matches!(self.covers.get(&key), Some(None)) {
                            self.covers.remove(&key);
                        }
                    }
                }
                self.pump_backlog()
            }
            Message::Lib(message) => self.update_library(message),
            Message::Settings(message) => self.update_settings(message),
            Message::Stats(message) => self.update_stats(message),
            Message::Import(message) => self.update_import(message),
            Message::Toast(message) => self.show_toast(message),
            Message::Coll(message) => self.update_collection(message),
            Message::Window(action) => self.update_window(action),
            Message::Noop => Task::none(),
            Message::ShutdownDone => iced::exit(),
        }
    }
}
