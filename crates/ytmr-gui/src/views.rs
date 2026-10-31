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

use crate::app::{
    App, CARD, HERO, LYRIC_SIZE, LYRICS_SCROLL, LYRICS_TOP, Load, Message, NP_GAP,
    NP_LYRICS_SCROLL, NP_PAD, PAGE_SCROLL, Panel, Phase, QUEUE_SCROLL, QueueDrag, RightPanel,
    Route, SEARCH_INPUT, THUMB, View,
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
        Phase::Ready(_) => main_layout(app),
    };
    // La structure de l'arbre ne change jamais (une couche vide quand il n'y a
    // rien) : sinon iced recrée la page dessous et ses listes défilantes
    // repartent en haut (clic droit, fenêtre d'ajout, message).
    let nothing = || -> El<'_> { Space::new().into() };
    let layers = stack![
        content,
        library::overlay(app).unwrap_or_else(nothing),
        app.toast
            .as_ref()
            .map_or_else(nothing, |(_, message)| toast(message)),
    ];
    // Barre de titre maison, cachée en plein écran.
    let immersive = app.fullscreen;
    let framed = column![
        if immersive {
            nothing()
        } else {
            crate::chrome::title_bar(app)
        },
        layers
    ];
    let framed = stack![
        framed,
        if app.maximized || app.fullscreen {
            nothing()
        } else {
            crate::chrome::resize_edges()
        },
        // Tout en haut, dans le repère de la fenêtre : il s'ouvre sous la souris.
        crate::context::menu(app).unwrap_or_else(nothing),
        if app.show_help {
            help_panel()
        } else {
            nothing()
        },
        drag_ghost(app).unwrap_or_else(nothing),
    ];
    // Note la position des clics droits (menu contextuel).
    crate::context::catch_points(
        container(framed)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::window_bg),
        |point| Message::Context(crate::context::Ctx::Point(point)),
        |point| Message::Context(crate::context::Ctx::LeftPoint(point)),
    )
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
        Route::Library => crate::collection::library_page(app),
        Route::Playlist(_) => library::playlist_page(app),
        Route::Settings => crate::settings::settings_page(app),
        Route::History => library::history_page(app),
        Route::Stats => crate::stats_page::stats_page(app),
    };
    // La barre de chargement passe par-dessus la page : elle ne la décale pas
    // (sinon une bande noire de 3 px restait au-dessus du dégradé).
    let page = container(stack![page, loading_bar(app)])
        .width(Length::Fill)
        .height(Length::Fill);
    // Grande vue « En cours » : plein écran, aucune barre latérale.
    if app.route == Route::NowPlaying {
        // Plein écran épuré : les contrôles n'apparaissent qu'au mouvement de la souris.
        // Les contrôles apparaissent et disparaissent en fondu : un voile du
        // fond de la page (noir) passe d'opaque à transparent par-dessus.
        // La barre se pose *par-dessus* le bas de la page (rien ne change de
        // taille). Le bas de la vue est noir : un voile noir qui s'efface y
        // donne un vrai fondu.
        let level = app.controls_level;
        let bar: El<'_> = if level > 0.001 {
            container(iced::widget::opaque(stack![
                player_bar(app),
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_| iced::widget::container::Style {
                        background: Some(style::alpha(style::bg(), 1.0 - level).into()),
                        ..Default::default()
                    }),
            ]))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Alignment::End)
            .into()
        } else {
            Space::new().into()
        };
        return stack![page, bar].into();
    }
    let mut middle = row![sidebar(app), resize_handle(Panel::Left), page];
    if app.show_queue {
        let side = match app.right_panel {
            RightPanel::Queue => drop_zone(queue_panel(app), crate::app::DropTarget::Queue),
            RightPanel::Lyrics => lyrics_side(app),
        };
        middle = middle.push(resize_handle(Panel::Right)).push(side);
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
                background: Some(style::line_color().into()),
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

    // Suggestions : un panneau posé *par-dessus* la navigation, juste sous le
    // champ (elles poussaient les boutons Accueil, Bibliothèque…).
    let mut suggestions = column![].spacing(2);
    let show_suggestions = app.route == Route::Search && !app.suggestions.is_empty();
    if show_suggestions {
        for s in &app.suggestions {
            suggestions = suggestions.push(
                button(
                    row![
                        icon(Icon::Search, 13.0, style::TEXT_FAINT),
                        text(s)
                            .size(13)
                            .wrapping(iced::widget::text::Wrapping::None)
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding(Padding::from([6, 14]))
                .style(style::nav(false, app.accent))
                .on_press(Message::PickSuggestion(s.clone())),
            );
        }
    }
    // Hauteur fixe du champ : le panneau se place juste dessous.
    const SEARCH_H: f32 = 40.0;
    let below = column![
        container(search).height(Length::Fixed(SEARCH_H)),
        column![
            nav("Accueil", Icon::Home, Route::Home),
            nav("Bibliothèque", Icon::Library, Route::Library),
            nav("Statistiques", Icon::Chart, Route::Stats),
            nav("Paramètres", Icon::Settings, Route::Settings),
        ]
        .spacing(4),
        Space::new().height(Length::Fill),
        library::account_widget(app),
    ]
    .spacing(18);
    let overlay: El<'_> = if show_suggestions {
        column![
            Space::new().height(Length::Fixed(SEARCH_H + 4.0)),
            iced::widget::opaque(
                container(suggestions)
                    .width(Length::Fill)
                    .padding(6)
                    .style(style::toast)
            ),
        ]
        .into()
    } else {
        Space::new().into()
    };

    container(
        column![
            container(logo).padding(Padding::from([6, 8])),
            stack![below, overlay].height(Length::Fill),
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

/// Pochette à demi transparente (fondu du mode focus).
fn faded_cover<'a>(handle: Option<image::Handle>, size: f32, radius: f32, opacity: f32) -> El<'a> {
    match handle {
        Some(h) => image(h)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .border_radius(radius)
            .opacity(opacity)
            .into(),
        None => cover(None, size, radius),
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
            Message::Lib(Lib::OpenAddTo(vec![track.clone()])),
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

/// Caractères d'un titre de page (40 px) qui tiennent à côté de la pochette.
fn title_chars(app: &App) -> usize {
    ((app.center_width() - 340.0) / 22.0).max(10.0) as usize
}

/// Bouton lecture rond posé sur une pochette au survol (accueil).
pub(crate) fn play_bubble<'a>(app: &App, message: Message) -> El<'a> {
    button(container(icon(Icon::Play, 16.0, style::ON_ACCENT)).center(Length::Fixed(40.0)))
        .padding(0)
        .style(style::primary_round(app.accent))
        .on_press(message)
        .into()
}

/// Pochette avec, au survol, le bouton lecture en bas à droite. La couche est
/// toujours là (vide au repos) : la structure ne change pas au survol.
pub(crate) fn cover_with_play<'a>(
    app: &App,
    art: El<'a>,
    side: f32,
    hovered: bool,
    play: Message,
) -> El<'a> {
    let bubble: El<'a> = if hovered {
        play_bubble(app, play)
    } else {
        Space::new().into()
    };
    stack![
        art,
        container(bubble)
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .align_x(Alignment::End)
            .align_y(Alignment::End)
            .padding(8),
    ]
    .into()
}

/// Pastille « téléchargé » : petit rond plein de la teinte, flèche sombre.
pub(crate) fn downloaded_badge<'a>(accent: Color) -> El<'a> {
    container(icon(Icon::Download, 10.0, style::ON_ACCENT))
        .center(Length::Fixed(16.0))
        .style(move |_| iced::widget::container::Style {
            background: Some(accent.into()),
            border: iced::Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Nom d'un morceau : un clic ouvre sa page (album ou single), s'il en a une.
pub(crate) fn track_title<'a>(track: &'a Track, size: u32, color: Color) -> El<'a> {
    let label = line(&track.title, size).font(SEMIBOLD).color(color);
    button(label)
        .padding(0)
        .style(style::link)
        .on_press(Message::OpenAlbumOf(track.clone()))
        .into()
}

/// Morceaux affichés dans les résultats de recherche.
pub(crate) const SEARCH_SONGS: usize = 10;

/// Morceaux affichés : 10 dans « Tout », tous dans « Titres ».
pub(crate) fn search_songs_shown(app: &App) -> usize {
    if app.search_tab == crate::app::SearchTab::Songs {
        usize::MAX
    } else {
        SEARCH_SONGS
    }
}

/// Zone où l'on peut déposer des morceaux glissés (file, playlist).
pub(crate) fn drop_zone<'a>(content: El<'a>, target: crate::app::DropTarget) -> El<'a> {
    mouse_area(content)
        .on_enter(Message::DropEnter(target.clone()))
        .on_exit(Message::DropLeave(target))
        .into()
}

/// Étiquette qui suit la souris pendant qu'on glisse des morceaux.
fn drag_ghost(app: &App) -> Option<El<'_>> {
    let drag = app.track_drag.as_ref().filter(|d| d.active)?;
    let what = match drag.tracks.as_slice() {
        [one] => one.title.clone(),
        many => format!("{} titres", many.len()),
    };
    let to = match &app.drop_target {
        Some(crate::app::DropTarget::Queue) => " → File d'attente".to_owned(),
        Some(crate::app::DropTarget::Playlist(id)) if id == ytmr_catalog::account::LIKED_ID => {
            " → Titres likés".to_owned()
        }
        Some(crate::app::DropTarget::Playlist(id)) => format!(" → {}", app.playlist_name(id)),
        None => String::new(),
    };
    let accent = app.accent;
    // Liseré d'accent quand le dépôt est possible à cet endroit.
    let has_target = !to.is_empty();
    let pill = container(
        row![
            icon(Icon::Music, 14.0, accent),
            text(format!("{}{to}", ellipsize(&what, 40)))
                .size(13)
                .font(SEMIBOLD),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([7, 12]))
    .style(move |t| {
        let mut s = style::toast(t);
        if has_target {
            s.border.color = accent;
            s.border.width = 1.5;
        }
        s
    });
    Some(
        container(pill)
            .padding(Padding {
                top: drag.pos.y + 14.0,
                left: drag.pos.x + 14.0,
                ..Padding::ZERO
            })
            .into(),
    )
}

/// Fenêtre d'aide (touche ?) : tous les raccourcis.
fn help_panel<'a>() -> El<'a> {
    const KEYS: [(&str, &str); 14] = [
        ("Espace", "Lecture / pause"),
        ("← / →", "Reculer / avancer de 5 s"),
        ("Ctrl + ← / →", "Morceau précédent / suivant"),
        ("↑ / ↓", "Volume + / − (5 %)"),
        ("M", "Couper / remettre le son"),
        (
            "L",
            "Liker / retirer le like (sélection ou morceau en cours)",
        ),
        ("/  ou  Ctrl + F", "Aller à la recherche"),
        ("Clic", "Sélectionner un morceau"),
        ("Ctrl + clic", "Ajouter / retirer de la sélection"),
        ("Maj + clic", "Sélectionner une plage"),
        ("Double-clic", "Lancer le morceau"),
        ("Glisser", "Vers la file ou une playlist de la bibliothèque"),
        ("F11", "Plein écran"),
        (
            "Échap",
            "Fermer, désélectionner, remonter en haut de la page",
        ),
    ];
    let mut list = column![].spacing(10);
    for (keys, what) in KEYS {
        list = list.push(
            row![
                container(text(keys).size(13).font(SEMIBOLD)).width(Length::Fixed(150.0)),
                text(what).size(13).color(style::TEXT_DIM),
            ]
            .align_y(Alignment::Center),
        );
    }
    let panel = container(
        column![
            row![
                text("Raccourcis").size(18).font(BOLD).width(Length::Fill),
                icon_btn(Icon::Close, 15.0, style::TEXT_DIM, Message::ToggleHelp),
            ]
            .align_y(Alignment::Center),
            list,
        ]
        .spacing(16),
    )
    .padding(22)
    .width(Length::Fixed(520.0))
    .style(style::panel);
    let dim = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| iced::widget::container::Style {
                background: Some(style::alpha(Color::BLACK, 0.55).into()),
                ..Default::default()
            }),
    )
    .on_press(Message::ToggleHelp);
    iced::widget::opaque(stack![
        dim,
        container(mouse_area(panel).on_press(Message::Noop)).center(Length::Fill)
    ])
}

