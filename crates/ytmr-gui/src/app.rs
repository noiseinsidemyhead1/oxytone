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
use crate::smooth::Scrolling;
use crate::style;

/// Tailles demandées au cache de jaquettes (en pixels physiques, ~2× l'affichage).
pub const THUMB: u32 = 96;
pub const CARD: u32 = 320;
pub const HERO: u32 = 640;
/// Mémoire réservée aux pochettes décodées (~1 600 vignettes ou 300 cartes).
const COVER_BUDGET: u64 = 64 << 20;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(320);
/// Attente maximale des pochettes avant d'afficher des résultats de recherche.
const SEARCH_COVERS_WAIT: Duration = Duration::from_millis(700);
/// Survol prolongé avant de préparer un morceau (évite de lancer yt-dlp en
/// passant simplement la souris sur une liste).
const HOVER_PREFETCH_DELAY: Duration = Duration::from_millis(250);
const TOAST_DURATION: Duration = Duration::from_secs(4);

pub const SEARCH_INPUT: &str = "recherche";
/// Défilement de la page centrale, remis en haut à chaque navigation.
pub const PAGE_SCROLL: &str = "page";
pub const QUEUE_SCROLL: &str = "file";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Home,
    /// Grande vue du morceau en cours (clic sur la pochette de la barre du bas).
    NowPlaying,
    Search,
    Album(String),
    Artist(String),
    Library,
    Playlist(String),
    Settings,
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
    Wheel(&'static str, f32),
    Scrolled(&'static str, iced::widget::scrollable::Viewport),
    /// Le défilement s'est arrêté : charger les pochettes des lignes visibles.
    CoversSettle(u64),
    DragStart(Panel),
    DragMove(f32),
    DragEnd,
    WindowResized(f32),
    /// Molette + Shift sur une rangée horizontale.
    WheelH(&'static str, f32),
    ShelfScrolled(&'static str, iced::widget::scrollable::Viewport),
    /// Pochette préchargée en tâche de fond.
    BacklogCover(String, u32, Result<image::Handle, String>),
    Frame(std::time::Instant),
    /// Résultats de recherche affichés même si des pochettes manquent encore.
    SearchReveal(u64),
    /// « Lecture » / « Aléatoire » de la page d'album ou d'artiste ouverte.
    PlayAlbum(bool),
    PlayArtist(bool),
    ToggleFullscreen,
    /// Clic sur le nom de l'artiste : (page connue, nom à chercher sinon).
    OpenArtist(Option<String>, String),
    ArtistFound(Result<Option<String>, String>),
    /// Handle natif de la fenêtre, pour l'overlay média du système.
    NativeWindow(Option<usize>),
    MediaControls(Result<(), String>),
    Lib(crate::library::Lib),
    Settings(crate::settings::Settings),
    /// Clic à absorber (panneau d'une fenêtre par-dessus la page).
    Noop,
    CloseRequested,
    ShutdownDone,
}

/// Barre latérale redimensionnable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Left,
    Right,
}

/// Partie visible d'une liste défilante.
#[derive(Debug, Clone, Copy)]
pub struct View {
    pub offset: f32,
    pub height: f32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            offset: 0.0,
            height: 900.0,
        }
    }
}

pub const LEFT_DEFAULT: f32 = 232.0;
pub const RIGHT_DEFAULT: f32 = 320.0;
const LEFT_RANGE: (f32, f32) = (180.0, 380.0);
const RIGHT_RANGE: (f32, f32) = (260.0, 560.0);
const COVERS_SETTLE: Duration = Duration::from_millis(120);

