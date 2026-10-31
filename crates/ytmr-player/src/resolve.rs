//! Résolution des flux à l'avance, via yt-dlp.
//!
//! En clair : demander à yt-dlp « où est le fichier audio de ce morceau ? »
//! prend 2 à 5 secondes. Plutôt que de laisser mpv le faire au dernier moment,
//! on pose la question en avance (pendant le morceau précédent) et on garde la
//! réponse en mémoire tant que l'adresse est valable (plusieurs heures).
//!
//! Deux modes (constaté le 2026-10-29, sans PO token) :
//! - [`StartMode::Quality`] : client par défaut de yt-dlp, meilleur audio seul.
//!   Compte gratuit : Opus 251 ~160 kb/s, utilisable seulement après quelques
//!   secondes (`available_at`). Avec les cookies d'un compte Premium : Opus 774
//!   ~264 kb/s, sans délai, mais ~7 s de résolution. Idéal pour le préchargement.
//! - [`StartMode::Fast`] : client « rapide » (config `player.fast_start_client`),
//!   format 18 (AAC ~96 kb/s), utilisable tout de suite. Désactivé par défaut.
//!
//! On ne touche pas à l'extraction : yt-dlp fait tout, on l'appelle juste plus tôt.
//!
//! Deux files : [`Priority::Urgent`] (morceau demandé, morceau suivant) et
//! [`Priority::Background`] (les ~10 suivants, le morceau survolé). Le travail
//! de fond ne retarde donc jamais un clic. Une même vidéo demandée deux fois
//! n'est résolue qu'une fois : la seconde demande attend la première.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, watch};

use crate::PlayerError;
use crate::deps::{Dependencies, JsRuntime};
use crate::worker::Worker;

/// Avec les cookies d'un compte, le client `web_music` sans page web préalable
/// donne le même format (Opus 774) bien plus vite : 2,8 s contre 4,8 s par
/// morceau dans le worker, 5,3 s contre 7,2 s avec l'exécutable (2026-10-29).
const ACCOUNT_CLIENT: &str = "web_music";
const ACCOUNT_SKIP: &str = "webpage,configs";

/// Au-delà, yt-dlp est considéré bloqué.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
/// Une adresse est jetée du cache un peu avant son expiration réelle.
const EXPIRY_MARGIN: Duration = Duration::from_secs(10 * 60);
/// Durée de vie supposée si l'URL ne donne pas `expire=`.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(4 * 3600);
/// yt-dlp lancés en même temps, par file.
const MAX_URGENT: usize = 2;
const MAX_BACKGROUND: usize = 2;
/// Au-delà, les adresses qui expirent le plus tôt sont oubliées (~1 Ko chacune).
const MAX_CACHED: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// L'utilisateur attend (clic, morceau suivant).
    Urgent,
    /// Anticipation : peut attendre son tour.
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    /// Son le plus vite possible, qualité moindre.
    Fast,
    /// Meilleure qualité, quitte à attendre quelques secondes.
    Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamQuality {
    /// Audio seul, meilleur format disponible.
    High,
    /// Format combiné de repli (AAC ~96 kb/s).
    Low,
    /// Inconnue : mpv résout lui-même via son hook yt-dlp (repli de secours).
    Unknown,
}

/// Ce qu'on donne à mpv pour jouer un morceau.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSource {
    pub url: String,
    /// User-Agent exigé par l'URL signée.
    pub user_agent: Option<String>,
    pub quality: StreamQuality,
    pub expires_at: Option<SystemTime>,
}

impl MediaSource {
    /// Repli : mpv appellera yt-dlp lui-même (plus lent, mais sans dépendre du résolveur).
    pub fn via_mpv_hook(watch_url: String) -> Self {
        Self {
            url: watch_url,
            user_agent: None,
            quality: StreamQuality::Unknown,
            expires_at: None,
        }
    }

    fn is_fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|t| t > SystemTime::now() + EXPIRY_MARGIN)
    }
}