/// Texte coupé à `max` caractères avec « … » (iced ne sait pas le faire seul).
pub(crate) fn ellipsize(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max {
        return s.into();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end()).into()
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

pub(crate) fn smooth_scroll<'a>(content: impl Into<El<'a>>, id: &'static str) -> El<'a> {
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
            "Retirer".into(),
        ));
    }
    container(buttons).width(width).into()
}

/// Ligne de morceau : clic = lecture ; boutons like, playlist, « lire ensuite »,
/// « ajouter à la file » et, dans une playlist, « retirer ».
pub(crate) fn track_row<'a>(
    app: &App,
    track: &'a Track,
    // Rang dans la liste affichée (sélection avec Maj).
    index: usize,
    number: Option<usize>,
    on_play: Message,
    remove: Option<Message>,
) -> El<'a> {
    let playing = app
        .state
        .current
        .as_ref()
        .is_some_and(|e| e.track.video_id == track.video_id);
    // Grisé par YouTube, ou pas téléchargé en mode hors ligne : en gris, jamais lancé.
    let playable = app.lib.playable(track);
    let title_color = if playing {
        app.accent
    } else if playable {
        style::TEXT
    } else {
        style::TEXT_FAINT
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
                track_title(track, 14, title_color),
                line(subtitle, 13).color(if playable {
                    style::TEXT_DIM
                } else {
                    style::TEXT_FAINT
                }),
            ]
            .spacing(2),
        )
        .width(Length::Fill)
        .clip(true),
        // Disponible sans réseau (cache audio) ; la place est toujours réservée.
        container(if app.lib.offline_ids.contains(&track.video_id) {
            downloaded_badge(app.accent)
        } else {
            Space::new().into()
        })
        .width(Length::Fixed(16.0)),
        // Écoutes selon YouTube : seulement sur les pages d'album et d'artiste.
        match (&track.plays, &app.route) {
            (Some(plays), Route::Album(_) | Route::Artist(_)) => El::from(
                container(text(plays).size(12).color(style::TEXT_FAINT))
                    .width(Length::Fixed(150.0))
                    .align_x(Alignment::End),
            ),
            _ => Space::new().into(),
        },
        text(fmt_opt(track.duration))
            .size(13)
            .color(style::TEXT_FAINT),
        row_actions(app, track, remove),
    ]
    .spacing(14)
    .align_y(Alignment::Center);

    // Pas de bouton : un clic simple ne lance rien, le double-clic lance.
    let hovered = app.hovered.as_deref() == Some(track.video_id.as_str());
    container(
        mouse_area(
            container(row_content)
                .width(Length::Fill)
                .padding(Padding::from([6, 10]))
                .style(style::row_box(
                    style::RowState {
                        active: playing,
                        hovered,
                        selected: app.selected.contains(&track.video_id),
                    },
                    app.accent,
                )),
        )
        .on_press(Message::Select(track.video_id.clone(), index))
        .on_double_click(if playable { on_play } else { Message::Noop })
        .interaction(iced::mouse::Interaction::Pointer)
        .on_enter(Message::Hover(track.video_id.clone()))
        .on_exit(Message::Unhover(track.video_id.clone()))
        .on_right_press(crate::context::open(crate::context::Target::Track(
            track.clone(),
        ))),
    )
    .height(Length::Fixed(ROW_H))
    .align_y(Alignment::Center)
    .into()
}

