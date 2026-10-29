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
use ytmr_core::{Repeat, Status, Track};
use ytmr_player::StreamQuality;

use crate::app::{
    App, CARD, HERO, Load, Message, PAGE_SCROLL, Panel, Phase, QUEUE_SCROLL, Route, SEARCH_INPUT,
    THUMB, View,
};
use crate::icons::{Icon, icon};
use crate::library::{self, Lib};
use crate::smooth::wheel;
use crate::style;

pub(crate) type El<'a> = Element<'a, Message>;

/// Hauteur fixe d'une ligne de morceau (vignette 44 + marges) : indispensable
/// pour savoir, sans tout mettre en page, quelles lignes sont visibles.
pub(crate) const ROW_H: f32 = 58.0;
/// Hauteur fixe d'une ligne de la file d'attente.
pub(crate) const QROW_H: f32 = 50.0;
/// En-tête d'une page de playlist (hauteur fixe, contenu rogné au besoin).
pub(crate) const PLAYLIST_HEADER_H: f32 = 316.0;
/// Champ de recherche sous l'en-tête d'une playlist.
pub(crate) const PLAYLIST_FILTER_H: f32 = 56.0;
/// Haut de la liste des morceaux dans la page de playlist.
pub(crate) const PLAYLIST_LIST_TOP: f32 = PLAYLIST_HEADER_H + PLAYLIST_FILTER_H + 4.0;
/// Lignes construites en plus, au-dessus et en dessous de la zone visible.
const OVERSCAN: usize = 6;

/// Lignes `[first, last)` visibles dans une liste dont le haut est à `top`.
pub(crate) fn visible_range(view: View, top: f32, row_h: f32, len: usize) -> (usize, usize) {
    let start = ((view.offset - top) / row_h).floor().max(0.0) as usize;
    let count = (view.height / row_h).ceil() as usize + 1;
    let first = start.saturating_sub(OVERSCAN).min(len);
    let last = (start + count + OVERSCAN).min(len).max(first);
    (first, last)
}

/// Liste dont seules les lignes `range` existent ; le reste n'est que de l'espace.
pub(crate) fn virtual_list<'a>(
    len: usize,
    row_h: f32,
    (first, last): (usize, usize),
    row: impl Fn(usize) -> El<'a>,
) -> El<'a> {
    let mut list = column![Space::new().height(Length::Fixed(first as f32 * row_h))];
    for i in first..last {
        list = list.push(row(i));
    }
    list.push(Space::new().height(Length::Fixed((len - last) as f32 * row_h)))
        .into()
}

/// Texte sur une seule ligne, coupé s'il dépasse (lignes de hauteur fixe).
fn line<'a>(content: impl text::IntoFragment<'a>, size: u32) -> iced::widget::Text<'a> {
    text(content)
        .size(size)
        .wrapping(iced::widget::text::Wrapping::None)
}

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
        Route::NowPlaying => now_playing(app),
        Route::Search => search_page(app),
        Route::Album(_) => album_page(app),
        Route::Artist(_) => artist_page(app),
        Route::Library => library::library_page(app),
        Route::Playlist(_) => library::playlist_page(app),
    };
    // La barre de chargement passe par-dessus la page : elle ne la décale pas
    // (sinon une bande noire de 3 px restait au-dessus du dégradé).
    let page = container(stack![page, loading_bar(app)])
        .width(Length::Fill)
        .height(Length::Fill);
    // Grande vue « En cours » : plein écran, aucune barre latérale.
    if app.route == Route::NowPlaying {
        return column![page, player_bar(app)].into();
    }
    let mut middle = row![sidebar(app), resize_handle(Panel::Left), page];
    if app.show_queue {
        middle = middle
            .push(resize_handle(Panel::Right))
            .push(queue_panel(app));
    }
    column![middle.height(Length::Fill), player_bar(app)].into()
}

