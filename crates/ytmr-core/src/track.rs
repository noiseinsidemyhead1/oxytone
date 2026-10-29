//! Description d'un morceau, indépendante de la source (catalogue) et du lecteur.
//!
//! En clair : la « fiche » d'un morceau que tout le monde se passe (titre,
//! artiste, durée, identifiant YouTube…). Elle ne contient jamais le flux audio.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// Identifiant vidéo YouTube (11 caractères).
    pub video_id: String,
    pub title: String,
    pub artist: String,
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
