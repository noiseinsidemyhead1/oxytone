//! Dessin de l'interface : barre latérale, pages, file d'attente, barre de lecture.
//!
//! En clair : des fonctions qui transforment l'état de [`App`] en widgets.
//! Elles ne modifient rien ; un clic produit un [`Message`] traité par `update`.

use std::time::Duration;

use iced::widget::{
    Space, button, column, container, image, mouse_area, row, scrollable, slider, stack, text,
    text_input, tooltip,
};
use iced::{Alignment, Color, Element, Font, Length, Padding, font};
use ytmr_catalog::{Album, AlbumSummary, Artist, ArtistSummary, SearchAll};
use ytmr_core::{QueuedTrack, Repeat, Status, Track};
use ytmr_player::StreamQuality;

use crate::app::{
    App, CARD, HERO, Load, Message, PAGE_SCROLL, Phase, QUEUE_SCROLL, Route, SEARCH_INPUT, THUMB,
};
use crate::icons::{Icon, icon};
use crate::library::{self, Lib};
use crate::smooth::wheel;
use crate::style;

pub(crate) type El<'a> = Element<'a, Message>;

pub(crate) const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};
pub(crate) const SEMIBOLD: Font = Font {
    weight: font::Weight::Semibold,
    ..Font::DEFAULT
};

pub fn view(app: &App) -> El<'_> {
    let content: El<'_> = match &app.phase {
        Phase::Starting => splash("Démarrage…"),
        Phase::Failed(e) => failure(e),
        Phase::Ready(_) => match library::overlay(app) {
            Some(overlay) => stack![main_layout(app), overlay].into(),
            None => main_layout(app),
        },
    };
    let with_toast: El<'_> = match &app.toast {
        Some((_, message)) => stack![content, toast(message)].into(),
        None => content,
    };
    container(with_toast)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::window_bg)
        .into()
}

fn splash(message: &str) -> El<'_> {
    container(
        column![
            icon(Icon::Music, 44.0, style::DEFAULT_ACCENT),
            text("ytmr").size(30).font(BOLD),
            text(message).size(14).color(style::TEXT_DIM),
        ]
        .spacing(10)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

fn failure(message: &str) -> El<'_> {
    container(
        column![
            text("ytmr n'a pas pu démarrer").size(24).font(BOLD),
            container(
                text(message)
                    .size(14)
                    .color(Color::from_rgb8(0xff, 0x9b, 0x9b))
            )
            .padding(16)
            .style(style::panel)
            .max_width(620),
            text(
                "Corrige le problème puis relance ytmr. Le détail est aussi dans le fichier de log."
            )
            .size(14)
            .color(style::TEXT_DIM),
        ]
        .spacing(16)
        .max_width(620),
    )
    .center(Length::Fill)
    .into()
}

fn toast(message: &str) -> El<'_> {
    container(
        container(text(message).size(14))
            .padding(Padding::from([10, 16]))
            .style(style::toast),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::End)
    .padding(Padding {
        bottom: 104.0,
        ..Padding::ZERO
    })
    .into()
}

fn main_layout(app: &App) -> El<'_> {
    let page: El<'_> = match &app.route {
        Route::Home => home(app),
        Route::Search => search_page(app),
        Route::Album(_) => album_page(app),
        Route::Artist(_) => artist_page(app),
        Route::Library => library::library_page(app),
        Route::Playlist(_) => library::playlist_page(app),
    };
    let mut middle = row![
        sidebar(app),
        container(page).width(Length::Fill).height(Length::Fill)
    ];
    if app.show_queue {
        middle = middle.push(queue_panel(app));
    }
    column![middle.height(Length::Fill), player_bar(app)].into()
}

// ----- barre latérale -----

