//! Statistiques d'écoute, calculées en local.
//!
//! En clair : chaque morceau écouté assez longtemps (30 s, ou la moitié s'il
//! est court) ajoute une ligne au fichier `plays.jsonl` du dossier de données.
//! La page « Statistiques » relit ce fichier et en tire les artistes et titres
//! les plus écoutés, le temps d'écoute et son évolution jour par jour. Rien ne
//! quitte la machine.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Track;

/// Durée d'écoute qui fait compter un morceau.
pub const COUNT_AFTER: Duration = Duration::from_secs(30);
/// Durée comptée au plus par écoute (mix d'une heure, podcast…).
const MAX_COUNTED: u64 = 15 * 60;

/// Une écoute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Play {
    /// Secondes depuis 1970 (UTC).
    pub at: u64,
    pub track: Track,
}

impl Play {
    pub fn now(track: Track) -> Self {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self { at, track }
    }

    /// Secondes comptées pour cette écoute (durée du morceau, bornée).
    pub fn seconds(&self) -> u64 {
        self.track
            .duration
            .map_or(180, |d| d.as_secs())
            .min(MAX_COUNTED)
    }
}

/// Seuil d'écoute pour un morceau de `duration` : 30 s, ou la moitié s'il est court.
pub fn threshold(duration: Option<Duration>) -> Duration {
    match duration {
        Some(d) if d < COUNT_AFTER * 2 => d / 2,
        _ => COUNT_AFTER,
    }
}

/// Ajoute une écoute au fichier (créé au besoin).
pub fn append(path: &Path, play: &Play) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(play).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

/// Toutes les écoutes du fichier (les lignes illisibles sont ignorées).
pub fn load(path: &Path) -> Vec<Play> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Artiste le plus écouté.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistStat {
    pub name: String,
    pub artist_id: Option<String>,
    pub plays: u32,
    pub seconds: u64,
    /// Un de ses morceaux (pour la pochette).
    pub sample: Track,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackStat {
    pub track: Track,
    pub plays: u32,
}

/// Résumé d'une période.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub plays: u32,
    pub seconds: u64,
    pub distinct_tracks: u32,
    pub distinct_artists: u32,
    pub top_artists: Vec<ArtistStat>,
    pub top_tracks: Vec<TrackStat>,
    /// Secondes écoutées par jour, du plus ancien (`days` jours) à aujourd'hui.
    pub per_day: Vec<u64>,
    /// Jours consécutifs avec au moins une écoute, jusqu'à aujourd'hui.
    pub streak: u32,
}

const DAY: u64 = 86_400;

