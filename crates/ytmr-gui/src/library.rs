//! Bibliothèque du compte YouTube : playlists, titres likés, modifications.
//!
//! En clair : tout ce qui touche au compte de l'utilisateur. Les changements
//! (like, ajout, création, renommage, suppression) s'affichent tout de suite,
//! puis sont envoyés à YouTube ; en cas de refus, l'affichage est corrigé et un
//! message explique pourquoi.

use std::collections::{HashMap, HashSet};
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
use ytmr_catalog::home::{HomeItem, HomeKind, HomeShelf};
use ytmr_core::{Command, Track};

use crate::app::{App, CARD, HERO, Load, Message, Route, THUMB};
use crate::backend::AccountState;
use crate::icons::{Icon, icon};
use crate::style;
use crate::views::{
    BOLD, El, SEMIBOLD, cover, error_box, icon_tip, loading, muted, page_header, page_scroll,
    section_title, track_row,
};

pub const NEW_PLAYLIST_INPUT: &str = "nouvelle-playlist";
pub const RENAME_INPUT: &str = "renommer-playlist";
/// Liste des playlists de la fenêtre « Ajouter à… ».
pub const ADD_TO_SCROLL: &str = "ajout-playlist";

/// Durées pendant lesquelles une donnée en cache est jugée à jour : au-delà
/// seulement, on la redemande à YouTube (ou sur demande, bouton ↻).
const PLAYLIST_TTL: Duration = Duration::from_secs(30 * 60);
const LIST_TTL: Duration = Duration::from_secs(30 * 60);
const LIKED_TTL: Duration = Duration::from_secs(60 * 60);

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

/// Tri d'une playlist ouverte (affichage et lecture).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlaylistSort {
    /// Ordre de la playlist (date d'ajout).
    #[default]
    Original,
    Title,
    Artist,
    Album,
    Duration,
}

