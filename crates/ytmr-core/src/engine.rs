//! Moteur de lecture : la seule source de vérité de ytmr.
//!
//! En clair : une tâche unique reçoit les ordres (« joue ça », « suivant »,
//! « mélange »…), écoute ce que raconte le lecteur (« morceau fini ») et tient
//! à jour la file et l'état. Tout le monde (CLI, API, interface) passe par un
//! [`CoreHandle`] : on envoie des [`Command`] et on lit l'état publié.
//!
//! Préchargement : dès qu'un morceau démarre, le suivant est résolu en qualité
//! haute en tâche de fond, puis confié au lecteur, qui l'enchaîne sans blanc.
//! Un démarrage « à froid » (premier morceau, saut vers un morceau non
//! préchargé) utilise le mode rapide pour que le son sorte au plus vite.
//!
//! Radio : quand il reste moins de deux morceaux, des titres similaires au
//! morceau courant sont ajoutés à la fin (si l'autoplay est actif).

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
// Horloge de tokio : figeable dans les tests.
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use ytmr_player::{
    AudioEffects, EndReason, EntryId, MediaSource, Player, PlayerError, PlayerEvent, Priority,
    Resolver, StartMode, StreamQuality,
};

use crate::Track;
use crate::queue::{Queue, QueueId, QueuedTrack, Removed, Repeat};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Source de morceaux similaires, pour l'autoplay.
pub trait Radio: Send + Sync + 'static {
    fn radio(&self, video_id: &str) -> impl Future<Output = Result<Vec<Track>, BoxError>> + Send;
}

/// En dessous, « précédent » revient au morceau d'avant ; au-dessus, au début du morceau.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);
/// Erreurs de lecture consécutives tolérées avant de s'arrêter.
const MAX_CONSECUTIVE_ERRORS: u32 = 3;
/// Autoplay : nombre de morceaux restants qui déclenche la radio.
const RADIO_LOW_WATER: usize = 2;
const RADIO_MAX_ADD: usize = 25;
/// Un préchargement en qualité prend ~9 s (résolution + délai imposé par
/// YouTube), un démarrage rapide ~3 s. Au-delà de ce temps écoulé, attendre le
/// préchargement en cours est plus rapide (et en meilleure qualité).
const PRELOAD_WAIT_THRESHOLD: Duration = Duration::from_secs(5);
/// Résolutions d'anticipation menées en même temps (file « de fond » du résolveur).
const MAX_WARMING: usize = 2;
/// Morceaux à venir résolus d'avance : on peut les enchaîner ou les sauter sans attendre.
const LOOKAHEAD: usize = 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    /// Résolution du flux ou ouverture en cours.
    Loading,
    Playing,
    Paused,
}

/// État publié en continu (via `watch`) : ce qu'une interface affiche.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybackState {
    pub current: Option<QueuedTrack>,
    pub status: Status,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub autoplay: bool,
    pub quality: Option<StreamQuality>,
    /// Le morceau suivant est déjà chargé dans le lecteur (enchaînement sans blanc).
    pub next_ready: bool,
    /// Égaliseur et volume homogène.
    pub effects: AudioEffects,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub entries: Vec<QueuedTrack>,
    pub current: Option<usize>,
}

/// Événements ponctuels, en plus de l'état continu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    TrackChanged(Option<QueuedTrack>),
    QueueChanged,
    /// Problème à montrer à l'utilisateur (morceau illisible, mpv arrêté…).
    Error(String),
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Remplace la file et lance `tracks[start]`.
    PlayTracks {
        tracks: Vec<Track>,
        start: usize,
    },
    /// Remplace la file par `tracks` dans un ordre aléatoire (aléatoire activé).
    PlayShuffled(Vec<Track>),
    /// Ajoute à la fin (lance la lecture si rien n'est en cours).
    Enqueue(Vec<Track>),
    /// Insère après le morceau courant.
    PlayNext(Vec<Track>),
    JumpTo(QueueId),
    Remove(QueueId),
    Move {
        id: QueueId,
        to: usize,
    },
    ClearUpcoming,
    TogglePause,
    Play,
    Pause,
    Next,
    Previous,
    Seek(Duration),
    SetVolume(u8),
    SetShuffle(bool),
    SetRepeat(Repeat),
    SetAutoplay(bool),
    /// Égaliseur, volume homogène (bornés par le moteur).
    SetEffects(AudioEffects),
    /// Indice : ce morceau risque d'être lancé bientôt (survol, page ouverte).
    /// Le résout en avance pour un démarrage quasi instantané ; sans effet sur la lecture.
    Prefetch(String),
    /// Arrête le lecteur et le moteur.
    Shutdown,
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("ce morceau n'est pas dans la file")]
    NotInQueue,
    #[error("rien à jouer")]
    NothingToPlay,
    #[error("valeur invalide : {0}")]
    Invalid(String),
    #[error(transparent)]
    Player(#[from] PlayerError),
    #[error("le moteur de lecture est arrêté")]
    Stopped,
}

pub struct CoreOptions {
    pub volume: u8,
    pub autoplay: bool,
    /// Fichier où garder la file et la position entre deux lancements.
    pub session_file: Option<PathBuf>,
}

/// File, position et réglages sauvegardés à la fermeture, repris au lancement suivant.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Session {
    tracks: Vec<Track>,
    current: Option<usize>,
    position_ms: u64,
    volume: Option<u8>,
    repeat: Option<Repeat>,
    autoplay: Option<bool>,
    #[serde(default)]
    effects: Option<AudioEffects>,
    #[serde(default)]
    shuffle: Option<bool>,
}

impl Session {
    fn load(path: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text)
            .inspect_err(|e| tracing::warn!(error = %e, "session illisible, ignorée"))
            .ok()
    }

    fn save(&self, path: &std::path::Path) {
        let result = serde_json::to_string(self)
            .map_err(std::io::Error::other)
            .and_then(|json| {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, json)
            });
        if let Err(e) = result {
            tracing::warn!(error = %e, "sauvegarde de la session");
        }
    }
}

type Request = (Command, oneshot::Sender<Result<(), CoreError>>);

/// Télécommande du moteur : clonable, à donner à la CLI, à l'API, à l'interface.
#[derive(Clone)]
pub struct CoreHandle {
    commands: mpsc::Sender<Request>,
    state: watch::Receiver<PlaybackState>,
    queue: watch::Receiver<Arc<QueueSnapshot>>,
    events: broadcast::Sender<CoreEvent>,
}

impl CoreHandle {
    pub async fn send(&self, command: Command) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send((command, tx))
            .await
            .map_err(|_| CoreError::Stopped)?;
        rx.await.map_err(|_| CoreError::Stopped)?
    }

    pub fn state(&self) -> PlaybackState {
        self.state.borrow().clone()
    }

    pub fn watch_state(&self) -> watch::Receiver<PlaybackState> {
        self.state.clone()
    }

    pub fn queue(&self) -> Arc<QueueSnapshot> {
        Arc::clone(&self.queue.borrow())
    }

    pub fn watch_queue(&self) -> watch::Receiver<Arc<QueueSnapshot>> {
        self.queue.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }
}

