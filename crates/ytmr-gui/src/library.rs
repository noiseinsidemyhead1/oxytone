//! Bibliothèque du compte YouTube : playlists, titres likés, modifications.
//!
//! En clair : tout ce qui touche au compte de l'utilisateur. Les changements
//! (like, ajout, création, renommage, suppression) s'affichent tout de suite,
//! puis sont envoyés à YouTube ; en cas de refus, l'affichage est corrigé et un
//! message explique pourquoi.

use std::collections::HashSet;
use std::sync::Arc;

use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, text, text_input,
};
use iced::{Alignment, Color, Length, Padding, Task};
use ytmr_catalog::CatalogError;
use ytmr_catalog::account::{Account, LIKED_ID, Playlist, PlaylistSummary, Privacy};
use ytmr_core::{Command, Track};

use crate::app::{App, CARD, HERO, Load, Message, Route, THUMB};
use crate::backend::AccountState;
use crate::icons::{Icon, icon};
use crate::style;
use crate::views::{
    BOLD, El, SEMIBOLD, cards, cover, error_box, icon_tip, loading, muted, page_header,
    page_scroll, section_title, track_row,
};

pub const NEW_PLAYLIST_INPUT: &str = "nouvelle-playlist";
pub const RENAME_INPUT: &str = "renommer-playlist";

/// Poignée clonable vers le compte (le jeton n'apparaît jamais dans les logs).
#[derive(Debug, Clone)]
pub struct AccountHandle(pub Arc<Account>);

#[derive(Default)]
pub enum AccountView {
    #[default]
    Connecting,
    Ready(Arc<Account>),
    Unavailable(String),
}

/// Fenêtres par-dessus la page.
#[derive(Debug, Clone)]
pub enum Overlay {
    AddTo(Track),
    NewPlaylist,
    ConfirmDelete { id: String, title: String },
}

#[derive(Default)]
pub struct LibraryState {
    pub account: AccountView,
    pub playlists: Option<Load<Vec<PlaylistSummary>>>,
    pub liked: HashSet<String>,
    pub liked_count: Option<usize>,
    pub playlist: Option<(String, Load<Playlist>)>,
    pub overlay: Option<Overlay>,
    pub renaming: Option<String>,
    pub new_name: String,
}

impl LibraryState {
    pub fn ready(&self) -> Option<Arc<Account>> {
        match &self.account {
            AccountView::Ready(a) => Some(Arc::clone(a)),
            _ => None,
        }
    }

    pub fn is_liked(&self, video_id: &str) -> bool {
        self.liked.contains(video_id)
    }

