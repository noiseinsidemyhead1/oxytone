//! Compte YouTube Music : playlists, titres likés, création et modification.
//!
//! En clair : les mêmes appels que l'appli officielle, faits avec la session de
//! l'utilisateur (cookies de son navigateur). Tout ce qui est modifié ici l'est
//! sur son compte : il le retrouve sur son téléphone.
//!
//! Limites de ytmapi-rs 0.3.3, contournées ici en lisant le JSON brut :
//! - les morceaux d'une playlist ne portent pas leur `setVideoId` (exigé pour
//!   en retirer un) ;
//! - son décodeur rejette une page entière dès qu'un morceau n'a pas de page
//!   artiste ;
//! - sa pagination ré-encode le jeton de continuation (déjà encodé par
//!   YouTube) : dès qu'il contient `=`, la page suivante échoue. Une playlist de
//!   1 189 titres s'arrêtait à 100, les 943 likes à 900 (2026-10-29).

use std::borrow::Cow;

use serde_json::{Value, json};
use ytmapi_rs::auth::{AuthToken, BrowserToken};
use ytmapi_rs::common::{ApiOutcome, LikeStatus, PlaylistID, SetVideoID, VideoID, YoutubeID};
use ytmapi_rs::parse::LibraryPlaylist;
use ytmapi_rs::parse::{ParseFrom, ProcessedResult};
use ytmapi_rs::query::playlist::PrivacyStatus;
use ytmapi_rs::query::{
    CreatePlaylistQuery, EditPlaylistQuery, GetPlaylistDetailsQuery, GetPlaylistTracksQuery,
};
use ytmapi_rs::query::{PostMethod, PostQuery, Query};
use ytmapi_rs::{YtMusic, YtMusicBuilder};
use ytmr_core::Track;
use ytmr_core::track::parse_duration;

use crate::{CatalogError, largest_thumbnail};

/// Playlist des titres likés.
pub const LIKED_ID: &str = "LM";
/// Listes automatiques de podcasts (« New Episodes »), sans intérêt ici.
const PODCAST_PREFIX: &str = "RDPN";

/// YouTube attend `VL<id>` pour lire une playlist (identifiant de navigation)
/// mais l'identifiant nu pour la modifier. ytmr manipule l'identifiant nu.
fn bare_id(id: &str) -> &str {
    id.strip_prefix("VL").unwrap_or(id)
}

fn browse_id(id: &str) -> PlaylistID<'static> {
    PlaylistID::from_raw(format!("VL{}", bare_id(id)))
}

fn edit_id(id: &str) -> PlaylistID<'static> {
    PlaylistID::from_raw(bare_id(id).to_owned())
}
/// Garde-fou pour les très grandes playlists (~100 titres par page).
const MAX_PAGES: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSummary {
    pub id: String,
    pub title: String,
    /// « 42 titres » tel qu'affiché par YouTube.
    pub tracks_text: String,
    pub author: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub author: String,
    pub description: Option<String>,
    pub duration_text: String,
    pub thumbnail_url: Option<String>,
    pub tracks: Vec<Track>,
    /// `false` si YouTube n'a pas rendu toutes les pages.
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    Private,
    Unlisted,
    Public,
}

pub struct Account {
    yt: YtMusic<BrowserToken>,
}

// À la main : le jeton contient les cookies, qui ne doivent pas finir dans les logs.
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account").finish_non_exhaustive()
    }
}

impl Account {
    /// `cookie` : en-tête `Cookie` de music.youtube.com (session du navigateur).
    pub async fn connect(cookie: &str) -> Result<Self, CatalogError> {
        let mut cookie = cookie.trim().trim_end_matches(';').to_owned();
        // Même contournement de la page de consentement RGPD que sans compte.
        if !cookie.contains("SOCS=") {
            cookie.push_str("; SOCS=CAI");
        }
        // ytmapi-rs attend un `;` après SAPISID pour le repérer.
        cookie.push(';');
        let yt = YtMusicBuilder::new()
            .with_browser_token_cookie(cookie)
            .build()
            .await?;
        Ok(Self { yt })
    }