pub(crate) fn album_card<'a>(app: &App, album: &'a AlbumSummary) -> El<'a> {
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
    let card = button(
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
    .on_press(Message::Navigate(Route::Album(album.id.clone())));
    mouse_area(card)
        .on_right_press(crate::context::open(crate::context::Target::Album(
            album.clone(),
        )))
        .into()
}

fn artist_card<'a>(app: &App, artist: &'a ArtistSummary) -> El<'a> {
    let id = app.url_cover(artist.thumbnail_url.as_deref());
    let card = button(
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
    .on_press(Message::Navigate(Route::Artist(artist.id.clone())));
    mouse_area(card)
        .on_right_press(crate::context::open(crate::context::Target::Artist(
            artist.clone(),
        )))
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
    if app.lib.offline_mode {
        return page_scroll(
            column![
                text("Hors ligne").size(34).font(BOLD),
                muted("Seulement ce qui est téléchargé sur cet appareil. Le mode se coupe dans les Paramètres."),
                crate::collection::offline_cards(app),
            ]
            .spacing(20)
            .padding(Padding {
                top: 28.0,
                right: 28.0,
                bottom: 32.0,
                left: 28.0,
            }),
        );
    }
    let greeting = column![
        text(greeting(app)).size(34).font(BOLD),
        muted("Reprends là où tu t'étais arrêté, ou cherche quelque chose."),
    ]
    .spacing(8);
    match library::home_sections(app) {
        Some(sections) => {
            // Léger dégradé de la teinte du morceau derrière la salutation,
            // qui se fond dans le noir de la page.
            let header = container(greeting)
                .width(Length::Fill)
                .padding(Padding {
                    top: 40.0,
                    right: 28.0,
                    bottom: 36.0,
                    left: 28.0,
                })
                .style(style::greeting(app.accent));
            page_scroll(column![
                header,
                container(sections).padding(Padding {
                    top: 0.0,
                    right: 28.0,
                    bottom: 32.0,
                    left: 28.0,
                }),
            ])
        }
        None => container(
            column![
                text("Que veux-tu écouter ?").size(34).font(BOLD),
                muted("Tape un titre, un artiste ou un album dans la recherche à gauche."),
                button(
                    row![
                        icon(Icon::Search, 16.0, style::ON_ACCENT),
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
    let side = app.np_cover();
    // Pleine définition dès qu'elle est prête (512 px en attendant) ; fondu
    // depuis la pochette du morceau précédent.
    let big = app.big_size();
    let art = |id: Option<&str>| app.cover(id, big).or_else(|| app.cover(id, HERO));
    let shown = match &app.np_fade {
        Some((since, previous)) => {
            let t = (app
                .frame_now
                .saturating_duration_since(*since)
                .as_secs_f32()
                / crate::app::NP_FADE.as_secs_f32())
            .clamp(0.0, 1.0);
            let eased = t * t * (3.0 - 2.0 * t);
            stack![
                cover(art(previous.as_deref()), side, 14.0),
                faded_cover(art(app.np_art.as_deref()), side, 14.0, eased),
            ]
            .into()
        }
        // La pochette prête (l'ancienne tant que la nouvelle charge).
        None => cover(art(app.np_art.as_deref().or(id.as_deref())), side, 14.0),
    };
    let big = container(shown).style(style::cover_frame);
    let fade = app.track_alpha();
    let mut info = column![
        // Titre long : sur deux lignes plutôt que coupé. Fondu au changement.
        text(&track.title)
            .size(34)
            .font(BOLD)
            .color(style::alpha(style::TEXT, fade))
            .width(Length::Fixed(side)),
        button(
            text(&track.artist)
                .size(19)
                .color(style::alpha(style::TEXT_DIM, fade))
        )
        .padding(0)
        .style(style::link)
        .on_press(Message::OpenArtist(
            track.artist_id.clone(),
            main_artist(&track.artist).to_owned(),
        )),
    ]
    .spacing(6);
    if let (Some(album), Some(album_id)) = (&track.album, &track.album_id) {
        info = info.push(
            button(
                text(album)
                    .size(14)
                    .color(style::alpha(style::TEXT_DIM, fade)),
            )
            .padding(0)
            .style(style::link)
            .on_press(Message::Navigate(Route::Album(album_id.clone()))),
        );
    }
    // Paroles seulement si elles suivent le son (synchronisées) ; sinon la
    // pochette et le titre seuls, au centre.
    // Pochette et titre centrés en hauteur ; les paroles synchronisées
    // arrivent à droite : la pochette glisse vers la gauche pendant qu'elles
    // apparaissent en fondu (`np_split` de 0 à 1). L'ensemble reste centré.
    let split = app.np_split;
    let lyrics_w = app.np_lyrics_text_width() + 4.0;
    let left = container(column![big, info].spacing(24).width(Length::Fixed(side)))
        .height(Length::Fill)
        .align_y(Alignment::Center);
    // Largeur qui grandit, contenu de largeur fixe : le texte ne se recompose
    // pas pendant le glissement.
    let lyrics = container(
        container(lyrics_body(
            app,
            NP_LYRICS_SCROLL,
            app.np_lyrics_text_width(),
            app.np_lyrics_view.height,
            false,
            app.np_lyric_size,
            split,
        ))
        .width(Length::Fixed(lyrics_w))
        .height(Length::Fill),
    )
    .width(Length::Fixed(lyrics_w * split))
    .height(Length::Fill)
    .clip(true);
    let main = container(
        row![
            left,
            Space::new().width(Length::Fixed(NP_GAP * split)),
            lyrics
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center);

    // Mode focus : rien n'apparaît tout seul ; un clic n'importe où affiche
    // ou masque les contrôles (Échap pour revenir). La marge du bas laisse la
    // place à la barre de contrôles : rien ne bouge quand elle apparaît.
    mouse_area(
        container(column![main].padding(Padding {
            top: NP_PAD,
            right: NP_PAD,
            bottom: 110.0,
            left: NP_PAD,
        }))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::hero(app.accent)),
    )
    .on_press(Message::ToggleControls)
    .into()
}

/// Panneau des paroles, à la place de la file d'attente.
///
/// Fond teinté par la pochette, grands caractères blancs. Synchronisées : la
/// ligne chantée est pleine, les suivantes estompées, les passées plus encore ;
/// elle glisse en douceur aux deux cinquièmes du panneau. Au survol, une ligne
/// s'éclaircit seulement (pas de cadre) ; un clic y amène la lecture. Défiler
/// à la molette suspend le suivi quelques secondes.
fn lyrics_side(app: &App) -> El<'_> {
    let accent = app.accent;
    let current = app.state.current.as_ref();
    let header = row![
        text("Paroles").size(15).font(BOLD).width(Length::Fill),
        icon_tip(
            Icon::Close,
            15.0,
            style::alpha(Color::WHITE, 0.7),
            Message::ToggleLyrics,
            "Fermer".into()
        ),
    ]
    .align_y(Alignment::Center);

    let lyrics = current.and_then(|e| match app.lyrics.get(&e.track.video_id) {
        Some(Load::Ready(Some(l))) => Some(l),
        _ => None,
    });
    let body = lyrics_body(
        app,
        LYRICS_SCROLL,
        crate::app::lyrics_text_width(app.queue_width),
        app.lyrics_view.height,
        true,
        LYRIC_SIZE,
        1.0,
    );
    let footer = text(
        lyrics
            .map(|l| format!("Paroles : {}", l.source))
            .unwrap_or_default(),
    )
    .size(11)
    .color(style::alpha(Color::WHITE, 0.45));

    container(column![header, body, footer].spacing(10).padding(Padding {
        top: 16.0,
        right: 16.0,
        bottom: 12.0,
        left: 20.0,
    }))
    .width(Length::Fixed(app.queue_width))
    .height(Length::Fill)
    .style(move |_| iced::widget::container::Style {
        // Teinte de la pochette, assombrie vers le bas pour la lisibilité.
        background: Some(iced::Background::Gradient(iced::Gradient::Linear(
            iced::gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, style::shade(accent, 0.30))
                .add_stop(1.0, style::shade(accent, 0.12)),
        ))),
        text_color: Some(Color::WHITE),
        ..Default::default()
    })
    .into()
}

/// Paroles du morceau en cours (synchronisées ou texte simple) pour la
/// liste défilante `scroll`, dont le texte fait `text_width` de large.
/// `interactive` faux (mode focus) : ni clic sur une ligne, ni barre de
/// défilement ; l'appli fait défiler seule.
fn lyrics_body<'a>(
    app: &'a App,
    scroll: &'static str,
    text_width: f32,
    view_height: f32,
    interactive: bool,
    size: f32,
    // Opacité d'ensemble (apparition des paroles du mode focus).
    alpha: f32,
) -> El<'a> {
    use ytmr_catalog::lyrics::LyricsBody;
    let current = app.state.current.as_ref();
    // Pendant un changement de morceau : les anciennes paroles s'estompent,
    // les nouvelles apparaissent ; rien entre les deux (pas de « Chargement… »).
    let shown = app.lyrics_on_screen();
    let message = |m: &'static str, a: f32| -> El<'_> {
        container(
            text(m)
                .size(15)
                .color(style::alpha(Color::WHITE, 0.6 * alpha * a)),
        )
        .center(Length::Fill)
        .into()
    };
    let Some(shown) = shown else {
        // Mode focus : rien (la pochette reste seule). Panneau : un mot
        // seulement quand on sait qu'il n'y a pas de paroles.
        let after_out = app.lyrics_out.is_none();
        return match current {
            _ if !interactive => Space::new().into(),
            None => message("Rien en lecture.", 1.0),
            Some(e) => match app.lyrics.get(&e.track.video_id) {
                _ if !after_out => Space::new().into(),
                Some(Load::Loading) | None if !app.lib.offline_mode => Space::new().into(),
                Some(Load::Ready(None) | Load::Failed(_)) if !app.lib.offline_mode => {
                    message("Paroles indisponibles pour ce titre.", app.track_alpha())
                }
                _ => message("Paroles indisponibles hors ligne.", app.track_alpha()),
            },
        };
    };
    let alpha = alpha * shown.alpha;
    let lyrics = shown.lyrics;
    match &lyrics.body {
        // Mode focus : seulement des paroles qui suivent la musique.
        LyricsBody::Plain(_) if !interactive => Space::new().into(),
        LyricsBody::Synced(lines) => {
            let sung = shown.sung;
            let mut list = column![Space::new().height(Length::Fixed(LYRICS_TOP))];
            let heights = crate::app::lyric_heights(
                crate::app::line_lengths(lyrics, app.romanize).into_iter(),
                text_width,
                size,
            );
            // Opacité de repos : pleine (chantée), 0,5 (à venir), 0,3 (passée) ;
            // la ligne qui s'allume et celle qui s'éteint passent en fondu.
            let level = |line: Option<usize>, i: usize| match line {
                Some(c) if c == i => 1.0,
                Some(c) if i < c => 0.3,
                _ => 0.5,
            };
            for ((i, line), height) in lines.iter().enumerate().zip(heights) {
                let target = level(sung, i);
                let from = level(shown.previous, i);
                let rest = from + (target - from) * shown.line_t;
                let words = if line.text.is_empty() {
                    "♪"
                } else {
                    line.text.as_str()
                };
                // Romanisation (réglage) : la lecture en petit sous la ligne.
                let romaji = app
                    .romanize
                    .then(|| lyrics.romanized.get(i).and_then(Option::as_deref))
                    .flatten();
                let words: El<'_> = match romaji {
                    Some(r) => column![
                        text(words).size(size).font(BOLD),
                        text(r).size(size * crate::app::ROMAJI_SCALE).font(SEMIBOLD),
                    ]
                    .spacing(2)
                    .into(),
                    None => text(words).size(size).font(BOLD).into(),
                };
                let words = container(words)
                    .height(Length::Fixed(height))
                    .align_y(Alignment::Center);
                if !interactive {
                    list = list.push(
                        container(words)
                            .width(Length::Fill)
                            .padding(Padding::from([0, 2]))
                            .style(move |_| iced::widget::container::Style {
                                text_color: Some(style::alpha(Color::WHITE, rest * alpha)),
                                ..Default::default()
                            }),
                    );
                    continue;
                }
                list = list.push(
                    button(words)
                        .width(Length::Fill)
                        .padding(Padding::from([0, 2]))
                        .style(move |_, status| button::Style {
                            background: None,
                            text_color: style::alpha(
                                Color::WHITE,
                                match status {
                                    button::Status::Hovered | button::Status::Pressed => {
                                        f32::max(rest, 0.85)
                                    }
                                    _ => rest,
                                },
                            ),
                            ..button::Style::default()
                        })
                        .on_press(Message::SeekTo(line.at)),
                );
            }
            // Marge du bas : la dernière ligne peut remonter comme les autres.
            list = list.push(Space::new().height(Length::Fixed(view_height)));
            lyrics_scroll(list, scroll, interactive)
        }
        LyricsBody::Plain(body) => lyrics_scroll(
            container(
                text(body)
                    .size(19)
                    .font(SEMIBOLD)
                    .line_height(1.6)
                    .color(style::alpha(Color::WHITE, 0.85 * alpha)),
            )
            .padding(Padding {
                top: LYRICS_TOP,
                bottom: 40.0,
                ..Padding::ZERO
            }),
            scroll,
            interactive,
        ),
    }
}