/// Résumé des `days` derniers jours (0 = tout), au plus `top` entrées par classement.
/// `now` et `utc_offset` (secondes) fixent le découpage en jours locaux.
pub fn summarize(plays: &[Play], days: u32, top: usize, now: u64, utc_offset: i64) -> Summary {
    let local_day = |t: u64| (t as i64 + utc_offset).div_euclid(DAY as i64);
    let today = local_day(now);
    let first_day = if days == 0 {
        i64::MIN
    } else {
        today - i64::from(days) + 1
    };
    let kept: Vec<&Play> = plays
        .iter()
        .filter(|p| local_day(p.at) >= first_day)
        .collect();

    let mut artists: HashMap<String, ArtistStat> = HashMap::new();
    let mut tracks: HashMap<String, TrackStat> = HashMap::new();
    let mut seconds = 0;
    for play in &kept {
        let secs = play.seconds();
        seconds += secs;
        let name = main_artist(&play.track.artist);
        let entry = artists.entry(name.to_lowercase()).or_insert(ArtistStat {
            name: name.to_owned(),
            artist_id: play.track.artist_id.clone(),
            plays: 0,
            seconds: 0,
            sample: play.track.clone(),
        });
        entry.plays += 1;
        entry.seconds += secs;
        if entry.artist_id.is_none() {
            entry.artist_id = play.track.artist_id.clone();
        }
        tracks
            .entry(play.track.video_id.clone())
            .or_insert(TrackStat {
                track: play.track.clone(),
                plays: 0,
            })
            .plays += 1;
    }

    let mut top_artists: Vec<ArtistStat> = artists.into_values().collect();
    top_artists.sort_by(|a, b| b.plays.cmp(&a.plays).then(b.seconds.cmp(&a.seconds)));
    let distinct_artists = top_artists.len() as u32;
    top_artists.truncate(top);
    let mut top_tracks: Vec<TrackStat> = tracks.into_values().collect();
    top_tracks.sort_by(|a, b| {
        b.plays
            .cmp(&a.plays)
            .then(a.track.title.cmp(&b.track.title))
    });
    let distinct_tracks = top_tracks.len() as u32;
    top_tracks.truncate(top);

    // Jours affichés : la période, ou les 30 derniers pour « tout ».
    let span = if days == 0 { 30 } else { days } as i64;
    let mut per_day = vec![0u64; span as usize];
    for play in plays {
        let offset = today - local_day(play.at);
        if (0..span).contains(&offset) {
            per_day[(span - 1 - offset) as usize] += play.seconds();
        }
    }

    let mut active: Vec<i64> = plays.iter().map(|p| local_day(p.at)).collect();
    active.sort_unstable();
    active.dedup();
    let mut streak = 0;
    let mut day = today;
    // Pas encore écouté aujourd'hui : la série d'hier compte encore.
    if active.binary_search(&day).is_err() {
        day -= 1;
    }
    while active.binary_search(&day).is_ok() {
        streak += 1;
        day -= 1;
    }

    Summary {
        plays: kept.len() as u32,
        seconds,
        distinct_tracks,
        distinct_artists,
        top_artists,
        top_tracks,
        per_day,
        streak,
    }
}

/// Premier nom d'une liste d'artistes (« A, B & C » → « A »).
fn main_artist(artists: &str) -> &str {
    artists
        .split([',', '&'])
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(artists)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, artist: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("titre {id}"),
            artist: artist.into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: Some(Duration::from_secs(200)),
            thumbnail_url: None,
        }
    }

    #[test]
    fn ranks_artists_and_tracks_over_a_period() {
        let now = 100 * DAY + 3600;
        let play = |id: &str, artist: &str, days_ago: u64| Play {
            at: now - days_ago * DAY,
            track: track(id, artist),
        };
        let plays = vec![
            play("a", "Daft Punk", 0),
            play("a", "Daft Punk, Pharrell", 1),
            play("b", "Justice", 1),
            play("c", "Daft Punk", 10),
        ];
        let week = summarize(&plays, 7, 10, now, 0);
        assert_eq!(week.plays, 3);
        assert_eq!(week.top_artists[0].name, "Daft Punk");
        assert_eq!(week.top_artists[0].plays, 2);
        assert_eq!(week.top_tracks[0].track.video_id, "a");
        assert_eq!(week.seconds, 600);
        assert_eq!(week.per_day.len(), 7);
        assert_eq!(week.per_day[6], 200, "aujourd'hui en dernier");
        assert_eq!(week.streak, 2);
        let all = summarize(&plays, 0, 10, now, 0);
        assert_eq!(all.plays, 4);
        assert_eq!(all.distinct_tracks, 3);
    }

    #[test]
    fn short_tracks_count_after_half() {
        assert_eq!(
            threshold(Some(Duration::from_secs(40))),
            Duration::from_secs(20)
        );
        assert_eq!(threshold(Some(Duration::from_secs(300))), COUNT_AFTER);
        assert_eq!(threshold(None), COUNT_AFTER);
    }

    #[test]
    fn appends_and_reloads() {
        let path = std::env::temp_dir().join(format!("ytmr-plays-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        append(&path, &Play::now(track("a", "X"))).unwrap();
        append(&path, &Play::now(track("b", "Y"))).unwrap();
        assert_eq!(load(&path).len(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
