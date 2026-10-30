//! Import de playlists Spotify vers YouTube Music.
//!
//! En clair : on lit la liste des morceaux d'une playlist Spotify (titre,
//! artistes, durée), puis on cherche chacun sur YouTube Music et on garde le
//! résultat le plus ressemblant. Deux sources, sans compte développeur :
//! - le lien d'une playlist **publique** : la page d'intégration de Spotify
//!   (`open.spotify.com/embed/playlist/…`) contient la liste, 100 morceaux au plus ;
//! - un fichier **CSV exporté par Exportify** (exportify.net) : toute la
//!   playlist, privée comprise.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use ytmr_core::Track;

use crate::{Catalog, CatalogError};

/// Morceaux d'une page d'intégration au plus (limite de Spotify).
pub const EMBED_LIMIT: usize = 100;
/// Recherches YouTube Music menées en même temps.
const PARALLEL_SEARCHES: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyTrack {
    pub title: String,
    /// « A, B » (tous les artistes).
    pub artists: String,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyPlaylist {
    pub name: String,
    pub tracks: Vec<SpotifyTrack>,
    /// Lue par la page d'intégration et pleine : la playlist est peut-être plus longue.
    pub maybe_truncated: bool,
}

/// Identifiant d'une playlist dans un lien (`open.spotify.com/playlist/ID?si=…`,
/// `open.spotify.com/intl-fr/playlist/ID`) ou une URI (`spotify:playlist:ID`).
pub fn playlist_id(link: &str) -> Option<String> {
    let link = link.trim();
    let rest = link
        .split_once("playlist/")
        .map(|(_, r)| r)
        .or_else(|| link.strip_prefix("spotify:playlist:"))?;
    let id: String = rest
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (id.len() >= 16).then_some(id)
}

/// Liste lue dans le HTML de la page d'intégration (`__NEXT_DATA__`).
pub fn parse_embed(html: &str) -> Option<SpotifyPlaylist> {
    const START: &str = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let json = html.split_once(START)?.1.split_once("</script>")?.0;
    let data: Value = serde_json::from_str(json).ok()?;
    let entity = data.pointer("/props/pageProps/state/data/entity")?;
    let name = entity
        .get("name")
        .or_else(|| entity.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Playlist Spotify")
        .to_owned();
    let tracks: Vec<SpotifyTrack> = entity
        .get("trackList")?
        .as_array()?
        .iter()
        .filter_map(|t| {
            Some(SpotifyTrack {
                title: t.get("title")?.as_str()?.to_owned(),
                // Les artistes sont séparés par « ,\u{a0} ».
                artists: t
                    .get("subtitle")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .replace('\u{a0}', " "),
                duration: t
                    .get("duration")
                    .and_then(Value::as_u64)
                    .map(Duration::from_millis),
            })
        })
        .collect();
    Some(SpotifyPlaylist {
        name,
        maybe_truncated: tracks.len() >= EMBED_LIMIT,
        tracks,
    })
}

/// Fichier CSV d'Exportify : colonnes « Track Name », « Artist Name(s) » et
/// « Duration (ms) » (repérées par leur nom, l'ordre peut changer).
pub fn parse_csv(text: &str, name: &str) -> Option<SpotifyPlaylist> {
    let mut rows = csv_rows(text.trim_start_matches('\u{feff}')).into_iter();
    let header = rows.next()?;
    let column = |wanted: &[&str]| {
        header
            .iter()
            .position(|h| wanted.iter().any(|w| h.trim().eq_ignore_ascii_case(w)))
    };
    let title = column(&["Track Name", "Nom du titre", "trackName"])?;
    let artists = column(&[
        "Artist Name(s)",
        "Artist Name",
        "Nom(s) de l'artiste",
        "artistName",
    ]);
    let duration = column(&["Duration (ms)", "Track Duration (ms)", "Durée (ms)"]);
    let tracks: Vec<SpotifyTrack> = rows
        .filter_map(|row| {
            let title = row.get(title)?.trim().to_owned();
            (!title.is_empty()).then(|| SpotifyTrack {
                title,
                artists: artists
                    .and_then(|i| row.get(i))
                    .map(|a| a.replace(';', ", ").trim().to_owned())
                    .unwrap_or_default(),
                duration: duration
                    .and_then(|i| row.get(i))
                    .and_then(|d| d.trim().parse::<u64>().ok())
                    .map(Duration::from_millis),
            })
        })
        .collect();
    (!tracks.is_empty()).then(|| SpotifyPlaylist {
        name: name.to_owned(),
        tracks,
        maybe_truncated: false,
    })
}

/// Découpe un CSV (guillemets, `""` échappés, retours à la ligne dans les champs).
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

impl Catalog {
    /// Playlist publique lue par sa page d'intégration (100 morceaux au plus).
    pub async fn spotify_playlist(&self, link: &str) -> Result<SpotifyPlaylist, CatalogError> {
        let id = playlist_id(link).ok_or(CatalogError::Refused(
            "lien de playlist Spotify non reconnu",
        ))?;
        let html = reqwest::Client::new()
            .get(format!("https://open.spotify.com/embed/playlist/{id}"))
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        parse_embed(&html).ok_or(CatalogError::Refused(
            "playlist Spotify illisible (privée ?) : exporte-la en CSV avec Exportify",
        ))
    }

    /// Chaque morceau cherché sur YouTube Music (`None` : rien d'assez proche).
    pub async fn match_spotify(&self, tracks: &[SpotifyTrack]) -> Vec<Option<Track>> {
        futures_util::stream::iter(tracks.to_vec())
            .map(|t| self.match_one(t))
            .buffered(PARALLEL_SEARCHES)
            .collect()
            .await
    }

    async fn match_one(&self, t: SpotifyTrack) -> Option<Track> {
        let main_artist = t.artists.split(',').next().unwrap_or_default().trim();
        match self
            .search_songs(&format!("{} {main_artist}", t.title))
            .await
        {
            Ok(results) => best_match(&t, &results).cloned(),
            Err(e) => {
                tracing::debug!(error = %e, title = %t.title, "import Spotify");
                None
            }
        }
    }
}

/// Résultat le plus ressemblant : titre, artiste principal et durée.
pub fn best_match<'a>(wanted: &SpotifyTrack, results: &'a [Track]) -> Option<&'a Track> {
    let title = fold(base_title(&wanted.title));
    let artist = fold(wanted.artists.split(',').next().unwrap_or_default());
    results
        .iter()
        .take(8)
        .map(|t| {
            let mut score = 0;
            let found = fold(base_title(&t.title));
            if found == title {
                score += 3;
            } else if !title.is_empty() && (found.contains(&title) || title.contains(&found)) {
                score += 1;
            }
            let found_artist = fold(&t.artist);
            if !artist.is_empty() && found_artist.contains(&artist) {
                score += 2;
            }
            if let (Some(a), Some(b)) = (wanted.duration, t.duration) {
                let gap = a.as_secs().abs_diff(b.as_secs());
                if gap <= 3 {
                    score += 2;
                } else if gap <= 10 {
                    score += 1;
                }
            }
            (score, t)
        })
        // À score égal, le premier résultat (le plus pertinent pour YouTube).
        .fold(None, |best: Option<(i32, &Track)>, (score, t)| match best {
            Some((s, _)) if s >= score => best,
            _ => Some((score, t)),
        })
        .filter(|(score, _)| *score >= 3)
        .map(|(_, t)| t)
}

