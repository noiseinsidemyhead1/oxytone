//! Bibliothèque du compte YouTube : playlists, titres likés, modifications.
//!
//! En clair : tout ce qui touche au compte de l'utilisateur. Les changements
//! (like, ajout, création, renommage, suppression) s'affichent tout de suite,
//! puis sont envoyés à YouTube ; en cas de refus, l'affichage est corrigé et un
//! message explique pourquoi.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use iced::futures::SinkExt;

use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, text, text_input,
};
use iced::{Alignment, Color, Length, Padding, Task};
use ytmr_catalog::CatalogError;
use ytmr_catalog::account::{
    Account, LIKED_ID, Playlist, PlaylistSummary, PlaylistUpdate, Privacy, Profile,
};
use ytmr_core::{Command, Track};

use crate::app::{App, CARD, HERO, Load, Message, Route, THUMB};
use crate::backend::AccountState;
use crate::icons::{Icon, icon};
use crate::style;
use crate::views::{
    BOLD, El, SEMIBOLD, cards, cover, error_box, icon_tip, loading, muted, page_header,
    page_scroll, section_title, track_row,
};

pub const NEW_PLAYLIST_INPUT: &str = "nouvelle-playlist";
pub const RENAME_INPUT: &str = "renommer-playlist";

/// Durées pendant lesquelles une donnée en cache est jugée à jour : au-delà
/// seulement, on la redemande à YouTube (ou sur demande, bouton ↻).
const PLAYLIST_TTL: Duration = Duration::from_secs(30 * 60);
const LIST_TTL: Duration = Duration::from_secs(30 * 60);
const LIKED_TTL: Duration = Duration::from_secs(60 * 60);
const HISTORY_TTL: Duration = Duration::from_secs(10 * 60);
/// Après une écoute signalée, l'historique peut être redemandé plus tôt.
const HISTORY_STALE_TTL: Duration = Duration::from_secs(3 * 60);

/// Poignée clonable vers le compte (le jeton n'apparaît jamais dans les logs).
#[derive(Debug, Clone)]
pub struct AccountHandle(pub Arc<Account>);

#[derive(Default)]
pub enum AccountView {
    #[default]
    Connecting,
    Ready(Arc<Account>),
    Unavailable(String),
    /// Déconnecté à la demande de l'utilisateur (mémorisé entre deux lancements).
    Disconnected,
}

/// Fenêtres par-dessus la page.
#[derive(Debug, Clone)]
pub enum Overlay {
    AddTo(Track),
    NewPlaylist,
    ConfirmDelete { id: String, title: String },
}

#[derive(Default)]
pub struct LibraryState {
    pub account: AccountView,
    pub playlists: Option<Load<Vec<PlaylistSummary>>>,
    pub liked: HashSet<String>,
    pub liked_count: Option<usize>,
    pub playlist: Option<(String, Load<Playlist>)>,
    pub overlay: Option<Overlay>,
    pub renaming: Option<String>,
    pub new_name: String,
    /// Dossier du cache disque (listes et playlists déjà vues).
    pub cache_dir: Option<PathBuf>,
    /// Génération du chargement en cours (les réponses périmées sont ignorées).
    load_generation: u64,
    /// Chargement en cours : version fraîche construite page à page, affichée
    /// à la fin seulement si une version en cache est déjà à l'écran.
    pub streaming: Option<Streaming>,
    pub profile: Option<Profile>,
    /// Écoutés récemment (historique du compte).
    pub history: Option<Load<Vec<Track>>>,
    history_stale: bool,
    history_at: Option<Instant>,
    /// Dernier morceau signalé à l'historique YouTube.
    reported: Option<ytmr_core::QueueId>,
}

pub struct Streaming {
    generation: u64,
    /// `Some` : une version en cache est affichée, celle-ci la remplacera.
    pending: Option<Playlist>,
    pub received: usize,
}

impl LibraryState {
    pub fn ready(&self) -> Option<Arc<Account>> {
        match &self.account {
            AccountView::Ready(a) => Some(Arc::clone(a)),
            _ => None,
        }
    }

    pub fn is_liked(&self, video_id: &str) -> bool {
        self.liked.contains(video_id)
    }

