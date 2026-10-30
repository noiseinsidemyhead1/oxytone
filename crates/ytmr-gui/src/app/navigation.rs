//! Navigation entre les pages, recherche, lectures lancées depuis une page.
//!
//! En clair : aller sur une page charge ce qu'elle affiche (album, artiste,
//! playlist…) ; une page pas encore prête reste en attente sous une barre de
//! chargement, puis s'affiche d'un coup.

use super::*;

impl App {
    /// Navigation déclenchée par le code (après une suppression, par exemple).
    pub(crate) fn go_to(&mut self, route: Route) -> Task<Message> {
        self.navigate(route)
    }

    pub(super) fn navigate(&mut self, route: Route) -> Task<Message> {
        if self.route == route {
            // Re-cliquer sur la pochette en grand ramène à la page d'avant.
            return if route == Route::NowPlaying {
                self.update(Message::Back)
            } else {
                Task::none()
            };
        }
        if self.pending_route.as_ref() == Some(&route) {
            return Task::none();
        }
        if self.needs_loading(&route) {
            // On charge d'abord, on change de page ensuite.
            self.pending_route = Some(route.clone());
            self.loading_since = Some(std::time::Instant::now());
            return self.open(route);
        }
        self.pending_route = None;
        self.switch_to(route)
    }

    /// Page dont les données ne sont pas encore là (ni en mémoire, ni en cache).
    pub(super) fn needs_loading(&self, route: &Route) -> bool {
        match route {
            Route::Album(id) => !matches!(&self.album, Some((cur, Load::Ready(_))) if cur == id),
            Route::Artist(id) => !matches!(&self.artist, Some((cur, Load::Ready(_))) if cur == id),
            Route::Playlist(id) => {
                let shown = matches!(&self.lib.playlist, Some((cur, Load::Ready(_))) if cur == id);
                !shown && !crate::library::has_cache(&self.lib, id)
            }
            _ => false,
        }
    }

    /// Données arrivées pour `route` : si on l'attendait, on y va.
    pub(crate) fn arrived(&mut self, route: &Route) -> Task<Message> {
        if self.pending_route.as_ref() == Some(route) {
            self.pending_route = None;
            self.loading_since = None;
            self.switch_to(route.clone())
        } else {
            Task::none()
        }
    }

    /// Le chargement attendu a échoué : on reste où on est, avec un message.
    pub(crate) fn arrival_failed(&mut self, route: &Route, error: &str) -> Task<Message> {
        if self.pending_route.as_ref() == Some(route) {
            self.pending_route = None;
            self.loading_since = None;
            self.show_toast(format!("Chargement impossible : {error}"))
        } else {
            Task::none()
        }
    }

    pub fn is_loading(&self) -> bool {
        self.pending_route.is_some() || self.lib.streaming.is_some() || self.search_inflight
    }

    /// Remet la page centrale en haut (nouvelle recherche dans une playlist…).
    pub(crate) fn scroll_page_top(&mut self) -> Task<Message> {
        self.scrolling.reset(PAGE_SCROLL);
        self.page_view.offset = 0.0;
        scroll_top()
    }

    pub(super) fn switch_to(&mut self, route: Route) -> Task<Message> {
        let previous = std::mem::replace(&mut self.route, route.clone());
        self.history.push(previous);
        self.scrolling.reset(PAGE_SCROLL);
        self.page_view.offset = 0.0;
        let open = self.open(route);
        self.update_page_accent();
        let hero = self.want_cover(self.page_cover(), HERO);
        Task::batch([open, scroll_top(), hero])
    }

    /// Charge les données de la page (album, artiste) si besoin.
    pub(super) fn open(&mut self, route: Route) -> Task<Message> {
        let Some(backend) = self.backend() else {
            return Task::none();
        };
        let catalog = Arc::clone(&backend.catalog);
        match route {
            Route::Album(id) => {
                if matches!(&self.album, Some((cur, Load::Ready(_))) if *cur == id) {
                    return Task::none();
                }
                self.album = Some((id.clone(), Load::Loading));
                Task::perform(
                    async move {
                        let result = catalog.album(&id).await.map_err(|e| e.to_string());
                        (id, result)
                    },
                    |(id, r)| Message::AlbumLoaded(id, r),
                )
            }
            Route::Artist(id) => {
                if matches!(&self.artist, Some((cur, Load::Ready(_))) if *cur == id) {
                    return Task::none();
                }
                self.artist = Some((id.clone(), Load::Loading));
                Task::perform(
                    async move {
                        let result = catalog.artist(&id).await.map_err(|e| e.to_string());
                        (id, result)
                    },
                    |(id, r)| Message::ArtistLoaded(id, r),
                )
            }
            Route::Search => operation::focus(SEARCH_INPUT),
            Route::Library => Task::batch([
                self.load_playlists(),
                self.load_collection(),
                self.collection_covers(),
            ]),
            Route::Playlist(id) => self.open_playlist(id),
            Route::Home => self.refresh_home(),
            Route::NowPlaying => {
                // Les paroles de la grande vue repartent de la ligne chantée.
                self.lyrics_line = None;
                self.follow_lyrics();
                self.want_lyrics()
            }
            Route::Settings => self.open_settings(),
            Route::History => self.load_history_page(),
            Route::Stats => self.load_stats(),
        }
    }