    fn playlist_title(&self, id: &str) -> String {
        if id == LIKED_ID {
            return "Titres likés".into();
        }
        match &self.playlists {
            Some(Load::Ready(list)) => list
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.title.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Lib {
    AccountReady(Result<AccountHandle, String>),
    PlaylistsLoaded(Result<Vec<PlaylistSummary>, String>),
    LikedLoaded(Result<Playlist, String>),
    PlaylistLoaded(String, Result<Playlist, String>),
    ToggleLike(Track),
    LikeDone(Track, bool, Result<(), String>),
    OpenAddTo(Track),
    OpenNew,
    CloseOverlay,
    AddTo(String, Track),
    Added(String, Result<(), String>),
    NewNameInput(String),
    Create,
    Created(Result<String, String>),
    RenameStart,
    RenameInput(String),
    RenameCommit,
    Renamed(String, Result<(), String>),
    AskDelete,
    Delete,
    Deleted(Result<(), String>),
    RemoveTrack(String, String),
    Removed(String, Result<(), String>),
}

/// Attend la fin de la connexion au compte (lancée par le backend).
pub fn wait_for_account(mut rx: tokio::sync::watch::Receiver<AccountState>) -> Task<Message> {
    Task::perform(
        async move {
            let state = rx
                .wait_for(|s| !matches!(s, AccountState::Connecting))
                .await
                .map(|s| s.clone());
            match state {
                Ok(AccountState::Ready(a)) => Ok(AccountHandle(a)),
                Ok(AccountState::Unavailable(e)) => Err(e),
                Ok(AccountState::Connecting) | Err(_) => Err("backend arrêté".to_owned()),
            }
        },
        |r| Message::Lib(Lib::AccountReady(r)),
    )
}

/// Lance une opération sur le compte (`None` = pas connecté).
fn call<T: Send + 'static>(
    op: Option<impl Future<Output = Result<T, CatalogError>> + Send + 'static>,
    done: impl Fn(Result<T, String>) -> Lib + Send + 'static,
) -> Task<Message> {
    match op {
        Some(op) => Task::perform(
            async move { op.await.map_err(|e| e.to_string()) },
            move |r| Message::Lib(done(r)),
        ),
        None => Task::done(Message::Lib(done(
            Err("compte YouTube non connecté".into()),
        ))),
    }
}

impl App {
    pub fn load_playlists(&mut self) -> Task<Message> {
        if self.lib.playlists.is_none() {
            self.lib.playlists = Some(Load::Loading);
        }
        let op = self.lib.ready().map(|a| async move { a.playlists().await });
        call(op, Lib::PlaylistsLoaded)
    }

    pub fn open_playlist(&mut self, id: String) -> Task<Message> {
        if !matches!(&self.lib.playlist, Some((cur, Load::Ready(_))) if *cur == id) {
            self.lib.playlist = Some((id.clone(), Load::Loading));
        }
        self.lib.renaming = None;
        let wanted = id.clone();
        let op = self
            .lib
            .ready()
            .map(|a| async move { a.playlist(&wanted).await });
        call(op, move |r| Lib::PlaylistLoaded(id.clone(), r))
    }

    pub fn update_library(&mut self, message: Lib) -> Task<Message> {
        match message {
            Lib::AccountReady(Ok(AccountHandle(account))) => {
                self.lib.account = AccountView::Ready(Arc::clone(&account));
                let liked = call(
                    Some(async move { account.playlist(LIKED_ID).await }),
                    Lib::LikedLoaded,
                );
                let mut tasks = vec![self.load_playlists(), liked];
                if let Route::Playlist(id) = self.route.clone() {
                    tasks.push(self.open_playlist(id));
                }
                Task::batch(tasks)
            }
            Lib::AccountReady(Err(e)) => {
                self.lib.account = AccountView::Unavailable(e);
                self.lib.playlists = None;
                Task::none()
            }
            Lib::PlaylistsLoaded(result) => {
                match result {
                    Ok(list) => {
                        let ids: Vec<_> = list
                            .iter()
                            .map(|p| self.url_cover(p.thumbnail_url.as_deref()))
                            .collect();
                        self.lib.playlists = Some(Load::Ready(list));
                        return Task::batch(ids.into_iter().map(|id| self.want_cover(id, CARD)));
                    }
                    Err(e) => self.lib.playlists = Some(Load::Failed(e)),
                }
                Task::none()
            }
            Lib::LikedLoaded(result) => {
                match result {
                    Ok(playlist) => {
                        self.lib.liked =
                            playlist.tracks.iter().map(|t| t.video_id.clone()).collect();
                        self.lib.liked_count = Some(playlist.tracks.len());
                    }
                    Err(e) => tracing::warn!("titres likés : {e}"),
                }
                Task::none()
            }
            Lib::PlaylistLoaded(id, result) => {
                if !matches!(&self.lib.playlist, Some((cur, _)) if *cur == id) {
                    return Task::none();
                }
                match result {
                    Ok(playlist) => {
                        let cover_id = self.url_cover(playlist.thumbnail_url.as_deref());
                        let mut tasks = vec![
                            self.want_cover(cover_id.clone(), HERO),
                            self.want_accent(cover_id),
                            self.want_track_covers(&playlist.tracks, THUMB),
                        ];
                        for track in playlist.tracks.iter().take(3).rev() {
                            tasks.push(self.send(Command::Prefetch(track.video_id.clone())));
                        }
                        if !playlist.complete {
                            tasks.push(
                                self.show_toast(
                                    "YouTube n'a pas rendu toute la playlist (affichage partiel)"
                                        .into(),
                                ),
                            );
                        }
                        if id == LIKED_ID {
                            self.lib.liked =
                                playlist.tracks.iter().map(|t| t.video_id.clone()).collect();
                            self.lib.liked_count = Some(playlist.tracks.len());
                        }
                        self.lib.playlist = Some((id, Load::Ready(playlist)));
                        Task::batch(tasks)
                    }
                    Err(e) => {
                        self.lib.playlist = Some((id, Load::Failed(e)));
                        Task::none()
                    }
                }
            }
            Lib::ToggleLike(track) => {
                let like = !self.lib.is_liked(&track.video_id);
                self.apply_like(&track, like);
                let account = self.lib.ready();
                let video_id = track.video_id.clone();
                let op = account.map(|a| async move { a.set_liked(&video_id, like).await });
                call(op, move |r| Lib::LikeDone(track.clone(), like, r))
            }
            Lib::LikeDone(track, like, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => {
                    self.apply_like(&track, !like);
                    self.show_toast(format!("Like refusé : {e}"))
                }
            },
            Lib::OpenAddTo(track) => {
                self.lib.new_name.clear();
                self.lib.overlay = Some(Overlay::AddTo(track));
                let load = if self.lib.playlists.is_none() {
                    self.load_playlists()
                } else {
                    Task::none()
                };
                Task::batch([load, iced::widget::operation::focus(NEW_PLAYLIST_INPUT)])
            }
            Lib::OpenNew => {
                self.lib.new_name.clear();
                self.lib.overlay = Some(Overlay::NewPlaylist);
                iced::widget::operation::focus(NEW_PLAYLIST_INPUT)
            }
            Lib::CloseOverlay => {
                self.lib.overlay = None;
                Task::none()
            }
            Lib::AddTo(playlist_id, track) => {
                self.lib.overlay = None;
                let title = self.lib.playlist_title(&playlist_id);
                let toast = self.show_toast(format!("« {} » ajouté à « {title} »", track.title));
                let video_id = track.video_id.clone();
                let target = playlist_id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.add_tracks(&target, &[video_id]).await });
                Task::batch([toast, call(op, move |r| Lib::Added(playlist_id.clone(), r))])
            }
            Lib::Added(playlist_id, result) => match result {
                Ok(()) => self.refresh_if_open(&playlist_id),
                Err(e) => self.show_toast(format!("Ajout refusé : {e}")),
            },
            Lib::NewNameInput(name) => {
                self.lib.new_name = name;
                Task::none()
            }
            Lib::Create => {
                let name = self.lib.new_name.trim().to_owned();
                if name.is_empty() {
                    return Task::none();
                }
                let with: Vec<String> = match &self.lib.overlay {
                    Some(Overlay::AddTo(track)) => vec![track.video_id.clone()],
                    _ => Vec::new(),
                };
                self.lib.overlay = None;
                self.lib.new_name.clear();
                let toast = self.show_toast(format!("Playlist « {name} » créée"));
                let op = self.lib.ready().map(|a| async move {
                    a.create_playlist(&name, Privacy::Private, &with).await
                });
                Task::batch([toast, call(op, Lib::Created)])
            }
            Lib::Created(result) => match result {
                Ok(_) => self.load_playlists(),
                Err(e) => self.show_toast(format!("Création refusée : {e}")),
            },
            Lib::RenameStart => {
                if let Some((_, Load::Ready(p))) = &self.lib.playlist {
                    self.lib.renaming = Some(p.title.clone());
                }
                iced::widget::operation::focus(RENAME_INPUT)
            }
            Lib::RenameInput(name) => {
                self.lib.renaming = Some(name);
                Task::none()
            }
            Lib::RenameCommit => {
                let Some(name) = self.lib.renaming.take().map(|n| n.trim().to_owned()) else {
                    return Task::none();
                };
                let Some((id, Load::Ready(playlist))) = &mut self.lib.playlist else {
                    return Task::none();
                };
                if name.is_empty() || name == playlist.title {
                    return Task::none();
                }
                playlist.title = name.clone();
                let id = id.clone();
                if let Some(Load::Ready(list)) = &mut self.lib.playlists
                    && let Some(p) = list.iter_mut().find(|p| p.id == id)
                {
                    p.title = name.clone();
                }
                let target = id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.rename_playlist(&target, &name).await });
                call(op, move |r| Lib::Renamed(id.clone(), r))
            }
            Lib::Renamed(id, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => Task::batch([
                    self.show_toast(format!("Renommage refusé : {e}")),
                    self.open_playlist(id),
                    self.load_playlists(),
                ]),
            },
            Lib::AskDelete => {
                if let Some((id, Load::Ready(p))) = &self.lib.playlist {
                    self.lib.overlay = Some(Overlay::ConfirmDelete {
                        id: id.clone(),
                        title: p.title.clone(),
                    });
                }
                Task::none()
            }
            Lib::Delete => {
                let Some(Overlay::ConfirmDelete { id, title }) = self.lib.overlay.take() else {
                    return Task::none();
                };
                if let Some(Load::Ready(list)) = &mut self.lib.playlists {
                    list.retain(|p| p.id != id);
                }
                let toast = self.show_toast(format!("Playlist « {title} » supprimée"));
                let nav = self.go_to(Route::Library);
                let target = id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.delete_playlist(&target).await });
                Task::batch([toast, nav, call(op, Lib::Deleted)])
            }
            Lib::Deleted(result) => match result {
                Ok(()) => Task::none(),
                Err(e) => Task::batch([
                    self.show_toast(format!("Suppression refusée : {e}")),
                    self.load_playlists(),
                ]),
            },
            Lib::RemoveTrack(playlist_id, video_id) => {
                if let Some((id, Load::Ready(p))) = &mut self.lib.playlist
                    && *id == playlist_id
                {
                    p.tracks.retain(|t| t.video_id != video_id);
                }
                let target = playlist_id.clone();
                let op = self
                    .lib
                    .ready()
                    .map(|a| async move { a.remove_track(&target, &video_id).await });
                call(op, move |r| Lib::Removed(playlist_id.clone(), r))
            }
            Lib::Removed(playlist_id, result) => match result {
                Ok(()) => Task::none(),
                Err(e) => Task::batch([
                    self.show_toast(format!("Retrait refusé : {e}")),
                    self.refresh_if_open(&playlist_id),
                ]),
            },
        }
    }

    /// Met à jour l'affichage d'un like sans attendre YouTube.
    fn apply_like(&mut self, track: &Track, like: bool) {
        if like {
            self.lib.liked.insert(track.video_id.clone());
        } else {
            self.lib.liked.remove(&track.video_id);
        }
        if let Some(count) = &mut self.lib.liked_count {
            *count = if like {
                *count + 1
            } else {
                count.saturating_sub(1)
            };
        }
        if let Some((id, Load::Ready(p))) = &mut self.lib.playlist
            && id == LIKED_ID
        {
            p.tracks.retain(|t| t.video_id != track.video_id);
            if like {
                p.tracks.insert(0, track.clone());
            }
        }
    }

    fn refresh_if_open(&mut self, playlist_id: &str) -> Task<Message> {
        match &self.lib.playlist {
            Some((id, _)) if id == playlist_id => self.open_playlist(playlist_id.to_owned()),
            _ => Task::none(),
        }
    }
}