    fn playlist_title(&self, id: &str) -> String {
        if id == LIKED_ID {
            return "Titres likés".into();
        }
        match &self.playlists {
            Some(Load::Ready(list)) => list
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.title.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Lib {
    AccountReady(Result<AccountHandle, String>),
    PlaylistsLoaded(Result<Vec<PlaylistSummary>, String>),
    LikedLoaded(Result<Playlist, String>),
    PlaylistUpdate(u64, PlaylistUpdate),
    PlaylistStreamEnded(u64, Result<(), String>),
    ToggleLike(Track),
    LikeDone(Track, bool, Result<(), String>),
    OpenAddTo(Track),
    OpenNew,
    CloseOverlay,
    AddTo(String, Track),
    Added(String, Result<(), String>),
    NewNameInput(String),
    Create,
    Created(Result<String, String>),
    RenameStart,
    RenameInput(String),
    RenameCommit,
    Renamed(String, Result<(), String>),
    AskDelete,
    Delete,
    Deleted(Result<(), String>),
    RemoveTrack(String, String),
    Removed(String, Result<(), String>),
    /// Lecture de la playlist ouverte à partir de la ligne donnée.
    PlayAt(usize),
    /// Mise à jour forcée de la playlist ouverte.
    Refresh,
    ProfileLoaded(Result<Profile, String>),
    HistoryLoaded(Result<Vec<Track>, String>),
    Disconnect,
    Reconnect,
    Reported(Result<(), String>),
}

/// Attend la fin de la connexion au compte (lancée par le backend).
pub fn wait_for_account(mut rx: tokio::sync::watch::Receiver<AccountState>) -> Task<Message> {
    Task::perform(
        async move {
            let state = rx
                .wait_for(|s| !matches!(s, AccountState::Connecting))
                .await
                .map(|s| s.clone());
            match state {
                Ok(AccountState::Ready(a)) => Ok(AccountHandle(a)),
                Ok(AccountState::Unavailable(e)) => Err(e),
                Ok(AccountState::Connecting) | Err(_) => Err("backend arrêté".to_owned()),
            }
        },
        |r| Message::Lib(Lib::AccountReady(r)),
    )
}

/// Lance une opération sur le compte (`None` = pas connecté).
fn call<T: Send + 'static>(
    op: Option<impl Future<Output = Result<T, CatalogError>> + Send + 'static>,
    done: impl Fn(Result<T, String>) -> Lib + Send + 'static,
) -> Task<Message> {
    match op {
        Some(op) => Task::perform(
            async move { op.await.map_err(|e| e.to_string()) },
            move |r| Message::Lib(done(r)),
        ),
        None => Task::done(Message::Lib(done(
            Err("compte YouTube non connecté".into()),
        ))),
    }
}

impl App {
    /// Écoutés récemment : rechargés à l'accueil quand une écoute a été signalée.
    pub fn load_history(&mut self) -> Task<Message> {
        let op = self.lib.ready().map(|a| async move { a.history().await });
        if op.is_none() {
            return Task::none();
        }
        if self.lib.history.is_none() {
            self.lib.history = Some(Load::Loading);
        }
        call(op, Lib::HistoryLoaded)
    }

    pub fn refresh_home(&mut self) -> Task<Message> {
        let age = self.lib.history_at.map(|t| t.elapsed());
        let due = match age {
            None => true,
            Some(age) if self.lib.history_stale => age > HISTORY_STALE_TTL,
            Some(age) => age > HISTORY_TTL,
        };
        if due {
            self.load_history()
        } else {
            Task::none()
        }
    }

    /// Signale à YouTube le morceau qui commence (une fois par entrée de file).
    pub fn report_play(&mut self, id: ytmr_core::QueueId, video_id: String) -> Task<Message> {
        if self.lib.reported == Some(id) {
            return Task::none();
        }
        self.lib.reported = Some(id);
        let op = self
            .lib
            .ready()
            .map(|a| async move { a.report_play(&video_id).await });
        if op.is_none() {
            return Task::none();
        }
        self.lib.history_stale = true;
        call(op, Lib::Reported)
    }

    fn disconnect_marker(&self) -> Option<PathBuf> {
        self.lib.cache_dir.as_ref().map(|d| d.join("disconnected"))
    }

    /// Liste des playlists : en cache si elle a moins de 30 minutes.
    pub fn load_playlists(&mut self) -> Task<Message> {
        if !matches!(self.lib.playlists, Some(Load::Ready(_))) {
            self.lib.playlists = match self.lib.cache_dir.as_deref().and_then(read_list) {
                Some(cached) => Some(Load::Ready(cached)),
                None => Some(Load::Loading),
            };
        }
        let fresh = self
            .lib
            .cache_dir
            .as_deref()
            .is_some_and(|dir| is_fresh(&dir.join("playlists.json"), LIST_TTL));
        if fresh && matches!(self.lib.playlists, Some(Load::Ready(_))) {
            return Task::none();
        }
        self.reload_playlists()
    }

    /// Liste des playlists redemandée sans condition (après une création…).
    pub fn reload_playlists(&mut self) -> Task<Message> {
        let op = self.lib.ready().map(|a| async move { a.playlists().await });
        call(op, Lib::PlaylistsLoaded)
    }

    /// Ouvre une playlist : la version en mémoire ou en cache s'affiche tout
    /// de suite ; YouTube n'est interrogé que si elle a plus de 30 minutes.
    pub fn open_playlist(&mut self, id: String) -> Task<Message> {
        self.lib.renaming = None;
        let shown = matches!(&self.lib.playlist, Some((cur, Load::Ready(_))) if *cur == id);
        let mut tasks = Vec::new();
        if !shown {
            let cached = self
                .lib
                .cache_dir
                .as_deref()
                .and_then(|dir| read_playlist(dir, &id));
            match cached {
                Some(cached) => {
                    tasks.push(self.show_playlist_extras(&cached));
                    self.lib.playlist = Some((id.clone(), Load::Ready(cached)));
                }
                None => self.lib.playlist = Some((id.clone(), Load::Loading)),
            }
        }
        let on_screen = matches!(&self.lib.playlist, Some((_, Load::Ready(_))));
        let fresh = self
            .lib
            .cache_dir
            .as_deref()
            .is_some_and(|dir| is_fresh(&playlist_file(dir, &id), PLAYLIST_TTL));
        if !(on_screen && fresh) && self.lib.streaming.is_none() {
            tasks.push(self.fetch_playlist(id));
        }
        Task::batch(tasks)
    }

    /// Redemande la playlist à YouTube, page par page.
    pub fn fetch_playlist(&mut self, id: String) -> Task<Message> {
        self.lib.load_generation += 1;
        let generation = self.lib.load_generation;
        let on_screen = matches!(&self.lib.playlist, Some((cur, Load::Ready(_))) if *cur == id);
        self.lib.streaming = Some(Streaming {
            generation,
            pending: on_screen.then(empty_playlist),
            received: 0,
        });
        match self.lib.ready() {
            Some(account) => stream_playlist(account, id, generation),
            None => Task::done(Message::Lib(Lib::PlaylistStreamEnded(
                generation,
                Err("compte YouTube non connecté".into()),
            ))),
        }
    }

    /// La version affichée a été modifiée sur place : le cache suit.
    fn save_open_playlist(&self) {
        if let (Some(dir), Some((_, Load::Ready(p)))) = (&self.lib.cache_dir, &self.lib.playlist) {
            write_playlist(dir, p);
        }
    }

    /// Pochette, teinte, premières vignettes et préparation des premiers titres.
    fn show_playlist_extras(&mut self, playlist: &Playlist) -> Task<Message> {
        let cover_id = if playlist.id == LIKED_ID {
            playlist.tracks.first().and_then(|t| self.track_cover(t))
        } else {
            self.url_cover(playlist.thumbnail_url.as_deref())
        };
        let mut tasks = vec![
            self.want_cover(cover_id.clone(), HERO),
            self.want_accent(cover_id),
            self.request_visible_covers(),
        ];
        for track in playlist.tracks.iter().take(3).rev() {
            tasks.push(self.send(Command::Prefetch(track.video_id.clone())));
        }
        Task::batch(tasks)
    }

    fn on_playlist_update(&mut self, generation: u64, update: PlaylistUpdate) -> Task<Message> {
        let Some(streaming) = self
            .lib
            .streaming
            .as_mut()
            .filter(|s| s.generation == generation)
        else {
            return Task::none();
        };
        // Version en cache à l'écran : on construit la fraîche à côté.
        if let Some(pending) = &mut streaming.pending {
            match update {
                PlaylistUpdate::Header(header) => {
                    *pending = Playlist {
                        tracks: std::mem::take(&mut pending.tracks),
                        ..header
                    }
                }
                PlaylistUpdate::Tracks(tracks) => {
                    streaming.received += tracks.len();
                    pending.tracks.extend(tracks);
                }
                PlaylistUpdate::Done { complete } => {
                    let mut fresh = streaming.pending.take().unwrap_or_else(empty_playlist);
                    fresh.complete = complete;
                    self.lib.streaming = None;
                    return self.finish_playlist(fresh);
                }
            }
            return Task::none();
        }
        // Rien en cache : on affiche au fil de l'eau.
        match update {
            PlaylistUpdate::Header(header) => {
                let id = header.id.clone();
                let task = self.show_playlist_extras(&header);
                self.lib.playlist = Some((id, Load::Ready(header)));
                task
            }
            PlaylistUpdate::Tracks(tracks) => {
                streaming.received += tracks.len();
                let Some((_, Load::Ready(playlist))) = &mut self.lib.playlist else {
                    return Task::none();
                };
                let first_page = playlist.tracks.is_empty();
                playlist.tracks.extend(tracks);
                let mut tasks = vec![self.request_visible_covers()];
                if first_page && let Some((_, Load::Ready(p))) = &self.lib.playlist {
                    let p = p.clone();
                    tasks.push(self.show_playlist_extras(&p));
                }
                Task::batch(tasks)
            }
            PlaylistUpdate::Done { complete } => {
                self.lib.streaming = None;
                let Some((_, Load::Ready(playlist))) = &mut self.lib.playlist else {
                    return Task::none();
                };
                playlist.complete = complete;
                let fresh = playlist.clone();
                self.finish_playlist(fresh)
            }
        }
    }

    /// Version complète reçue : affichage, cache disque, likes.
    fn finish_playlist(&mut self, fresh: Playlist) -> Task<Message> {
        if let Some(dir) = &self.lib.cache_dir {
            write_playlist(dir, &fresh);
        }
        if fresh.id == LIKED_ID {
            self.lib.liked = fresh.tracks.iter().map(|t| t.video_id.clone()).collect();
            self.lib.liked_count = Some(fresh.tracks.len());
        }
        let toast = if fresh.complete {
            Task::none()
        } else {
            self.show_toast("YouTube n'a pas rendu toute la playlist (affichage partiel)".into())
        };
        let covers = Task::none();
        if matches!(&self.lib.playlist, Some((cur, _)) if *cur == fresh.id) {
            self.lib.playlist = Some((fresh.id.clone(), Load::Ready(fresh)));
        }
        Task::batch([toast, covers, self.request_visible_covers()])
    }

    pub fn update_library(&mut self, message: Lib) -> Task<Message> {
        match message {
            Lib::AccountReady(Ok(_)) if self.disconnect_marker().is_some_and(|m| m.exists()) => {
                self.lib.account = AccountView::Disconnected;
                Task::none()
            }
            Lib::AccountReady(Ok(AccountHandle(account))) => {
                self.lib.account = AccountView::Ready(Arc::clone(&account));
                let profile = {
                    let account = Arc::clone(&account);
                    call(
                        Some(async move { account.profile().await }),
                        Lib::ProfileLoaded,
                    )
                };
                let liked_fresh = self
                    .lib
                    .cache_dir
                    .as_deref()
                    .is_some_and(|dir| is_fresh(&playlist_file(dir, LIKED_ID), LIKED_TTL));
                let liked = if liked_fresh {
                    Task::none()
                } else {
                    call(
                        Some(async move { account.playlist(LIKED_ID).await }),
                        Lib::LikedLoaded,
                    )
                };
                let mut tasks = vec![self.load_playlists(), liked, profile, self.load_history()];
                if let Route::Playlist(id) = self.route.clone() {
                    tasks.push(self.open_playlist(id));
                }
                Task::batch(tasks)
            }
            Lib::PlayAt(index) => match &self.lib.playlist {
                Some((_, Load::Ready(playlist))) if index < playlist.tracks.len() => {
                    let tracks = playlist.tracks.clone();
                    self.send(Command::PlayTracks {
                        tracks,
                        start: index,
                    })
                }
                _ => Task::none(),
            },
            Lib::Refresh => match &self.lib.playlist {
                Some((id, _)) => {
                    let id = id.clone();
                    self.fetch_playlist(id)
                }
                None => Task::none(),
            },
            Lib::ProfileLoaded(result) => match result {
                Ok(profile) => {
                    let photo = self.url_cover(profile.photo_url.as_deref());
                    self.lib.profile = Some(profile);
                    self.want_cover(photo, THUMB)
                }
                Err(e) => {
                    tracing::warn!("profil : {e}");
                    Task::none()
                }
            },
            Lib::HistoryLoaded(result) => match result {
                Ok(tracks) => {
                    self.lib.history_stale = false;
                    self.lib.history_at = Some(Instant::now());
                    let task = self.want_track_covers(tracks.iter().take(16), CARD);
                    self.lib.history = Some(Load::Ready(tracks));
                    task
                }
                Err(e) => {
                    if !matches!(self.lib.history, Some(Load::Ready(_))) {
                        self.lib.history = Some(Load::Failed(e));
                    }
                    Task::none()
                }
            },
            Lib::Reported(result) => {
                if let Err(e) = result {
                    tracing::debug!("écoute non signalée à YouTube : {e}");
                }
                Task::none()
            }
            Lib::Disconnect => {
                self.lib.account = AccountView::Disconnected;
                self.lib.profile = None;
                self.lib.playlists = None;
                self.lib.playlist = None;
                self.lib.history = None;
                self.lib.liked.clear();
                self.lib.liked_count = None;
                self.lib.streaming = None;
                if let Some(dir) = self.lib.cache_dir.clone() {
                    clear_cache(&dir);
                    if let Some(marker) = self.disconnect_marker() {
                        let _ = std::fs::write(marker, b"");
                    }
                }
                let nav = if matches!(self.route, Route::Library | Route::Playlist(_)) {
                    self.go_to(Route::Home)
                } else {
                    Task::none()
                };
                Task::batch([
                    nav,
                    self.show_toast("Compte YouTube déconnecté de ytmr".into()),
                ])
            }
            Lib::Reconnect => {
                if let Some(marker) = self.disconnect_marker() {
                    let _ = std::fs::remove_file(marker);
                }
                self.lib.account = AccountView::Connecting;
                match self.reconnect_task() {
                    Some(task) => task,
                    None => Task::none(),
                }
            }
            Lib::AccountReady(Err(e)) => {
                self.lib.account = AccountView::Unavailable(e);
                self.lib.playlists = None;
                Task::none()
            }
            Lib::PlaylistsLoaded(result) => {
                match result {
                    Ok(list) => {
                        if let Some(dir) = &self.lib.cache_dir {
                            write_list(dir, &list);
                        }
                        let ids: Vec<_> = list
                            .iter()
                            .map(|p| self.url_cover(p.thumbnail_url.as_deref()))
                            .collect();
                        self.lib.playlists = Some(Load::Ready(list));
                        return Task::batch(ids.into_iter().map(|id| self.want_cover(id, CARD)));
                    }
                    // Une liste en cache reste affichée si YouTube ne répond pas.
                    Err(e) if !matches!(self.lib.playlists, Some(Load::Ready(_))) => {
                        self.lib.playlists = Some(Load::Failed(e));
                    }
                    Err(e) => tracing::warn!("liste des playlists : {e}"),
                }
                Task::none()
            }
            Lib::LikedLoaded(result) => {
                match result {
                    Ok(playlist) => {
                        self.lib.liked =
                            playlist.tracks.iter().map(|t| t.video_id.clone()).collect();
                        self.lib.liked_count = Some(playlist.tracks.len());
                        if let Some(dir) = &self.lib.cache_dir {
                            write_playlist(dir, &playlist);
                        }
                    }
                    Err(e) => tracing::warn!("titres likés : {e}"),
                }
                Task::none()
            }
            Lib::PlaylistUpdate(generation, update) => self.on_playlist_update(generation, update),
            Lib::PlaylistStreamEnded(generation, result) => {
                let Err(e) = result else {
                    return Task::none();
                };
                if !self
                    .lib
                    .streaming
                    .as_ref()
                    .is_some_and(|s| s.generation == generation)
                {
                    return Task::none();
                }
                self.lib.streaming = None;
                match &mut self.lib.playlist {
                    // Rien à l'écran : l'erreur prend la page.
                    Some((_, load @ Load::Loading)) => {
                        *load = Load::Failed(e);
                        Task::none()
                    }
                    _ => self.show_toast(format!("Mise à jour de la playlist impossible : {e}")),
                }
            }
            Lib::ToggleLike(track) => {
                let like = !self.lib.is_liked(&track.video_id);
                self.apply_like(&track, like);
                let account = self.lib.ready();
                let video_id = track.video_id.clone();
                let op = account.map(|a| async move { a.set_liked(&video_id, like).await });
                call(op, move |r| Lib::LikeDone(track.clone(), like, r))
            }
            Lib::LikeDone(track, like, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => {
                    self.apply_like(&track, !like);
                    self.show_toast(format!("Like refusé : {e}"))
                }
            },
            Lib::OpenAddTo(track) => {
                self.lib.new_name.clear();
                self.lib.overlay = Some(Overlay::AddTo(track));
                let load = if self.lib.playlists.is_none() {
                    self.load_playlists()
                } else {
                    Task::none()
                };
                Task::batch([load, iced::widget::operation::focus(NEW_PLAYLIST_INPUT)])
            }
            Lib::OpenNew => {
                self.lib.new_name.clear();
                self.lib.overlay = Some(Overlay::NewPlaylist);
                iced::widget::operation::focus(NEW_PLAYLIST_INPUT)
            }
            Lib::CloseOverlay => {
                self.lib.overlay = None;
                Task::none()
            }
            Lib::AddTo(playlist_id, track) => {
                self.lib.overlay = None;
                let title = self.lib.playlist_title(&playlist_id);
                let toast = self.show_toast(format!("« {} » ajouté à « {title} »", track.title));
                let video_id = track.video_id.clone();
                let target = playlist_id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.add_tracks(&target, &[video_id]).await });
                Task::batch([toast, call(op, move |r| Lib::Added(playlist_id.clone(), r))])
            }
            Lib::Added(playlist_id, result) => match result {
                Ok(()) => self.refresh_if_open(&playlist_id),
                Err(e) => self.show_toast(format!("Ajout refusé : {e}")),
            },
            Lib::NewNameInput(name) => {
                self.lib.new_name = name;
                Task::none()
            }
            Lib::Create => {
                let name = self.lib.new_name.trim().to_owned();
                if name.is_empty() {
                    return Task::none();
                }
                let with: Vec<String> = match &self.lib.overlay {
                    Some(Overlay::AddTo(track)) => vec![track.video_id.clone()],
                    _ => Vec::new(),
                };
                self.lib.overlay = None;
                self.lib.new_name.clear();
                let toast = self.show_toast(format!("Playlist « {name} » créée"));
                let op = self.lib.ready().map(|a| async move {
                    a.create_playlist(&name, Privacy::Private, &with).await
                });
                Task::batch([toast, call(op, Lib::Created)])
            }
            Lib::Created(result) => match result {
                Ok(_) => self.reload_playlists(),
                Err(e) => self.show_toast(format!("Création refusée : {e}")),
            },
            Lib::RenameStart => {
                if let Some((_, Load::Ready(p))) = &self.lib.playlist {
                    self.lib.renaming = Some(p.title.clone());
                }
                iced::widget::operation::focus(RENAME_INPUT)
            }
            Lib::RenameInput(name) => {
                self.lib.renaming = Some(name);
                Task::none()
            }
            Lib::RenameCommit => {
                let Some(name) = self.lib.renaming.take().map(|n| n.trim().to_owned()) else {
                    return Task::none();
                };
                let Some((id, Load::Ready(playlist))) = &mut self.lib.playlist else {
                    return Task::none();
                };
                if name.is_empty() || name == playlist.title {
                    return Task::none();
                }
                playlist.title = name.clone();
                let id = id.clone();
                self.save_open_playlist();
                if let Some(Load::Ready(list)) = &mut self.lib.playlists
                    && let Some(p) = list.iter_mut().find(|p| p.id == id)
                {
                    p.title = name.clone();
                }
                let target = id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.rename_playlist(&target, &name).await });
                call(op, move |r| Lib::Renamed(id.clone(), r))
            }
            Lib::Renamed(id, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => Task::batch([
                    self.show_toast(format!("Renommage refusé : {e}")),
                    self.fetch_playlist(id),
                    self.reload_playlists(),
                ]),
            },
            Lib::AskDelete => {
                if let Some((id, Load::Ready(p))) = &self.lib.playlist {
                    self.lib.overlay = Some(Overlay::ConfirmDelete {
                        id: id.clone(),
                        title: p.title.clone(),
                    });
                }
                Task::none()
            }
            Lib::Delete => {
                let Some(Overlay::ConfirmDelete { id, title }) = self.lib.overlay.take() else {
                    return Task::none();
                };
                if let Some(Load::Ready(list)) = &mut self.lib.playlists {
                    list.retain(|p| p.id != id);
                }
                let toast = self.show_toast(format!("Playlist « {title} » supprimée"));
                let nav = self.go_to(Route::Library);
                let target = id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.delete_playlist(&target).await });
                Task::batch([toast, nav, call(op, Lib::Deleted)])
            }
            Lib::Deleted(result) => match result {
                Ok(()) => Task::none(),
                Err(e) => Task::batch([
                    self.show_toast(format!("Suppression refusée : {e}")),
                    self.reload_playlists(),
                ]),
            },
            Lib::RemoveTrack(playlist_id, video_id) => {
                if let Some((id, Load::Ready(p))) = &mut self.lib.playlist
                    && *id == playlist_id
                {
                    p.tracks.retain(|t| t.video_id != video_id);
                }
                self.save_open_playlist();
                let target = playlist_id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.remove_track(&target, &video_id).await });
                call(op, move |r| Lib::Removed(playlist_id.clone(), r))
            }
            Lib::Removed(playlist_id, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => Task::batch([
                    self.show_toast(format!("Retrait refusé : {e}")),
                    self.refresh_if_open(&playlist_id),
                ]),
            },
        }
    }

    /// Met à jour l'affichage d'un like sans attendre YouTube.
    fn apply_like(&mut self, track: &Track, like: bool) {
        if like {
            self.lib.liked.insert(track.video_id.clone());
        } else {
            self.lib.liked.remove(&track.video_id);
        }
        if let Some(count) = &mut self.lib.liked_count {
            *count = if like {
                *count + 1
            } else {
                count.saturating_sub(1)
            };
        }
        if let Some((id, Load::Ready(p))) = &mut self.lib.playlist
            && id == LIKED_ID
        {
            p.tracks.retain(|t| t.video_id != track.video_id);
            if like {
                p.tracks.insert(0, track.clone());
            }
            self.save_open_playlist();
        }
    }

    /// Après un ajout : la playlist ouverte est redemandée, une autre perd son
    /// cache (elle sera redemandée à sa prochaine ouverture).
    fn refresh_if_open(&mut self, playlist_id: &str) -> Task<Message> {
        match &self.lib.playlist {
            Some((id, _)) if id == playlist_id => self.fetch_playlist(playlist_id.to_owned()),
            _ => {
                if let Some(dir) = &self.lib.cache_dir {
                    let _ = std::fs::remove_file(playlist_file(dir, playlist_id));
                }
                Task::none()
            }
        }
    }
}