/// Contrat du résolveur, pour pouvoir le remplacer par un faux dans les tests du core.
/// Qualité demandée à yt-dlp (Paramètres).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QualityPreference {
    /// Le meilleur format disponible (Opus ~256 kb/s avec Premium).
    #[default]
    Max,
    /// Jusqu'à ~160 kb/s.
    High,
    /// Jusqu'à ~70 kb/s : économise les données.
    DataSaver,
}

impl QualityPreference {
    /// Sélecteur de format yt-dlp.
    pub fn format(self) -> &'static str {
        match self {
            Self::Max => "bestaudio/best",
            Self::High => "bestaudio[abr<=160]/bestaudio/best",
            Self::DataSaver => "bestaudio[abr<=70]/worstaudio/best",
        }
    }
}

pub trait Resolver: Send + Sync + 'static {
    /// Source déjà connue et encore valable, sans rien lancer.
    fn cached(&self, video_id: &str) -> Option<MediaSource>;
    /// Qualité des prochaines résolutions (les adresses déjà connues sont oubliées).
    fn set_quality(&self, _quality: QualityPreference) {}
    /// Garde (`true`) ou non des morceaux sur le disque, à l'abri de la limite du cache.
    fn keep_offline(&self, _video_ids: &[String], _keep: bool) {}
    /// Mode hors ligne : seuls les fichiers déjà sur le disque sont lus, rien
    /// n'est résolu ni téléchargé.
    fn set_offline(&self, _offline: bool) {}
    /// Résout (ou relit le cache). Ne rend la main que quand l'URL est utilisable.
    fn resolve(
        &self,
        video_id: &str,
        mode: StartMode,
        priority: Priority,
    ) -> impl Future<Output = Result<MediaSource, PlayerError>> + Send;
}

type Shared = watch::Receiver<Option<Result<MediaSource, String>>>;

/// Réglages du résolveur, tirés de la config.
#[derive(Debug, Clone, Default)]
pub struct ResolverOptions {
    /// Client YouTube pour [`StartMode::Fast`] ; `None` = toujours la qualité.
    pub fast_client: Option<String>,
    /// `firefox`, ou `firefox:<profil>` : session YouTube à utiliser.
    pub cookies_from_browser: Option<String>,
    /// Session capturée par la fenêtre de connexion de ytmr (prioritaire).
    pub session: Option<Vec<crate::session::SessionCookie>>,
    /// `clé=valeur` ajoutées à chaque appel de l'exécutable. S'il y en a, le
    /// worker n'est pas utilisé (ses options ne se traduisent pas toutes).
    pub raw_options: Vec<String>,
    /// Fichier où garder les adresses résolues entre deux lancements.
    pub cache_file: Option<PathBuf>,
    /// Fichiers audio déjà écoutés, relus sans yt-dlp.
    pub audio_cache: Option<Arc<crate::AudioCache>>,
}

pub struct YtDlpResolver {
    ytdlp: PathBuf,
    /// Arguments communs à tous les appels (moteur JS, options de la config).
    common_args: Vec<String>,
    fast_client: Option<String>,
    cookies: Option<String>,
    session: Option<Vec<crate::session::SessionCookie>>,
    node: bool,
    worker_allowed: bool,
    worker: Mutex<Option<Arc<Worker>>>,
    cache_file: Option<PathBuf>,
    cache: Mutex<HashMap<String, MediaSource>>,
    audio: Option<Arc<crate::AudioCache>>,
    /// Résolutions en cours, partagées entre demandeurs.
    inflight: Mutex<HashMap<String, Shared>>,
    urgent: Semaphore,
    background: Semaphore,
    quality: Mutex<QualityPreference>,
    offline: AtomicBool,
}

