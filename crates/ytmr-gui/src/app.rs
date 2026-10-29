//! État de l'interface et traitement des messages (architecture Elm d'iced).
//!
//! En clair : l'interface garde une copie de ce qu'elle affiche (état du
//! lecteur, page ouverte, jaquettes chargées). Chaque clic devient un
//! [`Message`] ; `update` le traite, souvent en envoyant une commande au moteur.
//! Le moteur, lui, reste la seule source de vérité : l'interface ne fait que
//! refléter ce qu'il publie.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::futures::SinkExt;
use iced::keyboard::{self, Key, key};
use iced::widget::{image, operation};
use iced::{Color, Subscription, Task, Theme, window};
use tokio::sync::broadcast::error::RecvError;
use ytmr_catalog::{Album, Artist, SearchAll};
use ytmr_core::{
    AppPaths, Command, Config, CoreEvent, PlaybackState, QueueId, QueueSnapshot, Status, Track,
};

use crate::backend::Backend;
use crate::style;

/// Tailles demandées au cache de jaquettes (en pixels physiques, ~2× l'affichage).
pub const THUMB: u32 = 96;
pub const CARD: u32 = 320;
pub const HERO: u32 = 640;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(320);
/// Survol prolongé avant de préparer un morceau (évite de lancer yt-dlp en
/// passant simplement la souris sur une liste).
const HOVER_PREFETCH_DELAY: Duration = Duration::from_millis(450);
const TOAST_DURATION: Duration = Duration::from_secs(4);

pub const SEARCH_INPUT: &str = "recherche";
/// Défilement de la page centrale, remis en haut à chaque navigation.
pub const PAGE_SCROLL: &str = "page";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Home,
    Search,
    Album(String),
    Artist(String),
}

#[derive(Debug, Clone)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// Valeur non clonable transportée dans un message (le backend).
#[derive(Debug)]
pub struct Slot<T>(Arc<Mutex<Option<T>>>);

// À la main : `derive` exigerait `T: Clone`, alors qu'on ne clone que l'`Arc`.
impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Slot<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    fn take(&self) -> Option<T> {
        self.0.lock().ok().and_then(|mut v| v.take())
    }
}

#[derive(Debug, Clone)]
pub enum CoreUpdate {
    State(PlaybackState),
    Queue(Arc<QueueSnapshot>),
    Event(CoreEvent),
}

#[derive(Debug, Clone)]
pub enum Message {
    BackendReady(Result<Slot<Backend>, String>),
    Core(CoreUpdate),
    Navigate(Route),
    Back,
    SearchInput(String),
    SearchSubmit,
    SearchDebounced(u64),
    SearchDone(u64, Result<SearchAll, String>),
    AlbumLoaded(String, Result<Album, String>),
    ArtistLoaded(String, Result<Artist, String>),
    CoverLoaded(String, u32, Result<image::Handle, String>),
    AccentLoaded(String, Result<[u8; 3], String>),
    Play(Vec<Track>, usize),
    PlayShuffled(Vec<Track>),
    PlayNext(Track),
    Enqueue(Track),
    TogglePause,
    Next,
    Previous,
    SeekPreview(f32),
    SeekCommit,
    Volume(u8),
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    ToggleAutoplay,
    ToggleQueue,
    JumpTo(QueueId),
    Remove(QueueId),
    ClearQueue,
    Hover(String),
    HoverSettled(String),
    Unhover,
    CommandDone(Result<(), String>),
    Key(keyboard::Event),
    DismissToast(u64),
    CloseRequested,
    ShutdownDone,
}

pub enum Phase {
    Starting,
    Ready(Backend),
    Failed(String),
}