/// Chargement page par page, transformé en messages pour l'interface.
fn stream_playlist(account: Arc<Account>, id: String, generation: u64) -> Task<Message> {
    Task::stream(iced::stream::channel(64, async move |mut out| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let fetch = async {
            let result = account.playlist_progressive(&id, tx).await;
            result.map_err(|e| e.to_string())
        };
        let mut forward_out = out.clone();
        let forward = async move {
            while let Some(update) = rx.recv().await {
                let message = Message::Lib(Lib::PlaylistUpdate(generation, update));
                if forward_out.send(message).await.is_err() {
                    break;
                }
            }
        };
        let (result, ()) = tokio::join!(fetch, forward);
        let _ = out
            .send(Message::Lib(Lib::PlaylistStreamEnded(generation, result)))
            .await;
    }))
}

fn empty_playlist() -> Playlist {
    Playlist {
        id: String::new(),
        title: String::new(),
        author: String::new(),
        description: None,
        duration_text: String::new(),
        thumbnail_url: None,
        tracks: Vec::new(),
        complete: false,
    }
}

// ----- cache disque -----
// Petits fichiers JSON (une playlist de 1 000 titres ≈ 300 Ko), lus et écrits
// directement : quelques millisecondes.

fn playlist_file(dir: &Path, id: &str) -> PathBuf {
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    dir.join(format!("playlist-{safe}.json"))
}

