//! Pochettes : demandes, cache décodé, mémoire, teintes.
//!
//! En clair : chaque pochette affichée est demandée au cache de jaquettes
//! (décodage en tâche de fond), gardée en mémoire tant qu'elle sert, et
//! libérée la première quand le budget est dépassé si elle n'est plus à l'écran.

use super::*;

impl App {
    /// Identifiant de jaquette d'un morceau (enregistre son URL au passage).
    pub fn track_cover(&self, track: &Track) -> Option<String> {
        self.backend()
            .map(|b| b.covers.register(&track.cover_url()))
    }

    pub fn url_cover(&self, url: Option<&str>) -> Option<String> {
        let url = url?;
        self.backend().map(|b| b.covers.register(url))
    }

    /// Demande la teinte d'une pochette si elle n'est pas encore connue.
    pub(crate) fn want_accent(&mut self, id: Option<String>) -> Task<Message> {
        let Some(id) = id else {
            return Task::none();
        };
        if self.accents.contains_key(&id) {
            return Task::none();
        }
        let Some(covers) = self.backend().map(|b| Arc::clone(&b.covers)) else {
            return Task::none();
        };
        Task::perform(
            async move {
                let result = covers.accent(&id).await.map_err(|e| e.to_string());
                (id, result)
            },
            |(id, r)| Message::AccentLoaded(id, r),
        )
    }

    pub fn cover(&self, id: Option<&str>, size: u32) -> Option<image::Handle> {
        self.covers
            .get(&(id?.to_owned(), size))
            .and_then(Clone::clone)
    }

    /// Lance le chargement d'une jaquette si elle n'est ni chargée ni en cours.
    pub(crate) fn want_cover(&mut self, id: Option<String>, size: u32) -> Task<Message> {
        let Some(id) = id else {
            return Task::none();
        };
        let Some(covers) = self.backend().map(|b| Arc::clone(&b.covers)) else {
            return Task::none();
        };
        let key = (id.clone(), size);
        self.touch_cover(&key);
        // Grandes pochettes : libérables (celles du morceau en cours et de la
        // page affichée sont protégées, et redemandées à chaque visite). Avant,
        // chaque page visitée en gardait une de plus en mémoire.
        if size >= HERO {
            self.cover_evictable.insert(key.clone());
        } else {
            self.cover_evictable.remove(&key);
        }
        if self.covers.contains_key(&key) {
            return Task::none();
        }
        self.covers.insert(key, None);
        Task::perform(
            async move {
                let result = covers
                    .get_rgba(&id, size)
                    .await
                    .map(|rgba| image::Handle::from_rgba(size, size, rgba))
                    .map_err(|e| e.to_string());
                (id, result)
            },
            move |(id, result)| Message::CoverLoaded(id, size, result),
        )
    }

