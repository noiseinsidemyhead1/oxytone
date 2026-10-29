//! Catalogue YouTube Music : recherche, albums, playlists, radio.
//!
//! En clair : c'est l'annuaire. Il trouve les morceaux mais ne les joue pas.
//! Enveloppe `ytmapi-rs` pour que le reste du code n'en dépende pas directement :
//! si YouTube change son API interne, seul ce crate est à adapter.

use reqwest::header::{COOKIE, HeaderMap, HeaderValue};
use thiserror::Error;
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::common::{Thumbnail, VideoID, YoutubeID};
use ytmapi_rs::parse::{SearchResultSong, WatchPlaylistTrack};
use ytmapi_rs::{YtMusic, YtMusicBuilder};
use ytmr_core::engine::BoxError;
use ytmr_core::track::parse_duration;
use ytmr_core::{Radio, Track};

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("YouTube Music : {0}")]
    Api(#[from] ytmapi_rs::Error),
    #[error("client HTTP : {0}")]
    Http(#[from] reqwest::Error),
}

/// Refus des cookies non essentiels. Sans lui, depuis l'UE, la page d'accueil
/// redirige vers `consent.youtube.com` (400, corps vide) et ytmapi-rs ne trouve
/// pas son `ytcfg`.
const CONSENT_COOKIE: &str = "SOCS=CAI";

/// Accès au catalogue. Garde une connexion HTTP réutilisable : à créer une fois.
pub struct Catalog {
    yt: YtMusic<NoAuthToken>,
}

impl Catalog {
    /// Accès sans compte : recherche, albums, radio. Pas de bibliothèque perso.
    pub async fn new_unauthenticated() -> Result<Self, CatalogError> {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static(CONSENT_COOKIE));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;
        let client = ytmapi_rs::Client::new_from_reqwest_client(http);
        Ok(Self {
            yt: YtMusicBuilder::new_with_client(client).build().await?,
        })
    }

    /// Recherche limitée aux morceaux (pas de vidéos, albums ni playlists).
    pub async fn search_songs(&self, query: &str) -> Result<Vec<Track>, CatalogError> {
        let results = self.yt.search_songs(query).await?;
        tracing::debug!(query, count = results.len(), "recherche de morceaux");
        Ok(results.into_iter().map(track_from_song).collect())
    }

    /// « Radio » du morceau : titres similaires, comme l'autoplay de YouTube Music.
    /// Le premier résultat est en général le morceau lui-même.
    pub async fn radio(&self, video_id: &str) -> Result<Vec<Track>, CatalogError> {
        let results = self
            .yt
            .get_watch_playlist_from_video_id(VideoID::from_raw(video_id.to_owned()))
            .await?;
        tracing::debug!(video_id, count = results.len(), "radio");
        Ok(results.into_iter().map(track_from_watch).collect())
    }
}

impl Radio for Catalog {
    async fn radio(&self, video_id: &str) -> Result<Vec<Track>, BoxError> {
        Ok(Catalog::radio(self, video_id).await?)
    }
}

fn track_from_watch(t: WatchPlaylistTrack) -> Track {
    Track {
        video_id: t.video_id.get_raw().to_owned(),
        title: t.title,
        artist: t.author,
        album: None,
        duration: parse_duration(&t.duration),
        thumbnail_url: largest_thumbnail(&t.thumbnails),
    }
}

fn track_from_song(song: SearchResultSong) -> Track {
    Track {
        video_id: song.video_id.get_raw().to_owned(),
        title: song.title,
        artist: song.artist,
        album: song.album.map(|a| a.name),
        duration: parse_duration(&song.duration),
        thumbnail_url: largest_thumbnail(&song.thumbnails),
    }
}

fn largest_thumbnail(thumbnails: &[Thumbnail]) -> Option<String> {
    thumbnails
        .iter()
        .max_by_key(|t| t.width * t.height)
        .map(|t| t.url.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test réseau : `cargo test -p ytmr-catalog -- --ignored`.
    #[tokio::test]
    #[ignore = "nécessite le réseau"]
    async fn search_returns_playable_tracks() {
        let catalog = Catalog::new_unauthenticated().await.unwrap();
        let tracks = catalog
            .search_songs("daft punk around the world")
            .await
            .unwrap();

        assert!(!tracks.is_empty());
        let first = &tracks[0];
        assert_eq!(first.video_id.len(), 11);
        assert!(first.duration.is_some());
    }

    #[tokio::test]
    #[ignore = "nécessite le réseau"]
    async fn radio_returns_similar_tracks() {
        let catalog = Catalog::new_unauthenticated().await.unwrap();
        let tracks = catalog.radio("Jb6gcoR266U").await.unwrap();
        assert!(tracks.len() > 5);
        assert!(tracks.iter().all(|t| t.video_id.len() == 11));
    }
}