/// Démarre le moteur. La tâche rendue se termine après [`Command::Shutdown`].
pub fn spawn<P: Player, R: Resolver, C: Radio>(
    player: P,
    resolver: Arc<R>,
    radio: Arc<C>,
    options: CoreOptions,
) -> (CoreHandle, JoinHandle<()>) {
    spawn_with_queue(player, resolver, radio, options, Queue::default())
}

fn spawn_with_queue<P: Player, R: Resolver, C: Radio>(
    player: P,
    resolver: Arc<R>,
    radio: Arc<C>,
    options: CoreOptions,
    mut queue: Queue,
) -> (CoreHandle, JoinHandle<()>) {
    let mut state = PlaybackState {
        volume: options.volume,
        autoplay: options.autoplay,
        ..PlaybackState::default()
    };
    // Reprise : réglages, puis file telle qu'à la fermeture, en pause sur le même morceau.
    let mut resume_at = None;
    let mut restored_volume = None;
    if let Some(session) = options.session_file.as_deref().and_then(Session::load) {
        if let Some(volume) = session.volume.filter(|v| *v <= 100) {
            state.volume = volume;
            restored_volume = Some(volume);
        }
        if let Some(autoplay) = session.autoplay {
            state.autoplay = autoplay;
        }
        if let Some(effects) = session.effects {
            state.effects = effects.clamped();
        }
        queue.set_repeat(session.repeat.unwrap_or_default());
        if let Some(id) = queue.restore(
            session.tracks,
            session.current.unwrap_or(0),
            session.shuffle.unwrap_or(false),
        ) {
            let position = Duration::from_millis(session.position_ms);
            state.position = position;
            state.duration = queue.current().and_then(|e| e.track.duration);
            resume_at = Some((id, position));
            tracing::info!(tracks = queue.entries().len(), "session reprise");
        }
    }
    let (commands_tx, commands_rx) = mpsc::channel(64);
    let (state_tx, state_rx) = watch::channel(state.clone());
    let (queue_tx, queue_rx) = watch::channel(Arc::new(QueueSnapshot::default()));
    let (events, _) = broadcast::channel(64);

    let engine = Engine {
        player_events: player.subscribe(),
        player,
        resolver,
        radio,
        queue,
        state,
        state_tx,
        queue_tx,
        events: events.clone(),
        queue_dirty: false,
        published_current: None,
        tasks: JoinSet::new(),
        current_entry: None,
        pending: None,
        preloading: None,
        awaiting_preload: None,
        load_generation: 0,
        paused: false,
        radio_inflight: false,
        radio_seed: None,
        radio_used: HashSet::new(),
        radio_retries: 0,
        warming: HashSet::new(),
        warm_wanted: VecDeque::new(),
        resume_at,
        restored_volume,
        session_file: options.session_file,
        consecutive_errors: 0,
        player_alive: true,
    };
    let task = tokio::spawn(engine.run(commands_rx));
    let handle = CoreHandle {
        commands: commands_tx,
        state: state_rx,
        queue: queue_rx,
        events,
    };
    (handle, task)
}

/// Morceau suivant déjà confié au lecteur.
struct Pending {
    id: QueueId,
    entry: EntryId,
    quality: StreamQuality,
}

enum TaskDone {
    /// Flux résolu pour un démarrage (périmé si `generation` a changé).
    Start {
        generation: u64,
        id: QueueId,
        result: Result<MediaSource, PlayerError>,
    },
    Preload {
        id: QueueId,
        result: Result<MediaSource, PlayerError>,
    },
    Radio {
        result: Result<Vec<Track>, BoxError>,
    },
    Warmed {
        video_id: String,
    },
}

struct Engine<P, R, C> {
    player: P,
    player_events: broadcast::Receiver<PlayerEvent>,
    resolver: Arc<R>,
    radio: Arc<C>,
    queue: Queue,
    state: PlaybackState,
    state_tx: watch::Sender<PlaybackState>,
    queue_tx: watch::Sender<Arc<QueueSnapshot>>,
    events: broadcast::Sender<CoreEvent>,
    queue_dirty: bool,
    published_current: Option<QueueId>,
    tasks: JoinSet<TaskDone>,
    /// Entrée du lecteur qui correspond au morceau courant.
    current_entry: Option<EntryId>,
    pending: Option<Pending>,
    /// Résolution de préchargement en cours, et depuis quand.
    preloading: Option<(QueueId, Instant)>,
    /// Démarrage en attente du préchargement en cours (génération de chargement).
    awaiting_preload: Option<u64>,
    load_generation: u64,
    paused: bool,
    radio_inflight: bool,
    /// Dernier morceau ayant servi de graine à la radio (évite de redemander en boucle).
    radio_seed: Option<QueueId>,
    /// Morceaux déjà utilisés comme graine (`video_id`).
    radio_used: HashSet<String>,
    /// Essais avec une autre graine quand la radio n'apporte que des doublons.
    radio_retries: u8,
    /// Morceaux en cours de résolution d'anticipation.
    warming: HashSet<String>,
    /// Morceaux à résoudre d'avance, par ordre de priorité.
    warm_wanted: VecDeque<String>,
    /// Position à rejoindre au premier démarrage du morceau repris.
    resume_at: Option<(QueueId, Duration)>,
    /// Volume de la session précédente, à transmettre au lecteur au démarrage.
    restored_volume: Option<u8>,
    session_file: Option<PathBuf>,
    consecutive_errors: u32,
    player_alive: bool,
}

