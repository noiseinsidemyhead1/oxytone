//! Bibliothèque complète : playlists, albums et EP enregistrés, artistes suivis.
//!
//! En clair : la page « Bibliothèque » avec ses onglets et son tri, et le
//! « like » des albums, playlists et artistes (les enregistrer dans la
//! bibliothèque YouTube, ou suivre un artiste). Comme pour les playlists,
//! l'affichage change tout de suite et se corrige si YouTube refuse.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use iced::widget::{Space, button, column, container, mouse_area, row, text};
use iced::{Alignment, Length, Padding, Task};
use ytmr_catalog::account::{LIKED_ID, PlaylistSummary};
use ytmr_catalog::home::HomeKind;
use ytmr_catalog::{AlbumSummary, ArtistSummary};

use crate::app::{App, CARD, Load, Message, Route};
use crate::icons::{Icon, icon};
use crate::library::{self, fold};
use crate::style;
use crate::views::{BOLD, El, SEMIBOLD, cards, cover, error_box, loading, muted, page_scroll};

const COLLECTION_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LibTab {
    #[default]
    Playlists,
    Albums,
    Artists,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LibSort {
    /// Ordre de YouTube : les derniers ajouts d'abord.
    #[default]
    Recent,
    Name,
    /// Auteur d'une playlist, artiste d'un album.
    Creator,
}

#[derive(Debug, Default)]
pub struct CollectionState {
    pub tab: LibTab,
    pub sort: LibSort,
    pub albums: Option<Load<Vec<AlbumSummary>>>,
    pub artists: Option<Load<Vec<ArtistSummary>>>,
}

#[derive(Debug, Clone)]
pub enum Coll {
    Tab(LibTab),
    Sort(LibSort),
    AlbumsLoaded(Result<Vec<AlbumSummary>, String>),
    ArtistsLoaded(Result<Vec<ArtistSummary>, String>),
    /// Enregistre (`true`) ou retire un album ; la playlist audio est cherchée
    /// si elle n'est pas connue.
    SaveAlbum(AlbumSummary, Option<String>, bool),
    SavePlaylist(PlaylistSummary, bool),
    FollowArtist(ArtistSummary, bool),
    /// Réponse de YouTube ; en cas de refus, la liste concernée est rechargée.
    Saved(LibTab, Result<(), String>),
}

impl App {
    pub fn update_collection(&mut self, message: Coll) -> Task<Message> {
        match message {
            Coll::Tab(tab) => {
                self.coll.tab = tab;
                Task::batch([self.load_collection(), self.collection_covers()])
            }
            Coll::Sort(sort) => {
                self.coll.sort = sort;
                Task::none()
            }
            Coll::AlbumsLoaded(result) => {
                if let (Ok(list), Some(dir)) = (&result, &self.lib.cache_dir) {
                    library::write_json(&dir.join("albums.json"), list);
                }
                self.coll.albums = Some(match result {
                    Ok(list) => Load::Ready(list),
                    Err(e) if !matches!(self.coll.albums, Some(Load::Ready(_))) => Load::Failed(e),
                    Err(_) => return Task::none(),
                });
                self.collection_covers()
            }
            Coll::ArtistsLoaded(result) => {
                if let (Ok(list), Some(dir)) = (&result, &self.lib.cache_dir) {
                    library::write_json(&dir.join("artists.json"), list);
                }
                self.coll.artists = Some(match result {
                    Ok(list) => Load::Ready(list),
                    Err(e) if !matches!(self.coll.artists, Some(Load::Ready(_))) => Load::Failed(e),
                    Err(_) => return Task::none(),
                });
                self.collection_covers()
            }
            Coll::SaveAlbum(album, audio_id, save) => {
                // Affiché tout de suite.
                if let Some(Load::Ready(list)) = &mut self.coll.albums {
                    list.retain(|a| a.id != album.id);
                    if save {
                        list.insert(0, album.clone());
                    }
                }
                self.save_collection_cache();
                let toast = self.show_toast(if save {
                    format!("« {} » ajouté à ta bibliothèque", album.title)
                } else {
                    format!("« {} » retiré de ta bibliothèque", album.title)
                });
                let (Some(account), Some(catalog)) = (self.lib.ready(), self.backend_catalog())
                else {
                    return toast;
                };
                let album_id = album.id.clone();
                let op = async move {
                    let audio = match audio_id {
                        Some(id) => id,
                        None => catalog
                            .album(&album_id)
                            .await
                            .map_err(|e| e.to_string())?
                            .audio_playlist_id
                            .ok_or("album sans playlist audio")?,
                    };
                    account
                        .save_playlist(&audio, save)
                        .await
                        .map_err(|e| e.to_string())
                };
                Task::batch([
                    toast,
                    Task::perform(op, |r| Message::Coll(Coll::Saved(LibTab::Albums, r))),
                ])
            }
            Coll::SavePlaylist(playlist, save) => {
                if let Some(Load::Ready(list)) = &mut self.lib.playlists {
                    list.retain(|p| p.id != playlist.id);
                    if save {
                        list.insert(0, playlist.clone());
                    }
                    if let Some(dir) = &self.lib.cache_dir {
                        library::write_json(&dir.join("playlists.json"), list);
                    }
                }
                let toast = self.show_toast(if save {
                    format!("« {} » ajoutée à ta bibliothèque", playlist.title)
                } else {
                    format!("« {} » retirée de ta bibliothèque", playlist.title)
                });
                let Some(account) = self.lib.ready() else {
                    return toast;
                };
                let id = playlist.id.clone();
                let op = async move {
                    account
                        .save_playlist(&id, save)
                        .await
                        .map_err(|e| e.to_string())
                };
                Task::batch([
                    toast,
                    Task::perform(op, |r| Message::Coll(Coll::Saved(LibTab::Playlists, r))),
                ])
            }
            Coll::FollowArtist(artist, follow) => {
                if let Some(Load::Ready(list)) = &mut self.coll.artists {
                    list.retain(|a| a.id != artist.id);
                    if follow {
                        list.insert(0, artist.clone());
                    }
                }
                self.save_collection_cache();
                let toast = self.show_toast(if follow {
                    format!("Tu suis {}", artist.name)
                } else {
                    format!("Tu ne suis plus {}", artist.name)
                });
                let Some(account) = self.lib.ready() else {
                    return toast;
                };
                let id = artist.id.clone();
                let op = async move {
                    account
                        .follow_artist(&id, follow)
                        .await
                        .map_err(|e| e.to_string())
                };
                Task::batch([
                    toast,
                    Task::perform(op, |r| Message::Coll(Coll::Saved(LibTab::Artists, r))),
                ])
            }
            Coll::Saved(tab, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => {
                    let toast = self.show_toast(format!("YouTube a refusé : {e}"));
                    let reload = match tab {
                        LibTab::Playlists => self.reload_playlists(),
                        LibTab::Albums => self.fetch_albums(),
                        LibTab::Artists => self.fetch_artists(),
                    };
                    Task::batch([toast, reload])
                }
            },
        }
    }

    fn backend_catalog(&self) -> Option<Arc<ytmr_catalog::Catalog>> {
        match &self.phase {
            crate::app::Phase::Ready(b) if !self.lib.offline_mode => Some(Arc::clone(&b.catalog)),
            _ => None,
        }
    }

    fn save_collection_cache(&self) {
        let Some(dir) = &self.lib.cache_dir else {
            return;
        };
        if let Some(Load::Ready(list)) = &self.coll.albums {
            library::write_json(&dir.join("albums.json"), list);
        }
        if let Some(Load::Ready(list)) = &self.coll.artists {
            library::write_json(&dir.join("artists.json"), list);
        }
    }

    /// Albums et artistes : cache disque tout de suite, YouTube toutes les 30 min.
    pub fn load_collection(&mut self) -> Task<Message> {
        let Some(dir) = self.lib.cache_dir.clone() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        if !matches!(self.coll.albums, Some(Load::Ready(_))) {
            self.coll.albums = read_json(&dir.join("albums.json")).map(Load::Ready);
        }
        if !matches!(self.coll.artists, Some(Load::Ready(_))) {
            self.coll.artists = read_json(&dir.join("artists.json")).map(Load::Ready);
        }
        if self.coll.albums.is_none()
            || !library::is_fresh(&dir.join("albums.json"), COLLECTION_TTL)
        {
            tasks.push(self.fetch_albums());
        }
        if self.coll.artists.is_none()
            || !library::is_fresh(&dir.join("artists.json"), COLLECTION_TTL)
        {
            tasks.push(self.fetch_artists());
        }
        Task::batch(tasks)
    }

    fn fetch_albums(&mut self) -> Task<Message> {
        if self.coll.albums.is_none() {
            self.coll.albums = Some(Load::Loading);
        }
        let Some(account) = self.lib.ready() else {
            return Task::none();
        };
        Task::perform(
            async move { account.library_albums().await.map_err(|e| e.to_string()) },
            |r| Message::Coll(Coll::AlbumsLoaded(r)),
        )
    }

    fn fetch_artists(&mut self) -> Task<Message> {
        if self.coll.artists.is_none() {
            self.coll.artists = Some(Load::Loading);
        }
        let Some(account) = self.lib.ready() else {
            return Task::none();
        };
        Task::perform(
            async move { account.library_artists().await.map_err(|e| e.to_string()) },
            |r| Message::Coll(Coll::ArtistsLoaded(r)),
        )
    }

    /// Pochettes de l'onglet affiché.
    pub fn collection_covers(&mut self) -> Task<Message> {
        let urls: Vec<Option<String>> = match self.coll.tab {
            LibTab::Playlists => return self.want_list_covers(),
            LibTab::Albums => match &self.coll.albums {
                Some(Load::Ready(list)) => list.iter().map(|a| a.thumbnail_url.clone()).collect(),
                _ => Vec::new(),
            },
            LibTab::Artists => match &self.coll.artists {
                Some(Load::Ready(list)) => list.iter().map(|a| a.thumbnail_url.clone()).collect(),
                _ => Vec::new(),
            },
        };
        let ids: Vec<Option<String>> = urls.iter().map(|u| self.url_cover(u.as_deref())).collect();
        Task::batch(ids.into_iter().map(|id| self.want_cover(id, CARD)))
    }

    // ----- état « liké » -----

    pub fn album_saved(&self, id: &str) -> bool {
        matches!(&self.coll.albums, Some(Load::Ready(list)) if list.iter().any(|a| a.id == id))
    }

    pub fn artist_followed(&self, id: &str) -> bool {
        matches!(&self.coll.artists, Some(Load::Ready(list)) if list.iter().any(|a| a.id == id))
    }

    pub fn playlist_saved(&self, id: &str) -> bool {
        matches!(&self.lib.playlists, Some(Load::Ready(list)) if list.iter().any(|p| p.id == id))
    }

    /// Playlist créée par le compte (rien à « liker » : elle est à lui).
    pub fn playlist_is_mine(&self, p: &PlaylistSummary) -> bool {
        match &self.lib.profile {
            Some(profile) => p.author.trim().is_empty() || p.author.contains(&profile.name),
            None => p.author.trim().is_empty(),
        }
    }
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Bouton cœur (plein = dans la bibliothèque).
pub fn heart<'a>(app: &App, on: bool, message: Message, tip: &str) -> El<'a> {
    crate::views::icon_tip(
        if on { Icon::HeartFilled } else { Icon::Heart },
        16.0,
        if on { app.accent } else { style::TEXT_DIM },
        message,
        tip.to_owned(),
    )
}

/// Carte avec un cœur à côté du titre (survol comme les autres cartes).
/// Le titre est coupé avant le cœur : il ne passe plus dessous.
/// Carte de la bibliothèque. `play` : (identifiant de survol, message) du
/// bouton lecture qui apparaît sur la pochette au survol, comme à l'accueil.
#[allow(clippy::too_many_arguments)]
fn card<'a>(
    app: &App,
    url: Option<&str>,
    radius: f32,
    title: &'a str,
    subtitle: String,
    open: Message,
    like: Option<El<'a>>,
    play: (String, Message),
    // Clic droit : menu de la carte (copier le lien…).
    menu: crate::context::Target,
) -> El<'a> {
    let id = app.url_cover(url);
    let max = if like.is_some() { 16 } else { 20 };
    let mut title_row = row![
        container(
            text(crate::views::ellipsize(title, max))
                .size(14)
                .font(SEMIBOLD)
                .wrapping(iced::widget::text::Wrapping::None)
        )
        .width(Length::Fill)
        .clip(true)
    ]
    .align_y(Alignment::Center)
    .spacing(2);
    if let Some(like) = like {
        title_row = title_row.push(like);
    }
    let (key, play) = play;
    let hovered = app.card_hover.as_deref() == Some(key.as_str());
    let art = crate::views::cover_with_play(
        app,
        cover(app.cover(id.as_deref(), CARD), 164.0, radius),
        164.0,
        hovered,
        play,
    );
    let body = column![
        art,
        container(title_row).width(Length::Fixed(164.0)),
        text(crate::views::ellipsize(&subtitle, 26).into_owned())
            .size(12)
            .color(style::TEXT_DIM)
            .wrapping(iced::widget::text::Wrapping::None)
            .width(Length::Fixed(164.0)),
    ]
    .spacing(6);
    // Le cœur est un bouton dans le bouton : il garde son clic pour lui.
    mouse_area(button(body).padding(10).style(style::card).on_press(open))
        .on_enter(Message::CardHover(key.clone()))
        .on_exit(Message::CardUnhover(key))
        .on_right_press(crate::context::open(menu))
        .into()
}