/// Titre sans « (feat. …) », « - Remastered 2011 »…
fn base_title(title: &str) -> &str {
    let cut = [" (", " [", " - "]
        .iter()
        .filter_map(|m| title.find(m))
        .min()
        .unwrap_or(title.len());
    &title[..cut]
}

/// Minuscules, sans accents ni ponctuation.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(id: &str, title: &str, artist: &str, secs: u64) -> Track {
        Track {
            video_id: id.into(),
            title: title.into(),
            artist: artist.into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: Some(Duration::from_secs(secs)),
            thumbnail_url: None,
            unavailable: false,
            plays: None,
        }
    }

    #[test]
    fn reads_playlist_links() {
        let id = "37i9dQZF1DXcBWIGoYBM5M";
        assert_eq!(
            playlist_id(&format!("https://open.spotify.com/playlist/{id}?si=abc")).as_deref(),
            Some(id)
        );
        assert_eq!(
            playlist_id(&format!("https://open.spotify.com/intl-fr/playlist/{id}")).as_deref(),
            Some(id)
        );
        assert_eq!(
            playlist_id(&format!("spotify:playlist:{id}")).as_deref(),
            Some(id)
        );
        assert_eq!(playlist_id("https://open.spotify.com/album/x"), None);
    }

    #[test]
    fn reads_the_embed_page() {
        let html = r#"<html><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"state":{"data":{"entity":{"name":"Ma liste","trackList":[{"title":"Loser","subtitle":"Tame Impala","duration":223069},{"title":"Hey","subtitle":"Gwen Stefani, Akon","duration":1000}]}}}}}}</script></html>"#;
        let p = parse_embed(html).unwrap();
        assert_eq!(p.name, "Ma liste");
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.tracks[1].artists, "Gwen Stefani, Akon");
        assert_eq!(p.tracks[0].duration, Some(Duration::from_millis(223_069)));
        assert!(!p.maybe_truncated);
    }

    #[test]
    fn reads_exportify_csv() {
        let csv = "\u{feff}\"Track URI\",\"Track Name\",\"Artist Name(s)\",\"Duration (ms)\"\r\n\
                   \"spotify:track:1\",\"Get Lucky (feat. Pharrell, Nile)\",\"Daft Punk;Pharrell Williams\",\"369626\"\r\n\
                   \"spotify:track:2\",\"Dit \"\"bonjour\"\"\",\"X\",\"1000\"\r\n";
        let p = parse_csv(csv, "Export").unwrap();
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.tracks[0].title, "Get Lucky (feat. Pharrell, Nile)");
        assert_eq!(p.tracks[0].artists, "Daft Punk, Pharrell Williams");
        assert_eq!(p.tracks[1].title, "Dit \"bonjour\"");
    }

    #[test]
    fn picks_the_closest_result() {
        let wanted = SpotifyTrack {
            title: "Get Lucky (feat. Pharrell Williams) - Radio Edit".into(),
            artists: "Daft Punk, Pharrell Williams".into(),
            duration: Some(Duration::from_secs(248)),
        };
        let results = vec![
            yt("a", "Get Lucky (Official Video)", "Daft Punk", 369),
            yt("b", "Get Lucky (Radio Edit)", "Daft Punk", 248),
            yt("c", "Lucky", "Britney Spears", 205),
        ];
        assert_eq!(
            best_match(&wanted, &results).map(|t| t.video_id.as_str()),
            Some("b")
        );
        let nothing = vec![yt("c", "Lucky", "Britney Spears", 205)];
        assert!(best_match(&wanted, &nothing).is_none());
    }

    #[tokio::test]
    #[ignore = "nécessite le réseau"]
    async fn imports_a_public_playlist() {
        let catalog = Catalog::new_unauthenticated().await.unwrap();
        let playlist = catalog
            .spotify_playlist("https://open.spotify.com/playlist/37i9dQZF1DX4o1oenSJRJd")
            .await
            .unwrap();
        assert!(playlist.tracks.len() >= 50);
        let sample = &playlist.tracks[..20];
        let found = catalog.match_spotify(sample).await;
        let hits = found.iter().filter(|t| t.is_some()).count();
        for (wanted, got) in sample.iter().zip(&found) {
            println!(
                "{} — {}  =>  {:?}",
                wanted.title,
                wanted.artists,
                got.as_ref().map(|t| format!("{} — {}", t.title, t.artist))
            );
        }
        assert!(hits >= 17, "{hits} / 20 trouvés");
    }
}