fn sidebar(app: &App) -> El<'_> {
    let nav = |label: &'static str, ic: Icon, route: Route| {
        let active = app.route == route
            || (route == Route::Search && matches!(app.route, Route::Album(_) | Route::Artist(_)))
            || (route == Route::Library && matches!(app.route, Route::Playlist(_)));
        let color = if active { app.accent } else { style::TEXT_DIM };
        button(
            row![icon(ic, 18.0, color), text(label).size(14).font(SEMIBOLD)]
                .spacing(12)
                .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([9, 12]))
        .style(style::nav(active, app.accent))
        .on_press(Message::Navigate(route))
    };

    let logo = row![
        icon(Icon::Music, 22.0, app.accent),
        text("ytmr").size(22).font(BOLD),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let search = text_input("Rechercher", &app.search_query)
        .id(SEARCH_INPUT)
        .on_input(Message::SearchInput)
        .on_submit(Message::SearchSubmit)
        .padding(Padding::from([9, 14]))
        .size(14)
        .style(style::search_input);

    container(
        column![
            container(logo).padding(Padding::from([6, 8])),
            search,
            column![
                nav("Accueil", Icon::Home, Route::Home),
                nav("Recherche", Icon::Search, Route::Search),
                nav("Bibliothèque", Icon::Library, Route::Library),
            ]
            .spacing(4),
            Space::new().height(Length::Fill),
            library::account_widget(app),
            text("Espace : lecture/pause · ← → : ±10 s · Ctrl+F : rechercher")
                .size(11)
                .color(style::TEXT_FAINT),
        ]
        .spacing(18)
        .padding(16),
    )
    .width(Length::Fixed(232.0))
    .height(Length::Fill)
    .style(style::sidebar)
    .into()
}

// ----- éléments communs -----

/// Jaquette carrée, ou un carré neutre tant qu'elle charge.
pub(crate) fn cover<'a>(handle: Option<image::Handle>, size: f32, radius: f32) -> El<'a> {
    match handle {
        Some(h) => image(h)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .border_radius(radius)
            .into(),
        None => container(icon(Icon::Music, size * 0.36, style::TEXT_FAINT))
            .center(Length::Fixed(size))
            .style(move |t| {
                let mut s = style::cover_frame(t);
                s.border.radius = radius.into();
                s.shadow = iced::Shadow::default();
                s
            })
            .into(),
    }
}

pub(crate) fn track_cover<'a>(app: &App, track: &Track, size: f32) -> El<'a> {
    let id = app.track_cover(track);
    cover(app.cover(id.as_deref(), THUMB), size, 6.0)
}

pub(crate) fn icon_btn<'a>(ic: Icon, size: f32, color: Color, message: Message) -> El<'a> {
    button(icon(ic, size, color))
        .padding(7)
        .style(style::icon_button)
        .on_press(message)
        .into()
}

/// Cœur et « ajouter à une playlist », quand le compte est connecté.
pub(crate) fn account_buttons<'a>(app: &App, track: &'a Track) -> El<'a> {
    if app.lib.ready().is_none() {
        return Space::new().width(Length::Fixed(0.0)).into();
    }
    let liked = app.lib.is_liked(&track.video_id);
    row![
        icon_tip(
            if liked {
                Icon::HeartFilled
            } else {
                Icon::Heart
            },
            16.0,
            if liked { app.accent } else { style::TEXT_DIM },
            Message::Lib(Lib::ToggleLike(track.clone())),
            if liked {
                "Retirer des titres likés"
            } else {
                "Liker"
            }
            .into(),
        ),
        icon_tip(
            Icon::PlaylistAdd,
            16.0,
            style::TEXT_DIM,
            Message::Lib(Lib::OpenAddTo(track.clone())),
            "Ajouter à une playlist".into(),
        ),
    ]
    .into()
}

/// Bouton icône avec une bulle d'aide au survol.
pub(crate) fn icon_tip<'a>(
    ic: Icon,
    size: f32,
    color: Color,
    message: Message,
    tip: String,
) -> El<'a> {
    tooltip(
        icon_btn(ic, size, color, message),
        container(text(tip).size(12))
            .padding(Padding::from([6, 10]))
            .style(style::toast),
        tooltip::Position::Top,
    )
    .gap(6)
    .delay(std::time::Duration::from_millis(400))
    .into()
}

pub(crate) fn section_title(title: &str) -> El<'_> {
    text(title).size(20).font(BOLD).into()
}

