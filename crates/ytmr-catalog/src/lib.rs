//! Catalogue YouTube Music : recherche, albums, artistes, radio.
//!
//! En clair : c'est l'annuaire. Il trouve les morceaux mais ne les joue pas.
//! Enveloppe `ytmapi-rs` pour que le reste du code n'en dépende pas directement :
//! si YouTube change son API interne, seul ce crate est à adapter.

pub mod account;

use reqwest::header::{COOKIE, HeaderMap, HeaderValue};
use thiserror::Error;
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::common::{AlbumID, AlbumType, ArtistChannelID, Thumbnail, VideoID, YoutubeID};
use ytmapi_rs::parse::{
    AlbumResult, AlbumSong, ArtistSong, GetAlbum, GetArtist, ParsedSongArtist, SearchResultAlbum,
    SearchResultArtist, SearchResultSong, WatchPlaylistTrack,
};
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
    #[error("{0}")]
    Refused(&'static str),
}

/// Refus des cookies non essentiels. Sans lui, depuis l'UE, la page d'accueil
/// redirige vers `consent.youtube.com` (400, corps vide) et ytmapi-rs ne trouve
/// pas son `ytcfg`.
const CONSENT_COOKIE: &str = "SOCS=CAI";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumSummary {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: String,
    /// `Album`, `Single` ou `EP`.
    pub kind: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistSummary {
    pub id: String,
    pub name: String,
    pub subscribers: Option<String>,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchAll {
    pub songs: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: String,
    pub kind: String,
    /// Durée totale telle qu'affichée par YouTube (« 1 heure et 2 minutes »).
    pub duration_text: String,
    pub thumbnail_url: Option<String>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub subscribers: Option<String>,
    pub thumbnail_url: Option<String>,
    pub songs: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
    pub singles: Vec<AlbumSummary>,
}

/// Accès au catalogue. Garde une connexion HTTP réutilisable : à créer une fois.
///
/// La connexion à YouTube Music se fait à la première utilisation (et se
/// retente en cas d'échec) : l'application s'ouvre même sans réseau.
pub struct Catalog {
    yt: tokio::sync::OnceCell<YtMusic<NoAuthToken>>,
}

/// Essais de connexion, espacés de plus en plus.
const CONNECT_ATTEMPTS: [u64; 3] = [0, 600, 1500];

impl Catalog {
    /// Accès sans compte : recherche, albums, radio. Ne touche pas au réseau.
    pub fn new() -> Self {
        Self {
            yt: tokio::sync::OnceCell::new(),
        }
    }

    /// Ancien nom, connexion immédiate (CLI, tests).
    pub async fn new_unauthenticated() -> Result<Self, CatalogError> {
        let catalog = Self::new();
        catalog.client().await?;
        Ok(catalog)
    }

    async fn client(&self) -> Result<&YtMusic<NoAuthToken>, CatalogError> {
        self.yt.get_or_try_init(connect).await
    }
    /// Recherche limitée aux morceaux (pas de vidéos, albums ni playlists).
    pub async fn search_songs(&self, query: &str) -> Result<Vec<Track>, CatalogError> {
        let results = self.client().await?.search_songs(query).await?;
        tracing::debug!(query, count = results.len(), "recherche de morceaux");
        Ok(results.into_iter().map(track_from_song).collect())
    }

    /// Morceaux, albums et artistes, trois requêtes lancées en parallèle.
    /// Un échec partiel (ex. albums) n'empêche pas de rendre le reste.
    pub async fn search_all(&self, query: &str) -> Result<SearchAll, CatalogError> {
        let yt = self.client().await?;
        let (songs, albums, artists) = tokio::join!(
            yt.search_songs(query),
            yt.search_albums(query),
            yt.search_artists(query),
        );
        let songs: Vec<Track> = songs?.into_iter().map(track_from_song).collect();
        let albums = albums
            .inspect_err(|e| tracing::warn!(error = %e, "recherche d'albums"))
            .map(|v| v.into_iter().map(album_from_search).collect())
            .unwrap_or_default();
        let artists = artists
            .inspect_err(|e| tracing::warn!(error = %e, "recherche d'artistes"))
            .map(|v| v.into_iter().map(artist_from_search).collect())
            .unwrap_or_default();
        Ok(SearchAll {
            songs,
            albums,
            artists,
        })
    }

    pub async fn album(&self, id: &str) -> Result<Album, CatalogError> {
        let album = self
            .client()
            .await?
            .get_album(AlbumID::from_raw(id.to_owned()))
            .await?;
        Ok(album_from_get(id, album))
    }

    pub async fn artist(&self, id: &str) -> Result<Artist, CatalogError> {
        let artist = self
            .client()
            .await?
            .get_artist(ArtistChannelID::from_raw(id.to_owned()))
            .await?;
        Ok(artist_from_get(artist))
    }

    /// « Radio » du morceau : titres similaires, comme l'autoplay de YouTube Music.
    /// Le premier résultat est en général le morceau lui-même.
    pub async fn radio(&self, video_id: &str) -> Result<Vec<Track>, CatalogError> {
        let results = self
            .client()
            .await?
            .get_watch_playlist_from_video_id(VideoID::from_raw(video_id.to_owned()))
            .await?;
        tracing::debug!(video_id, count = results.len(), "radio");
        Ok(results.into_iter().map(track_from_watch).collect())
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}

async fn connect() -> Result<YtMusic<NoAuthToken>, CatalogError> {
    let mut last = None;
    for delay in CONNECT_ATTEMPTS {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static(CONSENT_COOKIE));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        let client = ytmapi_rs::Client::new_from_reqwest_client(http);
        match YtMusicBuilder::new_with_client(client).build().await {
            Ok(yt) => return Ok(yt),
            Err(e) => {
                tracing::warn!(error = %e, "connexion à YouTube Music, nouvel essai");
                last = Some(e);
            }
        }
    }
    Err(last.map_or(
        CatalogError::Refused("connexion impossible"),
        CatalogError::from,
    ))
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
        album_id: None,
        duration: parse_duration(&t.duration),
        thumbnail_url: largest_thumbnail(&t.thumbnails),
    }
}

fn track_from_song(song: SearchResultSong) -> Track {
    let (album, album_id) = song
        .album
        .map(|a| (Some(a.name), Some(a.id.get_raw().to_owned())))
        .unwrap_or_default();
    Track {
        video_id: song.video_id.get_raw().to_owned(),
        title: song.title,
        artist: song.artist,
        album,
        album_id,
        duration: parse_duration(&song.duration),
        thumbnail_url: largest_thumbnail(&song.thumbnails),
    }
}

/// Les morceaux d'un album n'ont pas de vignette propre : ils prennent la pochette.
fn track_from_album_song(song: AlbumSong, album: &Album) -> Track {
    Track {
        video_id: song.video_id.get_raw().to_owned(),
        title: song.title,
        artist: album.artist.clone(),
        album: Some(album.title.clone()),
        album_id: Some(album.id.clone()),
        duration: parse_duration(&song.duration),
        thumbnail_url: album.thumbnail_url.clone(),
    }
}

fn track_from_artist_song(song: ArtistSong) -> Track {
    Track {
        video_id: song.video_id.get_raw().to_owned(),
        title: song.title,
        artist: join_artists(&song.artists),
        album: Some(song.album.name),
        album_id: Some(song.album.id.get_raw().to_owned()),
        duration: None,
        // Pas de vignette dans cette réponse : la vignette vidéo servira de repli.
        thumbnail_url: None,
    }
}

fn album_kind(kind: &AlbumType) -> String {
    match kind {
        AlbumType::Album => "Album",
        AlbumType::Single => "Single",
        AlbumType::EP => "EP",
    }
    .to_owned()
}

fn album_from_search(a: SearchResultAlbum) -> AlbumSummary {
    AlbumSummary {
        id: a.album_id.get_raw().to_owned(),
        title: a.title,
        artist: a.artist,
        year: a.year,
        kind: album_kind(&a.album_type),
        thumbnail_url: largest_thumbnail(&a.thumbnails),
    }
}

fn album_from_artist(a: AlbumResult, artist: &str, kind: &str) -> AlbumSummary {
    AlbumSummary {
        id: a.album_id.get_raw().to_owned(),
        title: a.title,
        artist: artist.to_owned(),
        year: a.year,
        kind: kind.to_owned(),
        thumbnail_url: largest_thumbnail(&a.thumbnails),
    }
}

fn artist_from_search(a: SearchResultArtist) -> ArtistSummary {
    ArtistSummary {
        id: a.browse_id.get_raw().to_owned(),
        name: a.artist,
        subscribers: a.subscribers,
        thumbnail_url: largest_thumbnail(&a.thumbnails),
    }
}

fn album_from_get(id: &str, a: GetAlbum) -> Album {
    let mut album = Album {
        id: id.to_owned(),
        title: a.title,
        artist: join_artists(&a.artists),
        artist_id: a
            .artists
            .iter()
            .find_map(|x| x.id.as_ref())
            .map(|i| i.get_raw().to_owned()),
        year: a.year,
        kind: album_kind(&a.category),
        duration_text: a.duration,
        thumbnail_url: largest_thumbnail(&a.thumbnails),
        tracks: Vec::new(),
    };
    album.tracks = a
        .tracks
        .into_iter()
        .map(|song| track_from_album_song(song, &album))
        .collect();
    album
}

fn artist_from_get(a: GetArtist) -> Artist {
    let releases = a.top_releases;
    let albums = releases
        .albums
        .map(|r| {
            r.results
                .into_iter()
                .map(|x| album_from_artist(x, &a.name, "Album"))
                .collect()
        })
        .unwrap_or_default();
    let singles = releases
        .singles
        .map(|r| {
            r.results
                .into_iter()
                .map(|x| album_from_artist(x, &a.name, "Single"))
                .collect()
        })
        .unwrap_or_default();
    Artist {
        id: a.channel_id.get_raw().to_owned(),
        description: a.description,
        subscribers: a.subscribers,
        thumbnail_url: largest_thumbnail(&a.thumbnails),
        songs: releases
            .songs
            .map(|s| s.results.into_iter().map(track_from_artist_song).collect())
            .unwrap_or_default(),
        albums,
        singles,
        name: a.name,
    }
}

pub(crate) fn join_artists(artists: &[ParsedSongArtist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn largest_thumbnail(thumbnails: &[Thumbnail]) -> Option<String> {
    thumbnails
        .iter()
        .max_by_key(|t| t.width * t.height)
        .map(|t| t.url.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests réseau : `cargo test -p ytmr-catalog -- --ignored`.
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

    #[tokio::test]
    #[ignore = "nécessite le réseau"]
    async fn search_all_then_album_and_artist_pages() {
        let catalog = Catalog::new_unauthenticated().await.unwrap();
        let results = catalog.search_all("daft punk discovery").await.unwrap();
        assert!(!results.songs.is_empty());
        let album = results.albums.first().expect("au moins un album");
        let artist = results.artists.first().expect("au moins un artiste");

        let album = catalog.album(&album.id).await.unwrap();
        assert!(!album.tracks.is_empty());
        assert!(album.tracks.iter().all(|t| t.thumbnail_url.is_some()));

        let artist = catalog.artist(&artist.id).await.unwrap();
        assert!(!artist.name.is_empty());
        assert!(!artist.songs.is_empty() || !artist.albums.is_empty());
    }
}