// ----- vues -----

fn liked_tile<'a>(app: &App) -> El<'a> {
    let count = app
        .lib
        .liked_count
        .map(|n| format!("{n} titres"))
        .unwrap_or_else(|| "…".into());
    let accent = app.accent;
    button(
        column![
            container(icon(Icon::HeartFilled, 54.0, Color::WHITE))
                .center(Length::Fixed(164.0))
                .style(move |_| iced::widget::container::Style {
                    background: Some(iced::Background::Gradient(iced::Gradient::Linear(
                        iced::gradient::Linear::new(std::f32::consts::FRAC_PI_4)
                            .add_stop(0.0, style::shade(accent, 0.45))
                            .add_stop(1.0, accent),
                    ))),
                    border: iced::Border {
                        radius: 8.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            text("Titres likés")
                .size(14)
                .font(SEMIBOLD)
                .width(Length::Fixed(164.0)),
            text(count)
                .size(12)
                .color(style::TEXT_DIM)
                .width(Length::Fixed(164.0)),
        ]
        .spacing(6),
    )
    .padding(10)
    .style(style::card)
    .on_press(Message::Navigate(Route::Playlist(LIKED_ID.to_owned())))
    .into()
}

fn playlist_card<'a>(app: &App, p: &'a PlaylistSummary) -> El<'a> {
    let id = app.url_cover(p.thumbnail_url.as_deref());
    let subtitle = [p.tracks_text.as_str(), p.author.as_str()]
        .into_iter()
        .filter(|s| !s.trim().is_empty() && s.trim() != "•")
        .collect::<Vec<_>>()
        .join(" · ");
    button(
        column![
            cover(app.cover(id.as_deref(), CARD), 164.0, 8.0),
            text(&p.title)
                .size(14)
                .font(SEMIBOLD)
                .width(Length::Fixed(164.0)),
            text(subtitle)
                .size(12)
                .color(style::TEXT_DIM)
                .width(Length::Fixed(164.0)),
        ]
        .spacing(6),
    )
    .padding(10)
    .style(style::card)
    .on_press(Message::Navigate(Route::Playlist(p.id.clone())))
    .into()
}

fn not_connected<'a>(app: &App) -> Option<El<'a>> {
    let message = match &app.lib.account {
        AccountView::Ready(_) => return None,
        AccountView::Connecting => "Connexion à ton compte YouTube…".to_owned(),
        AccountView::Unavailable(e) => format!(
            "Compte YouTube indisponible : {e}\n\nIl faut `cookies_from_browser = \"firefox\"` dans la section [player] de config.toml, et être connecté à YouTube dans Firefox."
        ),
    };
    Some(
        container(muted(message))
            .padding(Padding::from([40, 0]))
            .into(),
    )
}

pub fn library_page(app: &App) -> El<'_> {
    let header = row![
        text("Bibliothèque").size(30).font(BOLD).width(Length::Fill),
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
        .on_press_maybe(app.lib.ready().map(|_| Message::Lib(Lib::OpenNew))),
    ]
    .align_y(Alignment::Center);

    let body: El<'_> = if let Some(info) = not_connected(app) {
        info
    } else {
        match &app.lib.playlists {
            None | Some(Load::Loading) => loading("Chargement de tes playlists…"),
            Some(Load::Failed(e)) => error_box(e),
            Some(Load::Ready(list)) => {
                let mut tiles = vec![liked_tile(app)];
                tiles.extend(list.iter().map(|p| playlist_card(app, p)));
                cards(tiles)
            }
        }
    };
    page_scroll(column![header, body].spacing(20).padding(Padding {
        top: 28.0,
        ..Padding::ZERO
    }))
}

