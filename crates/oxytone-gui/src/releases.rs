//! « Nouveautés de tes artistes » sur l'accueil.
//!
//! En clair : une fois par jour, en fond, oxytone ouvre la page de chacun des
//! artistes suivis (40 au plus) et garde leurs sorties de l'année (albums,
//! singles, EP). Elles forment une rangée de l'accueil ; une sortie jamais vue
//! auparavant est signalée par un message.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iced::Task;
use iced::futures::StreamExt;
use oxytone_catalog::AlbumSummary;

use crate::app::{App, CARD, Message};

/// Nouvelle vérification au plus une fois par…
const REFRESH: Duration = Duration::from_secs(12 * 3600);
/// Artistes regardés (les suivis les plus récents d'abord).
const MAX_ARTISTS: usize = 40;
/// Sorties gardées dans la rangée.
const MAX_RELEASES: usize = 24;

#[derive(Debug, Clone)]
pub enum Rel {
    Loaded(Result<Vec<AlbumSummary>, String>),
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Cache {
    /// Dernière vérification (secondes Unix).
    at: u64,
    items: Vec<AlbumSummary>,
    /// Sorties déjà signalées.
    seen: HashSet<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl App {
    fn releases_file(&self) -> Option<PathBuf> {
        self.lib.cache_dir.as_ref().map(|d| d.join("releases.json"))
    }

    fn releases_cache(&self) -> Cache {
        self.releases_file()
            .and_then(|p| crate::collection::read_json(&p))
            .unwrap_or_default()
    }

    /// Compte prêt : la rangée en cache tout de suite, la vérification si elle date.
    pub fn load_releases(&mut self) -> Task<Message> {
        let cache = self.releases_cache();
        self.releases = cache.items.clone();
        let covers = self.want_release_covers();
        let fresh = now().saturating_sub(cache.at) < REFRESH.as_secs() && !cache.items.is_empty();
        if fresh || self.lib.offline_mode {
            return covers;
        }
        let (Some(account), Some(catalog)) = (self.lib.ready(), self.catalog()) else {
            return covers;
        };
        let year = crate::settings::current_year();
        let fetch = Task::perform(
            async move {
                let artists = account.library_artists().await.map_err(|e| e.to_string())?;
                let pages: Vec<_> =
                    iced::futures::stream::iter(artists.into_iter().take(MAX_ARTISTS))
                        .map(|a| {
                            let catalog = std::sync::Arc::clone(&catalog);
                            async move { catalog.artist(&a.id).await.ok() }
                        })
                        .buffered(4)
                        .collect()
                        .await;
                Ok(recent(pages.into_iter().flatten(), year))
            },
            |r| Message::Releases(Rel::Loaded(r)),
        );
        Task::batch([covers, fetch])
    }

    pub fn update_releases(&mut self, message: Rel) -> Task<Message> {
        match message {
            Rel::Loaded(Ok(items)) => {
                let mut cache = self.releases_cache();
                let first_time = cache.seen.is_empty();
                let new: Vec<&AlbumSummary> = items
                    .iter()
                    .filter(|a| !cache.seen.contains(&a.id))
                    .collect();
                let toast = match new.first() {
                    // Rien à signaler au tout premier passage (tout serait « nouveau »).
                    Some(a) if !first_time => {
                        let more = new.len() - 1;
                        let text = if more > 0 {
                            format!("Nouveau : « {} » de {} (+ {more})", a.title, a.artist)
                        } else {
                            format!("Nouveau : « {} » de {}", a.title, a.artist)
                        };
                        self.show_toast(text)
                    }
                    _ => Task::none(),
                };
                cache.seen.extend(items.iter().map(|a| a.id.clone()));
                cache.items = items;
                cache.at = now();
                if let Some(path) = self.releases_file() {
                    crate::library::write_json(&path, &cache);
                }
                self.releases = cache.items;
                Task::batch([toast, self.want_release_covers()])
            }
            Rel::Loaded(Err(e)) => {
                tracing::debug!("nouveautés des artistes : {e}");
                Task::none()
            }
        }
    }

    fn want_release_covers(&mut self) -> Task<Message> {
        let ids: Vec<_> = self
            .releases
            .iter()
            .map(|a| self.url_cover(a.thumbnail_url.as_deref()))
            .collect();
        Task::batch(ids.into_iter().map(|id| self.want_cover(id, CARD)))
    }
}

/// Sorties de l'année (et de la fin de l'année passée en janvier-février) :
/// les deux plus récentes de chaque artiste, dans l'ordre des artistes.
fn recent(
    pages: impl Iterator<Item = oxytone_catalog::Artist>,
    (year, month): (u32, u32),
) -> Vec<AlbumSummary> {
    let recent_enough = |a: &AlbumSummary| match a.year.trim().parse::<u32>() {
        Ok(y) => y == year || (month <= 2 && y + 1 == year),
        Err(_) => false,
    };
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    for artist in pages {
        // Sur la page, chaque rangée commence par la sortie la plus récente.
        let mut picks: Vec<AlbumSummary> = artist
            .singles
            .iter()
            .take(2)
            .chain(artist.albums.iter().take(2))
            .filter(|a| recent_enough(a))
            .cloned()
            .collect();
        picks.truncate(2);
        for mut a in picks {
            if a.artist.trim().is_empty() {
                a.artist = artist.name.clone();
            }
            if ids.insert(a.id.clone()) {
                out.push(a);
            }
        }
    }
    out.truncate(MAX_RELEASES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(id: &str, year: &str) -> AlbumSummary {
        AlbumSummary {
            id: id.into(),
            title: format!("titre {id}"),
            artist: String::new(),
            year: year.into(),
            kind: "Single".into(),
            thumbnail_url: None,
        }
    }

    fn artist(
        name: &str,
        singles: Vec<AlbumSummary>,
        albums: Vec<AlbumSummary>,
    ) -> oxytone_catalog::Artist {
        oxytone_catalog::Artist {
            id: format!("UC{name}"),
            name: name.into(),
            description: None,
            subscribers: None,
            thumbnail_url: None,
            songs: Vec::new(),
            albums,
            singles,
        }
    }

    #[test]
    fn keeps_this_year_releases_two_per_artist() {
        let pages = vec![
            artist(
                "A",
                vec![
                    release("a1", "2026"),
                    release("a2", "2026"),
                    release("a3", "2026"),
                ],
                vec![],
            ),
            artist(
                "B",
                vec![release("b1", "2024")],
                vec![release("b2", "2026")],
            ),
        ];
        let got = recent(pages.into_iter(), (2026, 10));
        let ids: Vec<&str> = got.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["a1", "a2", "b2"]);
        assert_eq!(got[2].artist, "B", "nom d'artiste complété");
        // En janvier, les sorties de l'année passée comptent encore.
        let pages = vec![artist("C", vec![release("c1", "2025")], vec![])];
        assert_eq!(recent(pages.into_iter(), (2026, 1)).len(), 1);
    }
}
