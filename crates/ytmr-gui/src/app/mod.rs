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
use ytmr_catalog::lyrics::Lyrics;
use ytmr_catalog::{Album, Artist, SearchAll};
use ytmr_core::{
    AppPaths, Command, Config, CoreEvent, PlaybackState, QueueId, QueueSnapshot, Status, Track,
};

mod core_sync;
mod covers;
mod fade;
mod keys;
mod lyrics;
mod native;
mod navigation;
mod selection;
mod update;

use core_sync::core_updates;
use fade::Fade;
pub use lyrics::{lyric_heights, lyrics_text_width};
use native::{native_handle, window_hwnd};

use crate::backend::Backend;
use crate::smooth::Scrolling;
use crate::style;

/// Tailles demandées au cache de jaquettes (en pixels physiques, ~2× l'affichage).
pub const THUMB: u32 = 96;
/// Cartes (~164 px à l'écran) : 160 Ko décodés chacune.
pub const CARD: u32 = 200;
/// Grandes pochettes (en-tête ~220 px, vue « En cours » ~420 px) : 1 Mo décodé.
pub const HERO: u32 = 512;
/// Pochette du mode focus : pleine définition (jusqu'à 620 px affichés, × l'échelle
/// de l'écran ; 512 était agrandi et paraissait flou).
pub const BIG: u32 = 1200;
/// Fondu entre deux pochettes dans le mode focus.
pub const NP_FADE: Duration = Duration::from_millis(700);
/// Attente maximale de la pochette en pleine définition avant de se contenter
/// de la 512 px.
pub const NP_HD_WAIT: Duration = Duration::from_secs(3);
/// Mémoire réservée aux pochettes décodées (~1 600 vignettes ou 300 cartes).
const COVER_BUDGET: u64 = 64 << 20;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(320);
/// Attente maximale des pochettes avant d'afficher des résultats de recherche.
/// (Garde-fou : une pochette qui ne répond pas ne bloque pas la recherche.)
const SEARCH_COVERS_WAIT: Duration = Duration::from_secs(6);
/// Survol prolongé avant de préparer un morceau (évite de lancer yt-dlp en
/// passant simplement la souris sur une liste).
const HOVER_PREFETCH_DELAY: Duration = Duration::from_millis(250);
const TOAST_DURATION: Duration = Duration::from_secs(4);
/// Fondu des contrôles de la vue « En cours ».
const CONTROLS_FADE: Duration = Duration::from_millis(260);
/// Suggestions demandées un peu avant la recherche complète.
const SUGGEST_DEBOUNCE: Duration = Duration::from_millis(140);
/// Paroles gardées en mémoire (les plus anciennes sont oubliées au-delà).
const LYRICS_KEPT: usize = 40;

pub const SEARCH_INPUT: &str = "recherche";
/// Défilement de la page centrale, remis en haut à chaque navigation.
pub const PAGE_SCROLL: &str = "page";
pub const QUEUE_SCROLL: &str = "file";
pub const LYRICS_SCROLL: &str = "paroles";
/// Paroles de la vue « En cours » (défilement à part du panneau de droite).
pub const NP_LYRICS_SCROLL: &str = "paroles-en-cours";
/// Vue « En cours » : marges, largeur de la colonne pochette et écart.
pub const NP_PAD: f32 = 64.0;
pub const NP_GAP: f32 = 72.0;
/// Largeur maximale de la colonne des paroles de la vue « En cours ».
pub const NP_LYRICS_MAX: f32 = 960.0;
/// Taille du texte des paroles (panneau de droite, vue « En cours »).
pub const LYRIC_SIZE: f32 = 20.0;
pub const NP_LYRIC_SIZE: f32 = 32.0;

/// Marge au-dessus de la première ligne de paroles.
pub const LYRICS_TOP: f32 = 24.0;

/// Onglets de la page de recherche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTab {
    All,
    Songs,
    Albums,
    Artists,
}

/// Recherches récentes gardées.
pub const RECENT_SEARCHES: usize = 12;

/// Où l'on peut déposer des morceaux glissés.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    Queue,
    /// Playlist du compte (ou titres likés).
    Playlist(String),
}

