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

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use ytmapi_rs::auth::{AuthToken, BrowserToken};
use ytmapi_rs::common::{ApiOutcome, LikeStatus, PlaylistID, SetVideoID, VideoID, YoutubeID};
use ytmapi_rs::parse::LibraryPlaylist;
use ytmapi_rs::parse::{ParseFrom, ProcessedResult};
use ytmapi_rs::query::playlist::PrivacyStatus;
use ytmapi_rs::query::{
    CreatePlaylistQuery, EditPlaylistQuery, GetHistoryQuery, GetPlaylistDetailsQuery,
    GetPlaylistTracksQuery,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    pub id: String,
    pub title: String,
    /// « 42 titres » tel qu'affiché par YouTube.
    pub tracks_text: String,
    pub author: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Morceau de playlist arrivé au fil du chargement (voir [`Account::playlist_progressive`]).
#[derive(Debug, Clone)]
pub enum PlaylistUpdate {
    /// Titre, auteur… (sans les morceaux).
    Header(Playlist),
    /// Une page de morceaux, à ajouter à la suite.
    Tracks(Vec<Track>),
    /// Fin ; `complete` vaut `false` si YouTube n'a pas rendu toutes les pages.
    Done { complete: bool },
}

/// Identité affichée (nom, @pseudo, photo), comme dans le menu de YouTube Music.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub handle: Option<String>,
    pub photo_url: Option<String>,
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
        let mut playlist = self.header(id).await?;
        playlist.complete = self
            .for_each_page(id, |page| collect_tracks(page, &mut playlist.tracks))
            .await?;
        Ok(playlist)
    }

    /// Comme [`Account::playlist`], mais envoie chaque page dès qu'elle arrive
    /// (les pages viennent forcément l'une après l'autre : chacune donne la clé
    /// de la suivante). Une grande playlist s'affiche ainsi dès la première.
    pub async fn playlist_progressive(
        &self,
        id: &str,
        tx: UnboundedSender<PlaylistUpdate>,
    ) -> Result<(), CatalogError> {
        let header = self.header(id).await?;
        let _ = tx.send(PlaylistUpdate::Header(header));
        let complete = self
            .for_each_page(id, |page| {
                let mut tracks = Vec::new();
                collect_tracks(page, &mut tracks);
                let _ = tx.send(PlaylistUpdate::Tracks(tracks));
            })
            .await?;
        let _ = tx.send(PlaylistUpdate::Done { complete });
        Ok(())
    }

    /// Détails d'une playlist, sans ses morceaux.
    async fn header(&self, id: &str) -> Result<Playlist, CatalogError> {
        let details = self
            .yt
            .query(GetPlaylistDetailsQuery::new(browse_id(id)))
            .await?;
        Ok(Playlist {
            id: bare_id(id).to_owned(),
            title: details.title,
            author: details.author,
            description: details.description,
            duration_text: details.duration,
            thumbnail_url: largest_thumbnail(&details.thumbnails),
            tracks: Vec::new(),
            complete: true,
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

    /// Nom, @pseudo et photo du compte (menu « compte » de YouTube Music).
    pub async fn profile(&self) -> Result<Profile, CatalogError> {
        let raw = self.yt.raw_json_query(AccountMenu).await?;
        let json: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        parse_profile(&json).ok_or(CatalogError::Refused("profil introuvable dans la réponse"))
    }

    /// Morceaux écoutés récemment (sans doublons, du plus récent au plus ancien).
    pub async fn history(&self) -> Result<Vec<Track>, CatalogError> {
        let raw = self.yt.raw_json_query(GetHistoryQuery).await?;
        let json: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let mut tracks = Vec::new();
        collect_items(&json, &mut tracks, false);
        let mut seen = std::collections::HashSet::new();
        tracks.retain(|t| seen.insert(t.video_id.clone()));
        Ok(tracks)
    }

    /// Signale une écoute à YouTube (historique et recommandations), comme
    /// l'appli officielle au début de chaque morceau.
    pub async fn report_play(&self, video_id: &str) -> Result<(), CatalogError> {
        let url = self
            .yt
            .get_song_tracking_url(VideoID::from_raw(video_id.to_owned()))
            .await?;
        self.yt.add_history_item(url).await?;
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
        let mut pairs = Vec::new();
        let complete = self
            .for_each_page(id, |page| collect_set_video_ids(page, &mut pairs))
            .await?;
        Ok((pairs, complete))
    }

    /// Appelle `on_page` pour chaque page brute de la playlist, dans l'ordre.
    /// Rend `false` si une page suivante a échoué (ce qui a été lu est gardé).
    async fn for_each_page(
        &self,
        id: &str,
        mut on_page: impl FnMut(&Value),
    ) -> Result<bool, CatalogError> {
        let first = self
            .yt
            .raw_json_query(GetPlaylistTracksQuery::new(browse_id(id)))
            .await?;
        let mut page: Value = serde_json::from_str(&first).unwrap_or(Value::Null);
        on_page(&page);
        for read in 1..MAX_PAGES {
            let Some(token) = continuation_token(&page) else {
                return Ok(true);
            };
            let next = NextPage {
                browse_id: browse_id(id).get_raw().to_owned(),
                token,
            };
            // YouTube refuse parfois une page isolée : un second essai suffit en général.
            let next_page = match self.yt.raw_json_query::<NextPage>(&next).await {
                Ok(page) => Ok(page),
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    self.yt.raw_json_query::<NextPage>(&next).await
                }
            };
            match next_page {
                Ok(text) => {
                    page = serde_json::from_str(&text).unwrap_or(Value::Null);
                    on_page(&page);
                }
                Err(e) => {
                    tracing::warn!(error = %e, id, pages = read, "playlist incomplète");
                    return Ok(false);
                }
            }
        }
        Ok(false)
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

/// Menu « compte » : seule la réponse brute nous intéresse.
struct AccountMenu;

impl<A: AuthToken> Query<A> for AccountMenu {
    type Output = RawOnly;
    type Method = PostMethod;
}

impl ParseFrom<AccountMenu> for RawOnly {
    fn parse_from(_: ProcessedResult<AccountMenu>) -> ytmapi_rs::Result<Self> {
        Ok(RawOnly)
    }
}

impl PostQuery for AccountMenu {
    fn header(&self) -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }
    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        Vec::new()
    }
    fn path(&self) -> &str {
        "account/account_menu"
    }
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m
            .get(key)
            .or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

fn text_of(v: &Value) -> Option<String> {
    v.get("simpleText")
        .or_else(|| v.pointer("/runs/0/text"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn parse_profile(json: &Value) -> Option<Profile> {
    let header = find_key(json, "activeAccountHeaderRenderer")?;
    Some(Profile {
        name: header.get("accountName").and_then(text_of)?,
        handle: header.get("channelHandle").and_then(text_of),
        photo_url: header
            .pointer("/accountPhoto/thumbnails")
            .and_then(Value::as_array)
            .and_then(|t| t.last())
            .and_then(|t| t.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Jeton de la page suivante (décodé), s'il y en a une.
fn continuation_token(page: &Value) -> Option<String> {
    // Les suggestions ont leur propre suite : on ne doit pas la suivre.
    fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
        match v {
            Value::Object(m) => m.get(key).or_else(|| {
                m.iter()
                    .filter(|(k, _)| !is_suggestion_shelf(k))
                    .find_map(|(_, x)| find(x, key))
            }),
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

/// Étagères de suggestions (« Suggestions », carrousels) affichées sous une
/// playlist par YouTube, surtout quand elle est vide ou courte. Les morceaux de
/// la playlist sont, eux, dans `musicPlaylistShelfRenderer` ou dans les pages
/// suivantes. Sans ce filtre, une playlist neuve « contenait » 7 suggestions.
fn is_suggestion_shelf(key: &str) -> bool {
    matches!(key, "musicShelfRenderer" | "musicCarouselShelfRenderer")
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
            map.iter()
                .filter(|(k, _)| !is_suggestion_shelf(k))
                .for_each(|(_, v)| collect_set_video_ids(v, out));
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

/// Morceaux d'une page de playlist, suggestions exclues.
fn collect_tracks(value: &Value, out: &mut Vec<Track>) {
    collect_items(value, out, true);
}

/// Morceaux (`musicResponsiveListItemRenderer`) trouvés dans une page brute.
/// Un élément incomplet (vidéo supprimée, sans titre) est simplement ignoré.
/// `skip_suggestions` : faux pour l'historique, rangé lui aussi en étagères.
fn collect_items(value: &Value, out: &mut Vec<Track>, skip_suggestions: bool) {
    match value {
        Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                if let Some(track) = track_from_raw(item) {
                    out.push(track);
                }
                return;
            }
            map.iter()
                .filter(|(k, _)| !(skip_suggestions && is_suggestion_shelf(k)))
                .for_each(|(_, v)| collect_items(v, out, skip_suggestions));
        }
        Value::Array(items) => items
            .iter()
            .for_each(|v| collect_items(v, out, skip_suggestions)),
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

    /// Crée une playlist vide, regarde son contenu, puis la supprime.
    #[tokio::test]
    #[ignore = "modifie le compte (playlist créée puis supprimée)"]
    async fn created_playlist_is_empty() {
        let cookie = std::env::var("YTMR_TEST_COOKIE").unwrap();
        let account = Account::connect(&cookie).await.unwrap();
        let id = account
            .create_playlist("ytmr test (supprimée aussitôt)", Privacy::Private, &[])
            .await
            .unwrap();
        println!("créée : {id}");
        let immediate = account.playlist(&id).await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let later = account.playlist(&id).await;
        account.delete_playlist(&id).await.unwrap();
        println!("supprimée");
        println!(
            "lecture immédiate : {:?}",
            immediate.as_ref().map(|p| p.tracks.len())
        );
        let content = later.unwrap();
        for t in &content.tracks {
            println!("  - {} / {}", t.title, t.artist);
        }
        println!("titres dans la playlist neuve : {}", content.tracks.len());
    }

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
        let profile = account.profile().await.unwrap();
        println!(
            "profil : {} {:?} photo={}",
            profile.name,
            profile.handle,
            profile.photo_url.is_some()
        );
        let history = account.history().await.unwrap();
        println!("historique : {} titres", history.len());
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
    fn parses_account_menu_profile() {
        let json = json!({"actions": [{"openPopupAction": {"popup": {"multiPageMenuRenderer": {
        "header": {"activeAccountHeaderRenderer": {
            "accountName": {"runs": [{"text": "Noise"}]},
            "channelHandle": {"runs": [{"text": "@noise"}]},
            "accountPhoto": {"thumbnails": [{"url": "https://yt3.ggpht.com/a=s48"},
                                            {"url": "https://yt3.ggpht.com/a=s88"}]}
        }}}}}}]});
        let profile = parse_profile(&json).unwrap();
        assert_eq!(profile.name, "Noise");
        assert_eq!(profile.handle.as_deref(), Some("@noise"));
        assert_eq!(
            profile.photo_url.as_deref(),
            Some("https://yt3.ggpht.com/a=s88")
        );
        assert!(parse_profile(&json!({})).is_none());
    }

    #[test]
    fn ignores_suggestion_shelves_under_a_playlist() {
        let item = |id: &str| {
            json!({"musicResponsiveListItemRenderer": {
                "playlistItemData": {"videoId": id, "playlistSetVideoId": "S"},
                "flexColumns": [{"musicResponsiveListItemFlexColumnRenderer":
                    {"text": {"runs": [{"text": "t"}]}}}]
            }})
        };
        let page = json!({"contents": {"sectionListRenderer": {"contents": [
            {"musicPlaylistShelfRenderer": {"contents": [item("AAAAAAAAAAA")]}},
            {"musicShelfRenderer": {"title": "Suggestions", "contents": [item("BBBBBBBBBBB")],
                "continuations": [{"nextContinuationData": {"continuation": "SUGG"}}]}}
        ]}}});
        let mut tracks = Vec::new();
        collect_tracks(&page, &mut tracks);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].video_id, "AAAAAAAAAAA");
        assert_eq!(
            continuation_token(&page),
            None,
            "pas de suite des suggestions"
        );
        let mut history = Vec::new();
        collect_items(&page, &mut history, false);
        assert_eq!(history.len(), 2, "l'historique garde ses étagères");
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
