//! Icônes vectorielles (SVG en ligne, tracés 24×24).
//!
//! En clair : les petits dessins des boutons (lecture, suivant, shuffle…). Ils
//! sont dans le binaire et nets à toutes les tailles ; la couleur est appliquée
//! au moment de l'affichage.

use std::sync::LazyLock;

use iced::widget::svg::{self, Svg};
use iced::{Color, Length};

use crate::style;

#[derive(Debug, Clone, Copy)]
pub enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    VolumeMute,
    Search,
    Home,
    Queue,
    Plus,
    PlayNext,
    Close,
    Back,
    Music,
    Radio,
    Heart,
    HeartFilled,
    Library,
    PlaylistAdd,
    Edit,
    Trash,
}

const STROKE: &str =
    r#"fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round""#;

fn source(icon: Icon) -> String {
    let body = match icon {
        Icon::Play => r#"<path fill="black" d="M7 4.5v15a1 1 0 0 0 1.5.86l12.5-7.5a1 1 0 0 0 0-1.72L8.5 3.64A1 1 0 0 0 7 4.5z"/>"#.to_owned(),
        Icon::Pause => r#"<rect fill="black" x="5.5" y="4" width="4.5" height="16" rx="1.2"/><rect fill="black" x="14" y="4" width="4.5" height="16" rx="1.2"/>"#.to_owned(),
        Icon::Prev => r#"<path fill="black" d="M18 5.2v13.6a.8.8 0 0 1-1.25.66L7.5 13a1.2 1.2 0 0 1 0-2l9.25-6.46A.8.8 0 0 1 18 5.2z"/><rect fill="black" x="5" y="4.5" width="2.4" height="15" rx="1"/>"#.to_owned(),
        Icon::Next => r#"<path fill="black" d="M6 5.2v13.6a.8.8 0 0 0 1.25.66L16.5 13a1.2 1.2 0 0 0 0-2L7.25 4.54A.8.8 0 0 0 6 5.2z"/><rect fill="black" x="16.6" y="4.5" width="2.4" height="15" rx="1"/>"#.to_owned(),
        Icon::Shuffle => format!(r#"<g {STROKE}><path d="M2 18h3.5a4 4 0 0 0 3.3-1.7l6.4-8.6A4 4 0 0 1 18.5 6H22"/><path d="M2 6h3.5a4 4 0 0 1 3.3 1.7l.9 1.2"/><path d="M14.3 15.1l.9 1.2a4 4 0 0 0 3.3 1.7H22"/><path d="M19 3l3 3-3 3"/><path d="M19 15l3 3-3 3"/></g>"#),
        Icon::Repeat => format!(r#"<g {STROKE}><path d="M17 2l4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="M7 22l-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/></g>"#),
        Icon::RepeatOne => format!(r#"<g {STROKE}><path d="M17 2l4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="M7 22l-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1v4"/></g>"#),
        Icon::Volume => format!(r#"<g {STROKE}><path fill="black" d="M11 5L6 9H2v6h4l5 4V5z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M19 5a10 10 0 0 1 0 14"/></g>"#),
        Icon::VolumeMute => format!(r#"<g {STROKE}><path fill="black" d="M11 5L6 9H2v6h4l5 4V5z"/><path d="M22 9l-6 6"/><path d="M16 9l6 6"/></g>"#),
        Icon::Search => format!(r#"<g {STROKE}><circle cx="11" cy="11" r="7"/><path d="M21 21l-4.3-4.3"/></g>"#),
        Icon::Home => format!(r#"<g {STROKE}><path d="M3 10.5L12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z"/></g>"#),
        Icon::Queue => format!(r#"<g {STROKE}><path d="M3 6h13"/><path d="M3 12h13"/><path d="M3 18h9"/><path fill="black" stroke="none" d="M17 14.5v7l5-3.5z"/></g>"#),
        Icon::Plus => format!(r#"<g {STROKE}><path d="M12 5v14"/><path d="M5 12h14"/></g>"#),
        // Flèche qui « passe devant » dans la liste : distincte de l'ajout en playlist.
        Icon::PlayNext => format!(r#"<g {STROKE}><path d="M11 6h10"/><path d="M11 12h10"/><path d="M11 18h10"/><path d="M3 5v5a3 3 0 0 0 3 3h3"/><path d="M7 10l2.5 3L7 16"/></g>"#),
        Icon::Close => format!(r#"<g {STROKE}><path d="M18 6L6 18"/><path d="M6 6l12 12"/></g>"#),
        Icon::Back => format!(r#"<g {STROKE}><path d="M15 18l-6-6 6-6"/></g>"#),
        Icon::Music => format!(r#"<g {STROKE}><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></g>"#),
        Icon::Heart => format!(r#"<g {STROKE}><path d="M19.5 12.6 12 20l-7.5-7.4A5 5 0 1 1 12 6a5 5 0 1 1 7.5 6.6z"/></g>"#),
        Icon::HeartFilled => r#"<path fill="black" d="M19.5 12.6 12 20l-7.5-7.4A5 5 0 1 1 12 6a5 5 0 1 1 7.5 6.6z"/>"#.to_owned(),
        Icon::Library => format!(r#"<g {STROKE}><rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><path d="M14 17.5h7"/><path d="M17.5 14v7"/></g>"#),
        Icon::PlaylistAdd => format!(r#"<g {STROKE}><path d="M3 6h12"/><path d="M3 12h12"/><path d="M3 18h7"/><path d="M17 14v8"/><path d="M13 18h8"/></g>"#),
        Icon::Edit => format!(r#"<g {STROKE}><path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/></g>"#),
        Icon::Trash => format!(r#"<g {STROKE}><path d="M3 6h18"/><path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/></g>"#),
        Icon::Radio => format!(r#"<g {STROKE}><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8a6 6 0 0 1 0 8.5"/><path d="M7.8 16.2a6 6 0 0 1 0-8.5"/><path d="M19.1 4.9a10 10 0 0 1 0 14.2"/><path d="M4.9 19.1a10 10 0 0 1 0-14.2"/></g>"#),
    };
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">{body}</svg>"#)
}

/// Une poignée par icône, créée une seule fois : iced garde ainsi le rendu en cache.
static HANDLES: LazyLock<Vec<svg::Handle>> = LazyLock::new(|| {
    use Icon::*;
    [
        Play,
        Pause,
        Prev,
        Next,
        Shuffle,
        Repeat,
        RepeatOne,
        Volume,
        VolumeMute,
        Search,
        Home,
        Queue,
        Plus,
        PlayNext,
        Close,
        Back,
        Music,
        Radio,
        Heart,
        HeartFilled,
        Library,
        PlaylistAdd,
        Edit,
        Trash,
    ]
    .into_iter()
    .map(|icon| svg::Handle::from_memory(source(icon).into_bytes()))
    .collect()
});

pub fn icon<'a>(icon: Icon, size: f32, color: Color) -> Svg<'a> {
    Svg::new(HANDLES[icon as usize].clone())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(style::icon_color(color))
}
