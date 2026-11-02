//! Lecture directe des réponses JSON de YouTube Music.
//!
//! En clair : quand le décodeur de ytmapi-rs est trop strict (il rejette une
//! recherche entière dès qu'un résultat n'a pas la forme attendue), on envoie
//! la même requête et on lit nous-mêmes ce qui nous intéresse, en ignorant
//! simplement les éléments incomplets.

use std::borrow::Cow;

use oxytone_core::Track;
use oxytone_core::track::parse_duration;
use serde_json::{Map, Value, json};
use ytmapi_rs::auth::AuthToken;
use ytmapi_rs::parse::{ParseFrom, ProcessedResult};
use ytmapi_rs::query::{PostMethod, PostQuery, Query};

/// Requête POST quelconque dont on ne garde que le JSON brut.
pub(crate) struct RawPost {
    pub path: &'static str,
    pub body: Map<String, Value>,
    /// Paramètres d'URL (jeton de continuation…).
    pub query: Vec<(&'static str, String)>,
}

#[derive(Debug)]
pub(crate) struct Ignored;

impl<A: AuthToken> Query<A> for RawPost {
    type Output = Ignored;
    type Method = PostMethod;
}

impl ParseFrom<RawPost> for Ignored {
    fn parse_from(_: ProcessedResult<RawPost>) -> ytmapi_rs::Result<Self> {
        Ok(Ignored)
    }
}

impl PostQuery for RawPost {
    fn header(&self) -> Map<String, Value> {
        self.body.clone()
    }
    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        self.query
            .iter()
            .map(|(k, v)| (*k, Cow::from(v.as_str())))
            .collect()
    }
    fn path(&self) -> &str {
        self.path
    }
}

/// Filtre « Titres » de la recherche, avec correction orthographique
/// (« daft pnk » trouve quand même Daft Punk). Même code que ytmapi-rs
/// (`SongsFilter` + `SpellingMode::WithSuggestions`).
const SONGS_WITH_SUGGESTIONS: &str = "EgWKAQIIAUICCAFqDBAOEAoQAxAEEAkQBQ%3D%3D";

pub(crate) fn songs_search(query: &str) -> RawPost {
    let mut body = Map::new();
    body.insert("query".into(), json!(query));
    body.insert("params".into(), json!(SONGS_WITH_SUGGESTIONS));
    RawPost {
        path: "search",
        body,
        query: Vec::new(),
    }
}

/// Espaces en trop retirés : « daft   punk  » et « daft punk » donnent la même recherche.
pub fn normalize_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Tous les objets `key` du JSON, dans l'ordre du document.
pub(crate) fn find_all<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            if let Some(x) = m.get(key) {
                out.push(x);
            }
            m.values().for_each(|x| find_all(x, key, out));
        }
        Value::Array(a) => a.iter().for_each(|x| find_all(x, key, out)),
        _ => {}
    }
}

pub(crate) fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m
            .get(key)
            .or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// Texte d'un objet `{runs: [{text}…]}` mis bout à bout.
pub(crate) fn runs_text(v: Option<&Value>) -> Option<String> {
    let runs = v?.get("runs")?.as_array()?;
    let text: String = runs
        .iter()
        .filter_map(|r| r.get("text").and_then(Value::as_str))
        .collect();
    (!text.trim().is_empty()).then_some(text)
}

pub(crate) fn browse_id(run: &Value) -> Option<&str> {
    run.pointer("/navigationEndpoint/browseEndpoint/browseId")
        .and_then(Value::as_str)
}

