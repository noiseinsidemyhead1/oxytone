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

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::PlayerError;
use crate::deps::{Dependencies, JsRuntime};

/// Au-delà, yt-dlp est considéré bloqué.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
/// Une adresse est jetée du cache un peu avant son expiration réelle.
const EXPIRY_MARGIN: Duration = Duration::from_secs(10 * 60);
/// Durée de vie supposée si l'URL ne donne pas `expire=`.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(4 * 3600);
/// Nombre de yt-dlp lancés en même temps.
const MAX_CONCURRENT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    /// Son le plus vite possible, qualité moindre.
    Fast,
    /// Meilleure qualité, quitte à attendre quelques secondes.
    Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamQuality {
    /// Audio seul, meilleur format disponible.
    High,
    /// Format combiné de repli (AAC ~96 kb/s).
    Low,
    /// Inconnue : mpv résout lui-même via son hook yt-dlp (repli de secours).
    Unknown,
}

/// Ce qu'on donne à mpv pour jouer un morceau.
#[derive(Debug, Clone, PartialEq, Eq)]
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
pub trait Resolver: Send + Sync + 'static {
    /// Source déjà connue et encore valable, sans rien lancer.
    fn cached(&self, video_id: &str) -> Option<MediaSource>;
    /// Résout (ou relit le cache). Ne rend la main que quand l'URL est utilisable.
    fn resolve(
        &self,
        video_id: &str,
        mode: StartMode,
    ) -> impl Future<Output = Result<MediaSource, PlayerError>> + Send;
}

pub struct YtDlpResolver {
    ytdlp: PathBuf,
    /// Arguments communs à tous les appels (moteur JS, options de la config).
    common_args: Vec<String>,
    fast_client: Option<String>,
    cache: Mutex<HashMap<String, MediaSource>>,
    slots: Semaphore,
}

impl YtDlpResolver {
    /// `fast_client` : client YouTube pour [`StartMode::Fast`] ; `None` = toujours la qualité.
    /// `raw_options` : `clé=valeur` de la config, ajoutées à chaque appel.
    pub fn new(deps: &Dependencies, fast_client: Option<String>, raw_options: &[String]) -> Self {
        let mut common_args = Vec::new();
        if deps.js_runtime == Some(JsRuntime::Node) {
            common_args.extend(["--js-runtimes".to_owned(), "node".to_owned()]);
        }
        common_args.extend(raw_options_to_args(raw_options));
        Self {
            ytdlp: deps.ytdlp.path.clone(),
            common_args,
            fast_client,
            cache: Mutex::default(),
            slots: Semaphore::new(MAX_CONCURRENT),
        }
    }

    /// Lance yt-dlp ; rend la source et l'instant à partir duquel elle est utilisable.
    async fn run(
        &self,
        video_id: &str,
        mode: StartMode,
    ) -> Result<(MediaSource, Option<SystemTime>), PlayerError> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| PlayerError::Closed)?;
        let url = format!("https://music.youtube.com/watch?v={video_id}");

        let mut cmd = crate::process::command(&self.ytdlp);
        cmd.args([
            "-J",
            "--no-warnings",
            "--no-playlist",
            "-f",
            "bestaudio/best",
        ])
        .args(&self.common_args);
        let client = match mode {
            StartMode::Fast => self.fast_client.as_deref(),
            StartMode::Quality => None,
        };
        if let Some(client) = client {
            cmd.arg("--extractor-args")
                .arg(format!("youtube:player_client={client}"));
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
    fn cached(&self, video_id: &str) -> Option<MediaSource> {
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

    async fn resolve(&self, video_id: &str, mode: StartMode) -> Result<MediaSource, PlayerError> {
        if let Some(source) = self.cached(video_id) {
            // Une source rapide en cache ne suffit pas quand on veut la qualité.
            if mode == StartMode::Fast || source.quality == StreamQuality::High {
                return Ok(source);
            }
        }
        let (source, available_at) = self.run(video_id, mode).await?;
        if let Some(wait) = available_at.and_then(|t| t.duration_since(SystemTime::now()).ok()) {
            tracing::debug!(
                video_id,
                wait_ms = wait.as_millis(),
                "attente de disponibilité du flux"
            );
            tokio::time::sleep(wait).await;
        }
        lock(&self.cache).insert(video_id.to_owned(), source.clone());
        Ok(source)
    }
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