/// Morceaux en train d'être glissés depuis une liste.
#[derive(Debug, Clone)]
pub struct TrackDrag {
    pub tracks: Vec<Track>,
    /// Premier point vu (le glissement commence après quelques pixels).
    origin: Option<iced::Point>,
    pub pos: iced::Point,
    /// Assez bougé : c'est un glissement, pas un clic.
    pub active: bool,
    /// Clic simple sur une ligne d'une sélection multiple : la sélection se
    /// réduit à elle au relâchement (si on n'a pas glissé).
    collapse_to: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum DragStep {
    Move(iced::Point),
    Release,
}

/// Ce qu'affiche le panneau de droite : la file ou les paroles, jamais les deux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightPanel {
    Queue,
    Lyrics,
}

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
    /// Historique d'écoute complet.
    History,
    /// Statistiques d'écoute (calculées en local).
    Stats,
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
    ToggleQueue,
    /// Bouton micro : panneau des paroles à la place de la file.
    ToggleLyrics,
    /// Clic sur un morceau (identifiant, rang dans la liste affichée) :
    /// sélection simple, Ctrl = ajouter / retirer, Maj = plage.
    Select(String, usize),
    /// Plusieurs morceaux : lire ensuite / ajouter à la file.
    PlayNextAll(Vec<Track>),
    EnqueueAll(Vec<Track>),
    DragTracks(DragStep),
    DropEnter(DropTarget),
    DropLeave(DropTarget),
    /// Fenêtre d'aide des raccourcis (touche ?).
    ToggleHelp,
    /// Survol d'une carte de l'accueil (bouton lecture).
    CardHover(String),
    CardUnhover(String),
    /// Bouton lecture d'une carte : album, playlist ou artiste à charger puis lire.
    PlayItem(ytmr_catalog::home::HomeKind, String),
    PlayFetched(Result<Vec<Track>, String>),
    /// Double-clic sur une ligne de la file.
    JumpTo(QueueId),
    /// La souris quitte une ligne de la file.
    QueueUnhover(usize),
    Remove(QueueId),
    ClearQueue,
    Hover(String),
    HoverSettled(String),
    /// La souris quitte la ligne de ce morceau.
    Unhover(String),
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
    /// Glisser-déposer dans la file d'attente.
    QueueDrag(QueueDrag),
    WindowResized(iced::Size),
    /// Plusieurs messages à la suite (menu contextuel : fermer puis agir).
    Batch(Vec<Message>),
    Context(crate::context::Ctx),
    LyricsLoaded(String, Result<Option<Lyrics>, String>),
    /// Clic sur une ligne de paroles synchronisées.
    SeekTo(Duration),
    SuggestDebounced(u64),
    Suggestions(u64, Result<Vec<String>, String>),
    PickSuggestion(String),
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
    /// Clic sur le nom d'un morceau : sa page d'album (cherchée s'il le faut).
    OpenAlbumOf(Track),
    AlbumOfFound(Result<Option<String>, String>),
    /// Handle natif de la fenêtre, pour l'overlay média du système.
    NativeWindow(Option<usize>),
    /// Vue « En cours » : un clic affiche ou masque les contrôles.
    ToggleControls,
    /// Survol d'une ligne de la file (distinct du survol des listes).
    QueueHover(Option<usize>),
    QueueHoverSettled(String),
    /// La fenêtre passe au premier plan (ou le quitte).
    Focus(bool),
    MediaControls(Result<(), String>),
    Lib(crate::library::Lib),
    Settings(crate::settings::Settings),
    Stats(crate::stats_page::Stats),
    Coll(crate::collection::Coll),
    Window(crate::chrome::Win),
    /// Clic à absorber (panneau d'une fenêtre par-dessus la page).
    Noop,
    CloseRequested,
    Import(crate::import::Imp),
    Toast(String),
    SearchTab(SearchTab),
    /// Relancer une recherche récente.
    SearchRecent(String),
    SearchForget(String),
    SearchClearRecent,
    ShutdownDone,
}

/// Barre latérale redimensionnable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Left,
    Right,
}

/// Étapes d'un glisser-déposer dans la file (indices de la file affichée).
#[derive(Debug, Clone, Copy)]
pub enum QueueDrag {
    Press(usize),
    Over(usize),
    Release,
}