fn sorted<T>(
    items: &[T],
    sort: LibSort,
    name: impl Fn(&T) -> &str,
    creator: impl Fn(&T) -> &str,
) -> Vec<&T> {
    let mut list: Vec<&T> = items.iter().collect();
    match sort {
        LibSort::Recent => {}
        LibSort::Name => list.sort_by_cached_key(|x| fold(name(x))),
        LibSort::Creator => list.sort_by_cached_key(|x| (fold(creator(x)), fold(name(x)))),
    }
    list
}

fn pill<'a>(app: &App, label: &'a str, active: bool, message: Message) -> El<'a> {
    let accent = app.accent;
    button(text(label).size(13).font(SEMIBOLD))
        .padding(Padding::from([7, 14]))
        .style(move |theme, status| {
            if active {
                style::pill(accent)(theme, status)
            } else {
                style::pill_ghost(theme, status)
            }
        })
        .on_press(message)
        .into()
}

pub fn library_page(app: &App) -> El<'_> {
    let header = row![
        text("Bibliothèque").size(30).font(BOLD).width(Length::Fill),
        // Import Spotify : seulement connecté (la playlist est créée sur le compte).
        if app.lib.ready().is_some() {
            crate::import::open_button()
        } else {
            Space::new().into()
        },
        button(
            row![
                icon(Icon::Plus, 16.0, style::BG),
                text("Nouvelle playlist").size(14).font(SEMIBOLD)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([9, 18]))
        .style(style::pill(app.accent))
        .on_press_maybe(app.lib.ready().map(|_| Message::Lib(library::Lib::OpenNew))),
    ]
    .align_y(Alignment::Center);

    let tab = app.coll.tab;
    let sort = app.coll.sort;
    let tabs = row![
        pill(
            app,
            "Playlists",
            tab == LibTab::Playlists,
            Message::Coll(Coll::Tab(LibTab::Playlists))
        ),
        pill(
            app,
            "Albums et EP",
            tab == LibTab::Albums,
            Message::Coll(Coll::Tab(LibTab::Albums))
        ),
        pill(
            app,
            "Artistes",
            tab == LibTab::Artists,
            Message::Coll(Coll::Tab(LibTab::Artists))
        ),
        Space::new().width(Length::Fill),
        text("Trier :").size(13).color(style::TEXT_DIM),
        pill(
            app,
            "Récents",
            sort == LibSort::Recent,
            Message::Coll(Coll::Sort(LibSort::Recent))
        ),
        pill(
            app,
            "A–Z",
            sort == LibSort::Name,
            Message::Coll(Coll::Sort(LibSort::Name))
        ),
        pill(
            app,
            if tab == LibTab::Artists {
                "Abonnés"
            } else {
                "Auteur"
            },
            sort == LibSort::Creator,
            Message::Coll(Coll::Sort(LibSort::Creator))
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let body: El<'_> = if let Some(info) = library::not_connected(app) {
        info
    } else {
        match tab {
            LibTab::Playlists => playlists_tab(app),
            LibTab::Albums => albums_tab(app),
            LibTab::Artists => artists_tab(app),
        }
    };
    page_scroll(column![header, tabs, body].spacing(20).padding(Padding {
        top: 28.0,
        right: 28.0,
        bottom: 32.0,
        left: 28.0,
    }))
}

fn playlists_tab(app: &App) -> El<'_> {
    match &app.lib.playlists {
        None | Some(Load::Loading) => loading("Chargement de tes playlists…"),
        Some(Load::Failed(e)) => error_box(e),
        Some(Load::Ready(list)) => {
            let mut tiles = vec![library::downloads_tile(app)];
            if !app.lib.offline_mode {
                tiles.insert(0, library::liked_tile(app));
            }
            for p in sorted(
                list,
                app.coll.sort,
                |p| p.title.as_str(),
                |p| p.author.as_str(),
            ) {
                // Hors ligne : seulement les playlists téléchargées.
                if p.id == LIKED_ID
                    || (app.lib.offline_mode && !app.lib.offline_playlists.contains(&p.id))
                {
                    continue;
                }
                let subtitle = [p.tracks_text.as_str(), p.author.as_str()]
                    .into_iter()
                    .filter(|s| !s.trim().is_empty() && s.trim() != "•")
                    .collect::<Vec<_>>()
                    .join(" · ");
                // Les playlists des autres se « likent » (enregistrées / retirées).
                let like = (!app.playlist_is_mine(p)).then(|| {
                    heart(
                        app,
                        true,
                        Message::Coll(Coll::SavePlaylist(p.clone(), false)),
                        "Retirer de la bibliothèque",
                    )
                });
                let tile = card(
                    app,
                    p.thumbnail_url.as_deref(),
                    8.0,
                    &p.title,
                    subtitle,
                    Message::Navigate(Route::Playlist(p.id.clone())),
                    like,
                    (
                        p.id.clone(),
                        Message::PlayItem(HomeKind::Playlist, p.id.clone()),
                    ),
                    crate::context::Target::Playlist(p.clone()),
                );
                tiles.push(if app.playlist_is_mine(p) {
                    crate::views::drop_zone(tile, crate::app::DropTarget::Playlist(p.id.clone()))
                } else {
                    tile
                });
            }
            cards(tiles)
        }
    }
}