/// Le fichier existe et a été écrit il y a moins de `ttl`.
fn is_fresh(path: &Path, ttl: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ttl)
}

fn read_playlist(dir: &Path, id: &str) -> Option<Playlist> {
    let text = std::fs::read_to_string(playlist_file(dir, id)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_playlist(dir: &Path, playlist: &Playlist) {
    write_json(&playlist_file(dir, &playlist.id), playlist);
}

fn read_list(dir: &Path) -> Option<Vec<PlaylistSummary>> {
    let text = std::fs::read_to_string(dir.join("playlists.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_list(dir: &Path, list: &[PlaylistSummary]) {
    write_json(&dir.join("playlists.json"), &list);
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    let result = serde_json::to_vec(value)
        .map_err(std::io::Error::other)
        .and_then(|json| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, path)
        });
    if let Err(e) = result {
        tracing::warn!(error = %e, path = %path.display(), "cache de la bibliothèque");
    }
}

/// Oublie la bibliothèque en cache (déconnexion, changement de compte).
fn clear_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("playlist") && name.ends_with(".json") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Titres likés en cache : les cœurs s'affichent avant même la connexion.
pub fn cached_liked(dir: &Path) -> Option<Playlist> {
    read_playlist(dir, LIKED_ID)
}

// ----- vues -----

fn liked_tile<'a>(app: &App) -> El<'a> {
    let count = app
        .lib
        .liked_count
        .map(|n| format!("{n} titres"))
        .unwrap_or_else(|| "…".into());
    let accent = app.accent;
    button(
        column![
            container(icon(Icon::HeartFilled, 54.0, Color::WHITE))
                .center(Length::Fixed(164.0))
                .style(move |_| iced::widget::container::Style {
                    background: Some(iced::Background::Gradient(iced::Gradient::Linear(
                        iced::gradient::Linear::new(std::f32::consts::FRAC_PI_4)
                            .add_stop(0.0, style::shade(accent, 0.45))
                            .add_stop(1.0, accent),
                    ))),
                    border: iced::Border {
                        radius: 8.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            text("Titres likés")
                .size(14)
                .font(SEMIBOLD)
                .width(Length::Fixed(164.0)),
            text(count)
                .size(12)
                .color(style::TEXT_DIM)
                .width(Length::Fixed(164.0)),
        ]
        .spacing(6),
    )
    .padding(10)
    .style(style::card)
    .on_press(Message::Navigate(Route::Playlist(LIKED_ID.to_owned())))
    .into()
}

fn playlist_card<'a>(app: &App, p: &'a PlaylistSummary) -> El<'a> {
    let id = app.url_cover(p.thumbnail_url.as_deref());
    let subtitle = [p.tracks_text.as_str(), p.author.as_str()]
        .into_iter()
        .filter(|s| !s.trim().is_empty() && s.trim() != "•")
        .collect::<Vec<_>>()
        .join(" · ");
    button(
        column![
            cover(app.cover(id.as_deref(), CARD), 164.0, 8.0),
            text(&p.title)
                .size(14)
                .font(SEMIBOLD)
                .width(Length::Fixed(164.0)),
            text(subtitle)
                .size(12)
                .color(style::TEXT_DIM)
                .width(Length::Fixed(164.0)),
        ]
        .spacing(6),
    )
    .padding(10)
    .style(style::card)
    .on_press(Message::Navigate(Route::Playlist(p.id.clone())))
    .into()
}

fn not_connected<'a>(app: &App) -> Option<El<'a>> {
    let message = match &app.lib.account {
        AccountView::Ready(_) => return None,
        AccountView::Connecting => "Connexion à ton compte YouTube…".to_owned(),
        AccountView::Unavailable(e) => format!(
            "Compte YouTube indisponible : {e}\n\nIl faut `cookies_from_browser = \"firefox\"` dans la section [player] de config.toml, et être connecté à YouTube dans Firefox."
        ),
        AccountView::Disconnected => {
            "Compte YouTube déconnecté. Reconnecte-le en bas à gauche.".to_owned()
        }
    };
    Some(
        container(muted(message))
            .padding(Padding::from([40, 0]))
            .into(),
    )
}

pub fn library_page(app: &App) -> El<'_> {
    let header = row![
        text("Bibliothèque").size(30).font(BOLD).width(Length::Fill),
        button(
            row![
                icon(Icon::Plus, 16.0, style::BG),
                text("Nouvelle playlist").size(14).font(SEMIBOLD)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([9, 18]))
        .style(style::pill(app.accent))
        .on_press_maybe(app.lib.ready().map(|_| Message::Lib(Lib::OpenNew))),
    ]
    .align_y(Alignment::Center);

    let body: El<'_> = if let Some(info) = not_connected(app) {
        info
    } else {
        match &app.lib.playlists {
            None | Some(Load::Loading) => loading("Chargement de tes playlists…"),
            Some(Load::Failed(e)) => error_box(e),
            Some(Load::Ready(list)) => {
                let mut tiles = vec![liked_tile(app)];
                tiles.extend(list.iter().map(|p| playlist_card(app, p)));
                cards(tiles)
            }
        }
    };
    page_scroll(column![header, body].spacing(20).padding(Padding {
        top: 28.0,
        right: 28.0,
        bottom: 32.0,
        left: 28.0,
    }))
}

pub fn playlist_page(app: &App) -> El<'_> {
    let Some((id, load)) = &app.lib.playlist else {
        return loading("Chargement…");
    };
    if let Some(info) = not_connected(app) {
        return page_scroll(info);
    }
    let body: El<'_> = match load {
        Load::Loading => crate::views::with_back(loading("Chargement de la playlist…")),
        Load::Failed(e) => crate::views::with_back(error_box(e)),
        Load::Ready(playlist) => playlist_body(app, id, playlist),
    };
    page_scroll(body)
}