pub(crate) fn muted(content: impl text::IntoFragment<'static>) -> El<'static> {
    text(content).size(13).color(style::TEXT_DIM).into()
}

fn scroll_page<'a>(content: impl Into<El<'a>>) -> El<'a> {
    page_scroll(container(content).padding(Padding {
        top: 0.0,
        right: 28.0,
        bottom: 32.0,
        left: 28.0,
    }))
}

/// Liste défilante de la page centrale, avec défilement animé à la molette.
pub(crate) fn page_scroll<'a>(content: impl Into<El<'a>>) -> El<'a> {
    smooth_scroll(content, PAGE_SCROLL)
}

fn smooth_scroll<'a>(content: impl Into<El<'a>>, id: &'static str) -> El<'a> {
    wheel(
        scrollable(content)
            .id(id)
            .on_scroll(move |viewport| Message::Scrolled(id, viewport))
            .height(Length::Fill)
            .style(style::scroll),
        move |lines| Message::Wheel(id, lines),
    )
    .into()
}

fn back_bar<'a>() -> El<'a> {
    row![icon_btn(Icon::Back, 18.0, style::TEXT, Message::Back)]
        .padding(Padding {
            top: 16.0,
            bottom: 8.0,
            ..Padding::ZERO
        })
        .into()
}

pub(crate) fn with_back(content: El<'_>) -> El<'_> {
    column![
        container(back_bar()).padding(Padding::from([0, 20])),
        container(content).padding(Padding::from([0, 28])),
    ]
    .into()
}

/// Boutons d'une ligne de morceau, visibles au survol de la ligne (un titre
/// liké garde son cœur). La place est toujours réservée : rien ne bouge.
fn row_actions<'a>(app: &App, track: &'a Track, remove: Option<Message>) -> El<'a> {
    let width = Length::Fixed(if remove.is_some() { 210.0 } else { 170.0 });
    let hovered = app.hovered.as_deref() == Some(track.video_id.as_str());
    if !hovered {
        let liked: El<'a> = if app.lib.is_liked(&track.video_id) {
            container(icon(Icon::HeartFilled, 16.0, app.accent))
                .padding(7)
                .into()
        } else {
            Space::new().into()
        };
        return container(liked).width(width).into();
    }
    let mut buttons = row![
        account_buttons(app, track),
        icon_tip(
            Icon::PlayNext,
            16.0,
            style::TEXT_DIM,
            Message::PlayNext(track.clone()),
            "Lire ensuite".into()
        ),
        icon_tip(
            Icon::Plus,
            16.0,
            style::TEXT_DIM,
            Message::Enqueue(track.clone()),
            "Ajouter à la file".into()
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center);
    if let Some(message) = remove {
        buttons = buttons.push(icon_tip(
            Icon::Close,
            14.0,
            style::TEXT_FAINT,
            message,
            "Retirer de la playlist".into(),
        ));
    }
    container(buttons).width(width).into()
}

/// Ligne de morceau : clic = lecture ; boutons like, playlist, « lire ensuite »,
/// « ajouter à la file » et, dans une playlist, « retirer ».
pub(crate) fn track_row<'a>(
    app: &App,
    track: &'a Track,
    number: Option<usize>,
    on_play: Message,
    remove: Option<Message>,
) -> El<'a> {
    let playing = app
        .state
        .current
        .as_ref()
        .is_some_and(|e| e.track.video_id == track.video_id);
    let title_color = if playing { app.accent } else { style::TEXT };
    let leading: El<'a> = match number {
        Some(n) => container(text(n.to_string()).size(14).color(style::TEXT_FAINT))
            .width(Length::Fixed(28.0))
            .align_x(Alignment::Center)
            .into(),
        None => track_cover(app, track, 44.0),
    };
    let subtitle = match &track.album {
        Some(album) if number.is_none() => format!("{} · {album}", track.artist),
        _ => track.artist.clone(),
    };
    let line = row![
        leading,
        column![
            text(&track.title)
                .size(14)
                .font(SEMIBOLD)
                .color(title_color),
            text(subtitle).size(13).color(style::TEXT_DIM),
        ]
        .spacing(2)
        .width(Length::Fill),
        text(fmt_opt(track.duration))
            .size(13)
            .color(style::TEXT_FAINT),
        row_actions(app, track, remove),
    ]
    .spacing(14)
    .align_y(Alignment::Center);

    mouse_area(
        button(line)
            .width(Length::Fill)
            .padding(Padding::from([6, 10]))
            .style(style::row(playing, app.accent))
            .on_press(on_play),
    )
    .on_enter(Message::Hover(track.video_id.clone()))
    .on_exit(Message::Unhover)
    .into()
}