    pub(crate) fn want_track_covers<'a>(
        &mut self,
        tracks: impl IntoIterator<Item = &'a Track>,
        size: u32,
    ) -> Task<Message> {
        let ids: Vec<_> = tracks.into_iter().map(|t| self.track_cover(t)).collect();
        Task::batch(ids.into_iter().map(|id| self.want_cover(id, size)))
    }

    /// Pochettes des lignes visibles (playlist ouverte, file d'attente).
    pub(crate) fn request_visible_covers(&mut self) -> Task<Message> {
        let mut tracks: Vec<Track> = Vec::new();
        if let Route::Playlist(_) = self.route
            && let Some((_, Load::Ready(playlist))) = &self.lib.playlist
        {
            // Lignes visibles dans l'ordre affiché (tri, recherche).
            let order = crate::library::view_order(self, playlist);
            let (first, last) = crate::views::visible_range(
                self.page_view,
                crate::views::PLAYLIST_LIST_TOP,
                crate::views::ROW_H,
                order.len(),
            );
            tracks.extend(
                order[first.min(order.len())..last.min(order.len())]
                    .iter()
                    .map(|&i| playlist.tracks[i].clone()),
            );
        }
        if self.show_queue {
            let (first, last) = crate::views::visible_range(
                self.queue_view,
                0.0,
                crate::views::QROW_H,
                self.queue.entries.len(),
            );
            tracks.extend(
                self.queue.entries[first..last]
                    .iter()
                    .map(|e| e.track.clone()),
            );
        }
        // Lignes de listes virtualisées : libérables, elles seront redemandées.
        let ids: Vec<Option<String>> = tracks.iter().map(|t| self.track_cover(t)).collect();
        let pinned: Vec<bool> = ids
            .iter()
            .map(|id| {
                id.as_ref().is_some_and(|id| {
                    let key = (id.clone(), THUMB);
                    self.covers.contains_key(&key) && !self.cover_evictable.contains(&key)
                })
            })
            .collect();
        self.visible_covers = ids.iter().flatten().map(|id| (id.clone(), THUMB)).collect();
        let task = Task::batch(ids.iter().cloned().map(|id| self.want_cover(id, THUMB)));
        for (id, pinned) in ids.into_iter().zip(pinned) {
            if let Some(id) = id
                && !pinned
            {
                self.cover_evictable.insert((id, THUMB));
            }
        }
        task
    }

    pub(super) fn touch_cover(&mut self, key: &(String, u32)) {
        self.cover_tick += 1;
        self.cover_used.insert(key.clone(), self.cover_tick);
    }

    /// Libère les pochettes décodées les moins récemment demandées au-delà
    /// de `COVER_BUDGET` (elles restent sur le disque : on les relit si on
    /// revient dessus). Une liste de 1 000 vignettes tient dans le budget.
    pub(super) fn trim_covers(&mut self) {
        let bytes = |size: u32| u64::from(size) * u64::from(size) * 4;
        let used: u64 = self
            .covers
            .iter()
            .filter(|(_, h)| h.is_some())
            .map(|((_, size), _)| bytes(*size))
            .sum();
        if used <= COVER_BUDGET {
            return;
        }
        let mut keep: Vec<Option<String>> = vec![
            self.current_cover(),
            self.page_cover(),
            // Pochettes du mode focus encore à l'écran (pendant l'attente, le fondu).
            self.np_art.clone(),
        ];
        if let Some((_, previous)) = &self.np_fade {
            keep.push(previous.clone());
        }
        // Jamais une pochette à l'écran : libérée, elle restait un carré vide
        // (seule celle du morceau en cours revenait).
        let mut loaded: Vec<((String, u32), u64)> = self
            .cover_evictable
            .iter()
            .filter(|key| matches!(self.covers.get(*key), Some(Some(_))))
            .filter(|key| !self.visible_covers.contains(*key))
            .filter(|key| !keep.contains(&Some(key.0.clone())))
            .map(|key| (key.clone(), self.cover_used.get(key).copied().unwrap_or(0)))
            .collect();
        loaded.sort_by_key(|(_, tick)| *tick);
        let mut used = used;
        for (key, _) in loaded {
            if used <= COVER_BUDGET * 3 / 4 {
                break;
            }
            used -= bytes(key.1);
            self.covers.remove(&key);
            self.cover_used.remove(&key);
            self.cover_evictable.remove(&key);
        }
    }

    /// Pochettes à précharger en tâche de fond (après celles qui sont visibles).
    pub(crate) fn backlog_covers(
        &mut self,
        ids: impl IntoIterator<Item = String>,
        size: u32,
    ) -> Task<Message> {
        for id in ids {
            if !self.covers.contains_key(&(id.clone(), size)) {
                self.cover_backlog.push_back((id, size));
            }
        }
        self.pump_backlog()
    }

    pub(super) fn pump_backlog(&mut self) -> Task<Message> {
        const MAX_BACKLOG_INFLIGHT: usize = 3;
        let Some(covers) = self.backend().map(|b| Arc::clone(&b.covers)) else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        while self.backlog_inflight < MAX_BACKLOG_INFLIGHT {
            let Some((id, size)) = self.cover_backlog.pop_front() else {
                break;
            };
            if self.covers.contains_key(&(id.clone(), size)) {
                continue;
            }
            self.covers.insert((id.clone(), size), None);
            self.backlog_inflight += 1;
            let covers = Arc::clone(&covers);
            tasks.push(Task::perform(
                async move {
                    let result = covers
                        .get_rgba(&id, size)
                        .await
                        .map(|rgba| image::Handle::from_rgba(size, size, rgba))
                        .map_err(|e| e.to_string());
                    (id, result)
                },
                move |(id, result)| Message::BacklogCover(id, size, result),
            ));
        }
        Task::batch(tasks)
    }

    /// Pochettes des cartes de la bibliothèque.
    pub(crate) fn want_list_covers(&mut self) -> Task<Message> {
        let ids: Vec<Option<String>> = match &self.lib.playlists {
            Some(Load::Ready(list)) => list
                .iter()
                .map(|p| self.url_cover(p.thumbnail_url.as_deref()))
                .collect(),
            _ => return Task::none(),
        };
        Task::batch(ids.into_iter().map(|id| self.want_cover(id, CARD)))
    }

    /// Pochette qui donne sa teinte à la page affichée.
    pub(super) fn page_cover(&self) -> Option<String> {
        match &self.route {
            Route::Album(id) => match &self.album {
                Some((cur, Load::Ready(a))) if cur == id => {
                    self.url_cover(a.thumbnail_url.as_deref())
                }
                _ => None,
            },
            Route::Artist(id) => match &self.artist {
                Some((cur, Load::Ready(a))) if cur == id => {
                    self.url_cover(a.thumbnail_url.as_deref())
                }
                _ => None,
            },
            Route::Playlist(id) => match &self.lib.playlist {
                Some((cur, Load::Ready(p))) if cur == id => {
                    if id == ytmr_catalog::account::LIKED_ID {
                        p.tracks.first().and_then(|t| self.track_cover(t))
                    } else {
                        self.url_cover(p.thumbnail_url.as_deref())
                    }
                }
                _ => None,
            },
            _ => self.current_cover(),
        }
    }

    /// Fait glisser la teinte de la page vers celle de sa pochette (connue).
    pub(crate) fn update_page_accent(&mut self) {
        let target = match &self.route {
            Route::Album(_) | Route::Artist(_) | Route::Playlist(_) => self
                .page_cover()
                .and_then(|id| self.accents.get(&id).copied())
                .unwrap_or(style::NEUTRAL_ACCENT),
            _ => self.accent_fade.to,
        };
        self.page_fade.set(self.page_accent, target);
    }

    pub fn current_cover(&self) -> Option<String> {
        let track = &self.state.current.as_ref()?.track;
        self.track_cover(track)
    }
}
