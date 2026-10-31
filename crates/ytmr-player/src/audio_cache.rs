//! Cache audio sur disque.
//!
//! En clair : la première écoute d'un morceau passe par yt-dlp (2 à 5 s pour
//! obtenir l'adresse), puis on garde le fichier audio sur le disque. Les fois
//! suivantes, mpv lit ce fichier directement : le son part tout de suite, sans
//! yt-dlp ni réseau. Les plus anciens fichiers sont effacés au-delà de la
//! taille fixée (`player.audio_cache_mb`), sauf ceux gardés hors ligne
//! (playlists épinglées : `pinned.json`), qui ne partent jamais.
//!
//! On ne réimplémente rien de l'extraction : on télécharge l'adresse que
//! yt-dlp a donnée, par morceaux de 1 Mo (comme yt-dlp, YouTube bride les
//! requêtes d'un seul tenant), avec le User-Agent qu'il a indiqué.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::resolve::{MediaSource, StreamQuality};

const EXTENSION: &str = "audio";
/// Liste des morceaux gardés hors ligne, dans le dossier du cache.
const PINNED_FILE: &str = "pinned.json";
const CHUNK: u64 = 1 << 20;
/// Un morceau de plus de 30 min (mix, album entier) n'est pas gardé.
const MAX_FILE: u64 = 60 << 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct AudioCache {
    dir: PathBuf,
    max_bytes: u64,
    http: reqwest::Client,
    /// Téléchargements en cours (`video_id`).
    inflight: Mutex<HashSet<String>>,
    /// Un seul téléchargement à la fois : la lecture garde la bande passante.
    lane: Semaphore,
    /// Morceaux gardés hors ligne : jamais effacés pour faire de la place.
    pinned: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for AudioCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioCache")
            .field("dir", &self.dir)
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

impl AudioCache {
    /// `max_mb` = 0 : pas de cache. Le dossier est créé au besoin.
    pub fn new(dir: PathBuf, max_mb: u64) -> Option<Arc<Self>> {
        if max_mb == 0 || std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        // Des téléchargements interrompus au dernier arrêt.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "part") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let http = reqwest::Client::builder()
            // Google ajoute des en-têtes énormes : la limite de 16 Ko ne suffit plus.
            .http2_max_header_list_size(256 * 1024)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok()?;
        let pinned = std::fs::read_to_string(dir.join(PINNED_FILE))
            .ok()
            .and_then(|t| serde_json::from_str::<HashSet<String>>(&t).ok())
            .unwrap_or_default();
        Some(Arc::new(Self {
            dir,
            max_bytes: max_mb << 20,
            http,
            inflight: Mutex::default(),
            lane: Semaphore::new(1),
            pinned: Mutex::new(pinned),
        }))
    }

    /// Garde (`true`) ou non des morceaux hors ligne. Retirés, ils redeviennent
    /// des morceaux en cache ordinaires (effacés un jour pour faire de la place).
    pub fn keep(&self, video_ids: &[String], keep: bool) {
        let mut pinned = lock(&self.pinned);
        for id in video_ids {
            if keep {
                pinned.insert(id.clone());
            } else {
                pinned.remove(id);
            }
        }
        if let Ok(json) = serde_json::to_string(&*pinned) {
            let _ = std::fs::write(self.dir.join(PINNED_FILE), json);
        }
    }

    /// Gardé exprès pour l'écoute hors ligne (« téléchargé »).
    pub fn is_pinned(&self, video_id: &str) -> bool {
        lock(&self.pinned).contains(video_id)
    }

    /// Le fichier du morceau est déjà sur le disque.
    pub fn has(&self, video_id: &str) -> bool {
        self.file(video_id).is_some_and(|p| p.exists())
    }

    fn file(&self, video_id: &str) -> Option<PathBuf> {
        // Un identifiant YouTube : 11 caractères sûrs. Rien d'autre ne devient un nom de fichier.
        let safe = video_id.len() == 11
            && video_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        safe.then(|| self.dir.join(format!("{video_id}.{EXTENSION}")))
    }

    /// Fichier local du morceau, s'il a déjà été écouté.
    pub fn get(&self, video_id: &str) -> Option<MediaSource> {
        let path = self.file(video_id)?;
        let file = std::fs::File::options().append(true).open(&path).ok()?;
        // Date de dernière écoute : c'est elle qui décide quoi effacer en premier.
        let _ = file.set_modified(SystemTime::now());
        Some(MediaSource {
            url: path.to_string_lossy().into_owned(),
            user_agent: None,
            quality: StreamQuality::High,
            expires_at: None,
        })
    }

    /// Garde le morceau en tâche de fond (si l'adresse est un vrai fichier audio).
    pub fn store(self: &Arc<Self>, video_id: &str, source: &MediaSource) {
        if source.quality != StreamQuality::High || !source.url.starts_with("https://") {
            return;
        }
        let Some(path) = self.file(video_id) else {
            return;
        };
        if path.exists() || !lock(&self.inflight).insert(video_id.to_owned()) {
            return;
        }
        let cache = Arc::clone(self);
        let video_id = video_id.to_owned();
        let source = source.clone();
        tokio::spawn(async move {
            let _lane = cache.lane.acquire().await;
            match cache.download(&source, &path).await {
                Ok(size) => {
                    tracing::debug!(video_id, size, "morceau gardé sur le disque");
                    cache.evict();
                }
                Err(e) => tracing::debug!(video_id, error = %e, "morceau non gardé"),
            }
            lock(&cache.inflight).remove(&video_id);
        });
    }

    async fn download(&self, source: &MediaSource, path: &Path) -> Result<u64, String> {
        let total = content_length(&source.url);
        if total.is_some_and(|t| t > MAX_FILE) {
            return Err("trop long".into());
        }
        let part = path.with_extension("part");
        let mut out = tokio::fs::File::create(&part)
            .await
            .map_err(|e| e.to_string())?;
        let result = async {
            let mut done: u64 = 0;
            loop {
                let end = done + CHUNK - 1;
                let mut request = self
                    .http
                    .get(&source.url)
                    .header(reqwest::header::RANGE, format!("bytes={done}-{end}"));
                if let Some(ua) = &source.user_agent {
                    request = request.header(reqwest::header::USER_AGENT, ua);
                }
                let mut response = request.send().await.map_err(|e| e.to_string())?;
                if !response.status().is_success() {
                    return Err(format!("HTTP {}", response.status()));
                }
                let mut received = 0;
                while let Some(bytes) = response.chunk().await.map_err(|e| e.to_string())? {
                    received += bytes.len() as u64;
                    crate::traffic::add(bytes.len() as u64);
                    out.write_all(&bytes).await.map_err(|e| e.to_string())?;
                }
                done += received;
                if done > MAX_FILE {
                    return Err("trop long".into());
                }
                let finished = match total {
                    Some(total) => done >= total,
                    None => received < CHUNK,
                };
                if finished || received == 0 {
                    break;
                }
            }
            out.flush().await.map_err(|e| e.to_string())?;
            if total.is_some_and(|t| done < t) || done == 0 {
                return Err("téléchargement incomplet".into());
            }
            Ok(done)
        }
        .await;
        drop(out);
        match result {
            Ok(size) => {
                tokio::fs::rename(&part, path)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(size)
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                Err(e)
            }
        }
    }

    /// Efface les morceaux écoutés il y a le plus longtemps, au-delà de la limite.
    fn evict(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == EXTENSION))
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                Some((meta.modified().ok()?, meta.len(), e.path()))
            })
            .collect();
        let mut used: u64 = files.iter().map(|(_, len, _)| len).sum();
        files.sort_by_key(|(at, _, _)| *at);
        let pinned = lock(&self.pinned).clone();
        for (_, len, path) in files {
            if used <= self.max_bytes {
                break;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if pinned.contains(id) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                used -= len;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Taille annoncée dans l'adresse (`clen=`), quand YouTube la donne.
fn content_length(url: &str) -> Option<u64> {
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("clen="))
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_length_from_url() {
        assert_eq!(
            content_length("https://x.googlevideo.com/videoplayback?a=1&clen=4321&b=2"),
            Some(4321)
        );
        assert_eq!(content_length("https://x/videoplayback?a=1"), None);
    }

    /// `YTMR_TEST_URL` / `YTMR_TEST_UA` : adresse et User-Agent donnés par yt-dlp.
    #[tokio::test]
    #[ignore = "nécessite le réseau et une adresse fraîche"]
    async fn downloads_a_resolved_stream() {
        let dir = std::env::temp_dir().join("ytmr-audio-cache-net");
        let cache = AudioCache::new(dir, 100).unwrap();
        let source = MediaSource {
            url: std::env::var("YTMR_TEST_URL").unwrap(),
            user_agent: std::env::var("YTMR_TEST_UA").ok(),
            quality: StreamQuality::High,
            expires_at: None,
        };
        let path = cache.file("Jb6gcoR266U").unwrap();
        let _ = std::fs::remove_file(&path);
        let started = std::time::Instant::now();
        let size = cache.download(&source, &path).await.unwrap();
        println!(
            "{size} octets en {:?}, attendu {:?}",
            started.elapsed(),
            content_length(&source.url)
        );
        assert!(cache.get("Jb6gcoR266U").is_some());
    }

    #[test]
    fn only_youtube_ids_become_file_names() {
        let dir = std::env::temp_dir().join("ytmr-audio-cache-test");
        let cache = AudioCache::new(dir, 10).unwrap();
        assert!(cache.file("Jb6gcoR266U").is_some());
        assert!(cache.file("../../etc/pw").is_none());
        assert!(cache.file("Jb6gcoR266U.").is_none());
        assert!(cache.get("Jb6gcoR266U").is_none());
    }
}