    pub async fn playlists(&self) -> Result<Vec<PlaylistSummary>, CatalogError> {
        let playlists = self.yt.get_library_playlists().await?;
        Ok(playlists
            .into_iter()
            .map(summary)
            .filter(|p| p.id != LIKED_ID && !p.id.starts_with(PODCAST_PREFIX))
            .collect())
    }

    /// Détails et morceaux (toutes les pages). `LIKED_ID` pour les titres likés.
    pub async fn playlist(&self, id: &str) -> Result<Playlist, CatalogError> {
        let details = self
            .yt
            .query(GetPlaylistDetailsQuery::new(browse_id(id)))
            .await?;
        let (pages, complete) = self.raw_pages(id).await?;
        let mut tracks = Vec::new();
        for page in &pages {
            collect_tracks(page, &mut tracks);
        }
        Ok(Playlist {
            id: bare_id(id).to_owned(),
            title: details.title,
            author: details.author,
            description: details.description,
            duration_text: details.duration,
            thumbnail_url: largest_thumbnail(&details.thumbnails),
            tracks,
            complete,
        })
    }

    pub async fn create_playlist(
        &self,
        title: &str,
        privacy: Privacy,
        video_ids: &[String],
    ) -> Result<String, CatalogError> {
        let privacy = match privacy {
            Privacy::Private => PrivacyStatus::Private,
            Privacy::Unlisted => PrivacyStatus::Unlisted,
            Privacy::Public => PrivacyStatus::Public,
        };
        let id = self
            .yt
            .create_playlist(CreatePlaylistQuery::new(title, None, privacy))
            .await?;
        let id = bare_id(id.get_raw()).to_owned();
        if !video_ids.is_empty() {
            self.add_tracks(&id, video_ids).await?;
        }
        Ok(id)
    }

    pub async fn rename_playlist(&self, id: &str, title: &str) -> Result<(), CatalogError> {
        let outcome = self
            .yt
            .edit_playlist(EditPlaylistQuery::new_title(edit_id(id), title.to_owned()))
            .await?;
        match outcome {
            ApiOutcome::Success => Ok(()),
            ApiOutcome::Failure => Err(CatalogError::Refused("renommage refusé par YouTube")),
        }
    }

    pub async fn delete_playlist(&self, id: &str) -> Result<(), CatalogError> {
        self.yt.delete_playlist(edit_id(id)).await?;
        Ok(())
    }

    pub async fn add_tracks(&self, id: &str, video_ids: &[String]) -> Result<(), CatalogError> {
        self.yt
            .add_video_items_to_playlist(
                edit_id(id),
                video_ids.iter().map(|v| VideoID::from_raw(v.clone())),
            )
            .await?;
        Ok(())
    }

    /// Retire toutes les occurrences de `video_id` de la playlist.
    pub async fn remove_track(&self, id: &str, video_id: &str) -> Result<(), CatalogError> {
        let (set_ids, complete) = self.set_video_ids(id).await?;
        let targets: Vec<SetVideoID<'static>> = set_ids
            .into_iter()
            .filter(|(v, _)| v == video_id)
            .map(|(_, s)| SetVideoID::from_raw(s))
            .collect();
        if targets.is_empty() {
            return if complete {
                Ok(())
            } else {
                Err(CatalogError::Refused(
                    "morceau introuvable : YouTube n'a pas rendu toute la playlist, réessaie",
                ))
            };
        }
        self.yt.remove_playlist_items(edit_id(id), targets).await?;
        Ok(())
    }

    /// Like (`true`) ou retrait du like (`false`) d'un morceau.
    pub async fn set_liked(&self, video_id: &str, liked: bool) -> Result<(), CatalogError> {
        let status = if liked {
            LikeStatus::Liked
        } else {
            LikeStatus::Indifferent
        };
        self.yt
            .rate_song(VideoID::from_raw(video_id.to_owned()), status)
            .await?;
        Ok(())
    }