/// Fine barre animée en haut de la page pendant un chargement.
fn loading_bar(app: &App) -> El<'_> {
    const HEIGHT: f32 = 3.0;
    if !app.is_loading() {
        return Space::new().into();
    }
    // Un segment qui va et vient (de 0 à 1 puis retour, ~1,2 s par aller).
    let t = (app.loading_phase / 1.2) % 2.0;
    let position = if t < 1.0 { t } else { 2.0 - t };
    let accent = app.accent;
    let before = (position * 1000.0) as u16 + 1;
    let after = ((1.0 - position) * 1000.0) as u16 + 1;
    row![
        Space::new().width(Length::FillPortion(before)),
        container(Space::new())
            .width(Length::FillPortion(400))
            .height(Length::Fixed(HEIGHT))
            .style(move |_| iced::widget::container::Style {
                background: Some(accent.into()),
                border: iced::Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        Space::new().width(Length::FillPortion(after)),
    ]
    .height(Length::Fixed(HEIGHT))
    .into()
}

/// Poignée de redimensionnement entre deux colonnes (glisser à la souris).
fn resize_handle<'a>(panel: Panel) -> El<'a> {
    mouse_area(
        container(Space::new())
            .width(Length::Fixed(5.0))
            .height(Length::Fill)
            .style(|_| iced::widget::container::Style {
                background: Some(style::LINE.into()),
                ..Default::default()
            }),
    )
    .interaction(iced::mouse::Interaction::ResizingHorizontally)
    .on_press(Message::DragStart(panel))
    .into()
}

// ----- barre latérale -----

fn sidebar(app: &App) -> El<'_> {
    let nav = |label: &'static str, ic: Icon, route: Route| {
        let active = app.route == route
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
                nav("Bibliothèque", Icon::Library, Route::Library),
            ]
            .spacing(4),
            Space::new().height(Length::Fill),
            library::account_widget(app),
        ]
        .spacing(18)
        .padding(16),
    )
    .width(Length::Fixed(app.left_width))
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
    let title_color = if playing {
        app.page_accent
    } else {
        style::TEXT
    };
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
    let row_content = row![
        leading,
        container(
            column![
                line(&track.title, 14).font(SEMIBOLD).color(title_color),
                line(subtitle, 13).color(style::TEXT_DIM),
            ]
            .spacing(2),
        )
        .width(Length::Fill)
        .clip(true),
        text(fmt_opt(track.duration))
            .size(13)
            .color(style::TEXT_FAINT),
        row_actions(app, track, remove),
    ]
    .spacing(14)
    .align_y(Alignment::Center);

    container(
        mouse_area(
            button(row_content)
                .width(Length::Fill)
                .padding(Padding::from([6, 10]))
                .style(style::row(playing, app.page_accent))
                .on_press(on_play),
        )
        .on_enter(Message::Hover(track.video_id.clone()))
        .on_exit(Message::Unhover),
    )
    .height(Length::Fixed(ROW_H))
    .align_y(Alignment::Center)
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
    let greeting = column![
        text(greeting(app)).size(34).font(BOLD),
        muted("Reprends là où tu t'étais arrêté, ou cherche quelque chose."),
    ]
    .spacing(8);
    match library::home_sections(app) {
        Some(sections) => page_scroll(column![greeting, sections].spacing(28).padding(Padding {
            top: 40.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0,
        })),
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
    }
}