/// Accueil du mode hors ligne : « Téléchargés » et les playlists téléchargées.
pub(crate) fn offline_cards(app: &App) -> El<'_> {
    let mut tiles = vec![library::downloads_tile(app)];
    if let Some(Load::Ready(list)) = &app.lib.playlists {
        for p in list
            .iter()
            .filter(|p| app.lib.offline_playlists.contains(&p.id))
        {
            tiles.push(card(
                app,
                p.thumbnail_url.as_deref(),
                8.0,
                &p.title,
                p.author.clone(),
                Message::Navigate(Route::Playlist(p.id.clone())),
                None,
                (
                    p.id.clone(),
                    Message::PlayItem(HomeKind::Playlist, p.id.clone()),
                ),
                crate::context::Target::Playlist(p.clone()),
            ));
        }
    }
    cards(tiles)
}

fn albums_tab(app: &App) -> El<'_> {
    if app.lib.offline_mode {
        return muted("Les albums ne sont pas disponibles hors ligne.");
    }
    match &app.coll.albums {
        None | Some(Load::Loading) => loading("Chargement de tes albums…"),
        Some(Load::Failed(e)) => error_box(e),
        Some(Load::Ready(list)) if list.is_empty() => muted("Aucun album enregistré."),
        Some(Load::Ready(list)) => cards(
            sorted(
                list,
                app.coll.sort,
                |a| a.title.as_str(),
                |a| a.artist.as_str(),
            )
            .into_iter()
            .map(|a| {
                let subtitle = [a.kind.as_str(), a.year.as_str(), a.artist.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                card(
                    app,
                    a.thumbnail_url.as_deref(),
                    8.0,
                    &a.title,
                    subtitle,
                    Message::Navigate(Route::Album(a.id.clone())),
                    Some(heart(
                        app,
                        true,
                        Message::Coll(Coll::SaveAlbum(a.clone(), None, false)),
                        "Retirer de la bibliothèque",
                    )),
                    (
                        a.id.clone(),
                        Message::PlayItem(HomeKind::Album, a.id.clone()),
                    ),
                    crate::context::Target::Album(a.clone()),
                )
            })
            .collect(),
        ),
    }
}