fn album_card<'a>(app: &App, album: &'a AlbumSummary) -> El<'a> {
    let id = app.url_cover(album.thumbnail_url.as_deref());
    let subtitle = [
        album.kind.as_str(),
        album.year.as_str(),
        album.artist.as_str(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    button(
        column![
            cover(app.cover(id.as_deref(), CARD), 164.0, 8.0),
            text(&album.title)
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
    .on_press(Message::Navigate(Route::Album(album.id.clone())))
    .into()
}

fn artist_card<'a>(app: &App, artist: &'a ArtistSummary) -> El<'a> {
    let id = app.url_cover(artist.thumbnail_url.as_deref());
    button(
        column![
            cover(app.cover(id.as_deref(), CARD), 140.0, 70.0),
            text(&artist.name).size(14).font(SEMIBOLD),
            text(artist.subscribers.clone().unwrap_or_default())
                .size(12)
                .color(style::TEXT_DIM),
        ]
        .spacing(6)
        .align_x(Alignment::Center)
        .width(Length::Fixed(150.0)),
    )
    .padding(10)
    .style(style::card)
    .on_press(Message::Navigate(Route::Artist(artist.id.clone())))
    .into()
}

pub(crate) fn cards<'a>(items: Vec<El<'a>>) -> El<'a> {
    row(items).spacing(6).wrap().into()
}

pub(crate) fn loading<'a>(label: &'static str) -> El<'a> {
    container(muted(label))
        .padding(Padding::from([40, 0]))
        .into()
}

pub(crate) fn error_box(message: &str) -> El<'_> {
    container(
        text(format!("Impossible de charger : {message}"))
            .size(14)
            .color(style::TEXT_DIM),
    )
    .padding(16)
    .style(style::panel)
    .into()
}

// ----- pages -----

fn home(app: &App) -> El<'_> {
    let sections = library::home_sections(app);
    let Some(entry) = &app.state.current else {
        // Rien en lecture : salutation, puis ce que le compte propose.
        let greeting = column![
            text(greeting(app)).size(34).font(BOLD),
            muted("Reprends là où tu t'étais arrêté, ou cherche quelque chose."),
        ]
        .spacing(8);
        return match sections {
            Some(sections) => {
                page_scroll(column![greeting, sections].spacing(28).padding(Padding {
                    top: 40.0,
                    right: 28.0,
                    bottom: 32.0,
                    left: 28.0,
                }))
            }
            None => container(
                column![
                    text("Que veux-tu écouter ?").size(34).font(BOLD),
                    muted("Tape un titre, un artiste ou un album dans la recherche à gauche."),
                    button(
                        row![
                            icon(Icon::Search, 16.0, style::BG),
                            text("Rechercher").size(14).font(SEMIBOLD)
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([10, 20]))
                    .style(style::pill(app.accent))
                    .on_press(Message::Navigate(Route::Search)),
                ]
                .spacing(16)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .into(),
        };
    };
    let track = &entry.track;
    let id = app.track_cover(track);
    let big =
        container(cover(app.cover(id.as_deref(), HERO), 300.0, 10.0)).style(style::cover_frame);

    let mut info = column![
        text("EN COURS").size(12).font(BOLD).color(app.accent),
        text(&track.title).size(38).font(BOLD),
        text(&track.artist).size(18).color(style::TEXT_DIM),
    ]
    .spacing(8);
    if let (Some(album), Some(album_id)) = (&track.album, &track.album_id) {
        info = info.push(
            button(text(album).size(15))
                .padding(0)
                .style(style::link)
                .on_press(Message::Navigate(Route::Album(album_id.clone()))),
        );
    }
    info = info.push(quality_badge(app));

    let hero = container(
        row![big, info.width(Length::Fill)]
            .spacing(36)
            .align_y(Alignment::End)
            .padding(Padding {
                top: 48.0,
                right: 28.0,
                bottom: 28.0,
                left: 28.0,
            }),
    )
    .width(Length::Fill)
    .style(style::hero(app.accent));

    let upcoming: Vec<&QueuedTrack> = app
        .queue
        .current
        .map(|c| app.queue.entries.iter().skip(c + 1).take(10).collect())
        .unwrap_or_default();
    let mut list = column![section_title("À suivre")].spacing(6);
    if upcoming.is_empty() {
        list = list.push(muted(if app.state.autoplay {
            "La radio va compléter la file automatiquement."
        } else {
            "Rien après ce morceau."
        }));
    }
    for e in upcoming {
        list = list.push(track_row(app, &e.track, None, Message::JumpTo(e.id), None));
    }

    let mut below = column![list].spacing(28);
    if let Some(sections) = sections {
        below = below.push(sections);
    }
    page_scroll(column![
        hero,
        container(below).padding(Padding {
            top: 8.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0,
        })
    ])
}

/// « Bonjour, <prénom> » selon l'heure locale.
fn greeting(app: &App) -> String {
    let hour = local_hour();
    let hello = if (5..18).contains(&hour) {
        "Bonjour"
    } else {
        "Bonsoir"
    };
    match app.lib.profile.as_ref() {
        Some(p) => format!("{hello}, {}", p.name),
        None => hello.to_owned(),
    }
}

/// Heure locale approximative, sans dépendance : fuseau de Paris (UTC+1, +2 l'été).
fn local_hour() -> u64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day_of_year = (secs / 86_400) % 365;
    // Heure d'été européenne : fin mars → fin octobre (approximation au jour près).
    let offset = if (85..300).contains(&day_of_year) {
        2
    } else {
        1
    };
    (secs / 3600 + offset) % 24
}