fn playlist_body<'a>(app: &'a App, id: &'a str, playlist: &'a Playlist) -> El<'a> {
    let liked = id == LIKED_ID;
    let count = match playlist.tracks.len() {
        1 => "1 titre".to_owned(),
        n => format!("{n} titres"),
    };
    let details = [
        playlist.author.as_str(),
        count.as_str(),
        playlist.duration_text.as_str(),
    ]
    .into_iter()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" · ");

    let title_or_edit: El<'a> = match &app.lib.renaming {
        Some(name) if !liked => text_input("Nom de la playlist", name)
            .id(RENAME_INPUT)
            .on_input(|n| Message::Lib(Lib::RenameInput(n)))
            .on_submit(Message::Lib(Lib::RenameCommit))
            .size(24)
            .padding(Padding::from([6, 12]))
            .style(style::search_input)
            .into(),
        _ => Space::new().height(Length::Fixed(0.0)).into(),
    };
    let mut actions = row![icon_tip(
        Icon::Refresh,
        17.0,
        style::TEXT_DIM,
        Message::Lib(Lib::Refresh),
        "Mettre à jour depuis YouTube".into(),
    )]
    .spacing(4)
    .align_y(Alignment::Center);
    if !liked {
        actions = actions
            .push(icon_tip(
                Icon::Edit,
                17.0,
                style::TEXT_DIM,
                match app.lib.renaming {
                    Some(_) => Message::Lib(Lib::RenameCommit),
                    None => Message::Lib(Lib::RenameStart),
                },
                if app.lib.renaming.is_some() {
                    "Valider le nom"
                } else {
                    "Renommer"
                }
                .into(),
            ))
            .push(icon_tip(
                Icon::Trash,
                17.0,
                style::TEXT_DIM,
                Message::Lib(Lib::AskDelete),
                "Supprimer la playlist".into(),
            ));
    }
    let progress: El<'a> = match &app.lib.streaming {
        Some(s) if s.received > 0 || playlist.tracks.is_empty() => {
            muted(format!("Mise à jour… {} titres reçus", s.received))
        }
        Some(_) => muted("Mise à jour…"),
        None => Space::new().height(Length::Fixed(0.0)).into(),
    };
    let subtitle = column![muted(details), progress, title_or_edit, actions].spacing(6);

    let cover_id = if liked {
        playlist.tracks.first().and_then(|t| app.track_cover(t))
    } else {
        app.url_cover(playlist.thumbnail_url.as_deref())
    };
    let header = page_header(
        app,
        cover_id,
        10.0,
        if liked {
            "TITRES LIKÉS".into()
        } else {
            "PLAYLIST".into()
        },
        &playlist.title,
        subtitle.into(),
        &playlist.tracks,
    );

    // Seules les lignes visibles sont construites, et le clic ne transporte
    // qu'un numéro : copier la playlist dans chaque ligne coûtait ~1,4 million
    // de copies de morceaux par image pour 1 189 titres.
    let range = crate::views::visible_range(
        app.page_view,
        crate::views::PLAYLIST_LIST_TOP,
        crate::views::ROW_H,
        playlist.tracks.len(),
    );
    let list: El<'a> = if playlist.tracks.is_empty() {
        muted("Cette playlist est vide.")
    } else {
        crate::views::virtual_list(playlist.tracks.len(), crate::views::ROW_H, range, |i| {
            let track = &playlist.tracks[i];
            let remove = if liked {
                Message::Lib(Lib::ToggleLike(track.clone()))
            } else {
                Message::Lib(Lib::RemoveTrack(id.to_owned(), track.video_id.clone()))
            };
            track_row(app, track, None, Message::Lib(Lib::PlayAt(i)), Some(remove))
        })
    };
    column![
        // Hauteur fixe : c'est elle qui permet de savoir quelles lignes sont visibles.
        container(header)
            .height(Length::Fixed(crate::views::PLAYLIST_HEADER_H))
            .clip(true),
        container(list).padding(Padding {
            top: crate::views::PLAYLIST_LIST_TOP - crate::views::PLAYLIST_HEADER_H,
            right: 28.0,
            bottom: 32.0,
            left: 28.0,
        }),
    ]
    .into()
}