pub struct App {
    pub phase: Phase,
    pub state: PlaybackState,
    pub queue: Arc<QueueSnapshot>,
    pub route: Route,
    history: Vec<Route>,
    pub search_query: String,
    search_seq: u64,
    pub search: Option<Load<SearchAll>>,
    pub album: Option<(String, Load<Album>)>,
    pub artist: Option<(String, Load<Artist>)>,
    covers: HashMap<(String, u32), Option<image::Handle>>,
    accents: HashMap<String, Color>,
    pub accent: Color,
    pub show_queue: bool,
    pub seek_preview: Option<f32>,
    muted_volume: Option<u8>,
    hovered: Option<String>,
    pub toast: Option<(u64, String)>,
    toast_seq: u64,
    shutting_down: bool,
}

impl App {
    pub fn boot(config: Arc<(Config, AppPaths)>) -> (Self, Task<Message>) {
        let start = {
            let config = Arc::clone(&config);
            Task::perform(
                async move {
                    Backend::start(&config.0, &config.1)
                        .await
                        .map(Slot::new)
                        .map_err(|e| format!("{e:#}"))
                },
                Message::BackendReady,
            )
        };
        let app = Self {
            state: PlaybackState {
                volume: config.0.player.volume,
                ..PlaybackState::default()
            },
            phase: Phase::Starting,
            queue: Arc::default(),
            route: Route::Home,
            history: Vec::new(),
            search_query: String::new(),
            search_seq: 0,
            search: None,
            album: None,
            artist: None,
            covers: HashMap::new(),
            accents: HashMap::new(),
            accent: style::DEFAULT_ACCENT,
            show_queue: true,
            seek_preview: None,
            muted_volume: None,
            hovered: None,
            toast: None,
            toast_seq: 0,
            shutting_down: false,
        };
        (app, start)
    }