/// Défilement des paroles ; sans barre en mode focus (la molette y est ignorée).
fn lyrics_scroll<'a>(content: impl Into<El<'a>>, id: &'static str, interactive: bool) -> El<'a> {
    if interactive {
        return smooth_scroll(content, id);
    }
    wheel(
        scrollable(content)
            .id(id)
            .on_scroll(move |viewport| Message::Scrolled(id, viewport))
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .height(Length::Fill)
            .style(style::scroll),
        move |lines| Message::Wheel(id, lines),
    )
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
    (secs.saturating_add_signed(utc_offset_secs()) / 3600) % 24
}

/// Décalage de l'heure locale (Paris : +1 h, +2 h l'été ; approximation au jour près).
pub(crate) fn utc_offset_secs() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day_of_year = (secs / 86_400) % 365;
    // Heure d'été européenne : fin mars → fin octobre.
    if (85..300).contains(&day_of_year) {
        7200
    } else {
        3600
    }
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
        if app.recent_searches.is_empty() {
            return invite(
                "Que veux-tu écouter ?",
                "Tape un titre, un artiste ou un album dans la barre de recherche.",
            );
        }
        return recent_searches(app);
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
        search_tabs(app),
        body
    ])
}

/// Onglets des résultats.
fn search_tabs(app: &App) -> El<'_> {
    use crate::app::SearchTab;
    let accent = app.accent;
    let tab = |label: &'static str, which: SearchTab| -> El<'_> {
        let active = app.search_tab == which;
        button(text(label).size(13).font(SEMIBOLD))
            .padding(Padding::from([7, 16]))
            .style(move |theme, status| {
                if active {
                    style::pill(accent)(theme, status)
                } else {
                    style::pill_ghost(theme, status)
                }
            })
            .on_press(Message::SearchTab(which))
            .into()
    };
    container(
        row![
            tab("Tout", SearchTab::All),
            tab("Titres", SearchTab::Songs),
            tab("Albums", SearchTab::Albums),
            tab("Artistes", SearchTab::Artists),
        ]
        .spacing(8),
    )
    .padding(Padding {
        bottom: 20.0,
        ..Padding::ZERO
    })
    .into()
}

