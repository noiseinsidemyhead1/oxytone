//! Page d'accueil de YouTube Music (« Réécouter », mix, recommandations).
//!
//! En clair : on demande à YouTube la même page d'accueil que l'appli
//! officielle (`FEmusic_home`) et on en lit les rangées : chacune a un titre
//! et des cartes (album, playlist ou mix, artiste, morceau). Tout ce qui n'a
//! pas une forme reconnue (podcasts, vidéos sans lien) est simplement ignoré.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ytmr_core::Track;

use crate::raw::{RawPost, artist_id, find_all, runs_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomeKind {
    Album,
    /// Playlist, mix ou radio (ouvrable comme une playlist).
    Playlist,
    Artist,
    Song,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeItem {
    pub kind: HomeKind,
    /// Album `MPRE…`, playlist (sans `VL`), artiste `UC…` ou vidéo.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
    /// Pour un morceau : de quoi le lire tout de suite.
    pub track: Option<Track>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeShelf {
    pub title: String,
    pub items: Vec<HomeItem>,
}

/// Première page de l'accueil, en français.
pub(crate) fn home_query() -> RawPost {
    let mut body = Map::new();
    body.insert("browseId".into(), json!("FEmusic_home"));
    body.insert("context".into(), french_context());
    RawPost {
        path: "browse",
        body,
        query: Vec::new(),
    }
}

/// Suite de l'accueil (rangées suivantes).
pub(crate) fn home_next(token: &str) -> RawPost {
    let mut body = Map::new();
    body.insert("context".into(), french_context());
    RawPost {
        path: "browse",
        body,
        query: vec![
            ("ctoken", token.to_owned()),
            ("continuation", token.to_owned()),
            ("type", "next".to_owned()),
        ],
    }
}

/// Contexte de la requête : le même que ytmapi-rs, avec la langue en plus
/// (sans elle, les titres des rangées arrivent en anglais).
pub(crate) fn french_context() -> Value {
    json!({"client": {
        "clientName": "WEB_REMIX",
        "clientVersion": "1.20251020.01.00",
        "hl": "fr",
        "gl": "FR",
        "user": {}
    }})
}

/// Rangées d'une page (première ou suite).
pub(crate) fn parse_shelves(page: &Value) -> Vec<HomeShelf> {
    let mut shelves = Vec::new();
    find_all(page, "musicCarouselShelfRenderer", &mut shelves);
    shelves.into_iter().filter_map(parse_shelf).collect()
}

fn parse_shelf(shelf: &Value) -> Option<HomeShelf> {
    let title = runs_text(shelf.pointer("/header/musicCarouselShelfBasicHeaderRenderer/title"))?;
    let items: Vec<HomeItem> = shelf
        .get("contents")?
        .as_array()?
        .iter()
        .filter_map(|c| {
            c.get("musicTwoRowItemRenderer")
                .and_then(two_row_item)
                .or_else(|| c.get("musicResponsiveListItemRenderer").and_then(list_item))
        })
        .collect();
    (!items.is_empty()).then_some(HomeShelf { title, items })
}

fn largest(thumbnails: Option<&Value>) -> Option<String> {
    thumbnails?
        .as_array()?
        .last()?
        .get("url")?
        .as_str()
        .map(str::to_owned)
}

/// Carte (album, playlist, artiste ou morceau).
fn two_row_item(r: &Value) -> Option<HomeItem> {
    let title = runs_text(r.get("title"))?;
    let subtitle = runs_text(r.get("subtitle")).unwrap_or_default();
    let thumbnail_url =
        largest(r.pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails"));
    let nav = r.get("navigationEndpoint")?;
    if let Some(browse) = nav.get("browseEndpoint") {
        let id = browse.get("browseId")?.as_str()?;
        let page_type = browse
            .pointer(
                "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType",
            )
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (kind, id) = match page_type {
            "MUSIC_PAGE_TYPE_ALBUM" => (HomeKind::Album, id),
            "MUSIC_PAGE_TYPE_PLAYLIST" => (HomeKind::Playlist, id.strip_prefix("VL").unwrap_or(id)),
            "MUSIC_PAGE_TYPE_ARTIST" => (HomeKind::Artist, id),
            _ => return None,
        };
        return Some(HomeItem {
            kind,
            id: id.to_owned(),
            title,
            subtitle,
            thumbnail_url,
            track: None,
        });
    }
    let video_id = nav.pointer("/watchEndpoint/videoId")?.as_str()?.to_owned();
    // « Titre • Artiste • Album » ou « Artiste • 164 k vues » : YouTube préfixe
    // parfois le type (« Titre » = morceau en français), qui n'est pas l'artiste.
    let runs = r
        .pointer("/subtitle/runs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let text = |group: &[Value]| -> String {
        group
            .iter()
            .filter_map(|x| x.get("text").and_then(Value::as_str))
            .collect()
    };
    let groups: Vec<&[Value]> = runs
        .split(|x| x.get("text").and_then(Value::as_str) == Some(" • "))
        .filter(|g| !is_type_label(&text(g)))
        .collect();
    let links_to = |g: &&[Value], prefix: &str| {
        g.iter()
            .any(|x| crate::raw::browse_id(x).is_some_and(|b| b.starts_with(prefix)))
    };
    let artist_group = groups
        .iter()
        .find(|g| links_to(g, "UC"))
        .or_else(|| groups.first());
    let album_run = runs
        .iter()
        .find(|x| crate::raw::browse_id(x).is_some_and(|b| b.starts_with("MPRE")));
    let subtitle = groups
        .iter()
        .map(|g| text(g))
        .collect::<Vec<_>>()
        .join(" • ");
    let track = Track {
        unavailable: false,
        plays: None,
        video_id: video_id.clone(),
        title: title.clone(),
        artist: artist_group.map(|g| text(g)).unwrap_or_default(),
        artist_id: artist_id(runs),
        album: album_run
            .and_then(|x| x.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        album_id: album_run.and_then(crate::raw::browse_id).map(str::to_owned),
        duration: None,
        thumbnail_url: thumbnail_url.clone(),
    };
    Some(HomeItem {
        kind: HomeKind::Song,
        id: video_id,
        title,
        subtitle,
        thumbnail_url,
        track: Some(track),
    })
}

/// Étiquette de type placée devant le sous-titre d'une carte (pas un nom).
fn is_type_label(text: &str) -> bool {
    matches!(
        text.trim(),
        "Titre" | "Morceau" | "Chanson" | "Song" | "Vidéo" | "Video" | "Single" | "EP" | "Album"
    )
}

/// Ligne de morceau (« Sélection rapide »).
fn list_item(r: &Value) -> Option<HomeItem> {
    let track = crate::raw::search_song(r)?;
    let subtitle = match &track.album {
        Some(album) => format!("{} • {album}", track.artist),
        None => track.artist.clone(),
    };
    Some(HomeItem {
        kind: HomeKind::Song,
        id: track.video_id.clone(),
        title: track.title.clone(),
        subtitle,
        thumbnail_url: track.thumbnail_url.clone(),
        track: Some(track),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cards_of_each_kind() {
        let card = |nav: Value, title: &str| {
            json!({"musicTwoRowItemRenderer": {
                "title": {"runs": [{"text": title}]},
                "subtitle": {"runs": [{"text": "Daft Punk", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCabc"}}}, {"text": " • "}, {"text": "1 M de vues"}]},
                "thumbnailRenderer": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [{"url": "a"}, {"url": "b"}]}}},
                "navigationEndpoint": nav
            }})
        };
        let browse = |id: &str, page: &str| json!({"browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": page}}}});
        let page = json!({"contents": [{"musicCarouselShelfRenderer": {
            "header": {"musicCarouselShelfBasicHeaderRenderer": {"title": {"runs": [{"text": "Réécouter"}]}}},
            "contents": [
                card(browse("MPREb_x", "MUSIC_PAGE_TYPE_ALBUM"), "Discovery"),
                card(browse("VLPLabc", "MUSIC_PAGE_TYPE_PLAYLIST"), "Ma liste"),
                card(browse("UCxyz", "MUSIC_PAGE_TYPE_ARTIST"), "Daft Punk"),
                card(json!({"watchEndpoint": {"videoId": "vid1"}}), "One More Time"),
                card(browse("MPSPx", "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE"), "Podcast"),
            ]
        }}]});
        let shelves = parse_shelves(&page);
        assert_eq!(shelves.len(), 1);
        let items = &shelves[0].items;
        assert_eq!(shelves[0].title, "Réécouter");
        assert_eq!(items.len(), 4, "le podcast est ignoré");
        assert_eq!(items[0].kind, HomeKind::Album);
        assert_eq!(items[1].id, "PLabc");
        assert_eq!(items[2].kind, HomeKind::Artist);
        let song = items[3].track.as_ref().unwrap();
        assert_eq!(song.artist, "Daft Punk");
        assert_eq!(song.artist_id.as_deref(), Some("UCabc"));
        assert_eq!(items[3].thumbnail_url.as_deref(), Some("b"));
    }

    #[test]
    fn type_label_is_not_the_artist() {
        // Constaté sur « Wonderpunch » (Xtrullor) : « Titre • Xtrullor • The 1st Era ».
        let item = json!({
            "title": {"runs": [{"text": "Wonderpunch"}]},
            "subtitle": {"runs": [
                {"text": "Titre"}, {"text": " • "},
                {"text": "Xtrullor", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCx"}}},
                {"text": " • "},
                {"text": "The 1st Era", "navigationEndpoint": {"browseEndpoint": {"browseId": "MPREb_era"}}}
            ]},
            "navigationEndpoint": {"watchEndpoint": {"videoId": "eFASApYWua8"}}
        });
        let card = two_row_item(&item).unwrap();
        let song = card.track.unwrap();
        assert_eq!(song.artist, "Xtrullor");
        assert_eq!(song.album.as_deref(), Some("The 1st Era"));
        assert_eq!(song.album_id.as_deref(), Some("MPREb_era"));
        assert_eq!(card.subtitle, "Xtrullor • The 1st Era");
    }
}
