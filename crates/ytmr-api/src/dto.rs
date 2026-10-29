//! Formes JSON échangées avec les clients (interface, deck CYD).
//!
//! En clair : la traduction entre les types internes et ce qui circule sur le
//! réseau. Les noms sont courts mais lisibles, et `/state` reste sous 1 Ko pour
//! l'ESP32. Toute entrée est validée ici avant d'atteindre le core.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use ytmr_catalog::{Album, AlbumSummary, Artist, ArtistSummary, SearchAll};
use ytmr_core::{PlaybackState, QueueSnapshot, QueuedTrack, Repeat, Status, Track};
use ytmr_media::Covers;
use ytmr_player::StreamQuality;

pub const MAX_TRACKS_PER_REQUEST: usize = 500;
const MAX_TEXT_LEN: usize = 500;

#[derive(Debug, Serialize)]
pub struct StateDto {
    pub status: &'static str,
    pub playing: bool,
    pub queue_id: Option<u64>,
    pub video_id: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub duration_ms: Option<u64>,
    pub position_ms: u64,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: &'static str,
    pub autoplay: bool,
    /// Toujours faux tant que l'authentification (phase 6) n'existe pas.
    pub liked: bool,
    pub cover_id: Option<String>,
    pub quality: Option<&'static str>,
    pub next_ready: bool,
}

impl StateDto {
    pub fn new(state: &PlaybackState, covers: &Covers) -> Self {
        let track = state.current.as_ref().map(|e| &e.track);
        Self {
            status: match state.status {
                Status::Idle => "idle",
                Status::Loading => "loading",
                Status::Playing => "playing",
                Status::Paused => "paused",
            },
            playing: state.status == Status::Playing,
            queue_id: state.current.as_ref().map(|e| e.id.0),
            video_id: track.map(|t| t.video_id.clone()),
            title: track.map(|t| t.title.clone()),
            artist: track.map(|t| t.artist.clone()),
            album: track.and_then(|t| t.album.clone()),
            album_id: track.and_then(|t| t.album_id.clone()),
            duration_ms: state.duration.map(millis),
            position_ms: millis(state.position),
            volume: state.volume,
            shuffle: state.shuffle,
            repeat: repeat_name(state.repeat),
            autoplay: state.autoplay,
            liked: false,
            cover_id: track.map(|t| covers.register(&t.cover_url())),
            quality: state.quality.map(|q| match q {
                StreamQuality::High => "high",
                StreamQuality::Low => "low",
                StreamQuality::Unknown => "unknown",
            }),
            next_ready: state.next_ready,
        }
    }

    /// Égalité hors position : sert à ne pousser `state` que sur un vrai changement.
    pub fn same_except_position(&self, other: &Self) -> bool {
        self.status == other.status
            && self.queue_id == other.queue_id
            && self.duration_ms == other.duration_ms
            && self.volume == other.volume
            && self.shuffle == other.shuffle
            && self.repeat == other.repeat
            && self.autoplay == other.autoplay
            && self.quality == other.quality
            && self.next_ready == other.next_ready
            && self.cover_id == other.cover_id
    }
}

pub fn repeat_name(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => "off",
        Repeat::All => "all",
        Repeat::One => "one",
    }
}