/// Fenêtres par-dessus la page.
#[derive(Debug, Clone)]
pub enum Overlay {
    /// Ajout d'un ou plusieurs morceaux à une playlist.
    AddTo(Vec<Track>),
    ConfirmDelete {
        id: String,
        title: String,
    },
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
    /// Playlists gardées hors ligne (`offline.json`).
    pub offline_playlists: HashSet<String>,
    /// Morceaux téléchargés un par un (`downloads.json`).
    pub downloads: Vec<Track>,
    /// Morceaux des playlists gardées hors ligne (playlist « Téléchargés »).
    pub offline_lists: HashMap<String, Vec<Track>>,
    /// Mode hors ligne (Paramètres) : aucun accès au réseau, seulement ce qui
    /// est téléchargé.
    pub offline_mode: bool,
    /// Morceaux présents dans le cache audio (lisibles sans réseau).
    pub offline_ids: HashSet<String>,
    /// Taille de chaque fichier téléchargé (page Téléchargements).
    pub offline_sizes: HashMap<String, u64>,
    pub downloads_by_size: bool,
    /// Suivi des téléchargements hors ligne : morceaux attendus, et nombre de
    /// relevés sans progrès (on arrête de regarder au bout d'un moment).
    offline_watch: Vec<String>,
    offline_stalled: u32,
    /// Historique d'écoute (page « Historique »).
    pub history: Option<Load<Vec<Track>>>,
    /// Rangées de l'accueil de YouTube Music.
    pub home: Option<Load<Vec<HomeShelf>>>,
    /// Accueil demandé pendant cette session (une seule fois).
    home_at: Option<Instant>,
    /// Recherche dans la playlist ouverte.
    pub filter: String,
    /// Tri de la playlist ouverte ; `sort_reversed` : second clic sur le même tri.
    pub sort: PlaylistSort,
    pub sort_reversed: bool,
    /// Playlists créées pendant cette session : YouTube met quelques secondes
    /// à les rendre lisibles, on les affiche vides en attendant.
    created: HashSet<String>,
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
        // Mode hors ligne : le compte n'est pas utilisé (aucune requête).
        if self.offline_mode {
            return None;
        }
        match &self.account {
            AccountView::Ready(a) => Some(Arc::clone(a)),
            _ => None,
        }
    }

    pub fn is_liked(&self, video_id: &str) -> bool {
        self.liked.contains(video_id)
    }

    /// Lisible maintenant : pas grisé par YouTube et, hors ligne, téléchargé.
    pub fn playable(&self, track: &Track) -> bool {
        !track.unavailable && (!self.offline_mode || self.offline_ids.contains(&track.video_id))
    }

    /// Tout ce qui est sur l'appareil : morceaux téléchargés un par un, puis
    /// ceux des playlists téléchargées (sans doublon, fichiers présents seulement).
    pub fn downloaded_tracks(&self) -> Vec<Track> {
        let mut seen = HashSet::new();
        let mut lists: Vec<(&String, &Vec<Track>)> = self.offline_lists.iter().collect();
        lists.sort_by(|a, b| a.0.cmp(b.0));
        self.downloads
            .iter()
            .chain(lists.into_iter().flat_map(|(_, tracks)| tracks.iter()))
            .filter(|t| self.offline_ids.contains(&t.video_id))
            .filter(|t| seen.insert(t.video_id.clone()))
            .cloned()
            .collect()
    }

    /// Playlist « Téléchargés ».
    pub fn downloads_playlist(&self) -> Playlist {
        let tracks = self.downloaded_tracks();
        Playlist {
            id: DOWNLOADS_ID.to_owned(),
            title: "Téléchargés".to_owned(),
            author: "Sur cet appareil".to_owned(),
            description: None,
            duration_text: String::new(),
            thumbnail_url: tracks.first().and_then(|t| t.thumbnail_url.clone()),
            tracks,
            complete: true,
            owned: false,
        }
    }

    pub(crate) fn playlist_title(&self, id: &str) -> String {
        if id == LIKED_ID {
            return "Titres likés".into();
        }
        if id == DOWNLOADS_ID {
            return "Téléchargés".into();
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
    OpenAddTo(Vec<Track>),
    OpenNew,
    CloseOverlay,
    AddTo(String, Vec<Track>),
    /// Playlist choisie dans la fenêtre d'ajout (les morceaux sont dans la fenêtre).
    AddToPicked(String),
    /// Retrait de plusieurs morceaux d'une playlist.
    RemoveTracks(String, Vec<String>),
    Added(String, Result<(), String>),
    NewNameInput(String),
    Create,
    Created(Result<(String, String), String>),
    /// Playlist créée par « Nouvelle playlist » : on l'ouvre pour la nommer.
    CreatedOpen(Result<(String, String), String>),
    /// Vérification différée de la liste après une création.
    ReloadPlaylists,
    FilterInput(String),
    Sort(PlaylistSort),
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
    PlayShuffled,
    /// Mise à jour forcée de la playlist ouverte.
    Refresh,
    ProfileLoaded(Result<Profile, String>),
    HomeLoaded(Result<Vec<HomeShelf>, String>),
    HistoryLoaded(Result<Vec<Track>, String>),
    /// Garder / ne plus garder la playlist ouverte hors ligne.
    ToggleOffline,
    /// Nouvel essai de connexion au compte (après une erreur réseau).
    RetryAccount,
    /// Télécharger (`true`) ou retirer des téléchargements des morceaux.
    Download(Vec<Track>, bool),
    /// Tous les morceaux de l'album ouvert (l'album n'entre pas dans la
    /// bibliothèque : seulement ses morceaux, dans « Téléchargés »).
    DownloadAlbum(bool),
    /// Relevé du cache audio (morceaux présents sur le disque).
    OfflineScanned(HashMap<String, u64>),
    /// Page Téléchargements : retirer une playlist, un titre, ou tout.
    RemoveOfflinePlaylist(String),
    RemoveDownload(String),
    RemoveAllDownloads,
    /// Tri de la page Téléchargements : par taille (sinon par nom).
    DownloadsBySize(bool),
    OfflineTick,
    /// Lecture de l'historique à partir de la ligne donnée.
    PlayHistory(usize),
    /// Morceau d'une rangée de l'accueil (rangée, carte).
    PlayHome(usize, usize),
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

/// Échec dû au réseau (YouTube injoignable), pas à la session du navigateur.
fn is_network_error(e: &str) -> bool {
    [
        "error sending request",
        "Web error",
        "timed out",
        "dns",
        "connexion refusée",
    ]
    .iter()
    .any(|m| e.contains(m))
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
    /// Accueil de YouTube Music, figé pendant toute la session : la version en
    /// cache s'affiche au lancement et la fraîche est seulement enregistrée
    /// (elle apparaîtra au prochain lancement). Sans cache, la fraîche s'affiche.
    pub fn load_home(&mut self) -> Task<Message> {
        if self.lib.home_at.is_some() {
            return Task::none();
        }
        let op = self.lib.ready().map(|a| async move { a.home().await });
        if op.is_none() {
            return Task::none();
        }
        let mut tasks = Vec::new();
        if self.lib.home.is_none() {
            let cached: Option<Vec<HomeShelf>> = self
                .lib
                .cache_dir
                .as_deref()
                .and_then(|dir| crate::collection::read_json(&dir.join("home.json")));
            self.lib.home = Some(match cached {
                Some(mut shelves) => {
                    ytmr_catalog::home::repair(&mut shelves);
                    Load::Ready(shelves)
                }
                None => Load::Loading,
            });
            tasks.push(self.want_home_covers());
        }
        self.lib.home_at = Some(Instant::now());
        tasks.push(call(op, Lib::HomeLoaded));
        Task::batch(tasks)
    }

    /// Pochettes des rangées de l'accueil visibles, plus une d'avance (au fil
    /// du défilement) : les décoder toutes d'un coup (~140) coûtait ~25 Mo de
    /// plus au lancement pour des rangées que l'on ne voit pas forcément.
    /// Une pochette demandée reste gardée : elle est affichée.
    pub(crate) fn want_home_covers(&mut self) -> Task<Message> {
        /// Hauteur approximative de la salutation, de la mosaïque et de « Tes playlists ».
        const TOP: f32 = 900.0;
        /// Hauteur approximative d'une rangée (titre, cartes, barre de défilement).
        const SHELF_H: f32 = 300.0;
        let Some(Load::Ready(shelves)) = &self.lib.home else {
            return Task::none();
        };
        let bottom = self.page_view.offset + self.page_view.height.max(800.0) + SHELF_H;
        let rows = ((bottom - TOP).max(0.0) / SHELF_H).ceil() as usize + 1;
        let urls: Vec<Option<String>> = shelves
            .iter()
            .filter(|s| is_listen_again(s))
            .chain(shelves.iter().filter(|s| !is_listen_again(s)).take(rows))
            .flat_map(|s| &s.items)
            .map(|i| i.thumbnail_url.clone())
            .collect();
        let ids: Vec<_> = urls.iter().map(|u| self.url_cover(u.as_deref())).collect();
        Task::batch(ids.into_iter().map(|id| self.want_cover(id, CARD)))
    }

    fn offline_file(&self) -> Option<PathBuf> {
        self.lib.cache_dir.as_ref().map(|d| d.join("offline.json"))
    }

    /// Pochettes de la mosaïque « Téléchargés ».
    fn want_downloads_covers(&mut self) -> Task<Message> {
        let mut ids: Vec<String> = Vec::new();
        for track in self.lib.downloaded_tracks() {
            if let Some(id) = self.track_cover(&track)
                && !ids.contains(&id)
            {
                ids.push(id);
                if ids.len() == 4 {
                    break;
                }
            }
        }
        let first = ids.first().cloned();
        let mut tasks = vec![self.want_cover(first, CARD)];
        for id in ids {
            tasks.push(self.want_cover(Some(id), THUMB));
        }
        Task::batch(tasks)
    }

    /// Garde ou libère des morceaux sur le disque. Libérés seulement s'ils ne
    /// sont plus voulus ailleurs (téléchargés seuls ou par une autre playlist).
    fn keep_offline(&mut self, mut ids: Vec<String>, keep: bool) -> Task<Message> {
        if !keep {
            let wanted: HashSet<&str> = self
                .lib
                .downloads
                .iter()
                .chain(self.lib.offline_lists.values().flatten())
                .map(|t| t.video_id.as_str())
                .collect();
            ids.retain(|id| !wanted.contains(id.as_str()));
        }
        Task::batch([
            self.send(Command::KeepOffline {
                video_ids: ids,
                keep,
            }),
            self.scan_offline(),
        ])
    }

    fn downloads_file(&self) -> Option<PathBuf> {
        self.lib
            .cache_dir
            .as_ref()
            .map(|d| d.join("downloads.json"))
    }

    /// Au lancement : playlists gardées hors ligne (leurs morceaux sont
    /// redonnés au moteur, nouveaux compris) et relevé du cache audio.
    pub fn load_offline(&mut self) -> Task<Message> {
        self.lib.offline_playlists = self
            .offline_file()
            .and_then(|p| crate::collection::read_json(&p))
            .unwrap_or_default();
        self.lib.downloads = self
            .downloads_file()
            .and_then(|p| crate::collection::read_json(&p))
            .unwrap_or_default();
        self.lib.offline_lists = match &self.lib.cache_dir {
            Some(dir) => self
                .lib
                .offline_playlists
                .iter()
                .filter_map(|id| read_playlist(dir, id).map(|p| (id.clone(), p.tracks)))
                .collect(),
            None => HashMap::new(),
        };
        let mut ids: Vec<String> = self
            .lib
            .offline_lists
            .values()
            .flatten()
            .map(|t| t.video_id.clone())
            .collect();
        ids.extend(self.lib.downloads.iter().map(|t| t.video_id.clone()));
        let keep = if ids.is_empty() {
            Task::none()
        } else {
            self.lib.offline_watch = ids.clone();
            self.send(Command::KeepOffline {
                video_ids: ids,
                keep: true,
            })
        };
        Task::batch([keep, self.scan_offline()])
    }

    /// Liste les morceaux présents dans le cache audio (en tâche de fond).
    pub fn scan_offline(&self) -> Task<Message> {
        let Some(dir) = self.settings.audio_dir.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                // Laisse le moteur écrire la liste des morceaux gardés.
                tokio::time::sleep(Duration::from_millis(300)).await;
                tokio::task::spawn_blocking(move || {
                    // Seuls les morceaux gardés exprès comptent comme téléchargés :
                    // le cache automatique (écoutes récentes) peut partir à tout moment.
                    let pinned: HashSet<String> = std::fs::read_to_string(dir.join("pinned.json"))
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok())
                        .unwrap_or_default();
                    std::fs::read_dir(&dir)
                        .map(|entries| {
                            entries
                                .flatten()
                                .map(|e| e.path())
                                .filter(|p| p.extension().is_some_and(|x| x == "audio"))
                                .filter_map(|p| {
                                    let id = p.file_stem().and_then(|s| s.to_str())?.to_owned();
                                    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                                    Some((id, size))
                                })
                                .filter(|(id, _)| pinned.contains(id))
                                .collect::<HashMap<String, u64>>()
                        })
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default()
            },
            |ids| Message::Lib(Lib::OfflineScanned(ids)),
        )
    }

    /// Page « Historique » : toujours redemandée (elle change à chaque écoute).
    pub fn load_history_page(&mut self) -> Task<Message> {
        let op = self.lib.ready().map(|a| async move { a.history().await });
        if op.is_none() {
            return Task::none();
        }
        if !matches!(self.lib.history, Some(Load::Ready(_))) {
            self.lib.history = Some(Load::Loading);
        }
        call(op, Lib::HistoryLoaded)
    }

    pub fn refresh_home(&mut self) -> Task<Message> {
        Task::batch([self.load_home(), self.want_home_covers()])
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
        let covers = self.want_list_covers();
        if fresh && matches!(self.lib.playlists, Some(Load::Ready(_))) {
            return covers;
        }
        Task::batch([covers, self.reload_playlists()])
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
        if !matches!(&self.lib.playlist, Some((cur, _)) if *cur == id) {
            self.lib.filter.clear();
        }
        if id == DOWNLOADS_ID {
            self.lib.playlist = Some((id, Load::Ready(self.lib.downloads_playlist())));
            return Task::none();
        }
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
                None if self.lib.offline_mode => {
                    self.lib.playlist = Some((
                        id.clone(),
                        Load::Failed("Cette playlist n'est pas disponible hors ligne.".into()),
                    ));
                }
                None => self.lib.playlist = Some((id.clone(), Load::Loading)),
            }
        }
        if self.lib.offline_mode {
            return Task::batch(tasks);
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
        tasks.push(self.backlog_playlist(playlist));
        Task::batch(tasks)
    }

    /// Toutes les vignettes de la playlist, préchargées en tâche de fond.
    fn backlog_playlist(&mut self, playlist: &Playlist) -> Task<Message> {
        let ids: Vec<String> = playlist
            .tracks
            .iter()
            .filter_map(|t| self.track_cover(t))
            .collect();
        self.backlog_covers(ids, THUMB)
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
                let id = playlist.id.clone();
                let ids: Vec<String> = tracks.iter().filter_map(|t| self.track_cover(t)).collect();
                if let Some((_, Load::Ready(playlist))) = &mut self.lib.playlist {
                    playlist.tracks.extend(tracks);
                }
                let mut tasks = vec![
                    self.request_visible_covers(),
                    self.backlog_covers(ids, THUMB),
                ];
                if first_page {
                    // Première page reçue : on peut afficher la page.
                    tasks.push(self.arrived(&Route::Playlist(id)));
                }
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
                let route = Route::Playlist(fresh.id.clone());
                Task::batch([self.finish_playlist(fresh), self.arrived(&route)])
            }
        }
    }

    /// Version complète reçue : affichage, cache disque, likes.
    fn finish_playlist(&mut self, fresh: Playlist) -> Task<Message> {
        let offline = if self.lib.offline_playlists.contains(&fresh.id) {
            self.lib
                .offline_lists
                .insert(fresh.id.clone(), fresh.tracks.clone());
            let ids: Vec<String> = fresh.tracks.iter().map(|t| t.video_id.clone()).collect();
            self.lib.offline_watch = ids.clone();
            Task::batch([
                self.send(Command::KeepOffline {
                    video_ids: ids,
                    keep: true,
                }),
                self.scan_offline(),
            ])
        } else {
            Task::none()
        };
        Task::batch([offline, self.finish_playlist_inner(fresh)])
    }

    fn finish_playlist_inner(&mut self, fresh: Playlist) -> Task<Message> {
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
        let mut fresh = fresh;
        let mut covers = Task::none();
        if let Some((cur, shown)) = &self.lib.playlist
            && *cur == fresh.id
        {
            // YouTube ne rend pas toujours la pochette dans la page, et en change
            // parfois seulement les paramètres (même image) : on garde alors
            // l'ancienne adresse, rien n'est rechargé ni ne clignote.
            if let Load::Ready(old) = shown {
                let same_image = match (&fresh.thumbnail_url, &old.thumbnail_url) {
                    (None, _) => true,
                    (Some(new), Some(old)) => {
                        ytmr_media::normalize_url(new) == ytmr_media::normalize_url(old)
                    }
                    (Some(_), None) => false,
                };
                if same_image {
                    fresh.thumbnail_url = old.thumbnail_url.clone();
                }
            }
            // L'adresse a pu changer : la nouvelle pochette doit être demandée,
            // sinon l'en-tête reste vide à la fin de la mise à jour.
            let cover_id = if fresh.id == LIKED_ID {
                fresh.tracks.first().and_then(|t| self.track_cover(t))
            } else {
                self.url_cover(fresh.thumbnail_url.as_deref())
            };
            covers = Task::batch([
                self.want_cover(cover_id.clone(), HERO),
                self.want_accent(cover_id),
            ]);
            self.lib.playlist = Some((fresh.id.clone(), Load::Ready(fresh)));
            self.update_page_accent();
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
                let mut tasks = vec![
                    self.load_playlists(),
                    liked,
                    profile,
                    self.load_home(),
                    self.load_collection(),
                    self.load_releases(),
                ];
                if let Route::Playlist(id) = self.route.clone() {
                    tasks.push(self.open_playlist(id));
                }
                Task::batch(tasks)
            }
            Lib::PlayShuffled => match &self.lib.playlist {
                Some((_, Load::Ready(playlist))) if !playlist.tracks.is_empty() => {
                    let tracks = playlist.tracks.clone();
                    self.update(Message::PlayShuffled(tracks))
                }
                _ => Task::none(),
            },
            Lib::PlayAt(index) => match &self.lib.playlist {
                Some((_, Load::Ready(playlist))) if index < playlist.tracks.len() => {
                    // Lecture dans l'ordre affiché (tri), toute la playlist.
                    let mut order = sorted_order(playlist, self.lib.sort, self.lib.sort_reversed);
                    order.retain(|&i| self.lib.playable(&playlist.tracks[i]));
                    let start = order.iter().position(|&i| i == index).unwrap_or(0);
                    let tracks = order.iter().map(|&i| playlist.tracks[i].clone()).collect();
                    self.send(Command::PlayTracks { tracks, start })
                }
                _ => Task::none(),
            },
            Lib::Sort(sort) => {
                if self.lib.sort == sort && sort != PlaylistSort::Original {
                    self.lib.sort_reversed = !self.lib.sort_reversed;
                } else {
                    self.lib.sort = sort;
                    self.lib.sort_reversed = false;
                }
                // D'autres lignes arrivent à l'écran : leurs pochettes.
                self.request_visible_covers()
            }
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
            Lib::HomeLoaded(result) => match result {
                Ok(shelves) => {
                    if let Some(dir) = &self.lib.cache_dir {
                        write_json(&dir.join("home.json"), &shelves);
                    }
                    // Déjà une version à l'écran : on n'y touche pas.
                    if matches!(self.lib.home, Some(Load::Ready(_))) {
                        return Task::none();
                    }
                    self.lib.home = Some(Load::Ready(shelves));
                    self.want_home_covers()
                }
                Err(e) => {
                    tracing::warn!("accueil : {e}");
                    if !matches!(self.lib.home, Some(Load::Ready(_))) {
                        self.lib.home = Some(Load::Failed(e));
                    }
                    Task::none()
                }
            },
            Lib::HistoryLoaded(result) => match result {
                Ok(tracks) => {
                    let task = self.want_track_covers(&tracks, THUMB);
                    self.lib.history = Some(Load::Ready(tracks));
                    task
                }
                Err(e) => {
                    self.lib.history = Some(Load::Failed(e));
                    Task::none()
                }
            },
            Lib::ToggleOffline => {
                let Some((id, Load::Ready(playlist))) = &self.lib.playlist else {
                    return Task::none();
                };
                let id = id.clone();
                let ids: Vec<String> = playlist.tracks.iter().map(|t| t.video_id.clone()).collect();
                let keep = !self.lib.offline_playlists.contains(&id);
                if keep {
                    self.lib
                        .offline_lists
                        .insert(id.clone(), playlist.tracks.clone());
                    self.lib.offline_playlists.insert(id);
                    self.lib.offline_watch = ids.clone();
                    self.lib.offline_stalled = 0;
                } else {
                    self.lib.offline_lists.remove(&id);
                    self.lib.offline_playlists.remove(&id);
                }
                if let Some(path) = self.offline_file() {
                    write_json(&path, &self.lib.offline_playlists);
                }
                let toast = self.show_toast(if keep {
                    "Téléchargement pour l'écoute hors ligne…".into()
                } else {
                    "Playlist retirée du hors ligne".into()
                });
                Task::batch([toast, self.keep_offline(ids, keep)])
            }
            Lib::DownloadAlbum(keep) => match &self.album {
                Some((_, Load::Ready(album))) => {
                    let tracks = album.tracks.clone();
                    self.update(Message::Lib(Lib::Download(tracks, keep)))
                }
                _ => Task::none(),
            },
            Lib::Download(tracks, keep) => {
                let ids: Vec<String> = tracks.iter().map(|t| t.video_id.clone()).collect();
                if keep {
                    for track in tracks {
                        if !self
                            .lib
                            .downloads
                            .iter()
                            .any(|t| t.video_id == track.video_id)
                        {
                            self.lib.downloads.insert(0, track);
                        }
                    }
                    self.lib.offline_watch = ids.clone();
                    self.lib.offline_stalled = 0;
                } else {
                    self.lib.downloads.retain(|t| !ids.contains(&t.video_id));
                }
                if let Some(path) = self.downloads_file() {
                    write_json(&path, &self.lib.downloads);
                }
                // La playlist « Téléchargés » ouverte suit.
                if matches!(&self.lib.playlist, Some((id, _)) if id == DOWNLOADS_ID) {
                    self.lib.playlist = Some((
                        DOWNLOADS_ID.to_owned(),
                        Load::Ready(self.lib.downloads_playlist()),
                    ));
                }
                let toast = self.show_toast(match (keep, ids.len()) {
                    (true, 1) => "Téléchargement…".into(),
                    (true, n) => format!("Téléchargement de {n} titres…"),
                    (false, _) => "Retiré des téléchargements".into(),
                });
                Task::batch([toast, self.keep_offline(ids, keep)])
            }
            Lib::DownloadsBySize(on) => {
                self.lib.downloads_by_size = on;
                Task::none()
            }
            Lib::RemoveOfflinePlaylist(id) => {
                self.lib.offline_playlists.remove(&id);
                let ids: Vec<String> = self
                    .lib
                    .offline_lists
                    .remove(&id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| t.video_id)
                    .collect();
                if let Some(path) = self.offline_file() {
                    write_json(&path, &self.lib.offline_playlists);
                }
                let toast = self.show_toast(format!(
                    "« {} » retirée des téléchargements",
                    self.lib.playlist_title(&id)
                ));
                Task::batch([toast, self.keep_offline(ids, false)])
            }
            Lib::RemoveDownload(video_id) => {
                let track: Vec<Track> = self
                    .lib
                    .downloads
                    .iter()
                    .filter(|t| t.video_id == video_id)
                    .cloned()
                    .collect();
                self.update(Message::Lib(Lib::Download(track, false)))
            }
            Lib::RemoveAllDownloads => {
                let mut ids: Vec<String> =
                    self.lib.downloads.drain(..).map(|t| t.video_id).collect();
                ids.extend(
                    self.lib
                        .offline_lists
                        .drain()
                        .flat_map(|(_, tracks)| tracks.into_iter().map(|t| t.video_id)),
                );
                ids.extend(self.lib.offline_ids.iter().cloned());
                self.lib.offline_playlists.clear();
                if let Some(path) = self.offline_file() {
                    write_json(&path, &self.lib.offline_playlists);
                }
                if let Some(path) = self.downloads_file() {
                    write_json(&path, &self.lib.downloads);
                }
                let toast = self.show_toast("Téléchargements supprimés".into());
                Task::batch([toast, self.keep_offline(ids, false)])
            }
            Lib::OfflineScanned(sizes) => {
                let before = self.lib.offline_ids.len();
                self.lib.offline_ids = sizes.keys().cloned().collect();
                self.lib.offline_sizes = sizes;
                // « Téléchargés » ouverte suit les fichiers qui arrivent.
                if matches!(&self.lib.playlist, Some((id, _)) if id == DOWNLOADS_ID) {
                    self.lib.playlist = Some((
                        DOWNLOADS_ID.to_owned(),
                        Load::Ready(self.lib.downloads_playlist()),
                    ));
                }
                let covers = self.want_downloads_covers();
                let missing = self
                    .lib
                    .offline_watch
                    .iter()
                    .filter(|v| !self.lib.offline_ids.contains(*v))
                    .count();
                if missing == 0 {
                    self.lib.offline_watch.clear();
                    return covers;
                }
                // Plus rien n'arrive depuis ~1 min (morceaux trop longs, indisponibles) : on arrête.
                if self.lib.offline_ids.len() == before {
                    self.lib.offline_stalled += 1;
                } else {
                    self.lib.offline_stalled = 0;
                }
                if self.lib.offline_stalled > 15 {
                    self.lib.offline_watch.clear();
                    return covers;
                }
                Task::batch([
                    covers,
                    Task::perform(tokio::time::sleep(Duration::from_secs(4)), |()| {
                        Message::Lib(Lib::OfflineTick)
                    }),
                ])
            }
            Lib::OfflineTick => self.scan_offline(),
            Lib::PlayHistory(index) => match &self.lib.history {
                Some(Load::Ready(tracks)) if index < tracks.len() => {
                    let tracks = tracks.clone();
                    self.send(Command::PlayTracks {
                        tracks,
                        start: index,
                    })
                }
                _ => Task::none(),
            },
            Lib::PlayHome(shelf, index) => {
                // Le morceau cliqué, puis ceux qui le suivent dans la rangée.
                let Some(Load::Ready(shelves)) = &self.lib.home else {
                    return Task::none();
                };
                let Some(items) = shelves.get(shelf).map(|s| &s.items) else {
                    return Task::none();
                };
                let start = items[..index.min(items.len())]
                    .iter()
                    .filter(|i| i.track.is_some())
                    .count();
                let tracks: Vec<Track> = items.iter().filter_map(|i| i.track.clone()).collect();
                if start >= tracks.len() {
                    return Task::none();
                }
                self.send(Command::PlayTracks { tracks, start })
            }
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
                self.lib.home = None;
                self.lib.home_at = None;
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
                let network = is_network_error(&e);
                self.lib.account = AccountView::Unavailable(e);
                self.lib.playlists = None;
                // YouTube injoignable (réseau) : nouvel essai seul chaque minute.
                if network && !self.lib.offline_mode {
                    Task::perform(tokio::time::sleep(Duration::from_secs(60)), |()| {
                        Message::Lib(Lib::RetryAccount)
                    })
                } else {
                    Task::none()
                }
            }
            Lib::RetryAccount => {
                if !matches!(self.lib.account, AccountView::Unavailable(_)) {
                    return Task::none();
                }
                self.lib.account = AccountView::Connecting;
                self.reconnect_task().unwrap_or_else(Task::none)
            }
            Lib::PlaylistsLoaded(result) => {
                match result {
                    Ok(mut list) => {
                        // Une playlist créée à l'instant que YouTube ne liste pas encore.
                        if let Some(Load::Ready(previous)) = &self.lib.playlists {
                            let missing: Vec<PlaylistSummary> = previous
                                .iter()
                                .filter(|p| self.lib.created.contains(&p.id))
                                .filter(|p| !list.iter().any(|q| q.id == p.id))
                                .cloned()
                                .collect();
                            for p in missing.into_iter().rev() {
                                list.insert(0, p);
                            }
                        }
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
                let Some((id, _)) = &self.lib.playlist else {
                    return Task::none();
                };
                let id = id.clone();
                let route = Route::Playlist(id.clone());
                if self.lib.created.contains(&id) {
                    // Créée à l'instant : YouTube ne la rend pas encore, elle est vide.
                    let title = self.lib.playlist_title(&id);
                    self.lib.playlist = Some((
                        id.clone(),
                        Load::Ready(Playlist {
                            id,
                            title,
                            ..empty_playlist()
                        }),
                    ));
                    return self.arrived(&route);
                }
                let failed = self.arrival_failed(&route, &e);
                match &mut self.lib.playlist {
                    // Rien à l'écran : l'erreur prend la page.
                    Some((_, load @ Load::Loading)) => {
                        *load = Load::Failed(e);
                        failed
                    }
                    _ => Task::batch([
                        failed,
                        self.show_toast(format!("Mise à jour de la playlist impossible : {e}")),
                    ]),
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
            Lib::OpenAddTo(tracks) => {
                self.lib.new_name.clear();
                self.lib.overlay = Some(Overlay::AddTo(tracks));
                let load = if self.lib.playlists.is_none() {
                    self.load_playlists()
                } else {
                    Task::none()
                };
                // Vignettes des lignes du sélecteur (petite taille : décodage léger).
                let ids: Vec<_> = match &self.lib.playlists {
                    Some(Load::Ready(list)) => list
                        .iter()
                        .map(|p| self.url_cover(p.thumbnail_url.as_deref()))
                        .collect(),
                    _ => Vec::new(),
                };
                let covers = Task::batch(ids.into_iter().map(|id| self.want_cover(id, THUMB)));
                Task::batch([
                    load,
                    covers,
                    iced::widget::operation::focus(NEW_PLAYLIST_INPUT),
                ])
            }
            // « Nouvelle playlist » : créée tout de suite (sans demander de nom)
            // et ouverte, le nom prêt à être modifié.
            Lib::OpenNew => {
                let name = NEW_PLAYLIST_NAME.to_owned();
                let op = self.lib.ready().map(|a| async move {
                    let id = a.create_playlist(&name, Privacy::Private, &[]).await?;
                    Ok((id, name))
                });
                call(op, Lib::CreatedOpen)
            }
            Lib::CreatedOpen(result) => {
                let Ok((id, title)) = result.clone() else {
                    return self.update(Message::Lib(Lib::Created(result)));
                };
                let listed = self.update(Message::Lib(Lib::Created(result)));
                let playlist = Playlist {
                    id: id.clone(),
                    title,
                    author: self
                        .lib
                        .profile
                        .as_ref()
                        .map(|p| p.name.clone())
                        .unwrap_or_default(),
                    description: None,
                    duration_text: String::new(),
                    thumbnail_url: None,
                    tracks: Vec::new(),
                    complete: true,
                    owned: true,
                };
                // En cache : la page s'ouvre sans redemander à YouTube (qui ne
                // la connaît pas encore).
                if let Some(dir) = &self.lib.cache_dir {
                    write_playlist(dir, &playlist);
                }
                self.lib.playlist = Some((id.clone(), Load::Ready(playlist)));
                let open = self.go_to(Route::Playlist(id));
                let rename = self.update(Message::Lib(Lib::RenameStart));
                Task::batch([listed, open, rename])
            }
            Lib::CloseOverlay => {
                self.lib.overlay = None;
                Task::none()
            }
            Lib::AddToPicked(playlist_id) => match self.lib.overlay.take() {
                Some(Overlay::AddTo(tracks)) => {
                    self.update_library(Lib::AddTo(playlist_id, tracks))
                }
                other => {
                    self.lib.overlay = other;
                    Task::none()
                }
            },
            Lib::AddTo(playlist_id, tracks) => {
                self.lib.overlay = None;
                if tracks.is_empty() {
                    return Task::none();
                }
                // Titres likés : « ajouter » = liker ceux qui ne le sont pas encore.
                if playlist_id == LIKED_ID {
                    let fresh: Vec<Track> = tracks
                        .into_iter()
                        .filter(|t| !self.lib.is_liked(&t.video_id))
                        .collect();
                    let count = fresh.len();
                    let mut tasks: Vec<Task<Message>> = fresh
                        .into_iter()
                        .map(|t| self.update_library(Lib::ToggleLike(t)))
                        .collect();
                    tasks.push(self.show_toast(format!("{count} titre(s) liké(s)")));
                    return Task::batch(tasks);
                }
                let title = self.lib.playlist_title(&playlist_id);
                let what = match tracks.as_slice() {
                    [one] => format!("« {} »", one.title),
                    many => format!("{} titres", many.len()),
                };
                let toast = self.show_toast(format!("{what} ajouté(s) à « {title} »"));
                let video_ids: Vec<String> = tracks.iter().map(|t| t.video_id.clone()).collect();
                let target = playlist_id.clone();
                // Affiché tout de suite : playlist ouverte, cache et compteur.
                let count = tracks.len() as i64;
                self.edit_cached(&playlist_id, |p| p.tracks.extend(tracks.iter().cloned()));
                self.bump_count(&playlist_id, count);
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.add_tracks(&target, &video_ids).await });
                Task::batch([toast, call(op, move |r| Lib::Added(playlist_id.clone(), r))])
            }
            Lib::Added(playlist_id, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => Task::batch([
                    self.show_toast(format!("Ajout refusé : {e}")),
                    self.refresh_if_open(&playlist_id),
                ]),
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
                    Some(Overlay::AddTo(tracks)) => {
                        tracks.iter().map(|t| t.video_id.clone()).collect()
                    }
                    _ => Vec::new(),
                };
                self.lib.overlay = None;
                self.lib.new_name.clear();
                let toast = self.show_toast(format!("Playlist « {name} » créée"));
                let op = self.lib.ready().map(|a| async move {
                    let id = a.create_playlist(&name, Privacy::Private, &with).await?;
                    Ok((id, name))
                });
                Task::batch([toast, call(op, Lib::Created)])
            }
            Lib::Created(result) => match result {
                Ok((id, title)) => {
                    // Affichée tout de suite ; YouTube ne la liste qu'après quelques secondes.
                    self.lib.created.insert(id.clone());
                    let summary = PlaylistSummary {
                        id,
                        title,
                        tracks_text: String::new(),
                        author: self
                            .lib
                            .profile
                            .as_ref()
                            .map(|p| p.name.clone())
                            .unwrap_or_default(),
                        thumbnail_url: None,
                    };
                    if let Some(Load::Ready(list)) = &mut self.lib.playlists {
                        list.insert(0, summary);
                        if let Some(dir) = &self.lib.cache_dir {
                            write_list(dir, list);
                        }
                    } else {
                        self.lib.playlists = Some(Load::Ready(vec![summary]));
                    }
                    Task::perform(tokio::time::sleep(Duration::from_secs(5)), |()| {
                        Message::Lib(Lib::ReloadPlaylists)
                    })
                }
                Err(e) => self.show_toast(format!("Création refusée : {e}")),
            },
            Lib::ReloadPlaylists => self.reload_playlists(),
            Lib::FilterInput(text) => {
                self.lib.filter = text;
                self.scroll_page_top()
            }
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
                    if let Some(dir) = &self.lib.cache_dir {
                        write_list(dir, list);
                        let _ = std::fs::remove_file(playlist_file(dir, &id));
                    }
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
                self.update_library(Lib::RemoveTracks(playlist_id, vec![video_id]))
            }
            Lib::RemoveTracks(playlist_id, video_ids) => {
                let mut removed = 0;
                self.edit_cached(&playlist_id, |p| {
                    let before = p.tracks.len();
                    p.tracks.retain(|t| !video_ids.contains(&t.video_id));
                    removed = before - p.tracks.len();
                });
                self.bump_count(&playlist_id, -(removed as i64));
                self.selected.retain(|v| !video_ids.contains(v));
                let target = playlist_id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.remove_tracks(&target, &video_ids).await });
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

    /// Modifie une playlist là où elle est : à l'écran si elle est ouverte,
    /// sinon dans son cache disque (sa prochaine ouverture sera juste).
    fn edit_cached(&mut self, playlist_id: &str, mut edit: impl FnMut(&mut Playlist)) {
        if let Some((id, Load::Ready(p))) = &mut self.lib.playlist
            && id == playlist_id
        {
            edit(p);
            self.save_open_playlist();
            return;
        }
        if let Some(dir) = &self.lib.cache_dir
            && let Some(mut p) = read_playlist(dir, playlist_id)
        {
            edit(&mut p);
            write_playlist(dir, &p);
        }
    }

    /// Met à jour « N titres » sur la carte de la playlist.
    fn bump_count(&mut self, playlist_id: &str, delta: i64) {
        if delta == 0 {
            return;
        }
        let Some(Load::Ready(list)) = &mut self.lib.playlists else {
            return;
        };
        let Some(p) = list.iter_mut().find(|p| p.id == playlist_id) else {
            return;
        };
        let digits: String = p.tracks_text.chars().filter(char::is_ascii_digit).collect();
        let count = digits.parse::<i64>().unwrap_or(0) + delta;
        p.tracks_text = match count.max(0) {
            1 => "1 titre".to_owned(),
            n => format!("{n} titres"),
        };
        if let Some(dir) = &self.lib.cache_dir {
            write_list(dir, list);
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
        complete: true,
        // Seules les playlists qu'on vient de créer partent de ce modèle.
        owned: true,
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

/// La playlist est en cache disque (son ouverture sera instantanée).
pub fn has_cache(lib: &LibraryState, id: &str) -> bool {
    lib.cache_dir
        .as_deref()
        .is_some_and(|dir| playlist_file(dir, id).exists())
}

/// Recherche tolérante : minuscules, sans accents.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            other => other,
        })
        .collect()
}