/// Réglages d'affichage gardés entre deux lancements.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct UiSettings {
    left_width: f32,
    queue_width: f32,
    show_queue: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            left_width: LEFT_DEFAULT,
            queue_width: RIGHT_DEFAULT,
            show_queue: true,
        }
    }
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
    /// Dernière demande de chaque pochette : les moins récentes sont libérées
    /// en premier quand la mémoire réservée aux images est pleine.
    cover_used: HashMap<(String, u32), u64>,
    cover_tick: u64,
    accents: HashMap<String, Color>,
    /// Teinte du morceau en cours (barre du bas, file), en fondu.
    pub accent: Color,
    accent_fade: Fade,
    /// Teinte de la page affichée (sa pochette), en fondu. Jamais mélangée
    /// avec celle du morceau en cours.
    pub page_accent: Color,
    page_fade: Fade,
    /// Requête dont les résultats sont affichés.
    pub search_shown: String,
    /// Résultats reçus, en attente de leurs pochettes (au plus ~0,7 s).
    search_pending: Option<(u64, String, SearchAll)>,
    search_inflight: bool,
    pub fullscreen: bool,
    pub show_queue: bool,
    pub seek_preview: Option<f32>,
    muted_volume: Option<u8>,
    pub hovered: Option<String>,
    pub toast: Option<(u64, String)>,
    toast_seq: u64,
    scrolling: Scrolling,
    pub lib: crate::library::LibraryState,
    pub settings: crate::settings::SettingsState,
    pub page_view: View,
    pub queue_view: View,
    covers_seq: u64,
    pub left_width: f32,
    pub queue_width: f32,
    window_width: f32,
    dragging: Option<Panel>,
    /// Point de départ du glissement : (x de la souris, largeur d'alors).
    drag_anchor: Option<(f32, f32)>,
    ui_file: Option<std::path::PathBuf>,
    /// Page demandée mais pas encore chargée : on reste sur la page actuelle
    /// (barre de chargement en haut) et on y passe dès que c'est prêt.
    pub pending_route: Option<Route>,
    /// Début de l'animation de la barre de chargement.
    pub loading_since: Option<std::time::Instant>,
    pub loading_phase: f32,
    cover_backlog: std::collections::VecDeque<(String, u32)>,
    backlog_inflight: usize,
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
            cover_used: HashMap::new(),
            cover_tick: 0,
            accents: HashMap::new(),
            accent: style::DEFAULT_ACCENT,
            accent_fade: Fade::new(style::DEFAULT_ACCENT),
            page_accent: style::DEFAULT_ACCENT,
            page_fade: Fade::new(style::DEFAULT_ACCENT),
            search_shown: String::new(),
            search_pending: None,
            search_inflight: false,
            fullscreen: false,
            show_queue: true,
            seek_preview: None,
            muted_volume: None,
            hovered: None,
            toast: None,
            toast_seq: 0,
            scrolling: Scrolling::default(),
            lib: crate::library::LibraryState::default(),
            settings: crate::settings::SettingsState::default(),
            page_view: View::default(),
            queue_view: View::default(),
            covers_seq: 0,
            left_width: LEFT_DEFAULT,
            queue_width: RIGHT_DEFAULT,
            window_width: 1320.0,
            dragging: None,
            drag_anchor: None,
            ui_file: Some(config.1.cache_dir.join("ui.json")),
            pending_route: None,
            loading_since: None,
            loading_phase: 0.0,
            cover_backlog: std::collections::VecDeque::new(),
            backlog_inflight: 0,
            shutting_down: false,
        };
        let mut app = app;
        // Largeurs des barres et cache de la bibliothèque disponibles dès
        // l'ouverture, avant même que le backend ait fini de démarrer.
        app.load_ui();
        app.lib.cache_dir = Some(config.1.cache_dir.join("library"));
        app.settings.audio_dir = Some(config.1.cache_dir.join("audio"));
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
        let mut subscriptions = vec![
            window::close_requests().map(|_| Message::CloseRequested),
            keyboard::listen().map(Message::Key),
            window::resize_events().map(|(_, size)| Message::WindowResized(size.width)),
        ];
        // Glissement d'une barre latérale : on suit la souris jusqu'au relâchement.
        if self.dragging.is_some() {
            subscriptions.push(iced::event::listen_with(|event, _, _| match event {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::DragMove(position.x))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Message::DragEnd),
                _ => None,
            }));
        }
        // Une image par rafraîchissement de l'écran, seulement pendant une animation.
        if self.scrolling.is_animating()
            || self.is_loading()
            || self.accent_fade.is_running()
            || self.page_fade.is_running()
        {
            subscriptions.push(window::frames().map(Message::Frame));
        }
        Subscription::batch(subscriptions)
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

    /// Envoie une commande au moteur ; une erreur s'affiche dans un toast.
    pub(crate) fn send(&self, command: Command) -> Task<Message> {
        let Some(backend) = self.backend() else {
            return Task::none();
        };
        let core = backend.core.clone();
        Task::perform(
            async move { core.send(command).await.map_err(|e| e.to_string()) },
            Message::CommandDone,
        )
    }

    pub(crate) fn show_toast(&mut self, message: String) -> Task<Message> {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((seq, message));
        Task::perform(tokio::time::sleep(TOAST_DURATION), move |()| {
            Message::DismissToast(seq)
        })
    }

    /// Pochettes des lignes visibles (playlist ouverte, file d'attente).
    pub(crate) fn request_visible_covers(&mut self) -> Task<Message> {
        let mut tracks: Vec<Track> = Vec::new();
        if let Route::Playlist(_) = self.route
            && let Some((_, Load::Ready(playlist))) = &self.lib.playlist
        {
            let (first, last) = crate::views::visible_range(
                self.page_view,
                crate::views::PLAYLIST_LIST_TOP,
                crate::views::ROW_H,
                playlist.tracks.len(),
            );
            tracks.extend(playlist.tracks[first..last].iter().cloned());
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
        self.want_track_covers(&tracks, THUMB)
    }

    fn touch_cover(&mut self, key: &(String, u32)) {
        self.cover_tick += 1;
        self.cover_used.insert(key.clone(), self.cover_tick);
    }

    /// Libère les pochettes décodées les moins récemment demandées au-delà
    /// de `COVER_BUDGET` (elles restent sur le disque : on les relit si on
    /// revient dessus). Une liste de 1 000 vignettes tient dans le budget.
    fn trim_covers(&mut self) {
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
        let keep: Vec<Option<String>> = vec![self.current_cover(), self.page_cover()];
        let mut loaded: Vec<((String, u32), u64)> = self
            .covers
            .iter()
            .filter(|(key, h)| h.is_some() && !keep.contains(&Some(key.0.clone())))
            .map(|(key, _)| (key.clone(), self.cover_used.get(key).copied().unwrap_or(0)))
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

    fn pump_backlog(&mut self) -> Task<Message> {
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

    fn load_ui(&mut self) {
        let Some(path) = &self.ui_file else {
            return;
        };
        let settings: UiSettings = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        self.left_width = settings.left_width.clamp(LEFT_RANGE.0, LEFT_RANGE.1);
        self.queue_width = settings.queue_width.clamp(RIGHT_RANGE.0, RIGHT_RANGE.1);
        self.show_queue = settings.show_queue;
    }

    fn save_ui(&self) {
        let Some(path) = &self.ui_file else {
            return;
        };
        let settings = UiSettings {
            left_width: self.left_width,
            queue_width: self.queue_width,
            show_queue: self.show_queue,
        };
        if let Ok(json) = serde_json::to_vec(&settings) {
            let _ = std::fs::write(path, json);
        }
    }

    /// « Se reconnecter » : nouvelle lecture de la session du navigateur.
    pub(crate) fn reconnect_task(&self) -> Option<Task<Message>> {
        let backend = self.backend()?;
        let reconnect = backend.reconnect();
        Some(Task::perform(reconnect, |state| {
            Message::Lib(crate::library::Lib::AccountReady(match state {
                crate::backend::AccountState::Ready(a) => Ok(crate::library::AccountHandle(a)),
                crate::backend::AccountState::Unavailable(e) => Err(e),
                crate::backend::AccountState::Connecting => Err("connexion interrompue".into()),
            }))
        }))
    }

    /// Navigation déclenchée par le code (après une suppression, par exemple).
    pub(crate) fn go_to(&mut self, route: Route) -> Task<Message> {
        self.navigate(route)
    }

    fn navigate(&mut self, route: Route) -> Task<Message> {
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
    fn needs_loading(&self, route: &Route) -> bool {
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

    fn switch_to(&mut self, route: Route) -> Task<Message> {
        let previous = std::mem::replace(&mut self.route, route.clone());
        self.history.push(previous);
        self.scrolling.reset(PAGE_SCROLL);
        self.page_view.offset = 0.0;
        let open = self.open(route);
        self.update_page_accent();
        Task::batch([open, scroll_top()])
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
            Route::Library => Task::batch([self.load_playlists(), self.want_list_covers()]),
            Route::Playlist(id) => self.open_playlist(id),
            Route::Home => self.refresh_home(),
            Route::NowPlaying => Task::none(),
            Route::Settings => self.open_settings(),
        }
    }

    fn run_search(&mut self) -> Task<Message> {
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

    pub fn update(&mut self, message: Message) -> Task<Message> {
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
                    self.phase = Phase::Ready(backend);
                    // Comme une mise à jour : charge aussi la jaquette d'une session reprise.
                    let initial = self.on_core(CoreUpdate::State(core.state()));
                    // Touches média et overlay du système : il faut la fenêtre native.
                    let native = window::oldest().and_then(|id| window::run(id, native_handle));
                    Task::batch([
                        initial,
                        account,
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
                    Task::batch([open, scroll_top()])
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
                    Err(e) => tracing::debug!(error = %e, "jaquette indisponible"),
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
                let mode = if self.fullscreen {
                    window::Mode::Fullscreen
                } else {
                    window::Mode::Windowed
                };
                window::oldest().and_then(move |id| window::set_mode(id, mode))
            }
            Message::OpenArtist(id, name) => match id {
                Some(id) => self.navigate(Route::Artist(id)),
                // Pas d'identifiant (morceau de radio) : on cherche l'artiste par son nom.
                None => {
                    let Some(catalog) = self.backend().map(|b| Arc::clone(&b.catalog)) else {
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
                Task::perform(
                    async move {
                        let runtime = tokio::runtime::Handle::current();
                        tokio::task::spawn_blocking(move || {
                            ytmr_os::start(core, handle, runtime).map_err(|e| e.to_string())
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    Message::MediaControls,
                )
            }
            Message::MediaControls(result) => {
                match result {
                    Ok(()) => tracing::info!("touches média et overlay du système branchés"),
                    Err(e) => tracing::warn!(error = %e, "touches média indisponibles"),
                }
                Task::none()
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
                self.save_ui();
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
            Message::Wheel(id, lines) => {
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
                } else if id == QUEUE_SCROLL {
                    self.queue_view = view;
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
            Message::DragEnd => {
                self.dragging = None;
                self.drag_anchor = None;
                self.save_ui();
                Task::none()
            }
            Message::WindowResized(width) => {
                self.window_width = width;
                Task::none()
            }
            Message::Frame(now) => {
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
                if let Ok(handle) = result {
                    let key = (id, size);
                    self.touch_cover(&key);
                    self.covers.insert(key, Some(handle));
                    self.trim_covers();
                }
                self.pump_backlog()
            }
            Message::Lib(message) => self.update_library(message),
            Message::Settings(message) => self.update_settings(message),
            Message::Noop => Task::none(),
            Message::ShutdownDone => iced::exit(),
        }
    }

    /// Lecture d'une liste, dans l'ordre ou en aléatoire.
    fn play_list(&mut self, tracks: Vec<Track>, shuffle: bool) -> Task<Message> {
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
    fn try_reveal_search(&mut self, force: bool) {
        let Some((_, _, results)) = &self.search_pending else {
            return;
        };
        let loaded = |id: Option<String>, size: u32| {
            id.is_none_or(|id| matches!(self.covers.get(&(id, size)), Some(Some(_))))
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

    /// Pochette qui donne sa teinte à la page affichée.
    fn page_cover(&self) -> Option<String> {
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

    fn on_core(&mut self, update: CoreUpdate) -> Task<Message> {
        match update {
            CoreUpdate::State(state) => {
                let track_changed = state.current.as_ref().map(|e| e.id)
                    != self.state.current.as_ref().map(|e| e.id);
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
                    return report;
                }
                let cover = self.current_cover();
                if let Some(color) = cover.as_ref().and_then(|id| self.accents.get(id)).copied() {
                    self.accent_fade.set(self.accent, color);
                }
                self.update_page_accent();
                Task::batch([
                    report,
                    self.want_cover(cover.clone(), HERO),
                    self.want_cover(cover.clone(), THUMB),
                    self.want_accent(cover),
                ])
            }
            CoreUpdate::Queue(queue) => {
                self.queue = queue;
                // Seulement les lignes visibles : une file de 900 titres ne doit
                // pas déclencher 900 téléchargements.
                self.request_visible_covers()
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
            Key::Character("f" | "k") if modifiers.control() => operation::focus(SEARCH_INPUT),
            Key::Named(key::Named::F11) => self.update(Message::ToggleFullscreen),
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

/// HWND sous Windows (exigé par l'overlay média) ; rien ailleurs.
fn native_handle(window: &dyn window::Window) -> Option<usize> {
    use raw_window_handle::RawWindowHandle;
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as usize),
        _ => None,
    }
}

/// Durée du fondu entre deux teintes.
const FADE: Duration = Duration::from_millis(600);

/// Teinte qui glisse d'une couleur à l'autre (pas de changement brutal).
#[derive(Debug, Clone, Copy)]
struct Fade {
    from: Color,
    to: Color,
    since: Option<std::time::Instant>,
}

impl Fade {
    fn new(color: Color) -> Self {
        Self {
            from: color,
            to: color,
            since: None,
        }
    }

    /// Nouvelle cible ; le fondu part de la couleur affichée `current`.
    fn set(&mut self, current: Color, to: Color) {
        if to == self.to {
            return;
        }
        self.from = current;
        self.to = to;
        self.since = Some(std::time::Instant::now());
    }

    fn is_running(&self) -> bool {
        self.since.is_some()
    }

    fn at(&mut self, now: std::time::Instant) -> Color {
        let Some(since) = self.since else {
            return self.to;
        };
        let t = (now.saturating_duration_since(since).as_secs_f32() / FADE.as_secs_f32()).min(1.0);
        if t >= 1.0 {
            self.since = None;
            return self.to;
        }
        // Douce au départ et à l'arrivée.
        let t = t * t * (3.0 - 2.0 * t);
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Color::from_rgb(
            mix(self.from.r, self.to.r),
            mix(self.from.g, self.to.g),
            mix(self.from.b, self.to.b),
        )
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