/// Fenêtre par-dessus la page (ajout à une playlist, confirmation).
pub fn overlay(app: &App) -> Option<El<'_>> {
    let panel: El<'_> = match app.lib.overlay.as_ref()? {
        Overlay::AddTo(track) => add_to_panel(app, Some(track)),
        Overlay::NewPlaylist => add_to_panel(app, None),
        Overlay::ConfirmDelete { title, .. } => confirm_panel(title),
    };
    let dim = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| iced::widget::container::Style {
                background: Some(style::alpha(Color::BLACK, 0.55).into()),
                ..Default::default()
            }),
    )
    .on_press(Message::Lib(Lib::CloseOverlay));
    let centered = container(
        // Le panneau capture les clics : ils ne ferment pas la fenêtre.
        mouse_area(
            container(panel)
                .padding(20)
                .width(Length::Fixed(400.0))
                .style(style::panel),
        )
        .on_press(Message::Noop),
    )
    .center(Length::Fill);
    Some(iced::widget::stack![dim, centered].into())
}

fn add_to_panel<'a>(app: &'a App, track: Option<&'a Track>) -> El<'a> {
    let title = match track {
        None => "Nouvelle playlist".to_owned(),
        Some(track) => format!("Ajouter « {} » à…", track.title),
    };
    let new_row = row![
        text_input("Nom de la nouvelle playlist", &app.lib.new_name)
            .id(NEW_PLAYLIST_INPUT)
            .on_input(|n| Message::Lib(Lib::NewNameInput(n)))
            .on_submit(Message::Lib(Lib::Create))
            .size(14)
            .padding(Padding::from([8, 12]))
            .style(style::search_input),
        button(text("Créer").size(14).font(SEMIBOLD))
            .padding(Padding::from([8, 16]))
            .style(style::pill(app.accent))
            .on_press_maybe(
                (!app.lib.new_name.trim().is_empty()).then_some(Message::Lib(Lib::Create)),
            ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut content = column![text(title).size(17).font(BOLD), new_row].spacing(14);
    if let Some(track) = track {
        let mut list = column![].spacing(2);
        match &app.lib.playlists {
            Some(Load::Ready(playlists)) => {
                for p in playlists {
                    let id = app.url_cover(p.thumbnail_url.as_deref());
                    list = list.push(
                        button(
                            row![
                                cover(app.cover(id.as_deref(), CARD), 36.0, 5.0),
                                column![
                                    text(&p.title).size(14).font(SEMIBOLD),
                                    text(&p.tracks_text).size(12).color(style::TEXT_DIM),
                                ]
                                .spacing(1),
                            ]
                            .spacing(10)
                            .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .padding(Padding::from([5, 8]))
                        .style(style::row(false, app.accent))
                        .on_press(Message::Lib(Lib::AddTo(p.id.clone(), track.clone()))),
                    );
                }
            }
            Some(Load::Failed(e)) => list = list.push(muted(e.clone())),
            _ => list = list.push(muted("Chargement…")),
        }
        content = content.push(section_title("Mes playlists")).push(
            scrollable(list)
                .height(Length::Fixed(300.0))
                .style(style::scroll),
        );
    }
    content.into()
}

fn confirm_panel(title: &str) -> El<'_> {
    column![
        text("Supprimer la playlist ?").size(17).font(BOLD),
        muted(format!(
            "« {title} » sera supprimée de ton compte YouTube. C'est définitif."
        )),
        row![
            Space::new().width(Length::Fill),
            button(text("Annuler").size(14))
                .padding(Padding::from([8, 16]))
                .style(style::pill_ghost)
                .on_press(Message::Lib(Lib::CloseOverlay)),
            button(text("Supprimer").size(14).font(SEMIBOLD))
                .padding(Padding::from([8, 16]))
                .style(style::pill(Color::from_rgb8(0xff, 0x6b, 0x6b)))
                .on_press(Message::Lib(Lib::Delete)),
        ]
        .spacing(10),
    ]
    .spacing(14)
    .into()
}

// ----- accueil -----

/// Carte d'un morceau (rangée « Écoutés récemment »).
fn track_card<'a>(app: &App, track: &'a Track) -> El<'a> {
    let id = app.track_cover(track);
    mouse_area(
        button(
            column![
                cover(app.cover(id.as_deref(), CARD), 150.0, 8.0),
                text(&track.title)
                    .size(14)
                    .font(SEMIBOLD)
                    .width(Length::Fixed(150.0)),
                text(&track.artist)
                    .size(12)
                    .color(style::TEXT_DIM)
                    .width(Length::Fixed(150.0)),
            ]
            .spacing(6),
        )
        .padding(8)
        .style(style::card)
        .on_press(Message::Play(vec![track.clone()], 0)),
    )
    .on_enter(Message::Hover(track.video_id.clone()))
    .on_exit(Message::Unhover)
    .into()
}

