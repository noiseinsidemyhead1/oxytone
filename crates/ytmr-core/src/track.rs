//! Description d'un morceau, indépendante de la source (catalogue) et du lecteur.
//!
//! En clair : la « fiche » d'un morceau que tout le monde se passe (titre,
//! artiste, durée, identifiant YouTube…). Elle ne contient jamais le flux audio.

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    /// Identifiant vidéo YouTube (11 caractères).
    pub video_id: String,
    pub title: String,
    pub artist: String,
    /// Page de l'artiste principal (`UC…`), quand YouTube la donne.
    #[serde(default)]
    pub artist_id: Option<String>,
    pub album: Option<String>,
    /// Identifiant d'album YouTube Music (`MPREb…`), pour ouvrir sa page.
    pub album_id: Option<String>,
    pub duration: Option<Duration>,
    /// URL de la plus grande jaquette connue.
    pub thumbnail_url: Option<String>,
}

impl Track {
    /// URL donnée à mpv, qui la résout lui-même via yt-dlp.
    pub fn watch_url(&self) -> String {
        format!("https://music.youtube.com/watch?v={}", self.video_id)
    }

    /// Jaquette connue, sinon la vignette vidéo (pochette au centre, à recadrer).
    pub fn cover_url(&self) -> String {
        self.thumbnail_url
            .clone()
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", self.video_id))
    }

    /// Même chanson, même sous un autre identifiant (clip / version audio,
    /// « (Official Audio) », « (feat. …) ») : titre sans parenthèses et
    /// artiste principal comparés en ignorant casse, accents et ponctuation.
    pub fn same_song(&self, other: &Track) -> bool {
        if self.video_id == other.video_id {
            return true;
        }
        let (a, b) = (song_key(&self.title), song_key(&other.title));
        !a.is_empty() && a == b && main_artist_key(&self.artist) == main_artist_key(&other.artist)
    }
}

/// Titre réduit à l'essentiel : sans « (…) », « […] » ni « - Remastered ».
fn song_key(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let base = out.split(" - ").next().unwrap_or_default().to_owned();
    fold(&base)
}

fn main_artist_key(artist: &str) -> String {
    let first = artist
        .split([',', '&'])
        .next()
        .unwrap_or_default()
        .split(" feat")
        .next()
        .unwrap_or_default();
    fold(first)
}

fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            c => c,
        })
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Lit une durée au format YouTube (`"3:45"`, `"1:02:03"`).
pub fn parse_duration(text: &str) -> Option<Duration> {
    let mut secs: u64 = 0;
    let mut parts = 0;
    for part in text.trim().split(':') {
        let value: u64 = part.parse().ok()?;
        if parts > 0 && value >= 60 {
            return None;
        }
        secs = secs.checked_mul(60)?.checked_add(value)?;
        parts += 1;
    }
    (1..=3).contains(&parts).then(|| Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, title: &str, artist: &str) -> Track {
        Track {
            video_id: id.into(),
            title: title.into(),
            artist: artist.into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: None,
            thumbnail_url: None,
        }
    }

    #[test]
    fn recognizes_the_same_song_under_another_id() {
        let audio = song("a", "Get Lucky (feat. Pharrell Williams)", "Daft Punk");
        let clip = song(
            "b",
            "Get Lucky (Official Video)",
            "Daft Punk, Pharrell Williams",
        );
        let other = song("c", "Lose Yourself to Dance", "Daft Punk");
        assert!(audio.same_song(&clip));
        assert!(!audio.same_song(&other));
        assert!(!audio.same_song(&song("d", "Get Lucky", "Quelqu'un d'autre")));
    }

    #[test]
    fn parses_youtube_durations() {
        assert_eq!(parse_duration("3:45"), Some(Duration::from_secs(225)));
        assert_eq!(parse_duration("1:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(parse_duration("42"), Some(Duration::from_secs(42)));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("3:75"), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("1:2:3:4"), None);
    }
}