impl YtDlpResolver {
    pub fn new(deps: &Dependencies, options: ResolverOptions) -> Self {
        let node = deps.js_runtime == Some(JsRuntime::Node);
        let mut common_args = Vec::new();
        if node {
            common_args.extend(["--js-runtimes".to_owned(), "node".to_owned()]);
        }
        if let Some(browser) = &options.cookies_from_browser {
            common_args.extend(["--cookies-from-browser".to_owned(), browser.clone()]);
        }
        common_args.extend(raw_options_to_args(&options.raw_options));
        let cache = options
            .cache_file
            .as_deref()
            .map(load_cache)
            .unwrap_or_default();
        Self {
            ytdlp: deps.ytdlp.path.clone(),
            common_args,
            fast_client: options.fast_client,
            cookies: options.cookies_from_browser,
            session: options.session,
            node,
            worker_allowed: options.raw_options.is_empty(),
            worker: Mutex::default(),
            cache_file: options.cache_file,
            cache: Mutex::new(cache),
            audio: options.audio_cache,
            inflight: Mutex::default(),
            urgent: Semaphore::new(MAX_URGENT),
            background: Semaphore::new(MAX_BACKGROUND),
            quality: Mutex::new(QualityPreference::default()),
            offline: AtomicBool::new(false),
        }
    }

    /// Options `YoutubeDL` du worker, équivalentes aux arguments de l'exécutable.
    pub fn worker_options(&self) -> Option<Value> {
        if !self.worker_allowed {
            return None;
        }
        let mut options = json!({
            "quiet": true,
            "no_warnings": true,
            "noplaylist": true,
            "skip_download": true,
            "format": "bestaudio/best",
        });
        if self.node {
            options["js_runtimes"] = json!({ "node": {} });
        }
        if let Some(session) = &self.session {
            // Le worker range ces cookies dans le pot de yt-dlp.
            options["ytmr_cookies"] = json!(session);
        } else if let Some(browser) = &self.cookies {
            let (name, profile) = match browser.split_once(':') {
                Some((name, profile)) => (name, Some(profile)),
                None => (browser.as_str(), None),
            };
            options["cookiesfrombrowser"] = json!([name, profile]);
        }
        if self.session.is_some() || self.cookies.is_some() {
            options["extractor_args"] = json!({ "youtube": {
                "player_client": [ACCOUNT_CLIENT],
                "player_skip": ACCOUNT_SKIP.split(',').collect::<Vec<_>>(),
            }});
        }
        Some(options)
    }

    /// Branche le worker : les résolutions « qualité » passeront par lui.
    pub fn attach_worker(&self, worker: Worker) {
        *lock(&self.worker) = Some(Arc::new(worker));
    }

    /// Session YouTube du navigateur configuré, lue par le worker.
    pub async fn browser_cookie(&self) -> Result<String, String> {
        // Connexion faite dans ytmr : pas besoin du navigateur.
        if let Some(session) = &self.session {
            return Ok(crate::session::cookie_header(session));
        }
        let browser = self.cookies.clone().ok_or("compte YouTube non connecté")?;
        let worker = self
            .live_worker()
            .ok_or("le worker yt-dlp n'est pas prêt")?;
        worker.browser_cookie(&browser).await
    }

    fn live_worker(&self) -> Option<Arc<Worker>> {
        let mut slot = lock(&self.worker);
        if slot.as_ref().is_some_and(|w| !w.is_alive()) {
            *slot = None;
        }
        slot.clone()
    }