pub fn parse_repeat(name: &str) -> Option<Repeat> {
    match name {
        "off" => Some(Repeat::Off),
        "all" => Some(Repeat::All),
        "one" => Some(Repeat::One),
        _ => None,
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Morceau tel qu'échangé avec l'interface (dans les deux sens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackDto {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub album_id: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    /// Calculé par le serveur ; ignoré en entrée.
    #[serde(default, skip_deserializing)]
    pub cover_id: Option<String>,
}

impl TrackDto {
    pub fn new(track: &Track, covers: &Covers) -> Self {
        Self {
            video_id: track.video_id.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            album_id: track.album_id.clone(),
            duration_ms: track.duration.map(millis),
            thumbnail_url: track.thumbnail_url.clone(),
            cover_id: Some(covers.register(&track.cover_url())),
        }
    }

    pub fn into_track(self) -> Result<Track, String> {
        if !is_video_id(&self.video_id) {
            return Err(format!("video_id invalide : {:?}", self.video_id));
        }
        for (name, value) in [("title", &self.title), ("artist", &self.artist)] {
            if value.is_empty() || value.len() > MAX_TEXT_LEN {
                return Err(format!("{name} vide ou trop long"));
            }
        }
        if let Some(album_id) = &self.album_id
            && !is_browse_id(album_id)
        {
            return Err("album_id invalide".into());
        }
        let thumbnail_url = self.thumbnail_url.filter(|u| is_allowed_image_url(u));
        Ok(Track {
            video_id: self.video_id,
            title: self.title,
            artist: self.artist,
            artist_id: None,
            album: self.album.filter(|a| a.len() <= MAX_TEXT_LEN),
            album_id: self.album_id,
            duration: self.duration_ms.map(Duration::from_millis),
            thumbnail_url,
        })
    }
}

pub fn tracks_from_dto(tracks: Vec<TrackDto>) -> Result<Vec<Track>, String> {
    if tracks.is_empty() {
        return Err("liste de morceaux vide".into());
    }
    if tracks.len() > MAX_TRACKS_PER_REQUEST {
        return Err(format!("plus de {MAX_TRACKS_PER_REQUEST} morceaux"));
    }
    tracks.into_iter().map(TrackDto::into_track).collect()
}

/// Identifiant vidéo YouTube : 11 caractères base64url.
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Identifiant d'album ou d'artiste (`MPREb…`, `UC…`).
pub fn is_browse_id(id: &str) -> bool {
    (2..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Le serveur ne télécharge des images que depuis les domaines de YouTube :
/// sinon, un client pourrait lui faire appeler n'importe quelle adresse.
pub fn is_allowed_image_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', ':']).next().unwrap_or_default();
    host == "i.ytimg.com"
        || host.ends_with(".googleusercontent.com")
        || host.ends_with(".ggpht.com")
        || host.ends_with(".ytimg.com")
}

#[derive(Debug, Serialize)]
pub struct QueueItemDto {
    pub id: u64,
    #[serde(flatten)]
    pub track: TrackDto,
}

impl QueueItemDto {
    fn new(entry: &QueuedTrack, covers: &Covers) -> Self {
        Self {
            id: entry.id.0,
            track: TrackDto::new(&entry.track, covers),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct QueueDto {
    pub total: usize,
    pub current: Option<usize>,
    pub offset: usize,
    pub items: Vec<QueueItemDto>,
}

impl QueueDto {
    pub fn new(queue: &QueueSnapshot, offset: usize, limit: usize, covers: &Covers) -> Self {
        Self {
            total: queue.entries.len(),
            current: queue.current,
            offset,
            items: queue
                .entries
                .iter()
                .skip(offset)
                .take(limit)
                .map(|e| QueueItemDto::new(e, covers))
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AlbumSummaryDto {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: String,
    pub kind: String,
    pub cover_id: Option<String>,
}

impl AlbumSummaryDto {
    pub fn new(a: &AlbumSummary, covers: &Covers) -> Self {
        Self {
            id: a.id.clone(),
            title: a.title.clone(),
            artist: a.artist.clone(),
            year: a.year.clone(),
            kind: a.kind.clone(),
            cover_id: a.thumbnail_url.as_deref().map(|u| covers.register(u)),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ArtistSummaryDto {
    pub id: String,
    pub name: String,
    pub subscribers: Option<String>,
    pub cover_id: Option<String>,
}

impl ArtistSummaryDto {
    pub fn new(a: &ArtistSummary, covers: &Covers) -> Self {
        Self {
            id: a.id.clone(),
            name: a.name.clone(),
            subscribers: a.subscribers.clone(),
            cover_id: a.thumbnail_url.as_deref().map(|u| covers.register(u)),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SearchDto {
    pub songs: Vec<TrackDto>,
    pub albums: Vec<AlbumSummaryDto>,
    pub artists: Vec<ArtistSummaryDto>,
}

impl SearchDto {
    pub fn new(r: &SearchAll, covers: &Covers) -> Self {
        Self {
            songs: r.songs.iter().map(|t| TrackDto::new(t, covers)).collect(),
            albums: r
                .albums
                .iter()
                .map(|a| AlbumSummaryDto::new(a, covers))
                .collect(),
            artists: r
                .artists
                .iter()
                .map(|a| ArtistSummaryDto::new(a, covers))
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AlbumDto {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: String,
    pub kind: String,
    pub duration_text: String,
    pub cover_id: Option<String>,
    pub tracks: Vec<TrackDto>,
}

impl AlbumDto {
    pub fn new(a: &Album, covers: &Covers) -> Self {
        Self {
            id: a.id.clone(),
            title: a.title.clone(),
            artist: a.artist.clone(),
            artist_id: a.artist_id.clone(),
            year: a.year.clone(),
            kind: a.kind.clone(),
            duration_text: a.duration_text.clone(),
            cover_id: a.thumbnail_url.as_deref().map(|u| covers.register(u)),
            tracks: a.tracks.iter().map(|t| TrackDto::new(t, covers)).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ArtistDto {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub subscribers: Option<String>,
    pub cover_id: Option<String>,
    pub songs: Vec<TrackDto>,
    pub albums: Vec<AlbumSummaryDto>,
    pub singles: Vec<AlbumSummaryDto>,
}

impl ArtistDto {
    pub fn new(a: &Artist, covers: &Covers) -> Self {
        let summaries =
            |v: &[AlbumSummary]| v.iter().map(|x| AlbumSummaryDto::new(x, covers)).collect();
        Self {
            id: a.id.clone(),
            name: a.name.clone(),
            description: a.description.clone(),
            subscribers: a.subscribers.clone(),
            cover_id: a.thumbnail_url.as_deref().map(|u| covers.register(u)),
            songs: a.songs.iter().map(|t| TrackDto::new(t, covers)).collect(),
            albums: summaries(&a.albums),
            singles: summaries(&a.singles),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dto(video_id: &str) -> TrackDto {
        TrackDto {
            video_id: video_id.into(),
            title: "t".into(),
            artist: "a".into(),
            album: None,
            album_id: None,
            duration_ms: Some(1000),
            thumbnail_url: None,
            cover_id: None,
        }
    }

    #[test]
    fn validates_video_ids() {
        assert!(dto("Jb6gcoR266U").into_track().is_ok());
        assert!(dto("short").into_track().is_err());
        assert!(dto("../../etc/pa").into_track().is_err());
    }

    #[test]
    fn drops_foreign_thumbnail_urls() {
        let mut t = dto("Jb6gcoR266U");
        t.thumbnail_url = Some("http://192.168.1.1/admin".into());
        assert_eq!(t.into_track().unwrap().thumbnail_url, None);

        let mut t = dto("Jb6gcoR266U");
        t.thumbnail_url = Some("https://lh3.googleusercontent.com/x=w60-h60".into());
        assert!(t.into_track().unwrap().thumbnail_url.is_some());

        assert!(!is_allowed_image_url("https://evil.com/i.ytimg.com/x"));
        assert!(!is_allowed_image_url(
            "https://googleusercontent.com.evil.com/x"
        ));
    }

    #[test]
    fn bounds_track_lists() {
        assert!(tracks_from_dto(vec![]).is_err());
        let many = vec![dto("Jb6gcoR266U"); MAX_TRACKS_PER_REQUEST + 1];
        assert!(tracks_from_dto(many).is_err());
    }
}