fn quality_badge(app: &App) -> El<'_> {
    let (label, color) = match (app.state.status, app.state.quality) {
        (Status::Loading, _) => ("Chargement…", style::TEXT_FAINT),
        (_, Some(StreamQuality::High)) => ("Haute qualité", app.accent),
        (_, Some(StreamQuality::Low)) => ("Qualité réduite", style::TEXT_DIM),
        _ => ("", style::TEXT_FAINT),
    };
    container(text(label).size(12).font(SEMIBOLD).color(color))
        .padding(Padding::from([3, 10]))
        .style(move |_| iced::widget::container::Style {
            border: iced::Border {
                color: style::alpha(color, 0.5),
                width: if label.is_empty() { 0.0 } else { 1.0 },
                radius: 999.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn search_page(app: &App) -> El<'_> {
    let body: El<'_> = match &app.search {
        None => container(muted("Tape quelque chose dans la recherche."))
            .padding(Padding::from([40, 0]))
            .into(),
        Some(Load::Loading) => loading("Recherche…"),
        Some(Load::Failed(e)) => error_box(e),
        Some(Load::Ready(results)) => search_results(app, results),
    };
    scroll_page(column![
        container(
            text(format!("Résultats pour « {} »", app.search_query.trim()))
                .size(26)
                .font(BOLD)
        )
        .padding(Padding {
            top: 28.0,
            bottom: 12.0,
            ..Padding::ZERO
        }),
        body
    ])
}

fn search_results<'a>(app: &'a App, r: &'a SearchAll) -> El<'a> {
    let mut page = column![].spacing(28);
    if !r.songs.is_empty() {
        let mut songs = column![section_title("Morceaux")].spacing(4);
        for track in r.songs.iter().take(10) {
            // Un morceau seul : la radio prend le relais, comme sur YouTube Music.
            songs = songs.push(track_row(
                app,
                track,
                None,
                Message::Play(vec![track.clone()], 0),
                None,
            ));
        }
        page = page.push(songs);
    }
    if !r.albums.is_empty() {
        page = page.push(
            column![
                section_title("Albums"),
                cards(
                    r.albums
                        .iter()
                        .take(12)
                        .map(|a| album_card(app, a))
                        .collect()
                )
            ]
            .spacing(10),
        );
    }
    if !r.artists.is_empty() {
        page = page.push(
            column![
                section_title("Artistes"),
                cards(
                    r.artists
                        .iter()
                        .take(8)
                        .map(|a| artist_card(app, a))
                        .collect()
                )
            ]
            .spacing(10),
        );
    }
    if r.songs.is_empty() && r.albums.is_empty() && r.artists.is_empty() {
        page = page.push(muted("Aucun résultat."));
    }
    page.into()
}

pub(crate) fn page_header<'a>(
    app: &App,
    cover_id: Option<String>,
    radius: f32,
    kicker: String,
    title: &'a str,
    subtitle: El<'a>,
    tracks: &[Track],
) -> El<'a> {
    let accent = app.accent_for(cover_id.as_deref());
    let art = container(cover(app.cover(cover_id.as_deref(), HERO), 220.0, radius))
        .style(style::cover_frame);
    let actions = row![
        button(
            row![
                icon(Icon::Play, 16.0, style::BG),
                text("Lecture").size(14).font(SEMIBOLD)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([10, 22]))
        .style(style::pill(accent))
        .on_press_maybe((!tracks.is_empty()).then(|| Message::Play(tracks.to_vec(), 0))),
        button(
            row![
                icon(Icon::Shuffle, 16.0, style::TEXT),
                text("Aléatoire").size(14).font(SEMIBOLD)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([10, 20]))
        .style(style::pill_ghost)
        .on_press_maybe((!tracks.is_empty()).then(|| Message::PlayShuffled(tracks.to_vec()))),
    ]
    .spacing(12);
    let header = container(
        row![
            art,
            column![
                text(kicker).size(12).font(BOLD).color(accent),
                text(title).size(40).font(BOLD),
                subtitle,
                Space::new().height(Length::Fixed(8.0)),
                actions,
            ]
            .spacing(8)
            .width(Length::Fill),
        ]
        .spacing(32)
        .align_y(Alignment::End)
        .padding(Padding {
            top: 8.0,
            right: 28.0,
            bottom: 24.0,
            left: 28.0,
        }),
    );
    // Le dégradé englobe aussi le bouton retour, pour ne pas laisser de bande noire.
    container(column![
        container(back_bar()).padding(Padding::from([0, 20])),
        header
    ])
    .width(Length::Fill)
    .style(style::hero(accent))
    .into()
}

fn album_page(app: &App) -> El<'_> {
    let Some((_, load)) = &app.album else {
        return loading("Chargement…");
    };
    let body: El<'_> = match load {
        Load::Loading => with_back(loading("Chargement de l'album…")),
        Load::Failed(e) => with_back(error_box(e)),
        Load::Ready(album) => album_body(app, album),
    };
    page_scroll(body)
}

