//! Palette et styles des widgets.
//!
//! En clair : toutes les couleurs, arrondis et effets de survol au même
//! endroit, pour que l'interface reste cohérente. La couleur d'accent change
//! avec la pochette du morceau en cours.

use iced::border::Radius;
use iced::widget::{button, container, scrollable, slider, svg, text_input};
use iced::{Background, Border, Color, Gradient, Shadow, Theme, Vector, color, gradient};

// Noir pur pour le fond : les pixels d'un écran OLED s'éteignent vraiment.
// Les panneaux restent à peine plus clairs pour garder la structure.
pub const BG: Color = color!(0x000000);
/// Teinte d'une page dont la pochette n'est pas encore analysée : neutre,
/// pour ne pas emprunter celle du morceau en cours.
pub const NEUTRAL_ACCENT: Color = color!(0xb8b8c4);
pub const SURFACE: Color = color!(0x0a0a0c);
pub const SURFACE_HI: Color = color!(0x16161a);
pub const SURFACE_HOVER: Color = color!(0x1f1f25);
pub const LINE: Color = color!(0x1c1c22);
pub const TEXT: Color = color!(0xf2f2f5);
pub const TEXT_DIM: Color = color!(0xa3a3ad);
pub const TEXT_FAINT: Color = color!(0x6c6c77);
pub const DEFAULT_ACCENT: Color = color!(0xff4d73);

pub const RADIUS: f32 = 10.0;

/// Accent lisible sur fond sombre, dérivé de la couleur dominante de la pochette.
pub fn accent_from_rgb([r, g, b]: [u8; 3]) -> Color {
    let (h, s, l) = rgb_to_hsl(r, g, b);
    let (r, g, b) = hsl_to_rgb(h, s.max(0.55), l.clamp(0.58, 0.72));
    Color::from_rgb(r, g, b)
}

/// Version très sombre de l'accent, pour le dégradé d'arrière-plan.
pub fn shade(accent: Color, lightness: f32) -> Color {
    let (h, s, _) = rgb_to_hsl(
        (accent.r * 255.0) as u8,
        (accent.g * 255.0) as u8,
        (accent.b * 255.0) as u8,
    );
    let (r, g, b) = hsl_to_rgb(h, (s * 0.7).min(0.6), lightness);
    Color::from_rgb(r, g, b)
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

pub fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

// ----- conteneurs -----

pub fn window_bg(_: &Theme) -> container::Style {
    container::Style {
        background: Some(BG.into()),
        text_color: Some(TEXT),
        ..container::Style::default()
    }
}

pub fn sidebar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE.into()),
        ..container::Style::default()
    }
}

pub fn player_bar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE.into()),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::default(),
        },
        ..container::Style::default()
    }
}

pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE.into()),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: RADIUS.into(),
        },
        ..container::Style::default()
    }
}

/// Dégradé de l'accent vers le fond, en haut des pages.
pub fn hero(accent: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Gradient(Gradient::Linear(
            gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, shade(accent, 0.22))
                .add_stop(1.0, BG),
        ))),
        ..container::Style::default()
    }
}

/// Cadre arrondi comme l'image qu'il entoure (rond pour un artiste).
pub fn cover_frame_round(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let mut style = cover_frame(theme);
        style.border.radius = radius.into();
        style
    }
}

/// Cadre des grandes pochettes. Pas d'ombre floue : le rendu logiciel la
/// dessine mal aux coins (points clairs), un liseré discret suffit.
pub fn cover_frame(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE_HI.into()),
        border: Border {
            color: alpha(Color::WHITE, 0.06),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

pub fn toast(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE_HOVER.into()),
        text_color: Some(TEXT),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: RADIUS.into(),
        },
        shadow: Shadow {
            color: alpha(Color::BLACK, 0.5),
            offset: Vector::new(0.0, 6.0),
            blur_radius: 20.0,
        },
        ..container::Style::default()
    }
}

// ----- boutons -----

/// Ligne de liste (morceau, entrée de file) : discrète, surlignée au survol.
pub fn row(active: bool, accent: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let background = match status {
            // Survol teinté par le morceau en cours, comme le reste de l'interface.
            button::Status::Hovered | button::Status::Pressed => Some(alpha(accent, 0.16).into()),
            _ if active => Some(alpha(accent, 0.12).into()),
            _ => None,
        };
        button::Style {
            background,
            text_color: TEXT,
            border: Border {
                radius: 8.0.into(),
                ..Border::default()
            },
            ..button::Style::default()
        }
    }
}