fn shelf<'a>(items: Vec<El<'a>>) -> El<'a> {
    scrollable(row(items).spacing(4).padding(Padding {
        bottom: 12.0,
        ..Padding::ZERO
    }))
    .direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new().width(4).scroller_width(4),
    ))
    .style(style::scroll)
    .into()
}

/// Sections de l'accueil liées au compte (écoutés récemment, playlists).
pub fn home_sections(app: &App) -> Option<El<'_>> {
    app.lib.ready()?;
    let mut page = column![].spacing(28);
    match &app.lib.history {
        Some(Load::Ready(tracks)) if !tracks.is_empty() => {
            page = page.push(
                column![
                    section_title("Écoutés récemment"),
                    shelf(tracks.iter().take(16).map(|t| track_card(app, t)).collect()),
                ]
                .spacing(10),
            );
        }
        Some(Load::Loading) | None => {
            page = page.push(column![
                section_title("Écoutés récemment"),
                loading("Chargement…")
            ]);
        }
        _ => {}
    }
    if let Some(Load::Ready(list)) = &app.lib.playlists {
        let mut tiles = vec![liked_tile(app)];
        tiles.extend(list.iter().take(9).map(|p| playlist_card(app, p)));
        page = page.push(
            column![
                row![
                    section_title("Tes playlists"),
                    Space::new().width(Length::Fill),
                    button(text("Tout voir").size(13))
                        .padding(Padding::from([4, 12]))
                        .style(style::pill_ghost)
                        .on_press(Message::Navigate(Route::Library)),
                ]
                .align_y(Alignment::Center),
                shelf(tiles),
            ]
            .spacing(10),
        );
    }
    Some(page.into())
}

