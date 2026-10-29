//! Cadre de la fenêtre : barre de titre maison et bords redimensionnables.
//!
//! En clair : la fenêtre n'a plus la barre de titre de Windows. ytmr dessine
//! la sienne (même fond que les barres latérales, avec réduire / agrandir /
//! fermer) : on la saisit pour
//! déplacer la fenêtre, un double-clic l'agrandit. Sans cadre système, les
//! bords ne se tirent plus d'eux-mêmes : de fines zones invisibles le long des
//! bords redonnent le redimensionnement.

use iced::widget::{Space, button, column, container, mouse_area, row};
use iced::window::{self, Direction};
use iced::{Alignment, Color, Length, Task, mouse};

use crate::app::{App, Message};
use crate::icons::{Icon, icon};
use crate::style;
use crate::views::El;

/// Hauteur de la barre de titre.
pub const TITLE_H: f32 = 32.0;
const EDGE: f32 = 5.0;
const CORNER: f32 = 12.0;

#[derive(Debug, Clone, Copy)]
pub enum Win {
    Drag,
    Minimize,
    ToggleMaximize,
    Resize(Direction),
    /// État d'agrandissement relu après un changement.
    Maximized(bool),
}

impl App {
    pub fn update_window(&mut self, action: Win) -> Task<Message> {
        match action {
            Win::Maximized(on) => {
                self.maximized = on;
                Task::none()
            }
            Win::Drag => window::oldest().and_then(window::drag),
            Win::Minimize => window::oldest().and_then(|id| window::minimize(id, true)),
            Win::ToggleMaximize => window::oldest().and_then(|id| {
                window::toggle_maximize::<Message>(id)
                    .chain(window::is_maximized(id).map(|on| Message::Window(Win::Maximized(on))))
            }),
            Win::Resize(direction) => {
                window::oldest().and_then(move |id| window::drag_resize(id, direction))
            }
        }
    }

    /// Après un redimensionnement (aimantation de Windows comprise).
    pub fn refresh_maximized(&self) -> Task<Message> {
        window::oldest()
            .and_then(|id| window::is_maximized(id).map(|on| Message::Window(Win::Maximized(on))))
    }
}

fn window_button<'a>(ic: Icon, message: Message, danger: bool) -> El<'a> {
    button(container(icon(ic, 14.0, style::TEXT_DIM)).center(Length::Fill))
        .width(Length::Fixed(46.0))
        .height(Length::Fixed(TITLE_H))
        .padding(0)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then(|| {
                    if danger {
                        Color::from_rgb8(0xc4, 0x2b, 0x1c).into()
                    } else {
                        style::alpha(Color::WHITE, 0.08).into()
                    }
                }),
                text_color: style::TEXT,
                ..button::Style::default()
            }
        })
        .on_press(message)
        .into()
}

/// Barre du haut : zone de saisie (déplacer, double-clic = agrandir) et boutons.
pub fn title_bar(app: &App) -> El<'_> {
    let grip = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(TITLE_H)),
    )
    .on_press(Message::Window(Win::Drag))
    .on_double_click(Message::Window(Win::ToggleMaximize));
    let bar = row![
        grip,
        window_button(Icon::Minimize, Message::Window(Win::Minimize), false),
        window_button(
            if app.maximized {
                Icon::Restore
            } else {
                Icon::Maximize
            },
            Message::Window(Win::ToggleMaximize),
            false
        ),
        window_button(Icon::Close, Message::CloseRequested, true),
    ]
    .height(Length::Fixed(TITLE_H))
    .align_y(Alignment::Center);
    // Même fond que les barres latérales et la barre de lecture, avec le même
    // filet de séparation : la fenêtre forme un seul cadre.
    column![
        container(bar).style(style::sidebar),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(1.0))
            .style(|_| container::Style {
                background: Some(style::LINE.into()),
                ..container::Style::default()
            }),
    ]
    .into()
}

fn edge<'a>(direction: Direction, width: Length, height: Length) -> El<'a> {
    let cursor = match direction {
        Direction::North | Direction::South => mouse::Interaction::ResizingVertically,
        Direction::East | Direction::West => mouse::Interaction::ResizingHorizontally,
        Direction::NorthWest | Direction::SouthEast => mouse::Interaction::ResizingDiagonallyDown,
        Direction::NorthEast | Direction::SouthWest => mouse::Interaction::ResizingDiagonallyUp,
    };
    mouse_area(container(Space::new()).width(width).height(height))
        .interaction(cursor)
        .on_press(Message::Window(Win::Resize(direction)))
        .into()
}

/// Zones de redimensionnement le long des bords (à poser par-dessus tout).
pub fn resize_edges<'a>() -> El<'a> {
    let fixed = Length::Fixed;
    column![
        row![
            edge(Direction::NorthWest, fixed(CORNER), fixed(EDGE)),
            edge(Direction::North, Length::Fill, fixed(EDGE)),
            edge(Direction::NorthEast, fixed(CORNER), fixed(EDGE)),
        ],
        row![
            edge(Direction::West, fixed(EDGE), Length::Fill),
            Space::new().width(Length::Fill).height(Length::Fill),
            edge(Direction::East, fixed(EDGE), Length::Fill),
        ]
        .height(Length::Fill),
        row![
            edge(Direction::SouthWest, fixed(CORNER), fixed(EDGE)),
            edge(Direction::South, Length::Fill, fixed(EDGE)),
            edge(Direction::SouthEast, fixed(CORNER), fixed(EDGE)),
        ],
    ]
    .into()
}