fn album_body<'a>(app: &'a App, album: &'a Album) -> El<'a> {
    let artist: El<'a> = match &album.artist_id {
        Some(id) => button(text(&album.artist).size(16).font(SEMIBOLD))
            .padding(0)
            .style(style::link)
            .on_press(Message::Navigate(Route::Artist(id.clone())))
            .into(),
        None => text(&album.artist).size(16).color(style::TEXT_DIM).into(),
    };
    let details = [album.year.as_str(), album.duration_text.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let count = match album.tracks.len() {
        1 => "1 morceau".to_owned(),
        n => format!("{n} morceaux"),
    };
    let subtitle = column![artist, muted(format!("{count} · {details}"))].spacing(4);
    let cover_id = app.url_cover(album.thumbnail_url.as_deref());
    let header = page_header(
        app,
        cover_id,
        10.0,
        album.kind.to_uppercase(),
        &album.title,
        subtitle.into(),
        &album.tracks,
    );
    let mut list = column![].spacing(2);
    for (i, track) in album.tracks.iter().enumerate() {
        list = list.push(track_row(
            app,
            track,
            Some(i + 1),
            Message::Play(album.tracks.clone(), i),
            None,
        ));
    }
    column![
        header,
        container(list).padding(Padding {
            top: 8.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0
        }),
    ]
    .into()
}