    pub fn title(&self) -> String {
        match self.state.current.as_ref() {
            Some(e) => format!("{} · {} — ytmr", e.track.title, e.track.artist),
            None => "ytmr".to_owned(),
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::custom(
            "ytmr".to_owned(),
            iced::theme::Palette {
                background: style::BG,
                text: style::TEXT,
                primary: self.accent,
                success: self.accent,
                warning: self.accent,
                danger: Color::from_rgb8(0xff, 0x6b, 0x6b),
            },
        )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            window::close_requests().map(|_| Message::CloseRequested),
            keyboard::listen().map(Message::Key),
        ])
    }

    fn backend(&self) -> Option<&Backend> {
        match &self.phase {
            Phase::Ready(b) => Some(b),
            _ => None,
        }
    }

    /// Identifiant de jaquette d'un morceau (enregistre son URL au passage).
    pub fn track_cover(&self, track: &Track) -> Option<String> {
        self.backend()
            .map(|b| b.covers.register(&track.cover_url()))
    }

    pub fn url_cover(&self, url: Option<&str>) -> Option<String> {
        let url = url?;
        self.backend().map(|b| b.covers.register(url))
    }

    /// Teinte tirée d'une pochette (celle du morceau en cours si inconnue).
    pub fn accent_for(&self, cover_id: Option<&str>) -> Color {
        cover_id
            .and_then(|id| self.accents.get(id))
            .copied()
            .unwrap_or(self.accent)
    }

    /// Demande la teinte d'une pochette si elle n'est pas encore connue.
    fn want_accent(&mut self, id: Option<String>) -> Task<Message> {
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
    fn want_cover(&mut self, id: Option<String>, size: u32) -> Task<Message> {
        let Some(id) = id else {
            return Task::none();
        };
        let Some(covers) = self.backend().map(|b| Arc::clone(&b.covers)) else {
            return Task::none();
        };
        let key = (id.clone(), size);
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

    fn want_track_covers<'a>(
        &mut self,
        tracks: impl IntoIterator<Item = &'a Track>,
        size: u32,
    ) -> Task<Message> {
        let ids: Vec<_> = tracks.into_iter().map(|t| self.track_cover(t)).collect();
        Task::batch(ids.into_iter().map(|id| self.want_cover(id, size)))
    }

    /// Envoie une commande au moteur ; une erreur s'affiche dans un toast.
    fn send(&self, command: Command) -> Task<Message> {
        let Some(backend) = self.backend() else {
            return Task::none();
        };
        let core = backend.core.clone();
        Task::perform(
            async move { core.send(command).await.map_err(|e| e.to_string()) },
            Message::CommandDone,
        )
    }

    fn show_toast(&mut self, message: String) -> Task<Message> {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((seq, message));
        Task::perform(tokio::time::sleep(TOAST_DURATION), move |()| {
            Message::DismissToast(seq)
        })
    }

    fn navigate(&mut self, route: Route) -> Task<Message> {
        if self.route == route {
            return Task::none();
        }
        let previous = std::mem::replace(&mut self.route, route.clone());
        self.history.push(previous);
        Task::batch([self.open(route), scroll_top()])
    }

    /// Charge les données de la page (album, artiste) si besoin.
    fn open(&mut self, route: Route) -> Task<Message> {
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
            Route::Home => Task::none(),
        }
    }

    fn run_search(&mut self) -> Task<Message> {
        let query = self.search_query.trim().to_owned();
        if query.is_empty() {
            self.search = None;
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
        Task::perform(
            async move { catalog.search_all(&query).await.map_err(|e| e.to_string()) },
            move |r| Message::SearchDone(seq, r),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::BackendReady(result) => match result.map(|slot| slot.take()) {
                Ok(Some(backend)) => {
                    let core = backend.core.clone();
                    self.state = core.state();
                    self.phase = Phase::Ready(backend);
                    Task::stream(core_updates(core))
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
                    Task::batch([self.open(route), scroll_top()])
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
                Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |()| {
                    Message::SearchDebounced(seq)
                })
            }
            Message::SearchDebounced(seq) => {
                if seq == self.search_seq {
                    self.run_search()
                } else {
                    Task::none()
                }
            }
            Message::SearchSubmit => {
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
                        for album in &results.albums {
                            let id = self.url_cover(album.thumbnail_url.as_deref());
                            tasks.push(self.want_cover(id, CARD));
                        }
                        for artist in &results.artists {
                            let id = self.url_cover(artist.thumbnail_url.as_deref());
                            tasks.push(self.want_cover(id, CARD));
                        }
                        // Le premier résultat est le plus probable : on le prépare.
                        if let Some(first) = results.songs.first() {
                            tasks.push(self.send(Command::Prefetch(first.video_id.clone())));
                        }
                        self.search = Some(Load::Ready(results));
                        Task::batch(tasks)
                    }
                    Err(e) => {
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
                        if let Some(first) = album.tracks.first() {
                            tasks.push(self.send(Command::Prefetch(first.video_id.clone())));
                        }
                        self.album = Some((id, Load::Ready(album)));
                        Task::batch(tasks)
                    }
                    Err(e) => {
                        self.album = Some((id, Load::Failed(e)));
                        Task::none()
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
                        self.artist = Some((id, Load::Ready(artist)));
                        Task::batch(tasks)
                    }
                    Err(e) => {
                        self.artist = Some((id, Load::Failed(e)));
                        Task::none()
                    }
                }
            }
            Message::CoverLoaded(id, size, result) => {
                match result {
                    Ok(handle) => {
                        self.covers.insert((id, size), Some(handle));
                    }
                    Err(e) => tracing::debug!(error = %e, "jaquette indisponible"),
                }
                Task::none()
            }
            Message::AccentLoaded(id, result) => {
                let Ok(rgb) = result else {
                    return Task::none();
                };
                let color = style::accent_from_rgb(rgb);
                self.accents.insert(id.clone(), color);
                if self.current_cover().as_deref() == Some(id.as_str()) {
                    self.accent = color;
                }
                Task::none()
            }
            Message::Play(tracks, start) => self.send(Command::PlayTracks { tracks, start }),
            Message::PlayShuffled(tracks) => {
                if tracks.is_empty() {
                    return Task::none();
                }
                let start = fastrand::usize(..tracks.len());
                Task::batch([
                    self.send(Command::SetShuffle(true)),
                    self.send(Command::PlayTracks { tracks, start }),
                ])
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
            Message::ToggleShuffle => self.send(Command::SetShuffle(!self.state.shuffle)),
            Message::CycleRepeat => self.send(Command::SetRepeat(self.state.repeat.cycle())),
            Message::ToggleAutoplay => self.send(Command::SetAutoplay(!self.state.autoplay)),
            Message::ToggleQueue => {
                self.show_queue = !self.show_queue;
                Task::none()
            }
            Message::JumpTo(id) => self.send(Command::JumpTo(id)),
            Message::Remove(id) => self.send(Command::Remove(id)),
            Message::ClearQueue => self.send(Command::ClearUpcoming),
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
            Message::Unhover => {
                self.hovered = None;
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
                self.shutting_down = true;
                match std::mem::replace(&mut self.phase, Phase::Starting) {
                    Phase::Ready(backend) => {
                        Task::perform(backend.shutdown(), |()| Message::ShutdownDone)
                    }
                    _ => iced::exit(),
                }
            }
            Message::ShutdownDone => iced::exit(),
        }
    }

    pub fn current_cover(&self) -> Option<String> {
        let track = &self.state.current.as_ref()?.track;
        self.track_cover(track)
    }

    fn on_core(&mut self, update: CoreUpdate) -> Task<Message> {
        match update {
            CoreUpdate::State(state) => {
                let track_changed = state.current.as_ref().map(|e| e.id)
                    != self.state.current.as_ref().map(|e| e.id);
                self.state = state;
                if self.muted_volume.is_some() && self.state.volume > 0 {
                    self.muted_volume = None;
                }
                if !track_changed {
                    return Task::none();
                }
                let cover = self.current_cover();
                if let Some(color) = cover.as_ref().and_then(|id| self.accents.get(id)) {
                    self.accent = *color;
                }
                Task::batch([
                    self.want_cover(cover.clone(), HERO),
                    self.want_cover(cover.clone(), THUMB),
                    self.want_accent(cover),
                ])
            }
            CoreUpdate::Queue(queue) => {
                self.queue = queue;
                let queue = Arc::clone(&self.queue);
                self.want_track_covers(queue.entries.iter().map(|e| &e.track), THUMB)
            }
            CoreUpdate::Event(CoreEvent::Error(message)) => self.show_toast(message),
            CoreUpdate::Event(_) => Task::none(),
        }
    }

    fn on_key(&mut self, event: keyboard::Event) -> Task<Message> {
        // `listen` ne transmet que les touches ignorées par les widgets : taper
        // un espace dans la recherche n'arrive donc pas ici.
        let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
            return Task::none();
        };
        match key.as_ref() {
            Key::Named(key::Named::Space) => self.send(Command::TogglePause),
            Key::Named(key::Named::ArrowRight) if modifiers.control() => self.send(Command::Next),
            Key::Named(key::Named::ArrowLeft) if modifiers.control() => {
                self.send(Command::Previous)
            }
            Key::Named(key::Named::ArrowRight) => self.seek_by(10.0),
            Key::Named(key::Named::ArrowLeft) => self.seek_by(-10.0),
            Key::Character("f" | "k") if modifiers.control() => {
                Task::batch([self.navigate(Route::Search), operation::focus(SEARCH_INPUT)])
            }
            Key::Named(key::Named::Escape) | Key::Named(key::Named::BrowserBack) => {
                self.update(Message::Back)
            }
            _ => Task::none(),
        }
    }

    fn seek_by(&mut self, delta: f32) -> Task<Message> {
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

fn scroll_top() -> Task<Message> {
    operation::snap_to(PAGE_SCROLL, iced::widget::scrollable::RelativeOffset::START)
}

/// Flux des changements du moteur, converti en messages pour l'interface.
fn core_updates(core: ytmr_core::CoreHandle) -> impl iced::futures::Stream<Item = Message> {
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
