//! Paroles : plusieurs sources interrogées en même temps, la meilleure gardée.
//!
//! En clair : on demande les paroles en même temps à LRCLIB (base libre de
//! paroles synchronisées : correspondance exacte, recherche par champs, recherche
//! libre), à NetEase (paroles synchronisées) et à YouTube Music (texte simple,
//! Musixmatch ; ses horodatages ne sont pas rendus aux clients web, même
//! connectés : vérifié le 2026-10-29). Des paroles synchronisées dont la durée
//! colle au morceau passent avant tout le reste ; sinon, un texte simple.

use std::time::Duration;

use serde::Deserialize;
use ytmapi_rs::common::YoutubeID;
use ytmr_core::Track;

use crate::{Catalog, CatalogError};

const LRCLIB: &str = "https://lrclib.net/api";
const NETEASE: &str = "https://music.163.com/api";
/// LRCLIB demande un User-Agent qui nomme l'application.
const USER_AGENT: &str = concat!("ytmr/", env!("CARGO_PKG_VERSION"));
/// Écart de durée toléré entre le morceau et les paroles synchronisées.
const DURATION_SLACK: f64 = 3.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    pub at: Duration,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricsBody {
    /// Une ligne par horodatage, dans l'ordre.
    Synced(Vec<LyricLine>),
    Plain(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    pub body: LyricsBody,
    /// « LRCLIB », « Musixmatch » (via YouTube Music)…
    pub source: String,
}

impl Lyrics {
    /// Index de la ligne chantée à `position` (synchronisées seulement).
    pub fn line_at(&self, position: Duration) -> Option<usize> {
        let LyricsBody::Synced(lines) = &self.body else {
            return None;
        };
        lines.iter().rposition(|l| l.at <= position)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibEntry {
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

/// Une réponse candidate, avec ce qu'il faut pour la classer.
struct Candidate {
    lyrics: Lyrics,
    /// Écart de durée en secondes (`None` : inconnu).
    gap: Option<f64>,
}

impl Candidate {
    /// Plus petit = meilleur.
    fn rank(&self) -> (u8, u64) {
        let synced = matches!(self.lyrics.body, LyricsBody::Synced(_));
        let fits = self.gap.is_none_or(|g| g <= DURATION_SLACK);
        let tier = match (synced, fits) {
            (true, true) => 0,
            (false, true) => 1,
            // Synchronisées mais d'une autre version : le texte reste juste,
            // pas le minutage.
            (true, false) => 2,
            (false, false) => 3,
        };
        (tier, (self.gap.unwrap_or(0.0) * 1000.0) as u64)
    }
}

impl Catalog {
    /// Meilleures paroles trouvées pour `track` (`None` : aucune source n'en a).
    pub async fn lyrics(&self, track: &Track) -> Result<Option<Lyrics>, CatalogError> {
        let title = clean_title(&track.title);
        let artist = main_artist(&track.artist);
        let duration = track.duration.map(|d| d.as_secs_f64());
        let free_text = format!("{artist} {title}");
        let by_fields = [
            ("track_name", title.as_str()),
            ("artist_name", artist.as_str()),
        ];
        let by_text = [("q", free_text.as_str())];
        let (exact, searched, loose, netease, youtube) = tokio::join!(
            self.lrclib_get(&title, &artist, track.album.as_deref(), duration),
            self.lrclib_search(&by_fields),
            self.lrclib_search(&by_text),
            self.netease(&title, &artist, duration),
            self.youtube_lyrics(&track.video_id),
        );
        let mut candidates: Vec<Candidate> = Vec::new();
        for entry in exact
            .ok()
            .flatten()
            .into_iter()
            .chain(searched.unwrap_or_default())
            .chain(loose.unwrap_or_default())
        {
            if entry.instrumental {
                continue;
            }
            let gap = match (entry.duration, duration) {
                (Some(a), Some(b)) => Some((a - b).abs()),
                _ => None,
            };
            if let Some(lines) = entry.synced_lyrics.as_deref().map(parse_lrc)
                && !lines.is_empty()
            {
                candidates.push(Candidate {
                    lyrics: Lyrics {
                        body: LyricsBody::Synced(lines),
                        source: "LRCLIB".into(),
                    },
                    gap,
                });
            } else if let Some(text) = entry.plain_lyrics.filter(|t| !t.trim().is_empty()) {
                candidates.push(Candidate {
                    lyrics: Lyrics {
                        body: LyricsBody::Plain(text),
                        source: "LRCLIB".into(),
                    },
                    gap,
                });
            }
        }
        if let Ok(Some(candidate)) = netease {
            candidates.push(candidate);
        }
        if let Ok(Some(lyrics)) = youtube {
            candidates.push(Candidate { lyrics, gap: None });
        }
        candidates.sort_by_key(Candidate::rank);
        Ok(candidates.into_iter().next().map(|c| c.lyrics))
    }

    async fn lrclib_get(
        &self,
        title: &str,
        artist: &str,
        album: Option<&str>,
        duration: Option<f64>,
    ) -> Result<Option<LrclibEntry>, CatalogError> {
        let mut query = vec![
            ("track_name", title.to_owned()),
            ("artist_name", artist.to_owned()),
        ];
        if let Some(album) = album {
            query.push(("album_name", album.to_owned()));
        }
        if let Some(d) = duration {
            query.push(("duration", format!("{}", d.round())));
        }
        let response = self
            .http
            .get(format!("{LRCLIB}/get"))
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .query(&query)
            .timeout(Duration::from_secs(8))
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let text = response.error_for_status()?.text().await?;
        Ok(serde_json::from_str(&text).ok())
    }

    /// NetEase : recherche, puis paroles LRC du résultat qui correspond le mieux
    /// (même titre, même artiste, durée la plus proche).
    async fn netease(
        &self,
        title: &str,
        artist: &str,
        duration: Option<f64>,
    ) -> Result<Option<Candidate>, CatalogError> {
        #[derive(Deserialize)]
        struct Search {
            result: Option<Found>,
        }
        #[derive(Deserialize)]
        struct Found {
            #[serde(default)]
            songs: Vec<Song>,
        }
        #[derive(Deserialize)]
        struct Song {
            id: u64,
            name: String,
            #[serde(default)]
            artists: Vec<Named>,
            /// Millisecondes.
            duration: Option<u64>,
        }
        #[derive(Deserialize)]
        struct Named {
            name: String,
        }
        #[derive(Deserialize)]
        struct LyricReply {
            lrc: Option<Lrc>,
        }
        #[derive(Deserialize)]
        struct Lrc {
            lyric: Option<String>,
        }
        let query = format!("{title} {artist}");
        let text = self
            .http
            .get(format!("{NETEASE}/search/get"))
            .header(reqwest::header::REFERER, "https://music.163.com")
            .query(&[("s", query.as_str()), ("type", "1"), ("limit", "8")])
            .timeout(Duration::from_secs(8))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let songs = serde_json::from_str::<Search>(&text)
            .ok()
            .and_then(|s| s.result)
            .map(|r| r.songs)
            .unwrap_or_default();
        let (want_title, want_artist) = (fold(title), fold(artist));
        let gap = |song: &Song| match (song.duration, duration) {
            (Some(ms), Some(d)) => Some((ms as f64 / 1000.0 - d).abs()),
            _ => None,
        };
        let best = songs
            .iter()
            .filter(|s| {
                let name = fold(&s.name);
                (name.contains(&want_title) || want_title.contains(&name))
                    && s.artists.iter().any(|a| {
                        let a = fold(&a.name);
                        a.contains(&want_artist) || want_artist.contains(&a)
                    })
            })
            .min_by_key(|s| (gap(s).unwrap_or(999.0) * 1000.0) as u64);
        let Some(song) = best else {
            return Ok(None);
        };
        let text = self
            .http
            .get(format!("{NETEASE}/song/lyric"))
            .header(reqwest::header::REFERER, "https://music.163.com")
            .query(&[("id", song.id.to_string().as_str()), ("lv", "1")])
            .timeout(Duration::from_secs(8))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let lrc = serde_json::from_str::<LyricReply>(&text)
            .ok()
            .and_then(|r| r.lrc)
            .and_then(|l| l.lyric)
            .unwrap_or_default();
        let lines: Vec<LyricLine> = parse_lrc(&lrc)
            .into_iter()
            .filter(|l| !is_credit(&l.text))
            .collect();
        if lines.iter().filter(|l| !l.text.is_empty()).count() < 3 {
            return Ok(None);
        }
        Ok(Some(Candidate {
            lyrics: Lyrics {
                body: LyricsBody::Synced(lines),
                source: "NetEase".into(),
            },
            gap: gap(song),
        }))
    }

    async fn lrclib_search(
        &self,
        query: &[(&str, &str)],
    ) -> Result<Vec<LrclibEntry>, CatalogError> {
        let text = self
            .http
            .get(format!("{LRCLIB}/search"))
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .query(query)
            .timeout(Duration::from_secs(8))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let mut entries: Vec<LrclibEntry> = serde_json::from_str(&text).unwrap_or_default();
        entries.truncate(10);
        Ok(entries)
    }

    /// Paroles affichées par YouTube Music (texte simple, souvent Musixmatch).
    async fn youtube_lyrics(&self, video_id: &str) -> Result<Option<Lyrics>, CatalogError> {
        let yt = self.client().await?;
        let id = yt
            .get_lyrics_id(ytmapi_rs::common::VideoID::from_raw(video_id.to_owned()))
            .await?;
        let lyrics = yt.get_lyrics(id).await?;
        if lyrics.lyrics.trim().is_empty() {
            return Ok(None);
        }
        let source = lyrics
            .source
            .trim()
            .trim_start_matches("Source:")
            .trim_start_matches("Source :")
            .trim()
            .to_owned();
        Ok(Some(Lyrics {
            body: LyricsBody::Plain(lyrics.lyrics),
            source: if source.is_empty() {
                "YouTube Music".into()
            } else {
                source
            },
        }))
    }
}

/// Comparaison souple : minuscules, sans accents ni ponctuation.
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

/// Lignes de crédits de NetEase (« 作词 : … », « 作曲 : … ») à ne pas afficher.
fn is_credit(text: &str) -> bool {
    let cjk = text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    cjk && (text.contains(':') || text.contains('：'))
}

/// « Titre (Official Video) [Remastered] » → « Titre ».
fn clean_title(title: &str) -> String {
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
    let out = out.split(" - ").next().unwrap_or(&out).trim().to_owned();
    if out.is_empty() {
        title.trim().to_owned()
    } else {
        out
    }
}

/// « Daft Punk, Pharrell Williams » → « Daft Punk ».
fn main_artist(artist: &str) -> String {
    let mut name = artist;
    // « & » est gardé : « Simon & Garfunkel » est un seul nom.
    for sep in [", ", " feat", " ft.", " Feat", " Ft."] {
        if let Some((first, _)) = name.split_once(sep) {
            name = first;
        }
    }
    name.trim().to_owned()
}

/// Lignes `[mm:ss.xx] texte` (plusieurs horodatages possibles par ligne).
pub fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let Some((stamp, tail)) = after.split_once(']') else {
                break;
            };
            match parse_stamp(stamp) {
                Some(at) => stamps.push(at),
                // Balise d'en-tête ([ar:…], [ti:…]) : ligne ignorée.
                None => {
                    stamps.clear();
                    rest = "";
                    break;
                }
            }
            rest = tail;
        }
        let text = rest.trim().to_owned();
        lines.extend(stamps.into_iter().map(|at| LyricLine {
            at,
            text: text.clone(),
        }));
    }
    lines.sort_by_key(|l| l.at);
    lines
}

fn parse_stamp(stamp: &str) -> Option<Duration> {
    let (min, sec) = stamp.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let sec: f64 = sec.trim().replace(',', ".").parse().ok()?;
    if !(0.0..60.0).contains(&sec) {
        return None;
    }
    Some(Duration::from_secs(min * 60) + Duration::from_secs_f64(sec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc_lines() {
        let lines = parse_lrc(
            "[ar:Daft Punk]\n[00:12.50]Around the world\n[01:02.00][00:05.00]Encore\n[00:20.00]\n",
        );
        let at: Vec<u64> = lines.iter().map(|l| l.at.as_millis() as u64).collect();
        assert_eq!(at, vec![5_000, 12_500, 20_000, 62_000]);
        assert_eq!(lines[0].text, "Encore");
        assert_eq!(
            lines[2].text, "",
            "les silences restent (ils effacent la ligne)"
        );
    }

    #[test]
    fn finds_line_at_position() {
        let lyrics = Lyrics {
            body: LyricsBody::Synced(parse_lrc("[00:01.00]a\n[00:03.00]b")),
            source: String::new(),
        };
        assert_eq!(lyrics.line_at(Duration::from_millis(500)), None);
        assert_eq!(lyrics.line_at(Duration::from_secs(2)), Some(0));
        assert_eq!(lyrics.line_at(Duration::from_secs(9)), Some(1));
    }

    #[test]
    fn prefers_synced_lyrics_of_the_right_length() {
        let plain = Candidate {
            lyrics: Lyrics {
                body: LyricsBody::Plain("x".into()),
                source: String::new(),
            },
            gap: None,
        };
        let synced_far = Candidate {
            lyrics: Lyrics {
                body: LyricsBody::Synced(parse_lrc("[00:01.00]a")),
                source: String::new(),
            },
            gap: Some(40.0),
        };
        let synced_close = Candidate {
            gap: Some(1.0),
            ..Candidate {
                lyrics: synced_far.lyrics.clone(),
                gap: None,
            }
        };
        let mut all = [synced_far, plain, synced_close];
        all.sort_by_key(Candidate::rank);
        assert_eq!(all[0].gap, Some(1.0));
        assert!(matches!(all[1].lyrics.body, LyricsBody::Plain(_)));
    }

    #[test]
    fn drops_netease_credits() {
        assert!(is_credit("作词 : Pharrell Williams"));
        assert!(!is_credit("Like the legend of the phoenix"));
        assert_eq!(fold("Beyoncé, Jay-Z"), "beyoncejayz");
    }

    #[test]
    fn cleans_titles_and_artists() {
        assert_eq!(
            clean_title("One More Time (Official Video) [HD]"),
            "One More Time"
        );
        assert_eq!(clean_title("Song - Remastered 2011"), "Song");
        assert_eq!(main_artist("Daft Punk, Pharrell Williams"), "Daft Punk");
        assert_eq!(main_artist("Simon & Garfunkel"), "Simon & Garfunkel");
    }

    #[tokio::test]
    #[ignore = "réseau"]
    async fn finds_synced_lyrics() {
        let catalog = Catalog::new();
        let track = Track {
            unavailable: false,
            plays: None,
            video_id: "5NV6Rdv1a3I".into(),
            title: "Get Lucky (feat. Pharrell Williams & Nile Rodgers)".into(),
            artist: "Daft Punk".into(),
            artist_id: None,
            album: Some("Random Access Memories".into()),
            album_id: None,
            duration: Some(Duration::from_secs(369)),
            thumbnail_url: None,
        };
        let lyrics = catalog.lyrics(&track).await.unwrap().unwrap();
        println!("source : {}", lyrics.source);
        assert!(matches!(lyrics.body, LyricsBody::Synced(_)));
    }
}

#[cfg(test)]
mod network {
    use super::*;

    #[tokio::test]
    #[ignore = "réseau"]
    async fn netease_finds_synced_lyrics() {
        let catalog = Catalog::new();
        let found = catalog
            .netease("Reality Surf", "Bladee", Some(123.0))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(found.lyrics.body, LyricsBody::Synced(ref l) if l.len() > 10));
        assert!(found.gap.is_some_and(|g| g < 3.0));
    }
}
