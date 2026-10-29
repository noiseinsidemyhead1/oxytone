//! Menu contextuel (clic droit) sur un morceau, un album, un artiste ou une playlist.
//!
//! En clair : un clic droit ouvre un petit menu à l'endroit du clic (radio,
//! lire ensuite, aller à l'album, ajouter à une playlist, copier le lien…).
//! Les lignes et cartes disent *sur quoi* on a cliqué (`mouse_area`) ; un
//! widget posé tout autour de la fenêtre note *où* (les listes défilantes
//! décalent la position vue par leurs lignes, lui la voit telle quelle).

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::widget::{Space, button, column, container, mouse_area, text};
use iced::{Element, Event, Length, Padding, Point, Rectangle, Size, Task, Vector};
use ytmr_catalog::account::PlaylistSummary;
use ytmr_catalog::{AlbumSummary, ArtistSummary};
use ytmr_core::{Command, Track};

use crate::app::{App, Message, Route};
use crate::style;
use crate::views::El;

const MENU_W: f32 = 230.0;
const ITEM_H: f32 = 34.0;

/// Ce sur quoi on a fait un clic droit.
#[derive(Debug, Clone)]
pub enum Target {
    Track(Track),
    Album(AlbumSummary),
    Artist(ArtistSummary),
    Playlist(PlaylistSummary),
}

#[derive(Debug, Clone)]
pub enum Ctx {
    /// Position du clic droit (notée par le cadre de la fenêtre, avant la cible).
    Point(Point),
    Open(Target),
    Close,
    Radio(Track),
    ArtistRadio(String),
    ArtistSongs(Result<Vec<Track>, String>),
    Copy(String),
}

/// Raccourci pour les vues : message d'ouverture du menu.
pub fn open(target: Target) -> Message {
    Message::Context(Ctx::Open(target))
}

impl App {
    pub fn update_context(&mut self, message: Ctx) -> Task<Message> {
        match message {
            Ctx::Point(point) => {
                self.context_point = point;
                // Un clic droit ailleurs referme le menu ouvert.
                self.context_menu = None;
                Task::none()
            }
            Ctx::Open(target) => {
                self.context_menu = Some((self.context_point, target));
                Task::none()
            }
            Ctx::Close => {
                self.context_menu = None;
                Task::none()
            }
            Ctx::Radio(track) => {
                self.context_menu = None;
                // La radio, c'est la lecture automatique à partir de ce titre.
                Task::batch([
                    self.send(Command::SetAutoplay(true)),
                    self.send(Command::PlayTracks {
                        tracks: vec![track],
                        start: 0,
                    }),
                ])
            }
            Ctx::ArtistRadio(id) => {
                self.context_menu = None;
                let Some(catalog) = self.catalog() else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        catalog
                            .artist(&id)
                            .await
                            .map(|a| a.songs)
                            .map_err(|e| e.to_string())
                    },
                    |r| Message::Context(Ctx::ArtistSongs(r)),
                )
            }
            Ctx::ArtistSongs(result) => match result {
                Ok(songs) if !songs.is_empty() => Task::batch([
                    self.send(Command::SetAutoplay(true)),
                    self.send(Command::PlayShuffled(songs)),
                ]),
                Ok(_) => self.show_toast("Aucun titre pour cet artiste".into()),
                Err(e) => self.show_toast(e),
            },
            Ctx::Copy(link) => {
                self.context_menu = None;
                Task::batch([
                    iced::clipboard::write(link),
                    self.show_toast("Lien copié".into()),
                ])
            }
        }
    }
}

fn link_track(t: &Track) -> String {
    format!("https://music.youtube.com/watch?v={}", t.video_id)
}