    fn remember(&self, video_id: &str, source: &MediaSource) {
        let mut cache = lock(&self.cache);
        cache.retain(|_, s| s.is_fresh());
        if cache.len() >= MAX_CACHED
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, s)| s.expires_at)
                .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(video_id.to_owned(), source.clone());
        if let Some(path) = &self.cache_file {
            save_cache(path, &cache);
        }
    }

    /// Résolution effective : file d'attente, yt-dlp, attente de disponibilité.
    async fn resolve_now(
        &self,
        video_id: &str,
        mode: StartMode,
        priority: Priority,
    ) -> Result<MediaSource, PlayerError> {
        let semaphore = match priority {
            Priority::Urgent => &self.urgent,
            Priority::Background => &self.background,
        };
        let (source, available_at) = {
            let _slot = semaphore.acquire().await.map_err(|_| PlayerError::Closed)?;
            match self.live_worker().filter(|_| mode == StartMode::Quality) {
                Some(worker) => match self.run_worker(&worker, video_id).await {
                    Ok(resolved) => resolved,
                    Err(e) => {
                        tracing::warn!(error = %e, video_id, "worker en échec, repli sur l'exécutable");
                        self.run(video_id, mode, priority).await?
                    }
                },
                None => self.run(video_id, mode, priority).await?,
            }
        };
        if let Some(wait) = available_at.and_then(|t| t.duration_since(SystemTime::now()).ok()) {
            tracing::debug!(
                video_id,
                wait_ms = wait.as_millis(),
                "attente de disponibilité du flux"
            );
            tokio::time::sleep(wait).await;
        }
        self.remember(video_id, &source);
        Ok(source)
    }

    async fn run_worker(
        &self,
        worker: &Worker,
        video_id: &str,
    ) -> Result<(MediaSource, Option<SystemTime>), PlayerError> {
        let started = std::time::Instant::now();
        let url = format!("https://music.youtube.com/watch?v={video_id}");
        let format = lock(&self.quality).format();
        let value = worker
            .extract(&url, format)
            .await
            .map_err(|e| PlayerError::Resolve(format!("{video_id} : {e}")))?;
        let info: YtDlpInfo = serde_json::from_value(value)
            .map_err(|e| PlayerError::Resolve(format!("réponse du worker illisible : {e}")))?;
        let resolved = info.into_source()?;
        tracing::debug!(
            video_id,
            quality = ?resolved.0.quality,
            elapsed_ms = started.elapsed().as_millis(),
            "flux résolu (worker)"
        );
        Ok(resolved)
    }

    /// Lance yt-dlp ; rend la source et l'instant à partir duquel elle est utilisable.
    async fn run(
        &self,
        video_id: &str,
        mode: StartMode,
        priority: Priority,
    ) -> Result<(MediaSource, Option<SystemTime>), PlayerError> {
        let url = format!("https://music.youtube.com/watch?v={video_id}");

        let mut cmd = crate::process::command(&self.ytdlp);
        if priority == Priority::Background {
            crate::process::below_normal_priority(&mut cmd);
        }
        cmd.args([
            "-J",
            "--no-warnings",
            "--no-playlist",
            "-f",
            lock(&self.quality).format(),
        ])
        .args(&self.common_args);
        let client = match mode {
            StartMode::Fast => self.fast_client.as_deref(),
            StartMode::Quality => None,
        };
        if let Some(client) = client {
            cmd.arg("--extractor-args")
                .arg(format!("youtube:player_client={client}"));
        } else if self.cookies.is_some() {
            cmd.arg("--extractor-args").arg(format!(
                "youtube:player_client={ACCOUNT_CLIENT};player_skip={ACCOUNT_SKIP}"
            ));
        }
        cmd.arg("--")
            .arg(&url)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let started = std::time::Instant::now();
        let output = tokio::time::timeout(RESOLVE_TIMEOUT, cmd.output())
            .await
            .map_err(|_| PlayerError::Resolve(format!("yt-dlp bloqué sur {video_id}")))?
            .map_err(|e| PlayerError::Resolve(format!("lancement de yt-dlp : {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr
                .lines()
                .rfind(|l| l.contains("ERROR"))
                .unwrap_or("échec sans message");
            return Err(PlayerError::Resolve(format!("{video_id} : {reason}")));
        }

        let info: YtDlpInfo = serde_json::from_slice(&output.stdout)
            .map_err(|e| PlayerError::Resolve(format!("JSON yt-dlp illisible : {e}")))?;
        let resolved = info.into_source()?;
        tracing::debug!(
            video_id,
            ?mode,
            quality = ?resolved.0.quality,
            elapsed_ms = started.elapsed().as_millis(),
            "flux résolu"
        );
        Ok(resolved)
    }
}

impl Resolver for YtDlpResolver {
    fn set_quality(&self, quality: QualityPreference) {
        let changed = std::mem::replace(&mut *lock(&self.quality), quality) != quality;
        if changed {
            // Les adresses déjà résolues sont d'une autre qualité.
            let mut cache = lock(&self.cache);
            cache.clear();
            if let Some(path) = &self.cache_file {
                save_cache(path, &cache);
            }
            tracing::info!(?quality, "qualité changée");
        }
    }

    fn keep_offline(&self, video_ids: &[String], keep: bool) {
        let Some(audio) = &self.audio else {
            return;
        };
        audio.keep(video_ids, keep);
        if !keep || self.offline.load(Ordering::Relaxed) {
            return;
        }
        // Adresse déjà connue mais fichier pas encore gardé : on le télécharge.
        let known: Vec<(String, MediaSource)> = {
            let cache = lock(&self.cache);
            video_ids
                .iter()
                .filter(|id| !audio.has(id))
                .filter_map(|id| {
                    cache
                        .get(id)
                        .filter(|s| s.is_fresh() && s.quality == StreamQuality::High)
                        .map(|s| (id.clone(), s.clone()))
                })
                .collect()
        };
        for (id, source) in known {
            audio.store(&id, &source);
        }
    }

    fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::Relaxed);
        tracing::info!(offline, "mode hors ligne");
    }

    fn cached(&self, video_id: &str) -> Option<MediaSource> {
        // Déjà sur le disque : rien à résoudre, rien à télécharger.
        let offline = self.offline.load(Ordering::Relaxed);
        // Hors ligne : seulement les morceaux téléchargés exprès (comme l'affiche
        // l'interface), pas le cache automatique des écoutes récentes.
        if let Some(audio) = &self.audio
            && (!offline || audio.is_pinned(video_id))
            && let Some(local) = audio.get(video_id)
        {
            return Some(local);
        }
        // Hors ligne, une adresse (même valide) exigerait le réseau.
        if offline {
            return None;
        }
        let mut cache = lock(&self.cache);
        match cache.get(video_id) {
            Some(source) if source.is_fresh() => Some(source.clone()),
            Some(_) => {
                cache.remove(video_id);
                None
            }
            None => None,
        }
    }

    async fn resolve(
        &self,
        video_id: &str,
        mode: StartMode,
        priority: Priority,
    ) -> Result<MediaSource, PlayerError> {
        if let Some(source) = self.cached(video_id) {
            // Une source rapide en cache ne suffit pas quand on veut la qualité.
            if mode == StartMode::Fast || source.quality == StreamQuality::High {
                return Ok(source);
            }
        }
        if self.offline.load(Ordering::Relaxed) {
            return Err(PlayerError::Resolve(
                "mode hors ligne : morceau non téléchargé".into(),
            ));
        }

        // Déjà en cours ailleurs : on attend ce résultat plutôt que de relancer yt-dlp.
        let joined = {
            let mut inflight = lock(&self.inflight);
            match inflight.get(video_id) {
                Some(rx) => Err(rx.clone()),
                None => {
                    let (tx, rx) = watch::channel(None);
                    inflight.insert(video_id.to_owned(), rx);
                    Ok(tx)
                }
            }
        };
        let tx = match joined {
            Ok(tx) => tx,
            Err(mut rx) => {
                tracing::debug!(
                    video_id,
                    ?priority,
                    "résolution déjà en cours, on la partage"
                );
                let shared = rx
                    .wait_for(Option::is_some)
                    .await
                    .map_err(|_| PlayerError::Resolve(format!("{video_id} : résolution annulée")))?
                    .clone();
                return match shared {
                    Some(Ok(source)) => Ok(source),
                    Some(Err(e)) => Err(PlayerError::Resolve(e)),
                    None => Err(PlayerError::Resolve(format!(
                        "{video_id} : résolution annulée"
                    ))),
                };
            }
        };

        // Retire l'entrée « en cours » quoi qu'il arrive (erreur, annulation).
        struct Done<'a>(&'a Mutex<HashMap<String, Shared>>, &'a str);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                lock(self.0).remove(self.1);
            }
        }
        let _done = Done(&self.inflight, video_id);

        let result = self.resolve_now(video_id, mode, priority).await;
        // Un seul téléchargement par morceau : ceux préparés en fond passent
        // par le cache (mpv les lira ensuite depuis le disque) ; un morceau
        // urgent (clic, suivant préchargé) est lu en direct par mpv, qui le
        // télécharge déjà : le cache le téléchargeait une seconde fois.
        // Les morceaux gardés hors ligne sont toujours téléchargés.
        if let (Some(audio), Ok(source)) = (&self.audio, &result)
            && (priority == Priority::Background || audio.is_pinned(video_id))
        {
            audio.store(video_id, source);
        }
        let _ = tx.send(Some(
            result
                .as_ref()
                .map(Clone::clone)
                .map_err(ToString::to_string),
        ));
        result
    }
}