/// Grande vue du morceau en cours (ouverte depuis la barre du bas).
fn now_playing(app: &App) -> El<'_> {
    let Some(entry) = &app.state.current else {
        return container(muted("Rien en lecture."))
            .center(Length::Fill)
            .into();
    };
    let track = &entry.track;
    let id = app.track_cover(track);
    let big =
        container(cover(app.cover(id.as_deref(), HERO), 380.0, 12.0)).style(style::cover_frame);
    let mut info = column![
        text("EN COURS").size(12).font(BOLD).color(app.accent),
        text(&track.title).size(40).font(BOLD),
        text(&track.artist).size(19).color(style::TEXT_DIM),
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
    info = info.push(
        row![
            button(text("Revenir").size(13).font(SEMIBOLD))
                .padding(Padding::from([8, 16]))
                .style(style::pill_ghost)
                .on_press(Message::Back),
            button(
                text(if app.fullscreen {
                    "Quitter le plein écran (F11)"
                } else {
                    "Plein écran (F11)"
                })
                .size(13)
                .font(SEMIBOLD)
            )
            .padding(Padding::from([8, 16]))
            .style(style::pill_ghost)
            .on_press(Message::ToggleFullscreen),
        ]
        .spacing(10),
    );
    container(
        row![big, info.width(Length::Fill)]
            .spacing(40)
            .align_y(Alignment::Center)
            .padding(48),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center_y(Length::Fill)
    .style(style::hero(app.accent))
    .into()
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
    let query = app.search_query.trim();
    let invite = |title: &'static str, hint: &'static str| -> El<'_> {
        container(
            column![
                icon(Icon::Search, 40.0, style::TEXT_FAINT),
                text(title).size(24).font(BOLD),
                muted(hint),
            ]
            .spacing(12)
            .align_x(Alignment::Center),
        )
        .center(Length::Fill)
        .into()
    };
    if query.is_empty() {
        return invite(
            "Que veux-tu écouter ?",
            "Tape un titre, un artiste ou un album dans la barre de recherche.",
        );
    }
    // Les résultats précédents restent affichés pendant la recherche suivante
    // (barre de chargement en haut) : pas de page « Chargement… ».
    let body: El<'_> = match &app.search {
        None | Some(Load::Loading) => return invite("Recherche…", " "),
        Some(Load::Failed(e)) => error_box(e),
        Some(Load::Ready(results)) => search_results(app, results),
    };
    scroll_page(column![
        container(
            text(format!("Résultats pour « {} »", app.search_shown))
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
    // (lecture, aléatoire) ; `None` pour une liste vide.
    play: Option<(Message, Message)>,
) -> El<'a> {
    let (play, shuffle) = play.unzip();
    // Une seule teinte par page : celle de sa pochette, jamais celle du morceau en cours.
    let accent = app.page_accent;
    let art = container(cover(app.cover(cover_id.as_deref(), HERO), 220.0, radius))
        .style(style::cover_frame_round(radius));
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
        .on_press_maybe(play),
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
        .on_press_maybe(shuffle),
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
        (!album.tracks.is_empty()).then_some((Message::PlayAlbum(false), Message::PlayAlbum(true))),
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
        (!artist.songs.is_empty())
            .then_some((Message::PlayArtist(false), Message::PlayArtist(true))),
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

    let entries = &app.queue.entries;
    let range = visible_range(app.queue_view, 0.0, QROW_H, entries.len());
    let list: El<'_> = if entries.is_empty() {
        muted("La file est vide.")
    } else {
        virtual_list(entries.len(), QROW_H, range, |i| {
            let entry = &entries[i];
            let current = app.queue.current == Some(i);
            let past = app.queue.current.is_some_and(|c| i < c);
            let title_color = if current {
                app.accent
            } else if past {
                style::TEXT_FAINT
            } else {
                style::TEXT
            };
            let content = row![
                track_cover(app, &entry.track, 36.0),
                container(
                    column![
                        line(&entry.track.title, 13)
                            .font(SEMIBOLD)
                            .color(title_color),
                        line(&entry.track.artist, 12).color(style::TEXT_DIM),
                    ]
                    .spacing(1),
                )
                .width(Length::Fill)
                .clip(true),
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
            container(
                mouse_area(
                    button(content)
                        .width(Length::Fill)
                        .padding(Padding::from([5, 8]))
                        .style(style::row(current, app.accent))
                        .on_press(Message::JumpTo(entry.id)),
                )
                .on_enter(Message::Hover(entry.track.video_id.clone()))
                .on_exit(Message::Unhover),
            )
            .height(Length::Fixed(QROW_H))
            .align_y(Alignment::Center)
            .into()
        })
    };

    container(
        column![header, smooth_scroll(list, QUEUE_SCROLL),]
            .spacing(12)
            .padding(14),
    )
    .width(Length::Fixed(app.queue_width))
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
            let title = button(line(&track.title, 14).font(SEMIBOLD))
                .padding(0)
                .style(style::link)
                .on_press(Message::Navigate(Route::NowPlaying));
            row![
                button(track_cover(app, track, 56.0))
                    .padding(0)
                    .style(style::link)
                    .on_press(Message::Navigate(Route::NowPlaying)),
                container(
                    column![
                        title,
                        button(line(&track.artist, 13).color(style::TEXT_DIM))
                            .padding(0)
                            .style(style::link)
                            .on_press(Message::OpenArtist(
                                track.artist_id.clone(),
                                main_artist(&track.artist).to_owned()
                            )),
                    ]
                    .spacing(3)
                )
                .clip(true),
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

/// Premier nom d'une liste d'artistes (« A, B & C » → « A »).
fn main_artist(artists: &str) -> &str {
    artists
        .split([',', '&'])
        .next()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or(artists)
}

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
    fn visible_range_covers_the_viewport_plus_margin() {
        let view = View {
            offset: 1000.0,
            height: 580.0,
        };
        // Haut de liste à 200 px, lignes de 58 px : la 1re visible est la 13e.
        let (first, last) = visible_range(view, 200.0, 58.0, 1000);
        assert_eq!(first, 13 - OVERSCAN);
        assert!(last >= 13 + 10, "toute la hauteur visible est couverte");
        assert!(last - first < 40, "pas plus que nécessaire");
        // Liste courte, ou défilée au-delà de la fin.
        assert_eq!(visible_range(view, 200.0, 58.0, 5), (5, 5));
        assert_eq!(visible_range(View::default(), 0.0, 58.0, 3), (0, 3));
    }

    #[test]
    fn formats_durations() {
        assert_eq!(fmt(Duration::from_secs(65)), "1:05");
        assert_eq!(fmt(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(fmt_opt(None), "");
    }
}