fn artists_tab(app: &App) -> El<'_> {
    if app.lib.offline_mode {
        return muted("Les artistes ne sont pas disponibles hors ligne.");
    }
    match &app.coll.artists {
        None | Some(Load::Loading) => loading("Chargement de tes artistes…"),
        Some(Load::Failed(e)) => error_box(e),
        Some(Load::Ready(list)) if list.is_empty() => muted("Aucun artiste suivi."),
        Some(Load::Ready(list)) => {
            let mut items = sorted(list, app.coll.sort, |a| a.name.as_str(), |_| "");
            if app.coll.sort == LibSort::Creator {
                items
                    .sort_by_key(|a| std::cmp::Reverse(subscriber_count(a.subscribers.as_deref())));
            }
            cards(
                items
                    .into_iter()
                    .map(|a| {
                        card(
                            app,
                            a.thumbnail_url.as_deref(),
                            82.0,
                            &a.name,
                            a.subscribers.clone().unwrap_or_default(),
                            Message::Navigate(Route::Artist(a.id.clone())),
                            Some(heart(
                                app,
                                true,
                                Message::Coll(Coll::FollowArtist(a.clone(), false)),
                                "Ne plus suivre",
                            )),
                            (
                                a.id.clone(),
                                Message::PlayItem(HomeKind::Artist, a.id.clone()),
                            ),
                            crate::context::Target::Artist(a.clone()),
                        )
                    })
                    .collect(),
            )
        }
    }
}

/// « 1,2 M d'abonnés » → 1 200 000 (approximatif, pour trier).
fn subscriber_count(text: Option<&str>) -> u64 {
    let Some(text) = text else {
        return 0;
    };
    let number: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.' || c.is_whitespace())
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    let value: f64 = number.parse().unwrap_or(0.0);
    let rest = text.to_lowercase();
    let factor = if rest.contains(" md") || rest.contains('b') && !rest.contains("abonn") {
        1e9
    } else if rest.contains(" m") || rest.contains("m ") {
        1e6
    } else if rest.contains(" k") || rest.contains("k ") || rest.contains("k\u{a0}") {
        1e3
    } else {
        1.0
    };
    (value * factor) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_subscriber_counts() {
        assert_eq!(subscriber_count(Some("1,2 M d'abonnés")), 1_200_000);
        assert_eq!(subscriber_count(Some("850 k abonnés")), 850_000);
        assert_eq!(subscriber_count(Some("42 abonnés")), 42);
        assert_eq!(subscriber_count(None), 0);
    }
}