/// Adresses encore valables d'un lancement précédent.
fn load_cache(path: &Path) -> HashMap<String, MediaSource> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut cache: HashMap<String, MediaSource> = serde_json::from_str(&text).unwrap_or_default();
    cache.retain(|_, s| s.is_fresh());
    tracing::info!(entries = cache.len(), "adresses de flux reprises du disque");
    cache
}

fn save_cache(path: &Path, cache: &HashMap<String, MediaSource>) {
    let Ok(json) = serde_json::to_vec(cache) else {
        return;
    };
    let path = path.to_path_buf();
    // Petit fichier (< 500 Ko), écrit hors du fil courant.
    std::thread::spawn(move || {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    });
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `["extractor-args=youtube:x", "force-ipv4="]` → `["--extractor-args", "youtube:x", "--force-ipv4"]`.
fn raw_options_to_args(options: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    for option in options {
        let (key, value) = option.split_once('=').unwrap_or((option, ""));
        args.push(format!("--{key}"));
        if !value.is_empty() {
            args.push(value.to_owned());
        }
    }
    args
}

/// Sous-ensemble utile de `yt-dlp -J` quand un seul format est choisi.
#[derive(Deserialize)]
struct YtDlpInfo {
    url: Option<String>,
    vcodec: Option<String>,
    available_at: Option<u64>,
    #[serde(default)]
    http_headers: HashMap<String, String>,
}

impl YtDlpInfo {
    fn into_source(mut self) -> Result<(MediaSource, Option<SystemTime>), PlayerError> {
        let url = self
            .url
            .ok_or_else(|| PlayerError::Resolve("yt-dlp n'a pas donné d'URL".into()))?;
        let audio_only = self.vcodec.as_deref() == Some("none");
        let expires_at = expire_param(&url)
            .map(unix)
            .unwrap_or_else(|| SystemTime::now() + DEFAULT_LIFETIME);
        let source = MediaSource {
            user_agent: self.http_headers.remove("User-Agent"),
            url,
            quality: if audio_only {
                StreamQuality::High
            } else {
                StreamQuality::Low
            },
            expires_at: Some(expires_at),
        };
        Ok((source, self.available_at.map(unix)))
    }
}

fn unix(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// Lit `expire=<unix>` dans une URL googlevideo.
fn expire_param(url: &str) -> Option<u64> {
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("expire="))
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_raw_options() {
        let args = raw_options_to_args(&[
            "extractor-args=youtube:player_client=web".to_owned(),
            "force-ipv4=".to_owned(),
        ]);
        assert_eq!(
            args,
            [
                "--extractor-args",
                "youtube:player_client=web",
                "--force-ipv4"
            ]
        );
    }

    #[test]
    fn parses_expire() {
        assert_eq!(
            expire_param("https://x.googlevideo.com/videoplayback?expire=1793293060&ei=a"),
            Some(1_793_293_060)
        );
        assert_eq!(expire_param("https://x/videoplayback?ei=a"), None);
    }

    #[test]
    fn classifies_quality_from_yt_dlp_json() {
        let json = r#"{"url": "https://x/videoplayback?expire=2000000000", "vcodec": "none",
                       "available_at": 1900000000, "http_headers": {"User-Agent": "UA"}}"#;
        let info: YtDlpInfo = serde_json::from_str(json).unwrap();
        let (source, available_at) = info.into_source().unwrap();
        assert_eq!(source.quality, StreamQuality::High);
        assert_eq!(source.user_agent.as_deref(), Some("UA"));
        assert_eq!(available_at, Some(unix(1_900_000_000)));
        assert_eq!(source.expires_at, Some(unix(2_000_000_000)));
    }
}
