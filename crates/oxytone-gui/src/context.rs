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
use oxytone_catalog::account::PlaylistSummary;
use oxytone_catalog::{AlbumSummary, ArtistSummary};
use oxytone_core::{Command, Track};

use crate::app::{App, Message, Route};
use crate::icons::{Icon, icon};
use crate::style;
use crate::views::El;

const MENU_W: f32 = 230.0;
const ITEM_H: f32 = 34.0;

/// Ce sur quoi on a fait un clic droit.
#[derive(Debug, Clone)]
pub enum Target {
    Track(Track),
    /// Plusieurs morceaux sélectionnés.
    Tracks(Vec<Track>),
    Album(AlbumSummary),
    Artist(ArtistSummary),
    Playlist(PlaylistSummary),
    /// Minuteur de sommeil (bouton lune de la barre).
    Sleep,
}

#[derive(Debug, Clone)]
pub enum Ctx {
    /// Position du clic droit (notée par le cadre de la fenêtre, avant la cible).
    Point(Point),
    Open(Target),
    Close,
    Radio(Track),
    /// Titres de la radio arrivés (morceau de départ, titres).
    RadioTracks(String, Result<Vec<Track>, String>),
    ArtistRadio(String),
    ArtistSongs(Result<Vec<Track>, String>),
    Copy(String),
    /// Position du dernier clic gauche.
    LeftPoint(Point),
}

/// Raccourci pour les vues : message d'ouverture du menu.
pub fn open(target: Target) -> Message {
    Message::Context(Ctx::Open(target))
}