/// Bas de la barre latérale : photo, nom, déconnexion / reconnexion.
pub fn account_widget(app: &App) -> El<'_> {
    match &app.lib.account {
        AccountView::Ready(_) => {
            let (name, handle, photo) = match &app.lib.profile {
                Some(p) => (
                    p.name.clone(),
                    p.handle.clone().unwrap_or_default(),
                    app.url_cover(p.photo_url.as_deref()),
                ),
                None => ("Compte YouTube".to_owned(), "connecté".to_owned(), None),
            };
            row![
                cover(app.cover(photo.as_deref(), THUMB), 34.0, 17.0),
                column![
                    text(name).size(13).font(SEMIBOLD),
                    text(handle).size(11).color(style::TEXT_DIM),
                ]
                .spacing(1)
                .width(Length::Fill),
                icon_tip(
                    Icon::Logout,
                    15.0,
                    style::TEXT_DIM,
                    Message::Lib(Lib::Disconnect),
                    "Se déconnecter de ytmr".into(),
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        }
        AccountView::Connecting => muted("Connexion au compte YouTube…"),
        AccountView::Disconnected => button(
            row![
                icon(Icon::Logout, 15.0, style::TEXT),
                text("Se connecter (session Firefox)")
                    .size(13)
                    .font(SEMIBOLD),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([8, 12]))
        .style(style::pill_ghost)
        .on_press(Message::Lib(Lib::Reconnect))
        .into(),
        AccountView::Unavailable(e) => row![
            column![
                text("Compte indisponible").size(13).font(SEMIBOLD),
                text(e.chars().take(80).collect::<String>())
                    .size(11)
                    .color(style::TEXT_FAINT),
            ]
            .spacing(1)
            .width(Length::Fill),
            icon_tip(
                Icon::Refresh,
                15.0,
                style::TEXT_DIM,
                Message::Lib(Lib::Reconnect),
                "Réessayer".into(),
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into(),
    }
}