    /// Paires (videoId, setVideoId) lues dans le JSON brut de la playlist.
    async fn set_video_ids(&self, id: &str) -> Result<(Vec<(String, String)>, bool), CatalogError> {
        let (pages, complete) = self.raw_pages(id).await?;
        let mut pairs = Vec::new();
        for page in &pages {
            collect_set_video_ids(page, &mut pairs);
        }
        Ok((pairs, complete))
    }

    /// Toutes les pages brutes d'une playlist. Rend aussi `false` si une page
    /// suivante a échoué (on garde alors ce qui a déjà été lu).
    async fn raw_pages(&self, id: &str) -> Result<(Vec<Value>, bool), CatalogError> {
        let first = self
            .yt
            .raw_json_query(GetPlaylistTracksQuery::new(browse_id(id)))
            .await?;
        let mut pages = vec![serde_json::from_str::<Value>(&first).unwrap_or(Value::Null)];
        while pages.len() < MAX_PAGES {
            let Some(token) = pages.last().and_then(continuation_token) else {
                return Ok((pages, true));
            };
            let next = NextPage {
                browse_id: browse_id(id).get_raw().to_owned(),
                token,
            };
            // YouTube refuse parfois une page isolée : un second essai suffit en général.
            let page = match self.yt.raw_json_query::<NextPage>(&next).await {
                Ok(page) => Ok(page),
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    self.yt.raw_json_query::<NextPage>(&next).await
                }
            };
            match page {
                Ok(page) => pages.push(serde_json::from_str(&page).unwrap_or(Value::Null)),
                Err(e) => {
                    tracing::warn!(error = %e, id, pages = pages.len(), "playlist incomplète");
                    return Ok((pages, false));
                }
            }
        }
        Ok((pages, false))
    }
}

/// Page suivante d'une liste, avec un jeton encodé une seule fois.
struct NextPage {
    browse_id: String,
    /// Jeton décodé : reqwest l'encode en l'ajoutant à l'URL.
    token: String,
}

/// Rien à décoder : seule la réponse brute nous intéresse.
#[derive(Debug)]
struct RawOnly;

impl<A: AuthToken> Query<A> for NextPage {
    type Output = RawOnly;
    type Method = PostMethod;
}

impl ParseFrom<NextPage> for RawOnly {
    fn parse_from(_: ProcessedResult<NextPage>) -> ytmapi_rs::Result<Self> {
        Ok(RawOnly)
    }
}

impl PostQuery for NextPage {
    fn header(&self) -> serde_json::Map<String, Value> {
        let mut map = serde_json::Map::new();
        map.insert("browseId".into(), json!(self.browse_id));
        map
    }
    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        vec![
            ("ctoken", self.token.as_str().into()),
            ("continuation", self.token.as_str().into()),
            ("type", "next".into()),
        ]
    }
    fn path(&self) -> &str {
        "browse"
    }
}

/// Jeton de la page suivante (décodé), s'il y en a une.
fn continuation_token(page: &Value) -> Option<String> {
    fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
        match v {
            Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find(x, key))),
            Value::Array(a) => a.iter().find_map(|x| find(x, key)),
            _ => None,
        }
    }
    let raw = find(page, "continuationItemRenderer")
        .and_then(|r| r.pointer("/continuationEndpoint/continuationCommand/token"))
        .or_else(|| find(page, "nextContinuationData").and_then(|d| d.get("continuation")))
        .and_then(Value::as_str)?;
    Some(percent_decode(raw))
}