/// Champ vide : les recherches récentes, à relancer d'un clic.
fn recent_searches(app: &App) -> El<'_> {
    let mut list = column![].spacing(2);
    for query in &app.recent_searches {
        list = list.push(
            container(
                row![
                    button(
                        row![
                            icon(Icon::Search, 15.0, style::TEXT_DIM),
                            text(query).size(15),
                        ]
                        .spacing(14)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding(Padding::from([10, 12]))
                    .style(style::card)
                    .on_press(Message::SearchRecent(query.clone())),
                    icon_btn(
                        Icon::Close,
                        14.0,
                        style::TEXT_FAINT,
                        Message::SearchForget(query.clone()),
                    ),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .max_width(720),
        );
    }
    scroll_page(column![
        container(
            row![
                text("Recherches récentes").size(26).font(BOLD),
                Space::new().width(Length::Fill),
                button(text("Tout effacer").size(13).font(SEMIBOLD))
                    .padding(Padding::from([7, 14]))
                    .style(style::pill_ghost)
                    .on_press(Message::SearchClearRecent),
            ]
            .align_y(Alignment::Center)
            .width(Length::Fixed(720.0)),
        )
        .padding(Padding {
            top: 28.0,
            bottom: 16.0,
            ..Padding::ZERO
        }),
        list
    ])
}

fn search_results<'a>(app: &'a App, r: &'a SearchAll) -> El<'a> {
    use crate::app::SearchTab;
    let tab = app.search_tab;
    let show = |which: SearchTab| tab == SearchTab::All || tab == which;
    // « Tout » : un aperçu de chaque ; un onglet : tout ce qui est arrivé.
    let (albums_max, artists_max) = match tab {
        SearchTab::All => (12, 8),
        _ => (usize::MAX, usize::MAX),
    };
    let mut page = column![].spacing(28);
    if show(SearchTab::Songs) && !r.songs.is_empty() {
        let mut songs = column![section_title("Morceaux")].spacing(4);
        for (i, track) in r.songs.iter().take(search_songs_shown(app)).enumerate() {
            // Un morceau seul : la radio prend le relais, comme sur YouTube Music.
            songs = songs.push(track_row(
                app,
                track,
                i,
                None,
                Message::Play(vec![track.clone()], 0),
                None,
            ));
        }
        page = page.push(songs);
    }
    if show(SearchTab::Albums) && !r.albums.is_empty() {
        page = page.push(
            column![
                section_title("Albums"),
                cards(
                    r.albums
                        .iter()
                        .take(albums_max)
                        .map(|a| album_card(app, a))
                        .collect()
                )
            ]
            .spacing(10),
        );
    }
    if show(SearchTab::Artists) && !r.artists.is_empty() {
        page = page.push(
            column![
                section_title("Artistes"),
                cards(
                    r.artists
                        .iter()
                        .take(artists_max)
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

/// Boutons sous le titre d'une page.
pub(crate) struct Actions<'a> {
    /// (lecture, aléatoire) ; `None` pour une liste vide.
    pub play: Option<(Message, Message)>,
    /// Like, suivre…
    pub extra: Option<El<'a>>,
}

pub(crate) fn page_header<'a>(
    app: &App,
    cover_id: Option<String>,
    radius: f32,
    kicker: String,
    title: &'a str,
    subtitle: El<'a>,
    actions: Actions<'a>,
) -> El<'a> {
    let (play, shuffle) = actions.play.unzip();
    // Boutons et titres suivent le morceau en cours ; seul le dégradé du haut
    // garde la teinte de la page (fixe, sans mélange).
    let accent = app.accent;
    let art = container(cover(app.cover(cover_id.as_deref(), HERO), 220.0, radius))
        .style(style::cover_frame_round(radius));
    let actions = row![
        button(
            row![
                icon(Icon::Play, 16.0, style::ON_ACCENT),
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
    .spacing(12)
    .align_y(Alignment::Center)
    .push(actions.extra.unwrap_or_else(|| Space::new().into()));
    let header = container(
        row![
            art,
            column![
                text(kicker).size(12).font(BOLD).color(accent),
                // Une seule ligne : un titre sur deux lignes poussait les boutons
                // hors de l'en-tête (hauteur fixe) et ils s'affichaient vides.
                text(ellipsize(title, title_chars(app)))
                    .size(40)
                    .font(BOLD)
                    .wrapping(iced::widget::text::Wrapping::None),
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
    .style(style::hero(app.page_accent))
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
        Actions {
            play: (!album.tracks.is_empty())
                .then_some((Message::PlayAlbum(false), Message::PlayAlbum(true))),
            extra: Some({
                let saved = app.album_saved(&album.id);
                // Téléchargé si tous ses morceaux sont dans « Téléchargés ».
                let pinned = !album.tracks.is_empty()
                    && album
                        .tracks
                        .iter()
                        .filter(|t| !t.unavailable)
                        .all(|t| app.lib.downloads.iter().any(|d| d.video_id == t.video_id));
                let download = crate::library::download_pill(
                    app,
                    &album.tracks,
                    pinned,
                    Message::Lib(crate::library::Lib::DownloadAlbum(!pinned)),
                );
                let heart = crate::collection::heart(
                    app,
                    saved,
                    Message::Coll(crate::collection::Coll::SaveAlbum(
                        AlbumSummary {
                            id: album.id.clone(),
                            title: album.title.clone(),
                            artist: album.artist.clone(),
                            year: album.year.clone(),
                            kind: album.kind.clone(),
                            thumbnail_url: album.thumbnail_url.clone(),
                        },
                        album.audio_playlist_id.clone(),
                        !saved,
                    )),
                    if saved {
                        "Retirer de la bibliothèque"
                    } else {
                        "Ajouter à la bibliothèque"
                    },
                );
                if app.lib.offline_mode {
                    heart
                } else {
                    row![download, heart]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .into()
                }
            }),
        },
    );
    let mut list = column![].spacing(2);
    for (i, track) in album.tracks.iter().enumerate() {
        list = list.push(track_row(
            app,
            track,
            i,
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
        Actions {
            play: (!artist.songs.is_empty())
                .then_some((Message::PlayArtist(false), Message::PlayArtist(true))),
            extra: Some({
                let followed = app.artist_followed(&artist.id);
                let accent = app.accent;
                button(
                    text(if followed { "Abonné" } else { "Suivre" })
                        .size(14)
                        .font(SEMIBOLD),
                )
                .padding(Padding::from([10, 20]))
                .style(move |theme, status| {
                    if followed {
                        style::pill_ghost(theme, status)
                    } else {
                        style::pill(accent)(theme, status)
                    }
                })
                .on_press(Message::Coll(crate::collection::Coll::FollowArtist(
                    ArtistSummary {
                        id: artist.id.clone(),
                        name: artist.name.clone(),
                        subscribers: artist.subscribers.clone(),
                        thumbnail_url: artist.thumbnail_url.clone(),
                    },
                    !followed,
                )))
                .into()
            }),
        },
    );
    let mut page = column![].spacing(28);
    if !artist.songs.is_empty() {
        let mut songs = column![section_title("Titres populaires")].spacing(4);
        for (i, track) in artist.songs.iter().enumerate() {
            songs = songs.push(track_row(
                app,
                track,
                i,
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
            "Fermer".into()
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let entries = &app.queue.entries;
    let range = visible_range(app.queue_view, 0.0, QROW_H, entries.len());
    let list: El<'_> = if entries.is_empty() {
        muted("La file est vide.")
    } else {
        let drag = app.queue_drag;
        let accent = app.accent;
        virtual_list(entries.len(), QROW_H, range, |i| {
            let entry = &entries[i];
            let current = app.queue.current == Some(i);
            let title_color = if current { accent } else { style::TEXT };
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
                    "Retirer".into()
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            // Pas de bouton ici : il garderait le clic pour lui. Un clic bref
            // lance le morceau, un clic maintenu le déplace (voir `QueueDrag`).
            let held = drag.is_some_and(|d| d.from == i && d.moved);
            let row_state = style::RowState {
                active: current,
                hovered: drag.is_none() && app.queue_hover == Some(i),
                // Saisie pour un déplacement : rendue comme sélectionnée.
                selected: held || app.queue_selected == Some(entry.id),
            };
            let body = container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(Alignment::Center)
                .padding(Padding::from([5, 8]))
                .style(style::row_box(row_state, accent));
            // Trait d'insertion là où le morceau va tomber.
            let marker: El<'_> = match drag {
                Some(d) if d.moved && d.over == i && d.from != i => container(
                    container(Space::new())
                        .width(Length::Fill)
                        .height(Length::Fixed(2.0))
                        .style(move |_| iced::widget::container::Style {
                            background: Some(accent.into()),
                            ..Default::default()
                        }),
                )
                .height(Length::Fill)
                .align_y(if d.over < d.from {
                    Alignment::Start
                } else {
                    Alignment::End
                })
                .into(),
                _ => Space::new().into(),
            };
            let area = mouse_area(stack![body, marker])
                .on_press(Message::QueueDrag(QueueDrag::Press(i)))
                .on_double_click(Message::JumpTo(entry.id))
                .on_enter(if drag.is_some() {
                    Message::QueueDrag(QueueDrag::Over(i))
                } else {
                    Message::QueueHover(Some(i))
                })
                .on_exit(Message::QueueUnhover(i))
                .on_right_press(crate::context::open(crate::context::Target::Track(
                    entry.track.clone(),
                )))
                .interaction(if drag.is_some_and(|d| d.moved) {
                    iced::mouse::Interaction::Grabbing
                } else {
                    iced::mouse::Interaction::Pointer
                });
            // Aléatoire activé / coupé : la nouvelle liste apparaît en fondu, de
            // haut en bas. Le voile a la couleur du panneau (unie) : c'est un
            // vrai fondu. Toujours présent (vide au repos) : l'arbre ne change pas.
            let veil = app
                .queue_fade
                .map_or(0.0, |f| f.veil(i.saturating_sub(range.0), app.frame_now));
            stack![
                container(area)
                    .height(Length::Fixed(QROW_H))
                    .align_y(Alignment::Center),
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_| iced::widget::container::Style {
                        background: (veil > 0.0)
                            .then(|| style::alpha(style::surface(), veil).into()),
                        ..Default::default()
                    }),
            ]
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
            // Nom : la page du morceau (album ou single) ; la grande vue
            // « En cours » s'ouvre seulement depuis la pochette.
            // Nouveau morceau : pochette et noms apparaissent en fondu.
            let fade = app.track_alpha();
            let title = button(
                line(&track.title, 14)
                    .font(SEMIBOLD)
                    .color(style::alpha(style::TEXT, fade)),
            )
            .padding(0)
            .style(style::link)
            .on_press(Message::OpenAlbumOf(track.clone()));
            let id = app.track_cover(track);
            row![
                button(faded_cover(
                    app.cover(id.as_deref(), THUMB),
                    56.0,
                    6.0,
                    fade
                ))
                .padding(0)
                .style(style::link)
                .on_press(Message::Navigate(Route::NowPlaying)),
                container(
                    column![
                        title,
                        button(line(&track.artist, 13).color(style::alpha(style::TEXT_DIM, fade)))
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
    let play =
        button(container(icon(toggle_icon, 18.0, style::ON_ACCENT)).center(Length::Fixed(40.0)))
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
            "Aléatoire".into()
        ),
        icon_tip(
            Icon::Prev,
            18.0,
            style::TEXT,
            Message::Previous,
            "Précédent".into()
        ),
        play,
        icon_tip(
            Icon::Next,
            18.0,
            style::TEXT,
            Message::Next,
            "Suivant".into()
        ),
        icon_tip(
            repeat_icon,
            17.0,
            lit(state.repeat != Repeat::Off),
            Message::CycleRepeat,
            "Répéter".into()
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
        slider(
            0.0..=total,
            app.anims.get(crate::anim::Key::Seek, shown).min(total),
            Message::SeekPreview
        )
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
    let lyrics_open = app.lyrics_shown();
    let sleep_tip = match app.sleep {
        Some(crate::app::Sleep::At(at)) => {
            let left = at.saturating_duration_since(std::time::Instant::now());
            format!("Arrêt dans {} min", left.as_secs().div_ceil(60))
        }
        Some(crate::app::Sleep::EndOfTrack) => "Arrêt à la fin du morceau".to_owned(),
        None => "Minuteur de sommeil".to_owned(),
    };
    let right = row![
        icon_tip(
            Icon::Moon,
            17.0,
            lit(app.sleep.is_some()),
            Message::Context(crate::context::Ctx::Open(crate::context::Target::Sleep)),
            sleep_tip,
        ),
        icon_tip(
            Icon::Mic,
            17.0,
            lit(lyrics_open),
            // Paroles à la place de la file ; second clic : fermées.
            Message::ToggleLyrics,
            "Paroles".into()
        ),
        icon_tip(
            Icon::Queue,
            17.0,
            lit(app.show_queue && app.right_panel == RightPanel::Queue),
            Message::ToggleQueue,
            "File d'attente".into()
        ),
        icon_tip(
            volume_icon,
            17.0,
            style::TEXT_DIM,
            Message::ToggleMute,
            "Volume".into()
        ),
        slider(
            0.0..=100.0_f32,
            app.anims
                .get(crate::anim::Key::Volume, f32::from(state.volume)),
            |v| Message::Volume(v.round().clamp(0.0, 100.0) as u8)
        )
        .step(1.0_f32)
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
pub(crate) fn main_artist(artists: &str) -> &str {
    artists
        .split([',', '&'])
        .next()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or(artists)
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