/// Morceau saisi : d'où il part, au-dessus de quelle ligne il est.
#[derive(Debug, Clone, Copy)]
pub struct QueueDragState {
    pub from: usize,
    pub over: usize,
    /// La souris a quitté la ligne de départ : ce n'est plus un simple clic.
    pub moved: bool,
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
    /// Notification système à chaque nouveau morceau.
    #[serde(default = "yes")]
    notifications: bool,
    /// Mode hors ligne (aucun accès au réseau).
    offline_mode: bool,
    /// Icône dans la zone de notification ; fermer la fenêtre la cache.
    tray: bool,
}

fn yes() -> bool {
    true
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            left_width: LEFT_DEFAULT,
            queue_width: RIGHT_DEFAULT,
            show_queue: true,
            notifications: true,
            offline_mode: false,
            tray: false,
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
    /// Pochettes libérables : vignettes des listes virtualisées et du
    /// préchargement (redemandées au défilement). Les autres (profil, cartes,
    /// grandes pochettes) restent tant que l'appli tourne.
    cover_evictable: std::collections::HashSet<(String, u32)>,
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
    /// Onglet des résultats, et recherches récentes (les plus récentes d'abord).
    pub search_tab: SearchTab,
    pub recent_searches: Vec<String>,
    /// Fenêtre « Importer depuis Spotify » ouverte.
    pub import: Option<crate::import::Import>,
    /// Résultats reçus, en attente de leurs pochettes (au plus ~0,7 s).
    search_pending: Option<(u64, String, SearchAll)>,
    search_inflight: bool,
    pub fullscreen: bool,
    /// Fenêtre agrandie (pas de poignées de redimensionnement).
    pub maximized: bool,
    /// Vue « En cours » : contrôles visibles (après un mouvement de souris).
    pub controls_visible: bool,
    /// Opacité des contrôles de la vue « En cours » (0 à 1, animée).
    pub controls_level: f32,
    /// Curseurs et interrupteurs animés.
    pub anims: crate::anim::Anims,
    /// Fondu des contrôles en cours : (début, opacité de départ).
    controls_anim: Option<(std::time::Instant, f32)>,
    /// Mode focus : pochette affichée, et fondu depuis la précédente.
    pub np_art: Option<String>,
    /// Icône dans la zone de notification (Windows) : fermer cache la fenêtre.
    pub tray: bool,
    pub np_fade: Option<(std::time::Instant, Option<String>)>,
    /// Nouveau morceau dont la pochette n'est pas encore prête (depuis quand).
    pub np_waiting: Option<std::time::Instant>,
    /// Ligne survolée dans la file d'attente.
    pub queue_hover: Option<usize>,
    queue_hover_video: Option<String>,
    /// Pochettes des lignes affichées : jamais libérées tant qu'elles le sont.
    visible_covers: std::collections::HashSet<(String, u32)>,
    /// Pochettes impossibles à charger : ne plus les attendre.
    cover_failed: std::collections::HashSet<(String, u32)>,
    pub show_queue: bool,
    pub right_panel: RightPanel,
    /// Morceau sélectionné d'un clic (listes) ; ligne sélectionnée de la file.
    pub selected: Vec<String>,
    /// Dernier morceau cliqué (point de départ d'une plage avec Maj).
    select_anchor: Option<usize>,
    /// Touches de modification enfoncées (Ctrl, Maj…).
    modifiers: keyboard::Modifiers,
    pub track_drag: Option<TrackDrag>,
    pub drop_target: Option<DropTarget>,
    pub show_help: bool,
    /// Carte de l'accueil survolée.
    pub card_hover: Option<String>,
    /// Morceau sélectionné dans la file (par identifiant : il suit le morceau).
    pub queue_selected: Option<QueueId>,
    /// Aléatoire demandé : la prochaine file reçue apparaîtra en fondu.
    shuffle_pending: bool,
    /// File remélangée en train de réapparaître en fondu.
    pub queue_fade: Option<crate::anim::QueueFade>,
    /// Instant de la dernière image (avancement du fondu de la file).
    pub frame_now: std::time::Instant,
    /// Défilement manuel des paroles : le suivi automatique reprend après.
    lyrics_manual_until: Option<std::time::Instant>,
    /// Mesure des images par seconde (`YTMR_FPS`) : début de la seconde, images.
    fps_meter: Option<(std::time::Instant, u32)>,
    /// Ligne de paroles suivie (pour ne glisser qu'au changement de ligne).
    lyrics_line: Option<usize>,
    pub lyrics_view: View,
    pub np_lyrics_view: View,
    pub seek_preview: Option<f32>,
    muted_volume: Option<u8>,
    pub hovered: Option<String>,
    pub toast: Option<(u64, String)>,
    toast_seq: u64,
    scrolling: Scrolling,
    pub lib: crate::library::LibraryState,
    pub settings: crate::settings::SettingsState,
    pub stats: crate::stats_page::StatsState,
    pub coll: crate::collection::CollectionState,
    pub page_view: View,
    pub queue_view: View,
    covers_seq: u64,
    pub left_width: f32,
    pub queue_width: f32,
    window_width: f32,
    pub window_size: iced::Size,
    /// Dernier clic droit (position dans la fenêtre).
    pub context_point: iced::Point,
    pub context_menu: Option<(iced::Point, crate::context::Target)>,
    /// Paroles par morceau (`Ready(None)` : aucune source n'en a).
    pub lyrics: HashMap<String, Load<Option<Lyrics>>>,
    /// Suggestions de la recherche en cours de frappe.
    pub suggestions: Vec<String>,
    /// Notifications (partagé avec `ytmr-os`).
    pub notify: ytmr_os::NotifySettings,
    dragging: Option<Panel>,
    pub queue_drag: Option<QueueDragState>,
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
        let size = window::oldest()
            .and_then(window::size)
            .map(Message::WindowResized);
        let start = Task::batch([start, size]);
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
            cover_evictable: std::collections::HashSet::new(),
            accents: HashMap::new(),
            accent: style::DEFAULT_ACCENT,
            accent_fade: Fade::new(style::DEFAULT_ACCENT),
            page_accent: style::DEFAULT_ACCENT,
            page_fade: Fade::new(style::DEFAULT_ACCENT),
            search_shown: String::new(),
            search_tab: SearchTab::All,
            recent_searches: Vec::new(),
            import: None,
            search_pending: None,
            search_inflight: false,
            fullscreen: false,
            maximized: false,
            controls_visible: false,
            controls_level: 0.0,
            anims: crate::anim::Anims::default(),
            controls_anim: None,
            np_art: None,
            tray: false,
            np_fade: None,
            np_waiting: None,
            queue_hover: None,
            queue_hover_video: None,
            visible_covers: std::collections::HashSet::new(),
            cover_failed: std::collections::HashSet::new(),
            show_queue: true,
            seek_preview: None,
            muted_volume: None,
            hovered: None,
            toast: None,
            toast_seq: 0,
            scrolling: Scrolling::default(),
            lib: crate::library::LibraryState::default(),
            settings: crate::settings::SettingsState::default(),
            stats: crate::stats_page::StatsState::default(),
            coll: crate::collection::CollectionState::default(),
            page_view: View::default(),
            queue_view: View::default(),
            covers_seq: 0,
            left_width: LEFT_DEFAULT,
            queue_width: RIGHT_DEFAULT,
            window_width: 1320.0,
            window_size: iced::Size::new(1320.0, 860.0),
            context_point: iced::Point::ORIGIN,
            context_menu: None,
            lyrics: HashMap::new(),
            suggestions: Vec::new(),
            notify: ytmr_os::NotifySettings::new(true),
            dragging: None,
            queue_drag: None,
            right_panel: RightPanel::Queue,
            selected: Vec::new(),
            select_anchor: None,
            modifiers: keyboard::Modifiers::default(),
            track_drag: None,
            drop_target: None,
            show_help: false,
            card_hover: None,
            queue_selected: None,
            shuffle_pending: false,
            queue_fade: None,
            frame_now: std::time::Instant::now(),
            lyrics_manual_until: None,
            fps_meter: std::env::var_os("YTMR_FPS").map(|_| (std::time::Instant::now(), 0)),
            lyrics_line: None,
            lyrics_view: View::default(),
            np_lyrics_view: View::default(),
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
        app.stats.file = Some(config.1.data_dir.join("plays.jsonl"));
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
            window::resize_events().map(|(_, size)| Message::WindowResized(size)),
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
        // Premier plan : pas de notification quand la fenêtre est sous les yeux.
        subscriptions.push(iced::event::listen_with(|event, _, _| match event {
            iced::Event::Window(window::Event::Focused) => Some(Message::Focus(true)),
            iced::Event::Window(window::Event::Unfocused) => Some(Message::Focus(false)),
            _ => None,
        }));
        // Morceaux saisis dans une liste : on suit la souris jusqu'au relâchement.
        if self.track_drag.is_some() {
            subscriptions.push(iced::event::listen_with(|event, _, _| match event {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::DragTracks(DragStep::Move(position)))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Message::DragTracks(DragStep::Release)),
                _ => None,
            }));
        }
        // Morceau saisi dans la file : il tombe au relâchement, où qu'il soit.
        if self.queue_drag.is_some() {
            subscriptions.push(iced::event::listen_with(|event, _, _| match event {
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Message::QueueDrag(QueueDrag::Release)),
                _ => None,
            }));
        }
        // Fichier CSV déposé sur la fenêtre pendant un import Spotify.
        if self.import.is_some() {
            subscriptions.push(iced::event::listen_with(|event, _, _| match event {
                iced::Event::Window(window::Event::FileDropped(path)) => {
                    Some(Message::Import(crate::import::Imp::File(path)))
                }
                _ => None,
            }));
        }
        // Une image par rafraîchissement de l'écran, seulement pendant une animation.
        // `YTMR_FPS=1` : redessin continu et images/s dans le journal (mesure).
        if self.scrolling.is_animating()
            || self.fps_meter.is_some()
            || self.controls_anim.is_some()
            || self.np_fade.is_some()
            || (self.route == Route::NowPlaying && self.np_waiting.is_some())
            || self.anims.is_running()
            || self.queue_fade.is_some()
            || self.is_loading()
            || self.accent_fade.is_running()
            || self.page_fade.is_running()
        {
            subscriptions.push(window::frames().map(Message::Frame));
        }
        Subscription::batch(subscriptions)
    }

    /// Largeur de la colonne centrale (entre les barres latérales).
    pub fn center_width(&self) -> f32 {
        let queue = if self.show_queue {
            self.queue_width
        } else {
            0.0
        };
        (self.window_width - self.left_width - queue).max(0.0)
    }

    pub(crate) fn notifications_on(&self) -> bool {
        self.notify
            .enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Catalogue YouTube Music ; aucun en mode hors ligne (pas de réseau).
    pub(crate) fn catalog(&self) -> Option<Arc<ytmr_catalog::Catalog>> {
        if self.lib.offline_mode {
            return None;
        }
        self.backend().map(|b| Arc::clone(&b.catalog))
    }

    fn backend(&self) -> Option<&Backend> {
        match &self.phase {
            Phase::Ready(b) => Some(b),
            _ => None,
        }
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
        self.notify
            .enabled
            .store(settings.notifications, std::sync::atomic::Ordering::Relaxed);
        self.lib.offline_mode = settings.offline_mode;
        self.tray = settings.tray;
        #[cfg(windows)]
        ytmr_os::tray::set_enabled(self.tray);
    }

    /// Mode hors ligne côté interface : plus de jaquettes téléchargées.
    pub(crate) fn apply_offline_mode(&self) {
        if let Some(backend) = self.backend() {
            backend.covers.set_offline(self.lib.offline_mode);
        }
    }

    pub(crate) fn save_ui(&self) {
        let Some(path) = &self.ui_file else {
            return;
        };
        let settings = UiSettings {
            left_width: self.left_width,
            queue_width: self.queue_width,
            show_queue: self.show_queue,
            notifications: self.notifications_on(),
            offline_mode: self.lib.offline_mode,
            tray: self.tray,
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
}

fn scroll_top() -> Task<Message> {
    operation::snap_to(PAGE_SCROLL, iced::widget::scrollable::RelativeOffset::START)
}