/// Entrée de la barre latérale.
pub fn nav(active: bool, accent: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: match status {
            _ if active => Some(alpha(accent, 0.14).into()),
            button::Status::Hovered | button::Status::Pressed => Some(alpha(accent, 0.08).into()),
            _ => None,
        },
        text_color: if active { accent } else { TEXT_DIM },
        border: Border {
            radius: 8.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// Bouton rond plein (lecture/pause).
pub fn primary_round(accent: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let bg = match status {
            button::Status::Hovered => Color::WHITE,
            button::Status::Pressed => alpha(Color::WHITE, 0.85),
            _ => alpha(Color::WHITE, 0.94),
        };
        button::Style {
            background: Some(bg.into()),
            text_color: BG,
            // Liseré d'accent plutôt qu'une ombre floue (le halo débordait en carré).
            border: Border {
                color: alpha(accent, 0.6),
                width: 2.0,
                radius: 999.0.into(),
            },
            ..button::Style::default()
        }
    }
}

/// Bouton pilule plein, couleur d'accent (« Lecture » sur une page d'album).
pub fn pill(accent: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(
            match status {
                button::Status::Hovered => accent,
                button::Status::Pressed => alpha(accent, 0.8),
                _ => alpha(accent, 0.92),
            }
            .into(),
        ),
        text_color: BG,
        border: Border {
            radius: 999.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// Bouton pilule discret (« Aléatoire »).
pub fn pill_ghost(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(
            match status {
                button::Status::Hovered | button::Status::Pressed => SURFACE_HOVER,
                _ => SURFACE_HI,
            }
            .into(),
        ),
        text_color: TEXT,
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 999.0.into(),
        },
        ..button::Style::default()
    }
}

/// Bouton icône sans fond.
pub fn icon_button(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(alpha(Color::WHITE, 0.08).into())
            }
            _ => None,
        },
        text_color: TEXT,
        border: Border {
            radius: 999.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// Lien textuel (nom d'artiste, d'album).
pub fn link(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => TEXT,
            _ => TEXT_DIM,
        },
        ..button::Style::default()
    }
}

/// Case de la mosaïque de l'accueil : fond discret, teinté au survol.
pub fn grid_tile(accent: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(
            match status {
                button::Status::Hovered | button::Status::Pressed => alpha(accent, 0.16),
                _ => SURFACE_HI,
            }
            .into(),
        ),
        text_color: TEXT,
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// Carte d'album ou d'artiste dans une grille.
pub fn card(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => Some(SURFACE_HI.into()),
            _ => None,
        },
        text_color: TEXT,
        border: Border {
            radius: RADIUS.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

// ----- icônes -----

pub fn icon_color(color: Color) -> impl Fn(&Theme, svg::Status) -> svg::Style {
    move |_, _| svg::Style { color: Some(color) }
}

// ----- champs et curseurs -----

pub fn search_input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: SURFACE_HI.into(),
        border: Border {
            color: if focused { alpha(TEXT, 0.35) } else { LINE },
            width: 1.0,
            radius: 999.0.into(),
        },
        icon: TEXT_DIM,
        placeholder: TEXT_FAINT,
        value: TEXT,
        selection: alpha(DEFAULT_ACCENT, 0.35),
    }
}

pub fn progress(accent: Color) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_, status| {
        let hovered = !matches!(status, slider::Status::Active);
        let fill = if hovered { accent } else { TEXT };
        slider::Style {
            rail: slider::Rail {
                backgrounds: (fill.into(), alpha(Color::WHITE, 0.16).into()),
                width: 4.0,
                border: Border {
                    radius: 2.0.into(),
                    ..Border::default()
                },
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle {
                    radius: if hovered { 6.0 } else { 0.0 },
                },
                background: Color::WHITE.into(),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
            },
        }
    }
}

pub fn scroll(_: &Theme, status: scrollable::Status) -> scrollable::Style {
    let hovered = matches!(
        status,
        scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. }
    );
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: alpha(Color::WHITE, if hovered { 0.22 } else { 0.08 }).into(),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: SURFACE_HI.into(),
            border: Border::default(),
            shadow: Shadow::default(),
            icon: TEXT,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_is_readable_even_from_dark_covers() {
        let c = accent_from_rgb([30, 20, 10]);
        let luminance = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        assert!(luminance > 0.35, "accent trop sombre : {c:?}");
    }

    #[test]
    fn hsl_round_trip() {
        let (h, s, l) = rgb_to_hsl(200, 60, 90);
        let (r, g, b) = hsl_to_rgb(h, s, l);
        assert!(((r * 255.0) - 200.0).abs() < 1.5);
        assert!(((g * 255.0) - 60.0).abs() < 1.5);
        assert!(((b * 255.0) - 90.0).abs() < 1.5);
    }
}