fn artist_page(app: &App) -> El<'_> {
    let Some((_, load)) = &app.artist else {
        return loading("Chargement…");
    };
    let body: El<'_> = match load {
        Load::Loading => with_back(loading("Chargement de l'artiste…")),
        Load::Failed(e) => with_back(error_box(e)),
        Load::Ready(artist) => artist_body(app, artist),
    };
    page_scroll(body)
}

fn artist_body<'a>(app: &'a App, artist: &'a Artist) -> El<'a> {
    let subtitle = muted(artist.subscribers.clone().unwrap_or_default());
    let cover_id = app.url_cover(artist.thumbnail_url.as_deref());
    let header = page_header(
        app,
        cover_id,
        110.0,
        "ARTISTE".to_owned(),
        &artist.name,
        subtitle,
        &artist.songs,
    );
    let mut page = column![].spacing(28);
    if !artist.songs.is_empty() {
        let mut songs = column![section_title("Titres populaires")].spacing(4);
        for (i, track) in artist.songs.iter().enumerate() {
            songs = songs.push(track_row(
                app,
                track,
                None,
                Message::Play(artist.songs.clone(), i),
                None,
            ));
        }
        page = page.push(songs);
    }
    for (title, list) in [
        ("Albums", &artist.albums),
        ("Singles et EP", &artist.singles),
    ] {
        if !list.is_empty() {
            page = page.push(
                column![
                    section_title(title),
                    cards(list.iter().map(|a| album_card(app, a)).collect())
                ]
                .spacing(10),
            );
        }
    }
    column![
        header,
        container(page).padding(Padding {
            top: 8.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0
        }),
    ]
    .into()
}

// ----- file d'attente -----