/// Les actions proposées pour une cible : (libellé, message).
fn items(app: &App, target: &Target) -> Vec<(&'static str, Message)> {
    let connected = app.lib.ready().is_some();
    let close_then = |m: Message| Message::Batch(vec![Message::Context(Ctx::Close), m]);
    let mut out = Vec::new();
    match target {
        Target::Track(t) => {
            out.push(("Lancer la radio", Message::Context(Ctx::Radio(t.clone()))));
            out.push(("Lire ensuite", close_then(Message::PlayNext(t.clone()))));
            out.push(("Ajouter à la file", close_then(Message::Enqueue(t.clone()))));
            if connected {
                out.push((
                    "Ajouter à une playlist",
                    close_then(Message::Lib(crate::library::Lib::OpenAddTo(t.clone()))),
                ));
            }
            if let Some(album) = &t.album_id {
                out.push((
                    "Aller à l'album",
                    close_then(Message::Navigate(Route::Album(album.clone()))),
                ));
            }
            out.push((
                "Aller à l'artiste",
                close_then(Message::OpenArtist(
                    t.artist_id.clone(),
                    crate::views::main_artist(&t.artist).to_owned(),
                )),
            ));
            out.push(("Copier le lien", Message::Context(Ctx::Copy(link_track(t)))));
        }
        Target::Album(a) => {
            out.push((
                "Ouvrir l'album",
                close_then(Message::Navigate(Route::Album(a.id.clone()))),
            ));
            if connected {
                let saved = app.album_saved(&a.id);
                out.push((
                    if saved {
                        "Retirer de la bibliothèque"
                    } else {
                        "Ajouter à la bibliothèque"
                    },
                    close_then(Message::Coll(crate::collection::Coll::SaveAlbum(
                        a.clone(),
                        None,
                        !saved,
                    ))),
                ));
            }
            out.push((
                "Copier le lien",
                Message::Context(Ctx::Copy(format!(
                    "https://music.youtube.com/browse/{}",
                    a.id
                ))),
            ));
        }
        Target::Artist(a) => {
            out.push((
                "Lancer la radio",
                Message::Context(Ctx::ArtistRadio(a.id.clone())),
            ));
            out.push((
                "Ouvrir l'artiste",
                close_then(Message::Navigate(Route::Artist(a.id.clone()))),
            ));
            if connected {
                let followed = app.artist_followed(&a.id);
                out.push((
                    if followed { "Ne plus suivre" } else { "Suivre" },
                    close_then(Message::Coll(crate::collection::Coll::FollowArtist(
                        a.clone(),
                        !followed,
                    ))),
                ));
            }
            out.push((
                "Copier le lien",
                Message::Context(Ctx::Copy(format!(
                    "https://music.youtube.com/channel/{}",
                    a.id
                ))),
            ));
        }
        Target::Playlist(p) => {
            out.push((
                "Ouvrir la playlist",
                close_then(Message::Navigate(Route::Playlist(p.id.clone()))),
            ));
            if p.id != ytmr_catalog::account::LIKED_ID {
                out.push((
                    "Copier le lien",
                    Message::Context(Ctx::Copy(format!(
                        "https://music.youtube.com/playlist?list={}",
                        p.id.strip_prefix("VL").unwrap_or(&p.id)
                    ))),
                ));
            }
        }
    }
    out
}

/// Le menu ouvert, par-dessus toute la fenêtre.
pub fn menu(app: &App) -> Option<El<'_>> {
    let (point, target) = app.context_menu.as_ref()?;
    let entries = items(app, target);
    let height = entries.len() as f32 * ITEM_H + 12.0;
    let accent = app.accent;
    let mut list = column![].spacing(0);
    for (label, message) in entries {
        list = list.push(
            button(text(label).size(13))
                .width(Length::Fill)
                .height(Length::Fixed(ITEM_H))
                .padding(Padding::from([8, 12]))
                .style(style::row(false, accent))
                .on_press(message),
        );
    }
    // Le menu reste dans la fenêtre (vers la gauche / le haut près des bords).
    let x = if point.x + MENU_W > app.window_size.width {
        (point.x - MENU_W).max(0.0)
    } else {
        point.x
    };
    let y = if point.y + height > app.window_size.height {
        (point.y - height).max(0.0)
    } else {
        point.y
    };
    let panel = container(list)
        .width(Length::Fixed(MENU_W))
        .padding(6)
        .style(style::toast);
    // Fond transparent : un clic ailleurs ferme le menu.
    let backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Message::Context(Ctx::Close))
    .on_right_press(Message::Context(Ctx::Close));
    let placed = container(mouse_area(panel).on_press(Message::Noop)).padding(Padding {
        top: y,
        left: x,
        ..Padding::ZERO
    });
    Some(iced::widget::stack![backdrop, placed].into())
}

/// Enveloppe la fenêtre : note la position de chaque clic droit, puis laisse
/// passer l'événement (la ligne cliquée envoie ensuite sa cible).
pub struct PointCatcher<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_right: Box<dyn Fn(Point) -> Message + 'a>,
}

pub fn catch_points<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_right: impl Fn(Point) -> Message + 'a,
) -> PointCatcher<'a, Message, Theme, Renderer> {
    PointCatcher {
        content: content.into(),
        on_right: Box::new(on_right),
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for PointCatcher<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = event
            && let Some(position) = cursor.position()
        {
            // Publié avant la cible : les messages sont traités dans l'ordre.
            shell.publish((self.on_right)(position));
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer: renderer::Renderer + 'a>
    From<PointCatcher<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
{
    fn from(widget: PointCatcher<'a, Message, Theme, Renderer>) -> Self {
        Element::new(widget)
    }
}