/// Le fichier existe et a été écrit il y a moins de `ttl`.
pub(crate) fn is_fresh(path: &Path, ttl: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ttl)
}

pub(crate) fn read_playlist(dir: &Path, id: &str) -> Option<Playlist> {
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

pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) {
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

/// Tuile « Téléchargés » de la bibliothèque (morceaux gardés un par un).
pub(crate) fn downloads_tile<'a>(app: &App) -> El<'a> {
    let tracks = app.lib.downloaded_tracks();
    let count = match tracks.len() {
        1 => "1 titre".to_owned(),
        n => format!("{n} titres"),
    };
    let hovered = app.card_hover.as_deref() == Some(DOWNLOADS_ID);
    let tile = button(
        column![
            crate::views::cover_with_play(
                app,
                downloads_art(app, &tracks, 164.0),
                164.0,
                hovered,
                Message::PlayItem(HomeKind::Playlist, DOWNLOADS_ID.to_owned()),
            ),
            text("Téléchargés")
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
    .on_press(Message::Navigate(Route::Playlist(DOWNLOADS_ID.to_owned())));
    mouse_area(tile)
        .on_enter(Message::CardHover(DOWNLOADS_ID.to_owned()))
        .on_exit(Message::CardUnhover(DOWNLOADS_ID.to_owned()))
        .into()
}

/// Image de « Téléchargés » : mosaïque 2 × 2 des premières pochettes (une seule
/// s'il y a moins de quatre albums différents, un aplat de la teinte si vide).
pub(crate) fn downloads_art<'a>(app: &App, tracks: &[Track], side: f32) -> El<'a> {
    let mut ids: Vec<String> = Vec::new();
    for track in tracks {
        if let Some(id) = app.track_cover(track)
            && !ids.contains(&id)
        {
            ids.push(id);
            if ids.len() == 4 {
                break;
            }
        }
    }
    let accent = app.accent;
    match ids.len() {
        0 => container(Space::new())
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .style(move |_| iced::widget::container::Style {
                background: Some(iced::Background::Gradient(iced::Gradient::Linear(
                    iced::gradient::Linear::new(std::f32::consts::FRAC_PI_4)
                        .add_stop(0.0, style::shade(accent, 0.25))
                        .add_stop(1.0, style::shade(accent, 0.7)),
                ))),
                border: iced::Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into(),
        1..=3 => cover(app.cover(Some(&ids[0]), CARD), side, 8.0),
        _ => {
            let half = side / 2.0;
            let tile = |id: &String| cover(app.cover(Some(id), THUMB), half, 0.0);
            container(column![
                row![tile(&ids[0]), tile(&ids[1])],
                row![tile(&ids[2]), tile(&ids[3])],
            ])
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .clip(true)
            .style(|_| iced::widget::container::Style {
                border: iced::Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
        }
    }
}

pub(crate) fn liked_tile<'a>(app: &App) -> El<'a> {
    let count = app
        .lib
        .liked_count
        .map(|n| format!("{n} titres"))
        .unwrap_or_else(|| "…".into());
    let accent = app.accent;
    let hovered = app.card_hover.as_deref() == Some(LIKED_ID);
    let tile = button(
        column![
            crate::views::cover_with_play(
                app,
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
                    })
                    .into(),
                164.0,
                hovered,
                Message::PlayItem(HomeKind::Playlist, LIKED_ID.to_owned()),
            ),
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
    .on_press(Message::Navigate(Route::Playlist(LIKED_ID.to_owned())));
    let tile = mouse_area(tile)
        .on_enter(Message::CardHover(LIKED_ID.to_owned()))
        .on_exit(Message::CardUnhover(LIKED_ID.to_owned()));
    crate::views::drop_zone(
        tile.into(),
        crate::app::DropTarget::Playlist(LIKED_ID.to_owned()),
    )
}

fn playlist_card<'a>(app: &App, p: &'a PlaylistSummary) -> El<'a> {
    let id = app.url_cover(p.thumbnail_url.as_deref());
    let subtitle = [p.tracks_text.as_str(), p.author.as_str()]
        .into_iter()
        .filter(|s| !s.trim().is_empty() && s.trim() != "•")
        .collect::<Vec<_>>()
        .join(" · ");
    let key = format!("tes:{}", p.id);
    let hovered = app.card_hover.as_deref() == Some(key.as_str());
    let card = button(
        column![
            crate::views::cover_with_play(
                app,
                cover(app.cover(id.as_deref(), CARD), 164.0, 8.0),
                164.0,
                hovered,
                Message::PlayItem(HomeKind::Playlist, p.id.clone()),
            ),
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
    .on_press(Message::Navigate(Route::Playlist(p.id.clone())));
    let card: El<'a> = mouse_area(card)
        .on_right_press(crate::context::open(crate::context::Target::Playlist(
            p.clone(),
        )))
        .on_enter(Message::CardHover(key.clone()))
        .on_exit(Message::CardUnhover(key))
        .into();
    // On peut y déposer des morceaux (playlists du compte seulement).
    if app.playlist_is_mine(p) {
        crate::views::drop_zone(card, crate::app::DropTarget::Playlist(p.id.clone()))
    } else {
        card
    }
}

/// Bouton de téléchargement d'une liste (playlist, album) : « Télécharger »,
/// puis « Téléchargement · n / m », puis pastille pleine « Téléchargée ».
/// `pinned` : la liste est demandée hors ligne.
pub(crate) fn download_pill<'a>(
    app: &App,
    tracks: &[Track],
    pinned: bool,
    on_press: Message,
) -> El<'a> {
    let available = tracks
        .iter()
        .filter(|t| app.lib.offline_ids.contains(&t.video_id))
        .count();
    let accent = app.accent;
    let playable = tracks.iter().filter(|t| !t.unavailable).count();
    // Tout est sur l'appareil : le bouton devient une pastille pleine.
    let complete = pinned && playable > 0 && available >= playable;
    let label = if complete {
        "Téléchargé".to_owned()
    } else if pinned {
        format!("Téléchargement · {available} / {playable}")
    } else {
        "Télécharger".to_owned()
    };
    // Téléchargé : pastille pleine, texte seul. Avant : petite icône.
    // Même taille que « Lecture » et « Aléatoire » à côté.
    let content: El<'a> = if complete {
        text(label).size(14).font(SEMIBOLD).into()
    } else {
        row![
            icon(
                Icon::Download,
                16.0,
                if pinned { accent } else { style::TEXT }
            ),
            text(label).size(14).font(SEMIBOLD),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    };
    button(content)
        .padding(Padding::from([10, 20]))
        .style(move |theme, status| {
            if complete {
                return style::pill(accent)(theme, status);
            }
            let mut s = style::pill_ghost(theme, status);
            if pinned {
                s.border.color = style::alpha(accent, 0.7);
            }
            s
        })
        .on_press(on_press)
        .into()
}

/// Nom d'une playlist créée par « Nouvelle playlist » (à renommer aussitôt).
const NEW_PLAYLIST_NAME: &str = "Nouvelle playlist";

/// Identifiant de la playlist « Téléchargés » (propre à ytmr).
pub const DOWNLOADS_ID: &str = "ytmr-downloads";

pub(crate) fn not_connected<'a>(app: &App) -> Option<El<'a>> {
    if app.lib.offline_mode
        || matches!(&app.lib.playlist, Some((id, _)) if id == DOWNLOADS_ID && app.route == Route::Playlist(DOWNLOADS_ID.to_owned()))
    {
        return None;
    }
    let message = match &app.lib.account {
        AccountView::Ready(_) => return None,
        AccountView::Connecting => "Connexion à ton compte YouTube…".to_owned(),
        // Réseau : pas la peine de parler des cookies (c'était trompeur).
        AccountView::Unavailable(e) if is_network_error(e) => {
            "YouTube est injoignable pour l'instant (connexion Internet). Nouvel essai automatique chaque minute, ou « Se reconnecter » en bas à gauche.".to_owned()
        }
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
        Some(name) if !liked && playlist.owned => text_input("Nom de la playlist", name)
            .id(RENAME_INPUT)
            .on_input(|n| Message::Lib(Lib::RenameInput(n)))
            .on_submit(Message::Lib(Lib::RenameCommit))
            .size(24)
            .padding(Padding::from([6, 12]))
            .style(style::search_input)
            .into(),
        _ => Space::new().height(Length::Fixed(0.0)).into(),
    };
    let pinned = app.lib.offline_playlists.contains(id);
    let offline = download_pill(
        app,
        &playlist.tracks,
        pinned,
        Message::Lib(Lib::ToggleOffline),
    );
    // Ordre : Lecture, Aléatoire, Télécharger, puis les petites icônes
    // (cœur, mise à jour, renommer, supprimer).
    let mut actions = row![icon_tip(
        Icon::Refresh,
        17.0,
        style::TEXT_DIM,
        Message::Lib(Lib::Refresh),
        "Mettre à jour depuis YouTube".into(),
    )]
    .spacing(4)
    .align_y(Alignment::Center);
    // « Téléchargés » et mode hors ligne : rien à mettre à jour depuis YouTube.
    let local = id == DOWNLOADS_ID || app.lib.offline_mode;
    let download: El<'_> = if local { Space::new().into() } else { offline };
    if local {
        actions = row![];
    }
    if !liked && playlist.owned && !app.lib.offline_mode {
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
    // Le nombre de titres (dans `details`) monte en direct pendant le chargement.
    let subtitle = column![
        text(crate::views::ellipsize(&details, 90).into_owned())
            .size(13)
            .color(style::TEXT_DIM)
            .wrapping(iced::widget::text::Wrapping::None),
        title_or_edit,
    ]
    .spacing(6);

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
        crate::views::Actions {
            play: (!playlist.tracks.is_empty()).then_some((
                Message::Lib(Lib::PlayAt(0)),
                Message::Lib(Lib::PlayShuffled),
            )),
            // Sur la même ligne que Lecture / Aléatoire : le cœur (playlist
            // d'un autre, à enregistrer), puis télécharger, mettre à jour…
            extra: Some({
                let heart: El<'_> = if !liked && !playlist.owned {
                    let saved = app.playlist_saved(id);
                    crate::collection::heart(
                        app,
                        saved,
                        Message::Coll(crate::collection::Coll::SavePlaylist(
                            PlaylistSummary {
                                id: playlist.id.clone(),
                                title: playlist.title.clone(),
                                tracks_text: format!("{} titres", playlist.tracks.len()),
                                author: playlist.author.clone(),
                                thumbnail_url: playlist.thumbnail_url.clone(),
                            },
                            !saved,
                        )),
                        if saved {
                            "Retirer de la bibliothèque"
                        } else {
                            "Ajouter à la bibliothèque"
                        },
                    )
                } else {
                    Space::new().into()
                };
                row![download, heart, actions]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            }),
        },
    );

    // Seules les lignes visibles sont construites, et le clic ne transporte
    // qu'un numéro : copier la playlist dans chaque ligne coûtait ~1,4 million
    // de copies de morceaux par image pour 1 189 titres.
    // Recherche : indices des morceaux correspondants (titre, artiste, album).
    let shown = view_order(app, playlist);
    let range = crate::views::visible_range(
        app.page_view,
        crate::views::PLAYLIST_LIST_TOP,
        crate::views::ROW_H,
        shown.len(),
    );
    let list: El<'a> = if playlist.tracks.is_empty() {
        muted("Cette playlist est vide.")
    } else if shown.is_empty() {
        muted("Aucun titre ne correspond.")
    } else {
        crate::views::virtual_list(shown.len(), crate::views::ROW_H, range, |row| {
            let i = shown[row];
            let track = &playlist.tracks[i];
            // Les playlists des autres ne se modifient pas.
            let remove = if liked {
                Some(Message::Lib(Lib::ToggleLike(track.clone())))
            } else if playlist.owned {
                Some(Message::Lib(Lib::RemoveTrack(
                    id.to_owned(),
                    track.video_id.clone(),
                )))
            } else {
                None
            };
            // `row` : rang à l'écran (sélection avec Maj) ; `i` : place dans la playlist.
            track_row(app, track, row, None, Message::Lib(Lib::PlayAt(i)), remove)
        })
    };
    column![
        // Hauteur fixe : c'est elle qui permet de savoir quelles lignes sont visibles.
        container(header)
            .height(Length::Fixed(crate::views::PLAYLIST_HEADER_H))
            .clip(true),
        container(
            row![
                text_input(
                    "Rechercher dans la playlist (titre, artiste, album)",
                    &app.lib.filter
                )
                .on_input(|t| Message::Lib(Lib::FilterInput(t)))
                .size(14)
                .padding(Padding::from([9, 14]))
                .style(style::search_input),
                sort_pills(app),
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .height(Length::Fixed(crate::views::PLAYLIST_FILTER_H))
        .padding(Padding {
            top: 4.0,
            right: 28.0,
            bottom: 8.0,
            left: 28.0,
        }),
        container(list).padding(Padding {
            top: crate::views::PLAYLIST_LIST_TOP
                - crate::views::PLAYLIST_HEADER_H
                - crate::views::PLAYLIST_FILTER_H,
            right: 28.0,
            bottom: 32.0,
            left: 28.0,
        }),
    ]
    .into()
}

/// Indices de la playlist dans l'ordre choisi (toute la playlist).
pub(crate) fn sorted_order(playlist: &Playlist, sort: PlaylistSort, reversed: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..playlist.tracks.len()).collect();
    let tracks = &playlist.tracks;
    match sort {
        PlaylistSort::Original => {}
        PlaylistSort::Title => order.sort_by_cached_key(|&i| fold(&tracks[i].title)),
        PlaylistSort::Artist => {
            order.sort_by_cached_key(|&i| (fold(&tracks[i].artist), fold(&tracks[i].title)))
        }
        PlaylistSort::Album => order.sort_by_cached_key(|&i| {
            (
                fold(tracks[i].album.as_deref().unwrap_or("")),
                fold(&tracks[i].title),
            )
        }),
        PlaylistSort::Duration => order.sort_by_key(|&i| tracks[i].duration),
    }
    if reversed {
        order.reverse();
    }
    order
}

/// Ordre affiché : trié, puis filtré par la recherche dans la playlist.
pub(crate) fn view_order(app: &App, playlist: &Playlist) -> Vec<usize> {
    let mut order = sorted_order(playlist, app.lib.sort, app.lib.sort_reversed);
    // Hors ligne : seulement ce qui est sur l'appareil.
    if app.lib.offline_mode {
        order.retain(|&i| app.lib.offline_ids.contains(&playlist.tracks[i].video_id));
    }
    let needle = fold(app.lib.filter.trim());
    if needle.is_empty() {
        return order;
    }
    order
        .into_iter()
        .filter(|&i| {
            let t = &playlist.tracks[i];
            fold(&t.title).contains(&needle)
                || fold(&t.artist).contains(&needle)
                || t.album
                    .as_deref()
                    .is_some_and(|a| fold(a).contains(&needle))
        })
        .collect()
}

/// Boutons de tri à côté de la recherche (second clic : ordre inverse).
fn sort_pills(app: &App) -> El<'_> {
    let accent = app.accent;
    let pill = |label: &'static str, sort: PlaylistSort| -> El<'_> {
        let active = app.lib.sort == sort;
        // Sens du tri écrit en clair (pas de flèches) : A–Z / Z–A, courts / longs.
        let way = match (active, sort, app.lib.sort_reversed) {
            (false, ..) | (true, PlaylistSort::Original, _) => "",
            (true, PlaylistSort::Duration, false) => " · courts",
            (true, PlaylistSort::Duration, true) => " · longs",
            (true, _, false) => " · A–Z",
            (true, _, true) => " · Z–A",
        };
        button(text(format!("{label}{way}")).size(12).font(SEMIBOLD))
            .padding(Padding::from([6, 11]))
            .style(move |theme, status| {
                if active {
                    style::pill(accent)(theme, status)
                } else {
                    style::pill_ghost(theme, status)
                }
            })
            .on_press(Message::Lib(Lib::Sort(sort)))
            .into()
    };
    row![
        pill("Ordre", PlaylistSort::Original),
        pill("Titre", PlaylistSort::Title),
        pill("Artiste", PlaylistSort::Artist),
        pill("Album", PlaylistSort::Album),
        pill("Durée", PlaylistSort::Duration),
    ]
    .spacing(4)
    .into()
}

/// Fenêtre par-dessus la page (ajout à une playlist, confirmation).
pub fn overlay(app: &App) -> Option<El<'_>> {
    let import = crate::import::panel(app);
    let importing = import.is_some();
    let panel: El<'_> = match import {
        Some(panel) => panel,
        None => match app.lib.overlay.as_ref()? {
            Overlay::AddTo(tracks) => add_to_panel(app, Some(tracks)),
            Overlay::ConfirmDelete { title, .. } => confirm_panel(title),
        },
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
    .on_press(if importing {
        Message::Import(crate::import::Imp::Close)
    } else {
        Message::Lib(Lib::CloseOverlay)
    });
    let wide = importing || matches!(app.lib.overlay, Some(Overlay::AddTo(_)));
    let centered = container(
        // Le panneau capture les clics : ils ne ferment pas la fenêtre.
        mouse_area(
            container(panel)
                .padding(20)
                .width(Length::Fixed(if wide { 560.0 } else { 400.0 }))
                .style(style::panel),
        )
        .on_press(Message::Noop),
    )
    .center(Length::Fill);
    // Opaque : survoler la fenêtre n'allume plus les morceaux de la page dessous.
    Some(iced::widget::opaque(iced::widget::stack![dim, centered]))
}

fn add_to_panel<'a>(app: &'a App, tracks: Option<&'a [Track]>) -> El<'a> {
    let title = match tracks {
        None => "Nouvelle playlist".to_owned(),
        Some([one]) => format!("Ajouter « {} » à…", one.title),
        Some(many) => format!("Ajouter {} titres à…", many.len()),
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
    if tracks.is_some() {
        let mut list = column![].spacing(2);
        match &app.lib.playlists {
            Some(Load::Ready(playlists)) => {
                for p in playlists {
                    let id = app.url_cover(p.thumbnail_url.as_deref());
                    list = list.push(
                        button(
                            row![
                                cover(app.cover(id.as_deref(), THUMB), 44.0, 6.0),
                                container(
                                    column![
                                        text(&p.title)
                                            .size(15)
                                            .font(SEMIBOLD)
                                            .wrapping(iced::widget::text::Wrapping::None),
                                        text(&p.tracks_text)
                                            .size(12)
                                            .color(style::TEXT_DIM)
                                            .wrapping(iced::widget::text::Wrapping::None),
                                    ]
                                    .spacing(2),
                                )
                                .width(Length::Fill)
                                .clip(true),
                            ]
                            .spacing(10)
                            .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .padding(Padding::from([5, 8]))
                        .style(style::row(false, app.accent))
                        // Seul l'identifiant voyage : les morceaux restent dans la fenêtre.
                        .on_press(Message::Lib(Lib::AddToPicked(p.id.clone()))),
                    );
                }
            }
            Some(Load::Failed(e)) => list = list.push(muted(e.clone())),
            _ => list = list.push(muted("Chargement…")),
        }
        content = content.push(section_title("Mes playlists")).push(
            // Défilement animé (molette), comme les autres listes : la liste
            // native sautait de 60 px par cran.
            container(crate::views::smooth_scroll(list, ADD_TO_SCROLL)).height(Length::Fixed(
                (app.window_size.height * 0.6).clamp(260.0, 680.0),
            )),
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

/// Identifiants des rangées horizontales de l'accueil (défilement animé).
const HOME_SHELVES: [&str; 16] = [
    "accueil-0",
    "accueil-1",
    "accueil-2",
    "accueil-3",
    "accueil-4",
    "accueil-5",
    "accueil-6",
    "accueil-7",
    "accueil-8",
    "accueil-9",
    "accueil-10",
    "accueil-11",
    "accueil-12",
    "accueil-13",
    "accueil-14",
    "accueil-15",
];
pub const PLAYLISTS_SHELF: &str = "rangee-playlists";
/// Rangée « Nouveautés de tes artistes ».
pub const RELEASES_SHELF: &str = "rangee-nouveautes";
/// Mosaïque « Réécouter » : 4 × 4 au plus.
const GRID_MAX: usize = 16;

/// Action d'une carte de l'accueil : ouvrir la page, ou lire le morceau.
fn home_open(item: &HomeItem, shelf: usize, index: usize) -> Message {
    match item.kind {
        HomeKind::Album => Message::Navigate(Route::Album(item.id.clone())),
        HomeKind::Playlist => Message::Navigate(Route::Playlist(item.id.clone())),
        HomeKind::Artist => Message::Navigate(Route::Artist(item.id.clone())),
        HomeKind::Song => Message::Lib(Lib::PlayHome(shelf, index)),
    }
}

/// Carte d'une rangée de l'accueil.
fn home_card<'a>(app: &App, item: &'a HomeItem, shelf: usize, index: usize) -> El<'a> {
    let id = app.url_cover(item.thumbnail_url.as_deref());
    let artist = item.kind == HomeKind::Artist;
    // Survol repéré par rangée : un même élément peut être dans deux rangées,
    // seule la carte sous la souris s'allume.
    let key = format!("{shelf}:{}", item.id);
    let hovered = app.card_hover.as_deref() == Some(key.as_str());
    let play = match item.kind {
        HomeKind::Song => home_open(item, shelf, index),
        kind => Message::PlayItem(kind, item.id.clone()),
    };
    let body = column![
        crate::views::cover_with_play(
            app,
            cover(
                app.cover(id.as_deref(), CARD),
                150.0,
                if artist { 75.0 } else { 8.0 }
            ),
            150.0,
            hovered,
            play,
        ),
        text(crate::views::ellipsize(&item.title, 19))
            .size(14)
            .font(SEMIBOLD)
            .wrapping(iced::widget::text::Wrapping::None),
        text(crate::views::ellipsize(&item.subtitle, 24))
            .size(12)
            .color(style::TEXT_DIM)
            .wrapping(iced::widget::text::Wrapping::None),
    ]
    .spacing(6)
    .width(Length::Fixed(150.0))
    .clip(true);
    match &item.track {
        // Morceau : double-clic pour le lancer (un clic simple ne lance rien).
        Some(track) => mouse_area(container(body).padding(8).style(style::card_box(hovered)))
            .on_double_click(home_open(item, shelf, index))
            .on_right_press(crate::context::open(home_target(item)))
            // Survol de la carte, et préchargement du morceau comme sur une ligne.
            .on_enter(Message::Batch(vec![
                Message::CardHover(key.clone()),
                Message::Hover(track.video_id.clone()),
            ]))
            .on_exit(Message::Batch(vec![
                Message::CardUnhover(key),
                Message::Unhover(track.video_id.clone()),
            ]))
            .interaction(iced::mouse::Interaction::Pointer)
            .into(),
        None => mouse_area(
            button(body)
                .padding(8)
                .style(style::card)
                .on_press(home_open(item, shelf, index)),
        )
        .on_right_press(crate::context::open(home_target(item)))
        .on_enter(Message::CardHover(key.clone()))
        .on_exit(Message::CardUnhover(key))
        .into(),
    }
}

/// Cible du menu contextuel pour une carte de l'accueil.
fn home_target(item: &HomeItem) -> crate::context::Target {
    use crate::context::Target;
    match (&item.track, item.kind) {
        (Some(track), _) => Target::Track(track.clone()),
        (None, HomeKind::Album) => Target::Album(ytmr_catalog::AlbumSummary {
            id: item.id.clone(),
            title: item.title.clone(),
            artist: item.subtitle.clone(),
            year: String::new(),
            kind: String::new(),
            thumbnail_url: item.thumbnail_url.clone(),
        }),
        (None, HomeKind::Artist) => Target::Artist(ytmr_catalog::ArtistSummary {
            id: item.id.clone(),
            name: item.title.clone(),
            subscribers: None,
            thumbnail_url: item.thumbnail_url.clone(),
        }),
        (None, _) => Target::Playlist(PlaylistSummary {
            id: item.id.clone(),
            title: item.title.clone(),
            tracks_text: String::new(),
            author: item.subtitle.clone(),
            thumbnail_url: item.thumbnail_url.clone(),
        }),
    }
}

/// Page « Historique » : tout ce que YouTube garde des écoutes récentes.
pub fn history_page(app: &App) -> El<'_> {
    let header = row![
        text("Historique").size(30).font(BOLD).width(Length::Fill),
        icon_tip(
            Icon::Refresh,
            17.0,
            style::TEXT_DIM,
            Message::Navigate(Route::History),
            "Actualiser".into(),
        ),
    ]
    .align_y(Alignment::Center);
    let body: El<'_> =
        if let Some(info) = not_connected(app) {
            info
        } else {
            match &app.lib.history {
                None | Some(Load::Loading) => loading("Chargement de l'historique…"),
                Some(Load::Failed(e)) => error_box(e),
                Some(Load::Ready(tracks)) if tracks.is_empty() => muted("Rien écouté récemment."),
                Some(Load::Ready(tracks)) => column(tracks.iter().enumerate().map(|(i, t)| {
                    track_row(app, t, i, None, Message::Lib(Lib::PlayHistory(i)), None)
                }))
                .into(),
            }
        };
    page_scroll(column![header, body].spacing(20).padding(Padding {
        top: 28.0,
        right: 28.0,
        bottom: 32.0,
        left: 28.0,
    }))
}

/// Case de la mosaïque « Réécouter » : petite pochette et titre.
fn grid_tile<'a>(app: &App, item: &'a HomeItem, shelf: usize, index: usize) -> El<'a> {
    let id = app.url_cover(item.thumbnail_url.as_deref());
    let radius = if item.kind == HomeKind::Artist {
        28.0
    } else {
        6.0
    };
    let key = format!("grille:{}", item.id);
    let hovered = app.card_hover.as_deref() == Some(key.as_str());
    let play: El<'a> = if hovered {
        container(crate::views::play_bubble(
            app,
            match item.kind {
                HomeKind::Song => home_open(item, shelf, index),
                kind => Message::PlayItem(kind, item.id.clone()),
            },
        ))
        .padding(Padding::from([0, 8]))
        .into()
    } else {
        Space::new().into()
    };
    let tile = button(
        row![
            // Taille « carte » : c'est celle que l'accueil demande pour toutes ses
            // pochettes (en vignette, rien ne la demandait : cases vides).
            cover(app.cover(id.as_deref(), CARD), 56.0, radius),
            container(
                text(&item.title)
                    .size(14)
                    .font(SEMIBOLD)
                    .wrapping(iced::widget::text::Wrapping::None),
            )
            .width(Length::Fill)
            .clip(true),
            play,
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(Padding {
        top: 0.0,
        right: 4.0,
        bottom: 0.0,
        left: 0.0,
    })
    .style(style::grid_tile(app.accent))
    .on_press(home_open(item, shelf, index));
    mouse_area(tile)
        .on_enter(Message::CardHover(key.clone()))
        .on_exit(Message::CardUnhover(key))
        .into()
}

fn home_grid<'a>(app: &App, shelf: usize, items: &'a [HomeItem]) -> El<'a> {
    // Albums, playlists, EP et artistes : les morceaux seuls font doublon avec l'historique.
    let picked: Vec<(usize, &HomeItem)> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.kind != HomeKind::Song)
        .take(GRID_MAX)
        .collect();
    let columns = if app.center_width() < 760.0 { 2 } else { 4 };
    let mut grid = column![].spacing(8);
    for line in picked.chunks(columns) {
        let mut r = row![].spacing(8);
        for (index, item) in line {
            r = r.push(grid_tile(app, item, shelf, *index));
        }
        // Dernière ligne incomplète : les cases gardent la même largeur.
        for _ in line.len()..columns {
            r = r.push(Space::new().width(Length::Fill));
        }
        grid = grid.push(r);
    }
    grid.into()
}