/// `%3D` → `=` ; le reste est laissé tel quel.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// Parcourt le JSON à la recherche de `playlistItemData`.
fn collect_set_video_ids(value: &Value, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(data) = map.get("playlistItemData")
                && let (Some(v), Some(s)) = (
                    data.get("videoId").and_then(Value::as_str),
                    data.get("playlistSetVideoId").and_then(Value::as_str),
                )
            {
                out.push((v.to_owned(), s.to_owned()));
            }
            map.values().for_each(|v| collect_set_video_ids(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_set_video_ids(v, out)),
        _ => {}
    }
}

fn summary(p: LibraryPlaylist) -> PlaylistSummary {
    PlaylistSummary {
        id: bare_id(p.playlist_id.get_raw()).to_owned(),
        title: p.title,
        tracks_text: p.tracks,
        author: p.author,
        thumbnail_url: largest_thumbnail(&p.thumbnails),
    }
}

/// Morceaux (`musicResponsiveListItemRenderer`) trouvés dans une page brute.
/// Un élément incomplet (vidéo supprimée, sans titre) est simplement ignoré.
fn collect_tracks(value: &Value, out: &mut Vec<Track>) {
    match value {
        Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                if let Some(track) = track_from_raw(item) {
                    out.push(track);
                }
                return;
            }
            map.values().for_each(|v| collect_tracks(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_tracks(v, out)),
        _ => {}
    }
}