pub fn playlist_page(app: &App) -> El<'_> {
    let Some((id, load)) = &app.lib.playlist else {
        return loading("Chargement…");
    };
    if let Some(info) = not_connected(app) {
        return page_scroll(info);
    }
    let body: El<'_> = match load {
        Load::Loading => crate::views::with_back(loading("Chargement de la playlist…")),
        Load::Failed(e) => crate::views::with_back(error_box(e)),
        Load::Ready(playlist) => playlist_body(app, id, playlist),
    };
    page_scroll(body)
}

fn playlist_body<'a>(app: &'a App, id: &'a str, playlist: &'a Playlist) -> El<'a> {
    let liked = id == LIKED_ID;
    let count = match playlist.tracks.len() {
        1 => "1 titre".to_owned(),
        n => format!("{n} titres"),
    };
    let details = [
        playlist.author.as_str(),
        count.as_str(),
        playlist.duration_text.as_str(),
    ]
    .into_iter()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" · ");

    let title_or_edit: El<'a> = match &app.lib.renaming {
        Some(name) if !liked => text_input("Nom de la playlist", name)
            .id(RENAME_INPUT)
            .on_input(|n| Message::Lib(Lib::RenameInput(n)))
            .on_submit(Message::Lib(Lib::RenameCommit))
            .size(24)
            .padding(Padding::from([6, 12]))
            .style(style::search_input)
            .into(),
        _ => Space::new().height(Length::Fixed(0.0)).into(),
    };
    let mut actions = row![].spacing(4).align_y(Alignment::Center);
    if !liked {
        actions = actions
            .push(icon_tip(
                Icon::Edit,
                17.0,
                style::TEXT_DIM,
                match app.lib.renaming {
                    Some(_) => Message::Lib(Lib::RenameCommit),
                    None => Message::Lib(Lib::RenameStart),
                },
                if app.lib.renaming.is_some() {
                    "Valider le nom"
                } else {
                    "Renommer"
                }
                .into(),
            ))
            .push(icon_tip(
                Icon::Trash,
                17.0,
                style::TEXT_DIM,
                Message::Lib(Lib::AskDelete),
                "Supprimer la playlist".into(),
            ));
    }
    let subtitle = column![muted(details), title_or_edit, actions].spacing(6);

    let cover_id = if liked {
        playlist.tracks.first().and_then(|t| app.track_cover(t))
    } else {
        app.url_cover(playlist.thumbnail_url.as_deref())
    };
    let header = page_header(
        app,
        cover_id,
        10.0,
        if liked {
            "TITRES LIKÉS".into()
        } else {
            "PLAYLIST".into()
        },
        &playlist.title,
        subtitle.into(),
        &playlist.tracks,
    );

    let mut list = column![].spacing(2);
    if playlist.tracks.is_empty() {
        list = list.push(muted("Cette playlist est vide."));
    }
    for (i, track) in playlist.tracks.iter().enumerate() {
        let remove = if liked {
            Message::Lib(Lib::ToggleLike(track.clone()))
        } else {
            Message::Lib(Lib::RemoveTrack(id.to_owned(), track.video_id.clone()))
        };
        list = list.push(track_row(
            app,
            track,
            None,
            Message::Play(playlist.tracks.clone(), i),
            Some(remove),
        ));
    }
    column![
        header,
        container(list).padding(Padding {
            top: 8.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0,
        }),
    ]
    .into()
}