/// Rangée horizontale ; Shift + molette la fait glisser en douceur.
fn shelf<'a>(items: Vec<El<'a>>, id: &'static str) -> El<'a> {
    crate::smooth::wheel_horizontal(
        scrollable(row(items).spacing(4).padding(Padding {
            bottom: 12.0,
            ..Padding::ZERO
        }))
        .id(id)
        .on_scroll(move |viewport| Message::ShelfScrolled(id, viewport))
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(4).scroller_width(4),
        ))
        .style(style::scroll),
        move |lines| Message::WheelH(id, lines),
    )
    .into()
}

/// La rangée « Réécouter » de YouTube devient la mosaïque du haut.
fn is_listen_again(shelf: &HomeShelf) -> bool {
    let t = shelf.title.to_lowercase();
    t.starts_with("réécouter") || t.starts_with("listen again")
}

/// Sections de l'accueil liées au compte : mosaïque « Réécouter », playlists,
/// puis les rangées de YouTube Music (mix, nouveautés, recommandations).
pub fn home_sections(app: &App) -> Option<El<'_>> {
    app.lib.ready()?;
    let mut page = column![].spacing(28);
    let shelves: &[HomeShelf] = match &app.lib.home {
        Some(Load::Ready(shelves)) => shelves,
        Some(Load::Loading) | None => {
            page = page.push(loading("Chargement de l'accueil…"));
            &[]
        }
        Some(Load::Failed(_)) => &[],
    };
    if let Some((index, top)) = shelves.iter().enumerate().find(|(_, s)| is_listen_again(s))
        && top.items.iter().any(|i| i.kind != HomeKind::Song)
    {
        page = page.push(
            column![section_title(&top.title), home_grid(app, index, &top.items)].spacing(12),
        );
    }
    if let Some(Load::Ready(list)) = &app.lib.playlists {
        let mut tiles = vec![liked_tile(app)];
        tiles.extend(list.iter().map(|p| playlist_card(app, p)));
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
                shelf(tiles, PLAYLISTS_SHELF),
            ]
            .spacing(10),
        );
    }
    if !app.releases.is_empty() {
        page = page.push(
            column![
                section_title("Nouveautés de tes artistes"),
                shelf(
                    app.releases
                        .iter()
                        .map(|a| crate::views::album_card(app, a))
                        .collect(),
                    RELEASES_SHELF
                ),
            ]
            .spacing(10),
        );
    }
    let rows = shelves
        .iter()
        .enumerate()
        .filter(|(_, s)| !is_listen_again(s))
        .zip(HOME_SHELVES);
    for ((index, s), id) in rows {
        page = page.push(
            column![
                section_title(&s.title),
                shelf(
                    s.items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| home_card(app, item, index, i))
                        .collect(),
                    id
                ),
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
                button(
                    row![
                        cover(app.cover(photo.as_deref(), THUMB), 34.0, 17.0),
                        // Une ligne chacun, coupée : la barre latérale peut être étroite.
                        container(
                            column![
                                text(name)
                                    .size(13)
                                    .font(SEMIBOLD)
                                    .wrapping(iced::widget::text::Wrapping::None),
                                text(handle)
                                    .size(11)
                                    .color(style::TEXT_DIM)
                                    .wrapping(iced::widget::text::Wrapping::None),
                            ]
                            .spacing(1),
                        )
                        .width(Length::Fill)
                        .clip(true),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding(Padding::from([4, 6]))
                .style(style::nav(app.route == Route::History, app.accent))
                .on_press(Message::Navigate(Route::History)),
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