fn track_from_raw(item: &Value) -> Option<Track> {
    let runs = |column: usize| {
        item.pointer(&format!(
            "/flexColumns/{column}/musicResponsiveListItemFlexColumnRenderer/text/runs"
        ))
        .and_then(Value::as_array)
    };
    let text_of = |run: &Value| run.get("text").and_then(Value::as_str).map(str::to_owned);
    let video_id = item
        .pointer("/playlistItemData/videoId")
        .or_else(|| {
            item.pointer(
                "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/watchEndpoint/videoId",
            )
        })
        .and_then(Value::as_str)?
        .to_owned();
    let title = runs(0)?.first().and_then(text_of)?;
    let artist: String = runs(1)
        .map(|r| r.iter().filter_map(text_of).collect())
        .unwrap_or_default();
    let album_run = runs(2).and_then(|r| r.first());
    let album_id = album_run
        .and_then(|r| r.pointer("/navigationEndpoint/browseEndpoint/browseId"))
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("MPRE"))
        .map(str::to_owned);
    let duration = item
        .pointer("/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text/runs/0/text")
        .or_else(|| {
            item.pointer(
                "/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text/simpleText",
            )
        })
        .and_then(Value::as_str)
        .and_then(parse_duration);
    let thumbnail_url = item
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|t| t.last())
        .and_then(|t| t.get("url"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(Track {
        video_id,
        title,
        artist,
        album: album_id.as_ref().and(album_run.and_then(text_of)),
        album_id,
        duration,
        thumbnail_url,
    })
}

/// Identifiants vidéo d'une liste (pour savoir ce qui est liké).
pub fn video_ids(tracks: &[Track]) -> std::collections::HashSet<String> {
    tracks.iter().map(|t| t.video_id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lecture seule sur un vrai compte : `YTMR_TEST_COOKIE` = en-tête Cookie YouTube.
    #[tokio::test]
    #[ignore = "nécessite une session YouTube"]
    async fn reads_library_without_modifying_it() {
        let cookie = std::env::var("YTMR_TEST_COOKIE").unwrap();
        let account = Account::connect(&cookie).await.unwrap();
        let playlists = account.playlists().await.unwrap();
        println!("playlists : {}", playlists.len());
        for p in playlists.iter().take(5) {
            println!("  {} — {} ({})", p.title, p.tracks_text, p.id);
        }
        let liked = account.playlist(LIKED_ID).await.unwrap();
        println!("titres likés : {}", liked.tracks.len());
        let (pairs, _) = account.set_video_ids(LIKED_ID).await.unwrap();
        println!("setVideoId trouvés : {}", pairs.len());
        if let Some(first) = playlists.first() {
            let p = account.playlist(&first.id).await.unwrap();
            let (ids, complete) = account.set_video_ids(&first.id).await.unwrap();
            println!("setVideoId complet : {complete}");
            println!(
                "« {} » : {} titres, {} setVideoId",
                p.title,
                p.tracks.len(),
                ids.len()
            );
            assert!(p.tracks.is_empty() || !ids.is_empty());
        }
    }

    #[test]
    fn parses_raw_items_and_skips_broken_ones() {
        let col = |runs: Value| {
            serde_json::json!({
                "musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": runs}}
            })
        };
        let good = serde_json::json!({"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "MS_ZaWvmUCs", "playlistSetVideoId": "S1"},
            "flexColumns": [
                col(serde_json::json!([{"text": "Interlude"}])),
                // Artiste sans page (pas de browseId) : ne doit pas tout casser.
                col(serde_json::json!([{"text": "Kpri"}, {"text": " & "}, {"text": "X"}])),
                col(serde_json::json!([{"text": "Tape 2", "navigationEndpoint":
                    {"browseEndpoint": {"browseId": "MPREb_abc"}}}])),
            ],
            "fixedColumns": [{"musicResponsiveListItemFixedColumnRenderer":
                {"text": {"runs": [{"text": "1:37"}]}}}],
            "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [
                {"url": "https://a/60"}, {"url": "https://a/120"}]}}}
        }});
        let broken = serde_json::json!({"musicResponsiveListItemRenderer": {"flexColumns": []}});
        let page = serde_json::json!({"items": [good, broken]});
        let mut tracks = Vec::new();
        collect_tracks(&page, &mut tracks);
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.video_id, "MS_ZaWvmUCs");
        assert_eq!(t.title, "Interlude");
        assert_eq!(t.artist, "Kpri & X");
        assert_eq!(t.album.as_deref(), Some("Tape 2"));
        assert_eq!(t.album_id.as_deref(), Some("MPREb_abc"));
        assert_eq!(t.duration, Some(std::time::Duration::from_secs(97)));
        assert_eq!(t.thumbnail_url.as_deref(), Some("https://a/120"));
    }

    #[test]
    fn decodes_continuation_tokens() {
        assert_eq!(percent_decode("4qmFsg%3D%3D"), "4qmFsg==");
        assert_eq!(percent_decode("abc"), "abc");
        assert_eq!(percent_decode("a%2"), "a%2");
        let page = json!({"contents": [{"continuationItemRenderer": {"continuationEndpoint":
            {"continuationCommand": {"token": "XYZ%3D"}}}}]});
        assert_eq!(continuation_token(&page).as_deref(), Some("XYZ="));
        assert_eq!(continuation_token(&json!({"a": 1})), None);
    }

    #[test]
    fn playlist_ids_are_normalised() {
        assert_eq!(bare_id("VLPLabc"), "PLabc");
        assert_eq!(bare_id("PLabc"), "PLabc");
        assert_eq!(browse_id("PLabc").get_raw(), "VLPLabc");
        assert_eq!(browse_id("VLLM").get_raw(), "VLLM");
        assert_eq!(edit_id("VLPLabc").get_raw(), "PLabc");
    }

    #[test]
    fn finds_set_video_ids_anywhere_in_the_json() {
        let json: Value = serde_json::json!({
            "contents": [{"musicResponsiveListItemRenderer": {
                "playlistItemData": {"videoId": "AAAAAAAAAAA", "playlistSetVideoId": "S1"}
            }}, {"nested": {"deeper": [{"playlistItemData": {
                "videoId": "BBBBBBBBBBB", "playlistSetVideoId": "S2"}}]}}]
        });
        let mut out = Vec::new();
        collect_set_video_ids(&json, &mut out);
        assert_eq!(
            out,
            [
                ("AAAAAAAAAAA".to_owned(), "S1".to_owned()),
                ("BBBBBBBBBBB".to_owned(), "S2".to_owned())
            ]
        );
    }
}