fn queue_panel(app: &App) -> El<'_> {
    let header = row![
        text("File d'attente")
            .size(16)
            .font(BOLD)
            .width(Length::Fill),
        button(text("Vider").size(13))
            .padding(Padding::from([4, 10]))
            .style(style::pill_ghost)
            .on_press(Message::ClearQueue),
        icon_tip(
            Icon::Close,
            16.0,
            style::TEXT_DIM,
            Message::ToggleQueue,
            "Masquer la file".into()
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut list = column![].spacing(2);
    if app.queue.entries.is_empty() {
        list = list.push(muted("La file est vide."));
    }
    for (i, entry) in app.queue.entries.iter().enumerate() {
        let current = app.queue.current == Some(i);
        let past = app.queue.current.is_some_and(|c| i < c);
        let title_color = if current {
            app.accent
        } else if past {
            style::TEXT_FAINT
        } else {
            style::TEXT
        };
        let line = row![
            track_cover(app, &entry.track, 36.0),
            column![
                text(&entry.track.title)
                    .size(13)
                    .font(SEMIBOLD)
                    .color(title_color),
                text(&entry.track.artist).size(12).color(style::TEXT_DIM),
            ]
            .spacing(1)
            .width(Length::Fill),
            icon_tip(
                Icon::Close,
                13.0,
                style::TEXT_FAINT,
                Message::Remove(entry.id),
                "Retirer de la file".into()
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        list = list.push(
            mouse_area(
                button(line)
                    .width(Length::Fill)
                    .padding(Padding::from([5, 8]))
                    .style(style::row(current, app.accent))
                    .on_press(Message::JumpTo(entry.id)),
            )
            .on_enter(Message::Hover(entry.track.video_id.clone()))
            .on_exit(Message::Unhover),
        );
    }

    container(
        column![header, smooth_scroll(list, QUEUE_SCROLL),]
            .spacing(12)
            .padding(14),
    )
    .width(Length::Fixed(320.0))
    .height(Length::Fill)
    .style(style::sidebar)
    .into()
}

// ----- barre de lecture -----

fn player_bar(app: &App) -> El<'_> {
    let state = &app.state;
    let accent = app.accent;

    let now: El<'_> = match &state.current {
        Some(entry) => {
            let track = &entry.track;
            let title = button(text(&track.title).size(14).font(SEMIBOLD))
                .padding(0)
                .style(style::link)
                .on_press(Message::Navigate(Route::Home));
            row![
                track_cover(app, track, 56.0),
                column![title, text(&track.artist).size(13).color(style::TEXT_DIM)].spacing(3),
                account_buttons(app, track),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
        }
        None => muted("Rien en lecture"),
    };

    let toggle_icon = if state.status == Status::Playing {
        Icon::Pause
    } else {
        Icon::Play
    };
    let play = button(container(icon(toggle_icon, 18.0, style::BG)).center(Length::Fixed(40.0)))
        .padding(0)
        .style(style::primary_round(accent))
        .on_press(Message::TogglePause);
    let lit = |on: bool| if on { accent } else { style::TEXT_DIM };
    let repeat_icon = if state.repeat == Repeat::One {
        Icon::RepeatOne
    } else {
        Icon::Repeat
    };
    let controls = row![
        icon_tip(
            Icon::Shuffle,
            17.0,
            lit(state.shuffle),
            Message::ToggleShuffle,
            format!("Lecture aléatoire : {}", on_off(state.shuffle))
        ),
        icon_tip(
            Icon::Prev,
            18.0,
            style::TEXT,
            Message::Previous,
            "Précédent (Ctrl+←)".into()
        ),
        play,
        icon_tip(
            Icon::Next,
            18.0,
            style::TEXT,
            Message::Next,
            "Suivant (Ctrl+→)".into()
        ),
        icon_tip(
            repeat_icon,
            17.0,
            lit(state.repeat != Repeat::Off),
            Message::CycleRepeat,
            repeat_label(state.repeat).into()
        ),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let total = state.duration.map_or(0.0, |d| d.as_secs_f32()).max(1.0);
    let shown = app
        .seek_preview
        .unwrap_or(state.position.as_secs_f32())
        .min(total);
    let progress = row![
        text(fmt(Duration::from_secs_f32(shown)))
            .size(12)
            .color(style::TEXT_FAINT)
            .width(Length::Fixed(42.0))
            .align_x(Alignment::End),
        slider(0.0..=total, shown, Message::SeekPreview)
            .on_release(Message::SeekCommit)
            .step(0.5_f32)
            .height(14)
            .style(style::progress(accent)),
        text(fmt_opt(state.duration))
            .size(12)
            .color(style::TEXT_FAINT)
            .width(Length::Fixed(42.0)),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let center = column![controls, progress]
        .spacing(4)
        .align_x(Alignment::Center)
        .width(Length::FillPortion(2))
        .max_width(640);

    let volume_icon = if state.volume == 0 {
        Icon::VolumeMute
    } else {
        Icon::Volume
    };
    let right = row![
        icon_tip(
            Icon::Radio,
            17.0,
            lit(state.autoplay),
            Message::ToggleAutoplay,
            format!(
                "Radio automatique : {}
Ajoute des titres similaires quand la file se termine",
                on_off(state.autoplay)
            )
        ),
        icon_tip(
            Icon::Queue,
            17.0,
            lit(app.show_queue),
            Message::ToggleQueue,
            "File d'attente".into()
        ),
        icon_tip(
            volume_icon,
            17.0,
            style::TEXT_DIM,
            Message::ToggleMute,
            format!("Volume {} % (clic : couper)", state.volume)
        ),
        slider(0..=100u8, state.volume, Message::Volume)
            .width(Length::Fixed(110.0))
            .height(14)
            .style(style::progress(accent)),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    container(
        row![
            container(now).width(Length::FillPortion(1)),
            center,
            container(right)
                .width(Length::FillPortion(1))
                .align_x(Alignment::End),
        ]
        .spacing(16)
        .align_y(Alignment::Center)
        .padding(Padding::from([12, 20])),
    )
    .width(Length::Fill)
    .height(Length::Fixed(88.0))
    .style(style::player_bar)
    .into()
}

// ----- formatage -----

fn on_off(on: bool) -> &'static str {
    if on { "activée" } else { "désactivée" }
}

fn repeat_label(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => "Répéter : non",
        Repeat::All => "Répéter : toute la file",
        Repeat::One => "Répéter : ce morceau",
    }
}

pub fn fmt(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_opt(d: Option<Duration>) -> String {
    d.map(fmt).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(fmt(Duration::from_secs(65)), "1:05");
        assert_eq!(fmt(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(fmt_opt(None), "");
    }
}