/// Plus grande vignette d'un `musicThumbnailRenderer` (ou équivalent).
pub(crate) fn thumbnail(item: &Value) -> Option<String> {
    item.pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .or_else(|| item.pointer("/thumbnail/croppedSquareThumbnailRenderer/thumbnail/thumbnails"))
        .and_then(Value::as_array)
        .and_then(|t| t.last())
        .and_then(|t| t.get("url"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn flex_runs(item: &Value, column: usize) -> Option<&Vec<Value>> {
    item.pointer(&format!(
        "/flexColumns/{column}/musicResponsiveListItemFlexColumnRenderer/text/runs"
    ))
    .and_then(Value::as_array)
}

pub(crate) fn video_id(item: &Value) -> Option<String> {
    item.pointer("/playlistItemData/videoId")
        .or_else(|| {
            item.pointer(
                "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/watchEndpoint/videoId",
            )
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Premier artiste cliquable (`UC…`) d'une liste de segments.
pub(crate) fn artist_id(runs: &[Value]) -> Option<String> {
    runs.iter()
        .filter_map(browse_id)
        .find(|id| id.starts_with("UC"))
        .map(str::to_owned)
}

/// Morceaux d'une recherche « Titres ». La 2e colonne a la forme
/// « Artiste(s) • Album • 3:45 » : on la découpe sur les « • ».
pub(crate) fn search_songs(page: &Value) -> Vec<Track> {
    let mut items = Vec::new();
    find_all(page, "musicResponsiveListItemRenderer", &mut items);
    items.into_iter().filter_map(search_song).collect()
}

pub(crate) fn search_song(item: &Value) -> Option<Track> {
    let video_id = video_id(item)?;
    let title = flex_runs(item, 0)?
        .first()?
        .get("text")
        .and_then(Value::as_str)?
        .to_owned();
    let runs = flex_runs(item, 1).map(Vec::as_slice).unwrap_or_default();
    let groups: Vec<&[Value]> = runs
        .split(|r| r.get("text").and_then(Value::as_str) == Some(" • "))
        .collect();
    let text = |group: &[Value]| -> String {
        group
            .iter()
            .filter_map(|r| r.get("text").and_then(Value::as_str))
            .collect()
    };
    let artist = groups.first().map(|g| text(g)).unwrap_or_default();
    let album = groups.iter().find_map(|g| {
        g.iter()
            .find(|r| browse_id(r).is_some_and(|b| b.starts_with("MPRE")))
    });
    let duration = groups.last().and_then(|g| parse_duration(&text(g)));
    Some(Track {
        unavailable: false,
        plays: None,
        video_id,
        title,
        artist_id: groups.first().and_then(|g| artist_id(g)),
        artist,
        album: album.and_then(|r| r.get("text").and_then(Value::as_str).map(str::to_owned)),
        album_id: album.and_then(browse_id).map(str::to_owned),
        duration,
        thumbnail_url: thumbnail(item),
    })
}

/// Page d'artiste lue sans exiger de forme précise : en-tête (nom, photo,
/// description, abonnés), titres populaires, albums, singles et EP. Ce qui
/// manque reste vide ; `None` seulement sans nom.
pub(crate) fn artist_page(id: &str, page: &Value) -> Option<crate::Artist> {
    let header = find_key(page, "musicImmersiveHeaderRenderer")
        .or_else(|| find_key(page, "musicVisualHeaderRenderer"))
        .or_else(|| find_key(page, "musicResponsiveHeaderRenderer"))?;
    let name = runs_text(header.get("title"))?;
    let thumbnail_url = thumbnail(header).or_else(|| {
        header
            .pointer("/foregroundThumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
            .and_then(Value::as_array)
            .and_then(|t| t.last())
            .and_then(|t| t.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let description = runs_text(header.get("description"));
    let subscribers = header
        .pointer("/subscriptionButton/subscribeButtonRenderer/subscriberCountText")
        .and_then(|v| runs_text(Some(v)));

    // Titres populaires : la première liste de morceaux de la page.
    let mut shelves = Vec::new();
    find_all(page, "musicShelfRenderer", &mut shelves);
    let songs: Vec<Track> = shelves
        .first()
        .and_then(|s| s.get("contents"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("musicResponsiveListItemRenderer"))
                .filter_map(|item| {
                    // 3e colonne des titres populaires : « 12 M de lectures ».
                    let mut t = search_song(item)?;
                    t.plays = flex_runs(item, 2)
                        .and_then(|r| r.first())
                        .and_then(|r| r.get("text"))
                        .and_then(Value::as_str)
                        .and_then(crate::plays_text);
                    Some(t)
                })
                .map(|mut t| {
                    if t.artist.trim().is_empty() {
                        t.artist = name.clone();
                    }
                    if t.artist_id.is_none() {
                        t.artist_id = Some(id.to_owned());
                    }
                    t
                })
                .collect()
        })
        .unwrap_or_default();

    // Rangées d'albums : « Albums », « Singles et EP »… (titres en français).
    let mut albums = Vec::new();
    let mut singles = Vec::new();
    for shelf in crate::home::parse_shelves(page) {
        let title = shelf.title.to_lowercase();
        let kind = if title.contains("single") || title.contains("ep") {
            "Single"
        } else if title.contains("album") {
            "Album"
        } else {
            continue;
        };
        let list: Vec<crate::AlbumSummary> = shelf
            .items
            .into_iter()
            .filter(|i| i.kind == crate::home::HomeKind::Album)
            .map(|i| crate::AlbumSummary {
                id: i.id,
                title: i.title,
                artist: name.clone(),
                year: i.subtitle,
                kind: kind.to_owned(),
                thumbnail_url: i.thumbnail_url,
            })
            .collect();
        if kind == "Album" {
            albums.extend(list);
        } else {
            singles.extend(list);
        }
    }
    Some(crate::Artist {
        id: id.to_owned(),
        name,
        description,
        subscribers,
        thumbnail_url,
        songs,
        albums,
        singles,
    })
}

/// En-tête d'une page de playlist, lu sans exiger de forme précise (les
/// playlists des autres n'ont pas toujours d'auteur en `straplineTextOne`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistHeader {
    pub title: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub second_line: Option<String>,
    pub thumbnail_url: Option<String>,
    /// La page propose de la modifier : elle appartient au compte.
    pub owned: bool,
}

pub(crate) fn playlist_header(page: &Value) -> PlaylistHeader {
    let owned = find_key(page, "musicEditablePlaylistDetailHeaderRenderer").is_some();
    let header = find_key(page, "musicResponsiveHeaderRenderer")
        .or_else(|| find_key(page, "musicDetailHeaderRenderer"))
        .or_else(|| find_key(page, "musicImmersiveHeaderRenderer"));
    let Some(h) = header else {
        return PlaylistHeader {
            owned,
            ..PlaylistHeader::default()
        };
    };
    let author = runs_text(h.get("straplineTextOne"))
        .or_else(|| {
            h.pointer("/facepile/avatarStackViewModel/text/content")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|a| !a.trim().is_empty());
    let description = h
        .pointer("/description/musicDescriptionShelfRenderer/description")
        .and_then(|d| runs_text(Some(d)))
        .or_else(|| runs_text(h.get("description")));
    PlaylistHeader {
        title: runs_text(h.get("title")),
        author,
        description,
        second_line: runs_text(h.get("secondSubtitle")),
        thumbnail_url: thumbnail(h),
        owned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, browse: Option<&str>) -> Value {
        match browse {
            Some(b) => {
                json!({"text": text, "navigationEndpoint": {"browseEndpoint": {"browseId": b}}})
            }
            None => json!({"text": text}),
        }
    }

    #[test]
    fn parses_search_song_columns() {
        let item = json!({"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "Jb6gcoR266U"},
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [run("Around the World", None)]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                    run("Daft Punk", Some("UCRr1xG_2WIDs18a6cIiCxeA")), run(" • ", None),
                    run("Homework", Some("MPREb_dMHztPsrDgN")), run(" • ", None), run("7:10", None)
                ]}}}
            ]
        }});
        // Un résultat sans titre ne fait pas échouer les autres.
        let broken =
            json!({"musicResponsiveListItemRenderer": {"playlistItemData": {"videoId": "x"}}});
        let tracks = search_songs(&json!([item, broken]));
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.artist, "Daft Punk");
        assert_eq!(t.artist_id.as_deref(), Some("UCRr1xG_2WIDs18a6cIiCxeA"));
        assert_eq!(t.album.as_deref(), Some("Homework"));
        assert_eq!(t.duration, Some(std::time::Duration::from_secs(430)));
    }

    #[test]
    fn reads_header_of_someone_elses_playlist() {
        let page = json!({"contents": {"twoColumnBrowseResultsRenderer": {"tabs": [{
            "musicResponsiveHeaderRenderer": {
                "title": {"runs": [run("daft punk playlist", None)]},
                "secondSubtitle": {"runs": [run("39 titres", None)]},
                "facepile": {"avatarStackViewModel": {"text": {"content": "Quelqu'un"}}}
            }
        }]}}});
        let h = playlist_header(&page);
        assert_eq!(h.title.as_deref(), Some("daft punk playlist"));
        assert_eq!(h.author.as_deref(), Some("Quelqu'un"));
        assert!(!h.owned);
    }

    #[test]
    fn artist_page_without_header_image() {
        let page = json!({"header": {"musicImmersiveHeaderRenderer": {
            "title": {"runs": [run("Artiste sans photo", None)]}
        }}, "contents": [{"musicShelfRenderer": {"contents": [{"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "v1"},
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [run("Titre", None)]}}}
            ]
        }}]}}]});
        let artist = artist_page("UCx", &page).unwrap();
        assert_eq!(artist.name, "Artiste sans photo");
        assert_eq!(artist.thumbnail_url, None);
        assert_eq!(artist.songs.len(), 1);
        assert_eq!(artist.songs[0].artist, "Artiste sans photo");
    }

    #[test]
    fn normalizes_spaces() {
        assert_eq!(normalize_query("  daft   punk "), "daft punk");
    }
}