/// Fenêtre par-dessus la page (ajout à une playlist, confirmation).
pub fn overlay(app: &App) -> Option<El<'_>> {
    let panel: El<'_> = match app.lib.overlay.as_ref()? {
        Overlay::AddTo(track) => add_to_panel(app, Some(track)),
        Overlay::NewPlaylist => add_to_panel(app, None),
        Overlay::ConfirmDelete { title, .. } => confirm_panel(title),
    };
    let dim = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| iced::widget::container::Style {
                background: Some(style::alpha(Color::BLACK, 0.55).into()),
                ..Default::default()
            }),
    )
    .on_press(Message::Lib(Lib::CloseOverlay));
    let centered = container(
        // Le panneau capture les clics : ils ne ferment pas la fenêtre.
        mouse_area(
            container(panel)
                .padding(20)
                .width(Length::Fixed(400.0))
                .style(style::panel),
        )
        .on_press(Message::Noop),
    )
    .center(Length::Fill);
    Some(iced::widget::stack![dim, centered].into())
}

fn add_to_panel<'a>(app: &'a App, track: Option<&'a Track>) -> El<'a> {
    let title = match track {
        None => "Nouvelle playlist".to_owned(),
        Some(track) => format!("Ajouter « {} » à…", track.title),
    };
    let new_row = row![
        text_input("Nom de la nouvelle playlist", &app.lib.new_name)
            .id(NEW_PLAYLIST_INPUT)
            .on_input(|n| Message::Lib(Lib::NewNameInput(n)))
            .on_submit(Message::Lib(Lib::Create))
            .size(14)
            .padding(Padding::from([8, 12]))
            .style(style::search_input),
        button(text("Créer").size(14).font(SEMIBOLD))
            .padding(Padding::from([8, 16]))
            .style(style::pill(app.accent))
            .on_press_maybe(
                (!app.lib.new_name.trim().is_empty()).then_some(Message::Lib(Lib::Create)),
            ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut content = column![text(title).size(17).font(BOLD), new_row].spacing(14);
    if let Some(track) = track {
        let mut list = column![].spacing(2);
        match &app.lib.playlists {
            Some(Load::Ready(playlists)) => {
                for p in playlists {
                    let id = app.url_cover(p.thumbnail_url.as_deref());
                    list = list.push(
                        button(
                            row![
                                cover(app.cover(id.as_deref(), CARD), 36.0, 5.0),
                                column![
                                    text(&p.title).size(14).font(SEMIBOLD),
                                    text(&p.tracks_text).size(12).color(style::TEXT_DIM),
                                ]
                                .spacing(1),
                            ]
                            .spacing(10)
                            .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .padding(Padding::from([5, 8]))
                        .style(style::row(false, app.accent))
                        .on_press(Message::Lib(Lib::AddTo(p.id.clone(), track.clone()))),
                    );
                }
            }
            Some(Load::Failed(e)) => list = list.push(muted(e.clone())),
            _ => list = list.push(muted("Chargement…")),
        }
        content = content.push(section_title("Mes playlists")).push(
            scrollable(list)
                .height(Length::Fixed(300.0))
                .style(style::scroll),
        );
    }
    content.into()
}

fn confirm_panel(title: &str) -> El<'_> {
    column![
        text("Supprimer la playlist ?").size(17).font(BOLD),
        muted(format!(
            "« {title} » sera supprimée de ton compte YouTube. C'est définitif."
        )),
        row![
            Space::new().width(Length::Fill),
            button(text("Annuler").size(14))
                .padding(Padding::from([8, 16]))
                .style(style::pill_ghost)
                .on_press(Message::Lib(Lib::CloseOverlay)),
            button(text("Supprimer").size(14).font(SEMIBOLD))
                .padding(Padding::from([8, 16]))
                .style(style::pill(Color::from_rgb8(0xff, 0x6b, 0x6b)))
                .on_press(Message::Lib(Lib::Delete)),
        ]
        .spacing(10),
    ]
    .spacing(14)
    .into()
}
