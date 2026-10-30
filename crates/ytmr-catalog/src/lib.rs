//! Catalogue YouTube Music : recherche, albums, artistes, radio.
//!
//! En clair : c'est l'annuaire. Il trouve les morceaux mais ne les joue pas.
//! Enveloppe `ytmapi-rs` pour que le reste du code n'en dépende pas directement :
//! si YouTube change son API interne, seul ce crate est à adapter.

pub mod account;
pub mod home;
pub mod lyrics;
mod raw;

pub use raw::normalize_query;

use reqwest::header::{COOKIE, HeaderMap, HeaderValue};
use thiserror::Error;
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::common::{AlbumID, AlbumType, ArtistChannelID, Thumbnail, VideoID, YoutubeID};
use ytmapi_rs::parse::{
    AlbumResult, AlbumSong, ArtistSong, GetAlbum, GetArtist, ParsedSongArtist, SearchResultAlbum,
    SearchResultArtist, WatchPlaylistTrack,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AlbumSummary {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: String,
    /// `Album`, `Single` ou `EP`.
    pub kind: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// Playlist audio de l'album (`OLAK5uy…`) : c'est elle qu'on enregistre
    /// dans la bibliothèque pour « liker » l'album.
    pub audio_playlist_id: Option<String>,
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
    /// Sources hors YouTube (paroles).
    http: reqwest::Client,
}

/// Essais de connexion, espacés de plus en plus.
const CONNECT_ATTEMPTS: [u64; 3] = [0, 600, 1500];

impl Catalog {
    /// Accès sans compte : recherche, albums, radio. Ne touche pas au réseau.
    pub fn new() -> Self {
        Self {
            yt: tokio::sync::OnceCell::new(),
            http: reqwest::Client::new(),
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

    /// Gain (dB, ≤ 0) qui ramène le morceau au niveau prévu par YouTube :
    /// réponse « player » du client iOS, où seules les métadonnées de son sont
    /// lues (`playerConfig.audioConfig`, aucune adresse de flux). Les clients
    /// web (WEB, WEB_REMIX, MWEB) répondent « UNPLAYABLE » sans ces données
    /// (constaté le 2026-10-30) ; iOS et Android répondent. Comme YouTube
    /// Music, on ne fait que baisser les morceaux trop forts.
    pub async fn loudness(&self, video_id: &str) -> Result<Option<f32>, CatalogError> {
        let body = serde_json::json!({
            "context": {"client": {
                "clientName": "IOS",
                "clientVersion": "20.10.4",
                "deviceModel": "iPhone16,2",
                "hl": "fr"
            }},
            "videoId": video_id,
        });
        let text = self
            .http
            .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header(reqwest::header::COOKIE, CONSENT_COOKIE)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let reply: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if cfg!(test) {
            println!(
                "statut : {:?} / {}",
                reply.pointer("/playabilityStatus/status"),
                &text[..text.len().min(300)]
            );
        }
        Ok(gain_from_audio_config(
            reply.pointer("/playerConfig/audioConfig"),
        ))
    }

    /// Suggestions pendant la frappe (complétions proposées par YouTube Music).
    pub async fn suggestions(&self, query: &str) -> Result<Vec<String>, CatalogError> {
        let query = normalize_query(query);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let found = self.client().await?.get_search_suggestions(query).await?;
        Ok(found
            .into_iter()
            .map(|s| {
                s.runs
                    .into_iter()
                    .map(ytmapi_rs::common::TextRun::take_text)
                    .collect::<String>()
            })
            .filter(|s| !s.trim().is_empty())
            .take(7)
            .collect())
    }
    /// Recherche limitée aux morceaux (pas de vidéos, albums ni playlists).
    ///
    /// Lue en JSON brut : le décodeur de ytmapi-rs rejetait toute la recherche
    /// dès qu'un résultat n'avait pas de page artiste (« Expected Song result »).
    pub async fn search_songs(&self, query: &str) -> Result<Vec<Track>, CatalogError> {
        let query = normalize_query(query);
        let raw = self
            .client()
            .await?
            .raw_json_query(raw::songs_search(&query))
            .await?;
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let tracks = raw::search_songs(&json);
        tracing::debug!(query, count = tracks.len(), "recherche de morceaux");
        Ok(tracks)
    }

    /// Morceaux, albums et artistes, trois requêtes lancées en parallèle.
    /// Un échec partiel (ex. albums) n'empêche pas de rendre le reste.
    pub async fn search_all(&self, query: &str) -> Result<SearchAll, CatalogError> {
        let query = normalize_query(query);
        let query = query.as_str();
        let yt = self.client().await?;
        let (songs, albums, artists) = tokio::join!(
            self.search_songs(query),
            yt.search_albums(query),
            yt.search_artists(query),
        );
        let songs = songs?;
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

    /// Page d'artiste correspondant à un nom (morceaux de radio sans identifiant).
    pub async fn find_artist(&self, name: &str) -> Result<Option<String>, CatalogError> {
        let results = self
            .client()
            .await?
            .search_artists(normalize_query(name))
            .await?;
        let wanted = name.trim().to_lowercase();
        // Le nom exact d'abord, sinon le premier résultat.
        let exact = results
            .iter()
            .find(|a| a.artist.trim().to_lowercase() == wanted)
            .or(results.first());
        Ok(exact.map(|a| a.browse_id.get_raw().to_owned()))
    }

    pub async fn album(&self, id: &str) -> Result<Album, CatalogError> {
        let album = self
            .client()
            .await?
            .get_album(AlbumID::from_raw(id.to_owned()))
            .await?;
        Ok(album_from_get(id, album))
    }

    /// Page d'un artiste. Si le décodeur de ytmapi-rs la refuse (il exige par
    /// exemple une image d'en-tête, absente chez certains artistes : « Key
    /// /header/musicImmersiveHeaderRenderer/thumbnail/… not found »), la page
    /// est relue en JSON brut, où tout ce qui manque est simplement laissé vide.
    pub async fn artist(&self, id: &str) -> Result<Artist, CatalogError> {
        let strict = self
            .client()
            .await?
            .get_artist(ArtistChannelID::from_raw(id.to_owned()))
            .await;
        match strict {
            Ok(artist) => Ok(artist_from_get(artist)),
            Err(e) => {
                tracing::debug!(error = %e, id, "page artiste relue en JSON brut");
                self.artist_raw(id).await?.ok_or(CatalogError::Api(e))
            }
        }
    }

    /// Lecture tolérante de la page d'un artiste.
    pub(crate) async fn artist_raw(&self, id: &str) -> Result<Option<Artist>, CatalogError> {
        let mut body = serde_json::Map::new();
        body.insert("browseId".into(), serde_json::json!(id));
        body.insert("context".into(), home::french_context());
        let raw = self
            .client()
            .await?
            .raw_json_query(raw::RawPost {
                path: "browse",
                body,
                query: Vec::new(),
            })
            .await?;
        let page: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        Ok(raw::artist_page(id, &page))
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

/// `trackAbsoluteLoudnessLkfs` et `loudnessTargetLkfs` (-14 si absente ;
/// sinon `loudnessDb`, l'écart déjà calculé) → gain à appliquer, jamais positif.
fn gain_from_audio_config(config: Option<&serde_json::Value>) -> Option<f32> {
    const DEFAULT_TARGET: f64 = -14.0;
    let config = config?;
    let number = |key: &str| config.get(key).and_then(serde_json::Value::as_f64);
    let excess = match number("trackAbsoluteLoudnessLkfs") {
        Some(track) => track - number("loudnessTargetLkfs").unwrap_or(DEFAULT_TARGET),
        None => number("loudnessDb")?,
    };
    Some((-excess).min(0.0) as f32)
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

    async fn loudness(&self, video_id: &str) -> Result<Option<f32>, BoxError> {
        Ok(Catalog::loudness(self, video_id).await?)
    }
}

fn track_from_watch(t: WatchPlaylistTrack) -> Track {
    Track {
        video_id: t.video_id.get_raw().to_owned(),
        title: t.title,
        artist: t.author,
        artist_id: None,
        album: None,
        album_id: None,
        duration: parse_duration(&t.duration),
        thumbnail_url: largest_thumbnail(&t.thumbnails),
    }
}

/// Les morceaux d'un album n'ont pas de vignette propre : ils prennent la pochette.
fn track_from_album_song(song: AlbumSong, album: &Album) -> Track {
    Track {
        video_id: song.video_id.get_raw().to_owned(),
        title: song.title,
        artist: album.artist.clone(),
        artist_id: album.artist_id.clone(),
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
        artist_id: song
            .artists
            .iter()
            .find_map(|a| a.id.as_ref())
            .map(|i| i.get_raw().to_owned()),
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

pub(crate) fn album_from_search(a: SearchResultAlbum) -> AlbumSummary {
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
        audio_playlist_id: a.audio_playlist_id.as_ref().map(|p| p.get_raw().to_owned()),
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

#[cfg(test)]
mod artist_raw_tests {
    /// Réseau : la lecture brute d'une vraie page d'artiste donne l'essentiel.
    #[tokio::test]
    #[ignore = "réseau"]
    async fn reads_a_real_artist_page_raw() {
        let c = crate::Catalog::new();
        let a = c
            .artist_raw("UC_kRDKYrUlrbtrSiyu5Tflg")
            .await
            .unwrap()
            .unwrap();
        println!(
            "{} / {:?} / {} titres / {} albums / {} singles / photo {}",
            a.name,
            a.subscribers,
            a.songs.len(),
            a.albums.len(),
            a.singles.len(),
            a.thumbnail_url.is_some()
        );
        assert!(!a.songs.is_empty());
    }
}

#[cfg(test)]
mod loudness_tests {
    use serde_json::json;

    #[test]
    fn only_lowers_loud_tracks() {
        let loud = json!({"trackAbsoluteLoudnessLkfs": -10.5, "loudnessTargetLkfs": -14});
        assert_eq!(super::gain_from_audio_config(Some(&loud)), Some(-3.5));
        let quiet = json!({"trackAbsoluteLoudnessLkfs": -20.0, "loudnessTargetLkfs": -14});
        assert_eq!(super::gain_from_audio_config(Some(&quiet)), Some(0.0));
        let no_target = json!({"trackAbsoluteLoudnessLkfs": -10.0});
        assert_eq!(super::gain_from_audio_config(Some(&no_target)), Some(-4.0));
        let old = json!({"loudnessDb": 2.0});
        assert_eq!(super::gain_from_audio_config(Some(&old)), Some(-2.0));
        assert_eq!(super::gain_from_audio_config(None), None);
    }

    #[tokio::test]
    #[ignore = "réseau"]
    async fn reads_a_real_track_loudness() {
        let gain = crate::Catalog::new().loudness("5NV6Rdv1a3I").await.unwrap();
        println!("gain : {gain:?}");
        assert!(gain.is_some());
    }
}