    pub(super) fn run_search(&mut self) -> Task<Message> {
        let query = ytmr_catalog::normalize_query(&self.search_query);
        if query.is_empty() {
            self.search = None;
            self.search_inflight = false;
            self.search_pending = None;
            return Task::none();
        }
        if query == self.search_shown && matches!(self.search, Some(Load::Ready(_))) {
            return Task::none();
        }
        let Some(catalog) = self.backend().map(|b| Arc::clone(&b.catalog)) else {
            return Task::none();
        };
        self.search_seq += 1;
        let seq = self.search_seq;
        if self.search.is_none() {
            self.search = Some(Load::Loading);
        }
        self.search_inflight = true;
        self.loading_since
            .get_or_insert_with(std::time::Instant::now);
        Task::perform(
            async move { catalog.search_all(&query).await.map_err(|e| e.to_string()) },
            move |r| Message::SearchDone(seq, r),
        )
    }

    /// Lecture d'une liste, dans l'ordre ou en aléatoire.
    pub(super) fn play_list(&mut self, tracks: Vec<Track>, shuffle: bool) -> Task<Message> {
        if tracks.is_empty() {
            return Task::none();
        }
        if shuffle {
            self.update(Message::PlayShuffled(tracks))
        } else {
            self.send(Command::PlayTracks { tracks, start: 0 })
        }
    }

    /// Affiche les résultats en attente si leurs pochettes sont là (ou `force`).
    pub(super) fn try_reveal_search(&mut self, force: bool) {
        let Some((_, _, results)) = &self.search_pending else {
            return;
        };
        let loaded = |id: Option<String>, size: u32| {
            id.is_none_or(|id| {
                let key = (id, size);
                matches!(self.covers.get(&key), Some(Some(_))) || self.cover_failed.contains(&key)
            })
        };
        let ready = results
            .songs
            .iter()
            .take(10)
            .all(|t| loaded(self.track_cover(t), THUMB))
            && results
                .albums
                .iter()
                .take(12)
                .all(|a| loaded(self.url_cover(a.thumbnail_url.as_deref()), CARD))
            && results
                .artists
                .iter()
                .take(8)
                .all(|a| loaded(self.url_cover(a.thumbnail_url.as_deref()), CARD));
        if !(ready || force) {
            return;
        }
        if let Some((_, query, results)) = self.search_pending.take() {
            self.search_shown = query;
            self.search = Some(Load::Ready(results));
            self.search_inflight = false;
        }
    }

    /// Bouton lecture d'une carte : la liste est chargée (cache s'il y en a un)
    /// puis lue depuis le début.
    pub(super) fn play_item(
        &mut self,
        kind: ytmr_catalog::home::HomeKind,
        id: String,
    ) -> Task<Message> {
        use ytmr_catalog::home::HomeKind;
        let done = |r: Result<Vec<Track>, ytmr_catalog::CatalogError>| {
            Message::PlayFetched(r.map_err(|e| e.to_string()))
        };
        match kind {
            HomeKind::Album => {
                let Some(catalog) = self.catalog() else {
                    return Task::none();
                };
                Task::perform(
                    async move { catalog.album(&id).await.map(|a| a.tracks) },
                    done,
                )
            }
            HomeKind::Artist => {
                let Some(catalog) = self.catalog() else {
                    return Task::none();
                };
                Task::perform(
                    async move { catalog.artist(&id).await.map(|a| a.songs) },
                    done,
                )
            }
            HomeKind::Playlist => {
                // Déjà ouverte ou en cache : tout de suite.
                if let Some((open, Load::Ready(p))) = &self.lib.playlist
                    && *open == id
                {
                    let tracks = p.tracks.clone();
                    return self.update(Message::PlayFetched(Ok(tracks)));
                }
                if let Some(p) = self
                    .lib
                    .cache_dir
                    .as_deref()
                    .and_then(|dir| crate::library::read_playlist(dir, &id))
                {
                    return self.update(Message::PlayFetched(Ok(p.tracks)));
                }
                let Some(account) = self.lib.ready() else {
                    return Task::none();
                };
                Task::perform(
                    async move { account.playlist(&id).await.map(|p| p.tracks) },
                    done,
                )
            }
            HomeKind::Song => Task::none(),
        }
    }
}