impl<P: Player, R: Resolver, C: Radio> Engine<P, R, C> {
    async fn run(mut self, mut commands: mpsc::Receiver<Request>) {
        // Session reprise : volume, file, et préparation des premiers morceaux.
        if let Some(volume) = self.restored_volume.take()
            && let Err(e) = self.player.set_volume(volume).await
        {
            tracing::warn!(error = %e, "volume de la session précédente");
        }
        if self.state.effects != AudioEffects::default()
            && let Err(e) = self.player.set_effects(&self.state.effects).await
        {
            tracing::warn!(error = %e, "effets audio de la session précédente");
        }
        self.queue_dirty = self.queue.current().is_some();
        self.refresh_lookahead();
        self.publish();
        loop {
            tokio::select! {
                request = commands.recv() => {
                    let Some((command, reply)) = request else {
                        // Plus aucune télécommande : on s'arrête.
                        self.save_session();
                        self.player.shutdown().await;
                        return;
                    };
                    if matches!(command, Command::Shutdown) {
                        self.save_session();
                        self.player.shutdown().await;
                        let _ = reply.send(Ok(()));
                        return;
                    }
                    let result = self.handle_command(command).await;
                    let _ = reply.send(result);
                }
                event = self.player_events.recv(), if self.player_alive => match event {
                    Ok(event) => self.on_player_event(event).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(n, "événements du lecteur perdus");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        self.on_player_event(PlayerEvent::Exited).await;
                    }
                },
                Some(done) = self.tasks.join_next(), if !self.tasks.is_empty() => match done {
                    Ok(done) => self.on_task(done).await,
                    Err(e) => tracing::error!(error = %e, "tâche de fond interrompue"),
                },
            }
            self.publish();
        }
    }

    // ----- commandes -----

    async fn handle_command(&mut self, command: Command) -> Result<(), CoreError> {
        tracing::debug!(?command, "commande");
        // Une action de l'utilisateur relance le compteur d'erreurs.
        self.consecutive_errors = 0;
        match command {
            Command::PlayTracks { tracks, start } => {
                let id = self
                    .queue
                    .replace(tracks, start)
                    .ok_or(CoreError::NothingToPlay)?;
                self.queue_dirty = true;
                self.start(id).await
            }
            Command::PlayShuffled(tracks) => {
                let id = self
                    .queue
                    .replace_shuffled(tracks)
                    .ok_or(CoreError::NothingToPlay)?;
                self.queue_dirty = true;
                self.start(id).await
            }
            Command::Enqueue(tracks) => {
                let ids = self.queue.append(tracks);
                self.after_insert(ids).await
            }
            Command::PlayNext(tracks) => {
                let ids = self.queue.insert_next(tracks);
                self.after_insert(ids).await
            }
            Command::JumpTo(id) => {
                self.queue.get(id).ok_or(CoreError::NotInQueue)?;
                self.start(id).await
            }
            Command::Remove(id) => self.remove(id).await,
            Command::Move { id, to } => {
                if !self.queue.move_to(id, to) {
                    return Err(CoreError::NotInQueue);
                }
                self.queue_dirty = true;
                self.refresh_preload().await;
                Ok(())
            }
            Command::ClearUpcoming => {
                self.queue.clear_upcoming();
                self.queue_dirty = true;
                self.refresh_preload().await;
                Ok(())
            }
            Command::TogglePause => {
                // Rien ne joue (arrêt, session reprise) : le bouton lance la lecture.
                let pause = matches!(self.state.status, Status::Playing | Status::Loading);
                self.set_paused(pause).await
            }
            Command::Play => self.set_paused(false).await,
            Command::Pause => self.set_paused(true).await,
            Command::Next => {
                let id = self.queue.next_manual().ok_or(CoreError::NothingToPlay)?;
                self.start(id).await
            }
            Command::Previous => {
                if self.current_entry.is_some() && self.state.position > RESTART_THRESHOLD {
                    return self.seek(Duration::ZERO).await;
                }
                match self.queue.prepare_previous() {
                    Some(id) => self.start(id).await,
                    None => self.seek(Duration::ZERO).await,
                }
            }
            Command::Seek(position) => self.seek(position).await,
            Command::SetVolume(volume) => {
                if volume > 100 {
                    return Err(CoreError::Invalid(format!("volume {volume} (0 à 100)")));
                }
                self.player.set_volume(volume).await?;
                self.state.volume = volume;
                Ok(())
            }
            Command::SetShuffle(on) => {
                self.queue.set_shuffle(on);
                self.queue_dirty = true;
                self.refresh_preload().await;
                Ok(())
            }
            Command::SetRepeat(repeat) => {
                self.queue.set_repeat(repeat);
                self.refresh_preload().await;
                Ok(())
            }
            Command::SetAutoplay(on) => {
                self.state.autoplay = on;
                if on {
                    // Remise en route : la radio repart du morceau courant.
                    self.radio_seed = None;
                    self.radio_retries = 0;
                }
                self.refresh_preload().await;
                Ok(())
            }
            Command::SetEffects(effects) => {
                let effects = effects.clamped();
                self.player.set_effects(&effects).await?;
                self.state.effects = effects;
                Ok(())
            }
            Command::Prefetch(video_id) => {
                self.warm(video_id);
                Ok(())
            }
            Command::Shutdown => Ok(()),
        }
    }

    /// Indice explicite (survol, page ouverte) : passe devant l'anticipation.
    fn warm(&mut self, video_id: String) {
        self.warm_wanted.retain(|v| *v != video_id);
        self.warm_wanted.push_front(video_id);
        self.pump_warm();
    }

    fn is_ready(&self, video_id: &str) -> bool {
        self.resolver
            .cached(video_id)
            .is_some_and(|s| s.quality == StreamQuality::High)
    }

    /// Recalcule les morceaux à préparer : le courant s'il ne joue pas encore,
    /// puis les [`LOOKAHEAD`] suivants (le tout prochain est géré par le préchargement).
    fn refresh_lookahead(&mut self) {
        let next = self.queue.peek_auto_next().map(|e| e.id);
        // Le courant seulement s'il est à l'arrêt (session reprise) : pendant un
        // démarrage, sa résolution urgente est déjà lancée et ne doit pas se
        // retrouver à attendre dans la file de fond.
        let current = self
            .queue
            .current()
            .filter(|_| self.current_entry.is_none() && self.state.status == Status::Idle)
            .map(|e| e.track.video_id.clone());
        let upcoming = self
            .queue
            .upcoming()
            .iter()
            .filter(|e| Some(e.id) != next)
            .take(LOOKAHEAD)
            .map(|e| e.track.video_id.clone());
        let wanted: Vec<String> = current
            .into_iter()
            .chain(upcoming)
            .filter(|v| !self.warming.contains(v) && !self.is_ready(v))
            .collect();
        // Les indices explicites encore utiles restent en tête.
        let hints: Vec<String> = self
            .warm_wanted
            .iter()
            .filter(|v| !wanted.contains(v))
            .take(2)
            .cloned()
            .collect();
        self.warm_wanted = hints.into_iter().chain(wanted).collect();
        self.pump_warm();
    }

    fn pump_warm(&mut self) {
        while self.warming.len() < MAX_WARMING {
            let Some(video_id) = self.warm_wanted.pop_front() else {
                return;
            };
            if self.warming.contains(&video_id) || self.is_ready(&video_id) {
                continue;
            }
            self.warming.insert(video_id.clone());
            let resolver = Arc::clone(&self.resolver);
            self.tasks.spawn(async move {
                let result = resolver
                    .resolve(&video_id, StartMode::Quality, Priority::Background)
                    .await;
                if let Err(e) = result {
                    tracing::debug!(error = %e, video_id, "anticipation échouée");
                }
                TaskDone::Warmed { video_id }
            });
        }
    }

    fn save_session(&self) {
        let Some(path) = &self.session_file else {
            return;
        };
        // Pas la peine de garder des milliers de morceaux de radio.
        let start = self.queue.current_index().unwrap_or(0).saturating_sub(50);
        let tracks: Vec<Track> = self
            .queue
            .entries()
            .iter()
            .skip(start)
            .take(500)
            .map(|e| e.track.clone())
            .collect();
        Session {
            current: self.queue.current_index().map(|c| c - start),
            position_ms: u64::try_from(self.state.position.as_millis()).unwrap_or(0),
            tracks,
            volume: Some(self.state.volume),
            repeat: Some(self.queue.repeat()),
            autoplay: Some(self.state.autoplay),
            effects: Some(self.state.effects),
            shuffle: Some(self.queue.shuffle()),
        }
        .save(path);
    }

    async fn after_insert(&mut self, ids: Vec<QueueId>) -> Result<(), CoreError> {
        let first = *ids.first().ok_or(CoreError::NothingToPlay)?;
        self.queue_dirty = true;
        if self.queue.current().is_none() || self.state.status == Status::Idle {
            return self.start(first).await;
        }
        self.refresh_preload().await;
        Ok(())
    }

    async fn remove(&mut self, id: QueueId) -> Result<(), CoreError> {
        let removing_current = self.queue.current().is_some_and(|e| e.id == id);
        let next = removing_current
            .then(|| self.queue.next_manual())
            .flatten()
            .filter(|&n| n != id);
        if self.queue.remove(id) == Removed::NotFound {
            return Err(CoreError::NotInQueue);
        }
        self.queue_dirty = true;
        if !removing_current {
            self.refresh_preload().await;
            return Ok(());
        }
        match next {
            Some(next) => self.start(next).await,
            None => {
                self.stop_playback().await;
                Ok(())
            }
        }
    }

    async fn set_paused(&mut self, pause: bool) -> Result<(), CoreError> {
        if self.current_entry.is_none() {
            if pause {
                return Ok(());
            }
            // Rien ne joue : « play » relance le morceau courant.
            let id = self
                .queue
                .current()
                .map(|e| e.id)
                .ok_or(CoreError::NothingToPlay)?;
            return self.start(id).await;
        }
        self.player.set_paused(pause).await?;
        Ok(())
    }

    async fn seek(&mut self, position: Duration) -> Result<(), CoreError> {
        if self.current_entry.is_none() {
            return Err(CoreError::NothingToPlay);
        }
        if let Some(duration) = self.state.duration
            && position > duration
        {
            return Err(CoreError::Invalid(format!(
                "position {}s au-delà de la durée {}s",
                position.as_secs(),
                duration.as_secs()
            )));
        }
        self.player.seek(position).await?;
        self.state.position = position;
        Ok(())
    }

    // ----- démarrage d'un morceau -----

    /// Rend `id` courant et le fait jouer, en profitant du préchargement si possible.
    async fn start(&mut self, id: QueueId) -> Result<(), CoreError> {
        let track = self
            .queue
            .get(id)
            .ok_or(CoreError::NotInQueue)?
            .track
            .clone();
        self.queue.set_current(id);
        self.queue_dirty = true;
        self.reset_track_state(&track);
        if self.resume_at.is_some_and(|(r, _)| r != id) {
            self.resume_at = None;
        }
        // File presque vide : la radio peut se préparer pendant le chargement.
        self.maybe_fetch_radio();
        self.refresh_lookahead();

        if let Some(pending) = self.pending.take_if(|p| p.id == id) {
            // Déjà chargé dans le lecteur : passage instantané.
            self.player.skip_to_pending().await?;
            self.current_entry = Some(pending.entry);
            self.state.quality = Some(pending.quality);
            self.state.next_ready = false;
            return Ok(());
        }

        self.pending = None;
        self.state.next_ready = false;
        self.load_generation += 1;
        if self.current_entry.take().is_some() {
            // Coupe l'ancien morceau pendant qu'on prépare le nouveau.
            self.player.stop().await?;
        }

        if let Some(source) = self.resolver.cached(&track.video_id) {
            return self.load_source(&source).await;
        }
        if self
            .preloading
            .is_some_and(|(p, since)| p == id && since.elapsed() >= PRELOAD_WAIT_THRESHOLD)
        {
            tracing::debug!(?id, "démarrage : attente du préchargement déjà avancé");
            self.awaiting_preload = Some(self.load_generation);
            return Ok(());
        }
        self.spawn_start(id, track.video_id);
        Ok(())
    }

    /// Résolution rapide pour un démarrage à froid.
    fn spawn_start(&mut self, id: QueueId, video_id: String) {
        let resolver = Arc::clone(&self.resolver);
        let generation = self.load_generation;
        self.tasks.spawn(async move {
            let result = resolver
                .resolve(&video_id, StartMode::Fast, Priority::Urgent)
                .await;
            TaskDone::Start {
                generation,
                id,
                result,
            }
        });
    }

    fn reset_track_state(&mut self, track: &Track) {
        self.state.status = Status::Loading;
        self.state.position = Duration::ZERO;
        self.state.duration = track.duration;
        self.state.quality = None;
    }

    async fn load_source(&mut self, source: &MediaSource) -> Result<(), CoreError> {
        let entry = self.player.load(source).await?;
        self.current_entry = Some(entry);
        self.state.quality = Some(source.quality);
        Ok(())
    }

    async fn stop_playback(&mut self) {
        self.load_generation += 1;
        self.pending = None;
        self.state.next_ready = false;
        if self.current_entry.take().is_some()
            && let Err(e) = self.player.stop().await
        {
            tracing::warn!(error = %e, "arrêt du lecteur");
        }
        self.state.status = Status::Idle;
        self.state.position = Duration::ZERO;
    }

    // ----- préchargement et radio -----

    /// Met le lecteur en accord avec « ce qui vient après » dans la file.
    async fn refresh_preload(&mut self) {
        self.maybe_fetch_radio();
        self.refresh_lookahead();
        if self.current_entry.is_none() {
            // On préchargera quand le morceau courant aura démarré.
            return;
        }
        let next = self
            .queue
            .peek_auto_next()
            .map(|e| (e.id, e.track.video_id.clone()));

        if let Some(pending) = &self.pending {
            if next.as_ref().is_some_and(|(id, _)| *id == pending.id) {
                return;
            }
            // La file a changé : l'entrée préparée n'est plus la bonne.
            self.pending = None;
            self.state.next_ready = false;
            if let Err(e) = self.player.clear_pending().await {
                tracing::warn!(error = %e, "nettoyage de la playlist du lecteur");
            }
        }

        let Some((id, video_id)) = next else {
            return;
        };
        if self.preloading.is_some_and(|(p, _)| p == id) {
            return;
        }
        if let Some(source) = self
            .resolver
            .cached(&video_id)
            .filter(|s| s.quality == StreamQuality::High)
        {
            self.enqueue_pending(id, &source).await;
            return;
        }
        self.preloading = Some((id, Instant::now()));
        let resolver = Arc::clone(&self.resolver);
        self.tasks.spawn(async move {
            let result = resolver
                .resolve(&video_id, StartMode::Quality, Priority::Urgent)
                .await;
            TaskDone::Preload { id, result }
        });
    }

    async fn enqueue_pending(&mut self, id: QueueId, source: &MediaSource) {
        match self.player.enqueue(source).await {
            Ok(entry) => {
                tracing::debug!(?id, ?entry, quality = ?source.quality, "morceau suivant préchargé");
                self.pending = Some(Pending {
                    id,
                    entry,
                    quality: source.quality,
                });
                self.state.next_ready = true;
            }
            Err(e) => tracing::warn!(error = %e, "préchargement refusé par le lecteur"),
        }
    }

    fn maybe_fetch_radio(&mut self) {
        if !self.state.autoplay
            || self.queue.repeat() != Repeat::Off
            || self.radio_inflight
            || self.queue.upcoming_len() >= RADIO_LOW_WATER
        {
            return;
        }
        let Some(seed) = self.queue.current() else {
            return;
        };
        if self.radio_seed == Some(seed.id) {
            return;
        }
        self.radio_seed = Some(seed.id);
        self.radio_retries = 0;
        let video_id = seed.track.video_id.clone();
        self.fetch_radio(video_id);
    }

    fn fetch_radio(&mut self, video_id: String) {
        self.radio_used.insert(video_id.clone());
        self.radio_inflight = true;
        let radio = Arc::clone(&self.radio);
        self.tasks.spawn(async move {
            TaskDone::Radio {
                result: radio.radio(&video_id).await,
            }
        });
    }

    /// Nouvelle graine pour la radio, parmi les derniers morceaux de la file.
    fn retry_radio(&mut self) {
        const MAX_RETRIES: u8 = 3;
        if self.radio_retries >= MAX_RETRIES || !self.state.autoplay {
            return;
        }
        self.radio_retries += 1;
        let candidates: Vec<String> = self
            .queue
            .entries()
            .iter()
            .rev()
            .take(30)
            .map(|e| e.track.video_id.clone())
            .filter(|v| !self.radio_used.contains(v))
            .collect();
        if candidates.is_empty() {
            return;
        }
        let seed = candidates[fastrand::usize(..candidates.len())].clone();
        self.fetch_radio(seed);
    }

    async fn on_task(&mut self, done: TaskDone) {
        match done {
            TaskDone::Start {
                generation,
                id,
                result,
            } => {
                if generation != self.load_generation {
                    return;
                }
                let source = result.unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "résolution échouée, repli sur le hook yt-dlp de mpv");
                    let url = self
                        .queue
                        .get(id)
                        .map(|e| e.track.watch_url())
                        .unwrap_or_default();
                    MediaSource::via_mpv_hook(url)
                });
                if let Err(e) = self.load_source(&source).await {
                    self.report(format!("lecture impossible : {e}"));
                    self.state.status = Status::Idle;
                }
            }
            TaskDone::Preload { id, result } => {
                if self.preloading.is_some_and(|(p, _)| p == id) {
                    self.preloading = None;
                }
                let awaited = self
                    .awaiting_preload
                    .take_if(|g| *g == self.load_generation)
                    .is_some()
                    && self.queue.current().is_some_and(|e| e.id == id);
                if awaited {
                    match result {
                        Ok(source) => {
                            if let Err(e) = self.load_source(&source).await {
                                self.report(format!("lecture impossible : {e}"));
                                self.state.status = Status::Idle;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "préchargement attendu échoué, démarrage rapide");
                            if let Some(video_id) =
                                self.queue.get(id).map(|e| e.track.video_id.clone())
                            {
                                self.spawn_start(id, video_id);
                            }
                        }
                    }
                    return;
                }
                match result {
                    Ok(source) => {
                        let still_next = self.queue.peek_auto_next().is_some_and(|e| e.id == id);
                        if still_next && self.pending.is_none() && self.current_entry.is_some() {
                            self.enqueue_pending(id, &source).await;
                        } else {
                            self.refresh_preload().await;
                        }
                    }
                    // Pas grave : le morceau démarrera à froid.
                    Err(e) => tracing::warn!(error = %e, "préchargement échoué"),
                }
            }
            TaskDone::Warmed { video_id } => {
                self.warming.remove(&video_id);
                self.pump_warm();
            }
            TaskDone::Radio { result } => {
                self.radio_inflight = false;
                match result {
                    Ok(tracks) => {
                        let mut fresh: Vec<Track> = Vec::new();
                        // YouTube met souvent le morceau de départ en tête de sa
                        // radio, parfois sous un autre identifiant (clip / audio).
                        let current = self.queue.current().map(|e| e.track.clone());
                        for track in tracks {
                            let duplicate = self.queue.contains_video(&track.video_id)
                                || current.as_ref().is_some_and(|c| c.same_song(&track))
                                || self
                                    .queue
                                    .visible()
                                    .iter()
                                    .any(|e| e.track.same_song(&track))
                                || fresh.iter().any(|t| t.same_song(&track));
                            if !duplicate && fresh.len() < RADIO_MAX_ADD {
                                fresh.push(track);
                            }
                        }
                        tracing::debug!(added = fresh.len(), "radio");
                        if fresh.is_empty() {
                            // Tout était déjà dans la file (radio déjà jouée depuis ce
                            // morceau) : on repart d'un autre titre récent.
                            self.retry_radio();
                            return;
                        }
                        {
                            let was_idle_at_end = self.current_entry.is_none()
                                && self.state.status == Status::Idle
                                && self.queue.current().is_some();
                            let ids = self.queue.append_mixed(fresh);
                            self.queue_dirty = true;
                            if was_idle_at_end && let Some(&first) = ids.first() {
                                // La file s'était épuisée en attendant la radio.
                                if let Err(e) = self.start(first).await {
                                    self.report(format!("autoplay : {e}"));
                                }
                                return;
                            }
                            self.refresh_preload().await;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "radio indisponible"),
                }
            }
        }
    }

    // ----- événements du lecteur -----

    async fn on_player_event(&mut self, event: PlayerEvent) {
        match event {
            PlayerEvent::Started(entry) => self.on_started(entry).await,
            PlayerEvent::Loaded => {
                if self.current_entry.is_some() {
                    self.state.status = if self.paused {
                        Status::Paused
                    } else {
                        Status::Playing
                    };
                }
            }
            PlayerEvent::Paused(paused) => {
                self.paused = paused;
                if matches!(self.state.status, Status::Playing | Status::Paused) {
                    self.state.status = if paused {
                        Status::Paused
                    } else {
                        Status::Playing
                    };
                }
            }
            PlayerEvent::Position(position) => {
                if self.current_entry.is_some() {
                    self.state.position = position;
                }
            }
            PlayerEvent::Duration(duration) => {
                if self.current_entry.is_some() {
                    self.state.duration = Some(duration);
                }
            }
            PlayerEvent::Volume(volume) => self.state.volume = volume,
            PlayerEvent::Ended { entry, reason } => self.on_ended(entry, reason).await,
            PlayerEvent::Idle => {}
            PlayerEvent::Exited => {
                self.player_alive = false;
                self.current_entry = None;
                self.pending = None;
                self.state.next_ready = false;
                self.state.status = Status::Idle;
                self.report("le lecteur mpv s'est arrêté".into());
            }
        }
    }

    async fn on_started(&mut self, entry: EntryId) {
        if let Some(pending) = self.pending.take_if(|p| p.entry == entry) {
            // Enchaînement automatique par le lecteur.
            let track = self.queue.get(pending.id).map(|e| e.track.clone());
            self.queue.set_current(pending.id);
            self.queue_dirty = true;
            if let Some(track) = track {
                self.reset_track_state(&track);
            }
            self.current_entry = Some(entry);
            self.state.quality = Some(pending.quality);
            self.state.next_ready = false;
        } else if self.current_entry != Some(entry) {
            return;
        }
        // Retire les entrées terminées de la playlist du lecteur.
        if let Err(e) = self.player.clear_pending().await {
            tracing::warn!(error = %e, "nettoyage de la playlist du lecteur");
        }
        // Session reprise : on repart là où on s'était arrêté.
        let current = self.queue.current().map(|e| e.id);
        if let Some((id, position)) = self.resume_at.take_if(|(r, _)| Some(*r) == current)
            && position > Duration::from_secs(2)
        {
            if let Err(e) = self.player.seek(position).await {
                tracing::warn!(error = %e, "reprise de la position");
            }
            tracing::debug!(?id, secs = position.as_secs(), "position reprise");
        }
        self.refresh_preload().await;
    }

    async fn on_ended(&mut self, entry: Option<EntryId>, reason: EndReason) {
        if entry.is_some() && entry != self.current_entry {
            return;
        }
        match reason {
            EndReason::Stopped => return,
            EndReason::Eof => self.consecutive_errors = 0,
            EndReason::Error(message) => {
                self.consecutive_errors += 1;
                let title = self
                    .queue
                    .current()
                    .map(|e| e.track.title.clone())
                    .unwrap_or_default();
                self.report(format!("« {title} » illisible : {message}"));
                if self.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    self.report("trop d'erreurs d'affilée, lecture arrêtée".into());
                    self.stop_playback().await;
                    return;
                }
            }
        }
        if self.pending.is_some() {
            // Le lecteur enchaîne seul ; `Started` suivra.
            return;
        }
        self.current_entry = None;
        match self.queue.advance_auto() {
            Some(next) => {
                if let Err(e) = self.start(next).await {
                    self.report(format!("morceau suivant : {e}"));
                }
            }
            None => {
                self.state.status = Status::Idle;
                self.state.position = Duration::ZERO;
                // La radio peut encore arriver et relancer la lecture.
                self.maybe_fetch_radio();
            }
        }
    }

    // ----- publication -----

    fn report(&self, message: String) {
        tracing::warn!("{message}");
        let _ = self.events.send(CoreEvent::Error(message));
    }

    fn publish(&mut self) {
        self.state.current = self.queue.current().cloned();
        self.state.shuffle = self.queue.shuffle();
        self.state.repeat = self.queue.repeat();

        let current_id = self.state.current.as_ref().map(|e| e.id);
        if current_id != self.published_current {
            self.published_current = current_id;
            let _ = self
                .events
                .send(CoreEvent::TrackChanged(self.state.current.clone()));
        }
        if self.queue_dirty {
            self.queue_dirty = false;
            self.queue_tx.send_replace(Arc::new(QueueSnapshot {
                // L'historique ne s'affiche pas : le morceau en cours, puis la suite.
                entries: self.queue.visible().to_vec(),
                current: self.queue.current().map(|_| 0),
            }));
            let _ = self.events.send(CoreEvent::QueueChanged);
        }
        self.state_tx.send_if_modified(|published| {
            if *published == self.state {
                false
            } else {
                published.clone_from(&self.state);
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Load(String),
        Enqueue(String),
        ClearPending,
        SkipToPending,
        Stop,
        Seek(Duration),
    }

    struct FakeInner {
        calls: Mutex<Vec<Call>>,
        next_entry: AtomicU64,
        events: broadcast::Sender<PlayerEvent>,
    }

    #[derive(Clone)]
    struct FakePlayer(Arc<FakeInner>);

    impl FakePlayer {
        fn new() -> Self {
            Self(Arc::new(FakeInner {
                calls: Mutex::default(),
                next_entry: AtomicU64::new(1),
                events: broadcast::channel(64).0,
            }))
        }
        fn record(&self, call: Call) {
            self.0.calls.lock().unwrap().push(call);
        }
        fn calls(&self) -> Vec<Call> {
            self.0.calls.lock().unwrap().clone()
        }
        fn emit(&self, event: PlayerEvent) {
            self.0.events.send(event).unwrap();
        }
        fn entry(&self) -> EntryId {
            EntryId(self.0.next_entry.fetch_add(1, Ordering::Relaxed))
        }
        /// Fin naturelle de `ended` puis démarrage de `started`, comme mpv en gapless.
        fn roll_over(&self, ended: u64, started: u64) {
            self.emit(PlayerEvent::Ended {
                entry: Some(EntryId(ended)),
                reason: EndReason::Eof,
            });
            self.emit(PlayerEvent::Started(EntryId(started)));
        }
    }

    impl Player for FakePlayer {
        async fn load(&self, source: &MediaSource) -> Result<EntryId, PlayerError> {
            self.record(Call::Load(source.url.clone()));
            Ok(self.entry())
        }
        async fn enqueue(&self, source: &MediaSource) -> Result<EntryId, PlayerError> {
            self.record(Call::Enqueue(source.url.clone()));
            Ok(self.entry())
        }
        async fn clear_pending(&self) -> Result<(), PlayerError> {
            self.record(Call::ClearPending);
            Ok(())
        }
        async fn skip_to_pending(&self) -> Result<(), PlayerError> {
            self.record(Call::SkipToPending);
            Ok(())
        }
        async fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
            self.emit(PlayerEvent::Paused(paused));
            Ok(())
        }
        async fn stop(&self) -> Result<(), PlayerError> {
            self.record(Call::Stop);
            Ok(())
        }
        async fn seek(&self, position: Duration) -> Result<(), PlayerError> {
            self.record(Call::Seek(position));
            Ok(())
        }
        async fn set_volume(&self, _volume: u8) -> Result<(), PlayerError> {
            Ok(())
        }
        async fn set_effects(&self, _effects: &AudioEffects) -> Result<(), PlayerError> {
            Ok(())
        }
        fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
            self.0.events.subscribe()
        }
        async fn shutdown(&self) {}
    }

    /// Résout en `src:<video_id>:<mode>`, instantanément sauf `quality_delay`.
    #[derive(Default)]
    struct FakeResolver {
        quality_delay: Duration,
        log: Mutex<Vec<(String, StartMode, Priority)>>,
    }

    impl FakeResolver {
        fn background(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, p)| *p == Priority::Background)
                .map(|(v, _, _)| v.clone())
                .collect()
        }
    }

    impl Resolver for FakeResolver {
        fn cached(&self, _video_id: &str) -> Option<MediaSource> {
            None
        }
        async fn resolve(
            &self,
            video_id: &str,
            mode: StartMode,
            priority: Priority,
        ) -> Result<MediaSource, PlayerError> {
            self.log
                .lock()
                .unwrap()
                .push((video_id.to_owned(), mode, priority));
            if mode == StartMode::Quality {
                tokio::time::sleep(self.quality_delay).await;
            }
            Ok(MediaSource {
                url: format!("src:{video_id}:{mode:?}"),
                user_agent: None,
                quality: match mode {
                    StartMode::Fast => StreamQuality::Low,
                    StartMode::Quality => StreamQuality::High,
                },
                expires_at: None,
            })
        }
    }

    struct FakeRadio(Vec<Track>);

    impl Radio for FakeRadio {
        async fn radio(&self, _video_id: &str) -> Result<Vec<Track>, BoxError> {
            Ok(self.0.clone())
        }
    }

    fn track(n: u32) -> Track {
        Track {
            video_id: format!("v{n}"),
            title: format!("t{n}"),
            artist: "a".into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: Some(Duration::from_secs(200)),
            thumbnail_url: None,
        }
    }

    fn tracks(range: std::ops::Range<u32>) -> Vec<Track> {
        range.map(track).collect()
    }

    fn start(player: &FakePlayer, autoplay: bool, radio: Vec<Track>) -> CoreHandle {
        start_with(player, autoplay, radio, FakeResolver::default())
    }

    fn start_with(
        player: &FakePlayer,
        autoplay: bool,
        radio: Vec<Track>,
        resolver: FakeResolver,
    ) -> CoreHandle {
        start_full(player, autoplay, radio, Arc::new(resolver), None)
    }

    fn start_full(
        player: &FakePlayer,
        autoplay: bool,
        radio: Vec<Track>,
        resolver: Arc<FakeResolver>,
        session_file: Option<PathBuf>,
    ) -> CoreHandle {
        let (handle, _task) = spawn_with_queue(
            player.clone(),
            resolver,
            Arc::new(FakeRadio(radio)),
            CoreOptions {
                volume: 50,
                autoplay,
                session_file,
            },
            Queue::with_rng(fastrand::Rng::with_seed(7)),
        );
        handle
    }

    /// Attend (10 s max, en temps réel ou simulé) qu'une condition soit vraie.
    async fn until(what: &str, mut cond: impl FnMut() -> bool) {
        for _ in 0..2000 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("délai dépassé : {what}");
    }

    fn current_title(core: &CoreHandle) -> Option<String> {
        core.state().current.map(|e| e.track.title)
    }

    #[tokio::test]
    async fn chains_three_tracks_gaplessly() {
        let player = FakePlayer::new();
        let core = start(&player, false, vec![]);

        core.send(Command::PlayTracks {
            tracks: tracks(0..3),
            start: 0,
        })
        .await
        .unwrap();
        until("chargement à froid", || {
            player.calls().contains(&Call::Load("src:v0:Fast".into()))
        })
        .await;

        player.emit(PlayerEvent::Started(EntryId(1)));
        player.emit(PlayerEvent::Loaded);
        until("préchargement de t1", || {
            player
                .calls()
                .contains(&Call::Enqueue("src:v1:Quality".into()))
        })
        .await;
        until("next_ready", || core.state().next_ready).await;

        player.roll_over(1, 2);
        until("passage à t1", || {
            current_title(&core).as_deref() == Some("t1")
        })
        .await;
        assert_eq!(core.state().quality, Some(StreamQuality::High));
        until("préchargement de t2", || {
            player
                .calls()
                .contains(&Call::Enqueue("src:v2:Quality".into()))
        })
        .await;

        player.roll_over(2, 3);
        until("passage à t2", || {
            current_title(&core).as_deref() == Some("t2")
        })
        .await;

        player.emit(PlayerEvent::Ended {
            entry: Some(EntryId(3)),
            reason: EndReason::Eof,
        });
        until("fin de file", || core.state().status == Status::Idle).await;

        let loads = player
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::Load(_)))
            .count();
        assert_eq!(
            loads, 1,
            "un seul chargement : le reste est enchaîné sans blanc"
        );
    }

    #[tokio::test]
    async fn manual_next_uses_preloaded_entry() {
        let player = FakePlayer::new();
        let core = start(&player, false, vec![]);
        core.send(Command::PlayTracks {
            tracks: tracks(0..3),
            start: 0,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));
        until("préchargement", || core.state().next_ready).await;

        core.send(Command::Next).await.unwrap();
        assert!(player.calls().contains(&Call::SkipToPending));
        assert_eq!(current_title(&core).as_deref(), Some("t1"));
        assert!(
            !core.state().next_ready,
            "plus rien en attente après le saut"
        );

        player.emit(PlayerEvent::Started(EntryId(2)));
        until("préchargement de t2", || {
            player
                .calls()
                .contains(&Call::Enqueue("src:v2:Quality".into()))
        })
        .await;
    }

    #[tokio::test]
    async fn queue_change_replaces_preloaded_entry() {
        let player = FakePlayer::new();
        let core = start(&player, false, vec![]);
        core.send(Command::PlayTracks {
            tracks: tracks(0..3),
            start: 0,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));
        until("préchargement", || core.state().next_ready).await;

        let next_id = core.queue().entries[1].id;
        core.send(Command::Remove(next_id)).await.unwrap();

        let calls = player.calls();
        let clear = calls
            .iter()
            .rposition(|c| *c == Call::ClearPending)
            .unwrap();
        let enqueue_t1 = calls
            .iter()
            .position(|c| *c == Call::Enqueue("src:v1:Quality".into()))
            .unwrap();
        assert!(clear > enqueue_t1, "l'entrée préchargée de t1 est retirée");
        until("préchargement de t2", || {
            player
                .calls()
                .contains(&Call::Enqueue("src:v2:Quality".into()))
        })
        .await;
    }

    #[tokio::test]
    async fn autoplay_appends_radio_without_duplicates() {
        let player = FakePlayer::new();
        let radio = vec![track(0), track(10), track(11), track(10)];
        let core = start(&player, true, radio);
        core.send(Command::PlayTracks {
            tracks: tracks(0..1),
            start: 0,
        })
        .await
        .unwrap();

        until("radio ajoutée", || core.queue().entries.len() == 3).await;
        let titles: Vec<_> = core
            .queue()
            .entries
            .iter()
            .map(|e| e.track.title.clone())
            .collect();
        assert_eq!(titles, ["t0", "t10", "t11"]);

        player.emit(PlayerEvent::Started(EntryId(1)));
        until("préchargement du premier titre radio", || {
            player
                .calls()
                .contains(&Call::Enqueue("src:v10:Quality".into()))
        })
        .await;
    }

    #[tokio::test]
    async fn radio_comes_back_after_being_switched_off_and_on() {
        let player = FakePlayer::new();
        let radio = vec![track(10), track(11), track(12)];
        let core = start(&player, false, radio);
        core.send(Command::PlayTracks {
            tracks: tracks(0..1),
            start: 0,
        })
        .await
        .unwrap();
        core.send(Command::SetAutoplay(true)).await.unwrap();
        until("radio ajoutée", || core.queue().entries.len() == 4).await;
        core.send(Command::SetAutoplay(false)).await.unwrap();
        core.send(Command::PlayTracks {
            tracks: tracks(0..1),
            start: 0,
        })
        .await
        .unwrap();
        core.send(Command::SetAutoplay(true)).await.unwrap();
        until("radio de nouveau", || core.queue().entries.len() == 4).await;
    }

    #[tokio::test(start_paused = true)]
    async fn next_waits_for_advanced_preload_instead_of_cold_start() {
        let player = FakePlayer::new();
        let resolver = FakeResolver {
            quality_delay: Duration::from_secs(8),
            ..FakeResolver::default()
        };
        let core = start_with(&player, false, vec![], resolver);
        core.send(Command::PlayTracks {
            tracks: tracks(0..2),
            start: 0,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));

        // Préchargement lancé depuis 6 s (sur 8) : l'attendre vaut mieux qu'un démarrage rapide.
        tokio::time::sleep(Duration::from_secs(6)).await;
        core.send(Command::Next).await.unwrap();
        until("chargement en qualité", || {
            player
                .calls()
                .contains(&Call::Load("src:v1:Quality".into()))
        })
        .await;
        assert!(!player.calls().contains(&Call::Load("src:v1:Fast".into())));
    }

    #[tokio::test(start_paused = true)]
    async fn next_cold_starts_when_preload_just_began() {
        let player = FakePlayer::new();
        let resolver = FakeResolver {
            quality_delay: Duration::from_secs(8),
            ..FakeResolver::default()
        };
        let core = start_with(&player, false, vec![], resolver);
        core.send(Command::PlayTracks {
            tracks: tracks(0..2),
            start: 0,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));
        tokio::task::yield_now().await;

        core.send(Command::Next).await.unwrap();
        until("démarrage rapide", || {
            player.calls().contains(&Call::Load("src:v1:Fast".into()))
        })
        .await;
    }

    /// Le cas de l'utilisateur : un morceau en cours, rien derrière ; activer
    /// puis couper l'aléatoire (radio automatique active, qui renvoie le
    /// morceau lui-même et sa version clip ; répétition de la file aussi).
    #[tokio::test]
    async fn toggling_shuffle_never_adds_the_playing_track() {
        let mut clip = track(0);
        clip.video_id = "v0clip".into();
        clip.title = "t0 (Official Video)".into();
        for repeat in [Repeat::Off, Repeat::All] {
            let player = FakePlayer::new();
            let resolver = Arc::new(FakeResolver::default());
            let core = start_full(
                &player,
                true,
                vec![track(0), clip.clone()],
                Arc::clone(&resolver),
                None,
            );
            core.send(Command::PlayTracks {
                tracks: vec![track(0)],
                start: 0,
            })
            .await
            .unwrap();
            until("chargement", || !player.calls().is_empty()).await;
            player.emit(PlayerEvent::Started(EntryId(1)));
            core.send(Command::SetRepeat(repeat)).await.unwrap();
            for on in [true, false, true, false] {
                core.send(Command::SetShuffle(on)).await.unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
                let queue = core.queue();
                let copies = queue
                    .entries
                    .iter()
                    .filter(|e| e.track.same_song(&track(0)))
                    .count();
                assert_eq!(
                    copies, 1,
                    "aléatoire {on}, répétition {repeat:?} : le morceau en cours est apparu {copies} fois"
                );
            }
        }
    }

    #[tokio::test]
    async fn resolves_the_next_ten_tracks_in_background() {
        let player = FakePlayer::new();
        let resolver = Arc::new(FakeResolver::default());
        let core = start_full(&player, false, vec![], Arc::clone(&resolver), None);
        core.send(Command::PlayTracks {
            tracks: tracks(0..15),
            start: 0,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));

        // On attend que *tous* les attendus soient là (et non « 10 résolutions » :
        // sous charge, une résolution relancée comptait deux fois et la
        // comparaison partait trop tôt), puis on compare sans les doublons.
        let expected: std::collections::BTreeSet<String> =
            (2..12).map(|n| format!("v{n}")).collect();
        until("anticipation", || {
            let warmed: std::collections::BTreeSet<String> =
                resolver.background().into_iter().collect();
            warmed.is_superset(&expected)
        })
        .await;
        let warmed: std::collections::BTreeSet<String> =
            resolver.background().into_iter().collect();
        assert_eq!(
            warmed, expected,
            "v1 est préchargé en urgent, v12+ hors fenêtre"
        );
    }

    #[tokio::test]
    async fn session_is_saved_and_resumed_at_the_same_position() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("session.json");

        let player = FakePlayer::new();
        let core = start_full(
            &player,
            false,
            vec![],
            Arc::new(FakeResolver::default()),
            Some(file.clone()),
        );
        core.send(Command::PlayTracks {
            tracks: tracks(0..3),
            start: 1,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));
        player.emit(PlayerEvent::Position(Duration::from_secs(42)));
        until("position", || {
            core.state().position == Duration::from_secs(42)
        })
        .await;
        core.send(Command::SetVolume(33)).await.unwrap();
        core.send(Command::SetRepeat(Repeat::All)).await.unwrap();
        core.send(Command::Shutdown).await.unwrap();
        assert!(file.exists());

        // Nouveau lancement : même file, en pause sur t1, à 42 s.
        let player = FakePlayer::new();
        let core = start_full(
            &player,
            false,
            vec![],
            Arc::new(FakeResolver::default()),
            Some(file),
        );
        until("file reprise", || core.queue().entries.len() == 3).await;
        assert_eq!(current_title(&core).as_deref(), Some("t1"));
        assert_eq!(core.state().status, Status::Idle);
        assert_eq!(core.state().position, Duration::from_secs(42));
        assert_eq!(core.state().volume, 33, "volume gardé");
        assert_eq!(core.state().repeat, Repeat::All, "repeat gardé");

        // « Lecture » relance le morceau et rejoint la position.
        core.send(Command::TogglePause).await.unwrap();
        until("chargement repris", || {
            player.calls().iter().any(|c| matches!(c, Call::Load(_)))
        })
        .await;
        player.emit(PlayerEvent::Started(EntryId(1)));
        until("position rejointe", || {
            player
                .calls()
                .contains(&Call::Seek(Duration::from_secs(42)))
        })
        .await;
    }

    #[tokio::test]
    async fn previous_restarts_track_after_three_seconds() {
        let player = FakePlayer::new();
        let core = start(&player, false, vec![]);
        core.send(Command::PlayTracks {
            tracks: tracks(0..2),
            start: 1,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));
        player.emit(PlayerEvent::Position(Duration::from_secs(10)));
        until("position", || {
            core.state().position == Duration::from_secs(10)
        })
        .await;

        core.send(Command::Previous).await.unwrap();
        assert!(player.calls().contains(&Call::Seek(Duration::ZERO)));
        assert_eq!(current_title(&core).as_deref(), Some("t1"));

        core.send(Command::Previous).await.unwrap();
        assert_eq!(current_title(&core).as_deref(), Some("t0"));
    }

    #[tokio::test]
    async fn rejects_invalid_input() {
        let player = FakePlayer::new();
        let core = start(&player, false, vec![]);
        assert!(matches!(
            core.send(Command::SetVolume(101)).await,
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            core.send(Command::Seek(Duration::from_secs(1))).await,
            Err(CoreError::NothingToPlay)
        ));
        assert!(matches!(
            core.send(Command::PlayTracks {
                tracks: vec![],
                start: 0
            })
            .await,
            Err(CoreError::NothingToPlay)
        ));
        core.send(Command::PlayTracks {
            tracks: tracks(0..1),
            start: 0,
        })
        .await
        .unwrap();
        until("chargement", || !player.calls().is_empty()).await;
        player.emit(PlayerEvent::Started(EntryId(1)));
        assert!(matches!(
            core.send(Command::Seek(Duration::from_secs(999))).await,
            Err(CoreError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn stops_after_repeated_errors() {
        let player = FakePlayer::new();
        let core = start(&player, false, vec![]);
        let mut events = core.subscribe();
        core.send(Command::PlayTracks {
            tracks: tracks(0..5),
            start: 0,
        })
        .await
        .unwrap();
        let loads = |p: &FakePlayer| {
            p.calls()
                .iter()
                .filter(|c| matches!(c, Call::Load(_)))
                .count()
        };

        // Chaque morceau échoue avant d'avoir démarré (donc sans préchargement).
        for entry in 1..=3u64 {
            until("chargement", || loads(&player) == entry as usize).await;
            player.emit(PlayerEvent::Ended {
                entry: Some(EntryId(entry)),
                reason: EndReason::Error("boom".into()),
            });
        }
        until("arrêt", || core.state().status == Status::Idle).await;
        assert_eq!(loads(&player), 3, "pas de quatrième tentative");

        let mut errors = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(event, CoreEvent::Error(_)) {
                errors += 1;
            }
        }
        assert_eq!(errors, 4, "3 morceaux illisibles + l'arrêt");
    }
}