impl App {
    pub fn update_context(&mut self, message: Ctx) -> Task<Message> {
        match message {
            Ctx::LeftPoint(point) => {
                self.left_point = point;
                Task::none()
            }
            Ctx::Point(point) => {
                self.context_point = point;
                // Un clic droit ailleurs referme le menu ouvert.
                self.context_menu = None;
                Task::none()
            }
            Ctx::Open(target) => {
                // Clic droit sur une ligne d'une sélection multiple : le menu
                // agit sur toute la sélection.
                let target = match target {
                    Target::Track(t)
                        if self.selected.len() > 1 && self.selected.contains(&t.video_id) =>
                    {
                        Target::Tracks(self.selected_tracks())
                    }
                    other => other,
                };
                let point = if matches!(target, Target::Sleep) {
                    self.left_point
                } else {
                    self.context_point
                };
                self.context_menu = Some((point, target));
                Task::none()
            }
            Ctx::Close => {
                self.context_menu = None;
                Task::none()
            }
            Ctx::Radio(track) => {
                self.context_menu = None;
                // Le titre part tout de suite ; la radio YouTube (titres
                // similaires) remplit la file dès qu'elle arrive.
                let Some(catalog) = self.catalog() else {
                    return Task::none();
                };
                let seed = track.video_id.clone();
                // Radio du morceau en cours : il continue (le relancer le faisait
                // repartir du début). Sinon, il démarre.
                let playing = self
                    .state
                    .current
                    .as_ref()
                    .is_some_and(|e| e.track.video_id == seed);
                let start = if playing {
                    Task::none()
                } else {
                    self.send(Command::PlayTracks {
                        tracks: vec![track],
                        start: 0,
                    })
                };
                Task::batch([
                    start,
                    Task::perform(
                        async move {
                            let result = catalog.radio(&seed).await.map_err(|e| e.to_string());
                            (seed, result)
                        },
                        |(seed, r)| Message::Context(Ctx::RadioTracks(seed, r)),
                    ),
                ])
            }
            Ctx::RadioTracks(seed, result) => match result {
                Ok(tracks) => {
                    // L'utilisateur a peut-être lancé autre chose entre-temps.
                    let seed_title = self
                        .state
                        .current
                        .as_ref()
                        .map(|e| format!("{}|{}", fold(&e.track.title), fold(&e.track.artist)));
                    let still = self
                        .state
                        .current
                        .as_ref()
                        .is_some_and(|e| e.track.video_id == seed);
                    // Ni le titre de départ (YouTube le met en tête, parfois sous un
                    // autre identifiant), ni le morceau en cours : la suite, elle,
                    // est vidée juste avant.
                    let tracks: Vec<Track> = tracks
                        .into_iter()
                        .filter(|t| t.video_id != seed && !same_song(t, seed_title.as_deref()))
                        .filter(|t| {
                            !self
                                .queue
                                .entries
                                .iter()
                                .take(1)
                                .any(|e| e.track.video_id == t.video_id)
                        })
                        .collect();
                    if !still || tracks.is_empty() {
                        return Task::none();
                    }
                    // La radio remplace la suite : vider puis ajouter, dans cet ordre.
                    let fill = self
                        .send(Command::ClearUpcoming)
                        .chain(self.send(Command::Enqueue(tracks)));
                    Task::batch([fill, self.show_toast("Radio lancée".into())])
                }
                Err(e) => self.show_toast(format!("Radio indisponible : {e}")),
            },
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

/// Icône d'une action du menu, d'après son libellé.
fn item_icon(label: &str) -> Icon {
    match label {
        "Lancer la radio" => Icon::Radio,
        "Télécharger" | "Retirer des téléchargements" => Icon::Download,
        "Lire ensuite" => Icon::PlayNext,
        "Ajouter à la file" => Icon::Queue,
        "Ajouter à une playlist" => Icon::PlaylistAdd,
        "Aller à l'album" | "Ouvrir l'album" => Icon::Disc,
        "Aller à l'artiste" | "Ouvrir l'artiste" => Icon::Person,
        "Copier le lien" | "Copier les liens" => Icon::Link,
        "Retirer de la playlist" => Icon::Trash,
        "Ouvrir la playlist" => Icon::Library,
        "Retirer de la bibliothèque" | "Ne plus suivre" => Icon::HeartFilled,
        "Ajouter à la bibliothèque" | "Suivre" => Icon::Heart,
        "Fin du morceau" | "15 minutes" | "30 minutes" | "45 minutes" | "1 heure" => Icon::Moon,
        "Annuler le minuteur" => Icon::Close,
        _ => Icon::Music,
    }
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
                    close_then(Message::Lib(crate::library::Lib::OpenAddTo(vec![
                        t.clone(),
                    ]))),
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
            let downloaded = app.lib.downloads.iter().any(|d| d.video_id == t.video_id);
            out.push(if downloaded {
                (
                    "Retirer des téléchargements",
                    close_then(Message::Lib(crate::library::Lib::Download(
                        vec![t.clone()],
                        false,
                    ))),
                )
            } else {
                (
                    "Télécharger",
                    close_then(Message::Lib(crate::library::Lib::Download(
                        vec![t.clone()],
                        true,
                    ))),
                )
            });
            out.push(("Copier le lien", Message::Context(Ctx::Copy(link_track(t)))));
        }
        Target::Tracks(tracks) => {
            out.push((
                "Lire ensuite",
                close_then(Message::PlayNextAll(tracks.clone())),
            ));
            out.push((
                "Ajouter à la file",
                close_then(Message::EnqueueAll(tracks.clone())),
            ));
            out.push((
                "Télécharger",
                close_then(Message::Lib(crate::library::Lib::Download(
                    tracks.clone(),
                    true,
                ))),
            ));
            if connected {
                out.push((
                    "Ajouter à une playlist",
                    close_then(Message::Lib(crate::library::Lib::OpenAddTo(tracks.clone()))),
                ));
                let ids: Vec<String> = tracks.iter().map(|t| t.video_id.clone()).collect();
                // Dans une playlist du compte : retirer toute la sélection.
                if let (Route::Playlist(id), Some((_, crate::app::Load::Ready(p)))) =
                    (&app.route, &app.lib.playlist)
                    && (p.owned || id == oxytone_catalog::account::LIKED_ID)
                {
                    let message = if id == oxytone_catalog::account::LIKED_ID {
                        Message::Batch(
                            tracks
                                .iter()
                                .map(|t| Message::Lib(crate::library::Lib::ToggleLike(t.clone())))
                                .collect(),
                        )
                    } else {
                        Message::Lib(crate::library::Lib::RemoveTracks(id.clone(), ids.clone()))
                    };
                    out.push(("Retirer de la playlist", close_then(message)));
                }
            }
            let links = tracks.iter().map(link_track).collect::<Vec<_>>().join("\n");
            out.push(("Copier les liens", Message::Context(Ctx::Copy(links))));
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
        Target::Sleep => {
            use crate::app::SleepChoice;
            let set = |c: SleepChoice| close_then(Message::Sleep(Some(c)));
            out.push(("15 minutes", set(SleepChoice::Minutes(15))));
            out.push(("30 minutes", set(SleepChoice::Minutes(30))));
            out.push(("45 minutes", set(SleepChoice::Minutes(45))));
            out.push(("1 heure", set(SleepChoice::Minutes(60))));
            out.push(("Fin du morceau", set(SleepChoice::EndOfTrack)));
            if app.sleep.is_some() {
                out.push(("Annuler le minuteur", close_then(Message::Sleep(None))));
            }
        }
        Target::Playlist(p) => {
            out.push((
                "Ouvrir la playlist",
                close_then(Message::Navigate(Route::Playlist(p.id.clone()))),
            ));
            if p.id != oxytone_catalog::account::LIKED_ID {
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
    let header = match target {
        Target::Tracks(t) => Some(format!("{} titres sélectionnés", t.len())),
        _ => None,
    };
    let height = (entries.len() + usize::from(header.is_some())) as f32 * ITEM_H + 12.0;
    let accent = app.accent;
    let mut list = column![].spacing(0);
    if let Some(header) = header {
        list = list.push(
            container(text(header).size(12).color(style::TEXT_FAINT))
                .height(Length::Fixed(ITEM_H))
                .padding(Padding::from([10, 12])),
        );
    }
    for (label, message) in entries {
        list = list.push(
            button(
                iced::widget::row![
                    icon(item_icon(label), 15.0, style::TEXT_DIM),
                    text(label).size(13)
                ]
                .spacing(12)
                .align_y(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fixed(ITEM_H))
            .padding(Padding::from([8, 12]))
            .style(style::row(false, accent))
            .on_press(message),
        );
    }
    // Juste sous la pointe de la souris ; vers la gauche / le haut près des
    // bords, pour rester dans la fenêtre.
    const NUDGE: f32 = 2.0;
    let x = if point.x + NUDGE + MENU_W > app.window_size.width {
        (point.x - MENU_W - NUDGE).max(0.0)
    } else {
        point.x + NUDGE
    };
    let y = if point.y + NUDGE + height > app.window_size.height {
        (point.y - height - NUDGE).max(0.0)
    } else {
        point.y + NUDGE
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
    // Opaque : la page dessous ne réagit plus à la souris tant que le menu est ouvert.
    Some(iced::widget::opaque(iced::widget::stack![backdrop, placed]))
}

/// Enveloppe la fenêtre : note la position de chaque clic droit, puis laisse
/// passer l'événement (la ligne cliquée envoie ensuite sa cible).
pub struct PointCatcher<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_right: Box<dyn Fn(Point) -> Message + 'a>,
    on_left: Box<dyn Fn(Point) -> Message + 'a>,
}

pub fn catch_points<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_right: impl Fn(Point) -> Message + 'a,
    on_left: impl Fn(Point) -> Message + 'a,
) -> PointCatcher<'a, Message, Theme, Renderer> {
    PointCatcher {
        content: content.into(),
        on_right: Box::new(on_right),
        on_left: Box::new(on_left),
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
        // Clic gauche : position gardée pour les menus ouverts par un bouton
        // (minuteur de sommeil).
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && let Some(position) = cursor.position()
        {
            shell.publish((self.on_left)(position));
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

/// Même chanson sous un autre identifiant (clip / version audio).
fn same_song(track: &Track, seed: Option<&str>) -> bool {
    seed.is_some_and(|s| format!("{}|{}", fold(&track.title), fold(&track.artist)) == s)
}

fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}
