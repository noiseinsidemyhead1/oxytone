//! Fenêtre « Importer depuis Spotify ».
//!
//! En clair : on colle le lien d'une playlist Spotify publique (ou on dépose
//! sur la fenêtre le fichier CSV d'Exportify), ytmr lit ses morceaux, cherche
//! chacun sur YouTube Music (par paquets de 10, avec la progression), puis
//! crée une playlist privée sur le compte avec ce qui a été trouvé.

use std::path::PathBuf;

use iced::widget::{Space, button, column, progress_bar, row, text, text_input};
use iced::{Alignment, Length, Padding, Task};
use ytmr_catalog::account::Privacy;
use ytmr_catalog::spotify::{SpotifyPlaylist, SpotifyTrack};
use ytmr_core::Track;

use crate::app::{App, Message};
use crate::style;
use crate::views::{BOLD, El, SEMIBOLD, muted};

/// Morceaux cherchés par paquet (la progression avance à chaque paquet).
const CHUNK: usize = 10;
/// Morceaux ajoutés à la playlist par requête.
const ADD_BATCH: usize = 100;
const LINK_INPUT: &str = "import-spotify";

#[derive(Debug, Clone)]
pub enum Imp {
    Open,
    Close,
    Link(String),
    Fetch,
    Fetched(u64, Result<SpotifyPlaylist, String>),
    /// Fichier déposé sur la fenêtre (CSV d'Exportify).
    File(PathBuf),
    Matched(u64, Vec<Option<Track>>),
    Name(String),
    Create,
}

#[derive(Debug, Default)]
pub struct Import {
    link: String,
    phase: Phase,
    /// Un nouvel import annule les réponses de l'ancien.
    generation: u64,
}

#[derive(Debug, Default)]
enum Phase {
    #[default]
    Idle,
    Fetching,
    Matching(Found),
    Ready(Found),
    Creating,
    Failed(String),
}

#[derive(Debug)]
struct Found {
    name: String,
    source: Vec<SpotifyTrack>,
    found: Vec<Option<Track>>,
    maybe_truncated: bool,
}

impl App {
    pub fn update_import(&mut self, message: Imp) -> Task<Message> {
        match message {
            Imp::Open => {
                self.import = Some(Import::default());
                iced::widget::operation::focus(LINK_INPUT)
            }
            Imp::Close => {
                self.import = None;
                Task::none()
            }
            Imp::Link(link) => {
                if let Some(import) = &mut self.import {
                    import.link = link;
                }
                Task::none()
            }
            Imp::Fetch => {
                let Some(catalog) = self.catalog() else {
                    return Task::none();
                };
                let Some(import) = &mut self.import else {
                    return Task::none();
                };
                if import.link.trim().is_empty() {
                    return Task::none();
                }
                import.generation += 1;
                import.phase = Phase::Fetching;
                let generation = import.generation;
                let link = import.link.clone();
                Task::perform(
                    async move {
                        catalog
                            .spotify_playlist(&link)
                            .await
                            .map_err(|e| e.to_string())
                    },
                    move |r| Message::Import(Imp::Fetched(generation, r)),
                )
            }
            Imp::File(path) => {
                let Some(import) = &mut self.import else {
                    return Task::none();
                };
                import.generation += 1;
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Playlist Spotify")
                    .replace('_', " ");
                let parsed = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| ytmr_catalog::spotify::parse_csv(&t, &name))
                    .ok_or_else(|| {
                        "Fichier non reconnu : il faut le CSV d'une playlist exportée avec Exportify."
                            .to_owned()
                    });
                let generation = import.generation;
                self.update_import(Imp::Fetched(generation, parsed))
            }
            Imp::Fetched(generation, result) => {
                let Some(import) = self.import.as_mut().filter(|i| i.generation == generation)
                else {
                    return Task::none();
                };
                match result {
                    Ok(playlist) if playlist.tracks.is_empty() => {
                        import.phase = Phase::Failed("Cette playlist est vide.".into());
                        Task::none()
                    }
                    Ok(playlist) => {
                        import.phase = Phase::Matching(Found {
                            name: playlist.name,
                            source: playlist.tracks,
                            found: Vec::new(),
                            maybe_truncated: playlist.maybe_truncated,
                        });
                        self.next_chunk()
                    }
                    Err(e) => {
                        import.phase = Phase::Failed(e);
                        Task::none()
                    }
                }
            }
            Imp::Matched(generation, results) => {
                let Some(import) = self.import.as_mut().filter(|i| i.generation == generation)
                else {
                    return Task::none();
                };
                let Phase::Matching(found) = &mut import.phase else {
                    return Task::none();
                };
                found.found.extend(results);
                if found.found.len() >= found.source.len() {
                    if let Phase::Matching(found) = std::mem::take(&mut import.phase) {
                        import.phase = Phase::Ready(found);
                    }
                    Task::none()
                } else {
                    self.next_chunk()
                }
            }
            Imp::Name(name) => {
                if let Some(Import {
                    phase: Phase::Ready(found),
                    ..
                }) = &mut self.import
                {
                    found.name = name;
                }
                Task::none()
            }
            Imp::Create => {
                let Some(account) = self.lib.ready() else {
                    return self.show_toast("Compte YouTube non connecté".into());
                };
                let Some(import) = &mut self.import else {
                    return Task::none();
                };
                let Phase::Ready(found) = std::mem::replace(&mut import.phase, Phase::Creating)
                else {
                    return Task::none();
                };
                let name = found.name.trim().to_owned();
                let ids: Vec<String> = found
                    .found
                    .iter()
                    .flatten()
                    .map(|t| t.video_id.clone())
                    .collect();
                let count = ids.len();
                Task::perform(
                    async move {
                        // Par paquets : une seule requête pour des centaines de
                        // morceaux risque d'être refusée.
                        let mut chunks = ids.chunks(ADD_BATCH);
                        let first = chunks.next().unwrap_or_default();
                        let id = account
                            .create_playlist(&name, Privacy::Private, first)
                            .await?;
                        for chunk in chunks {
                            account.add_tracks(&id, chunk).await?;
                        }
                        Ok::<_, ytmr_catalog::CatalogError>((id, name))
                    },
                    move |r: Result<(String, String), _>| {
                        Message::Batch(vec![
                            Message::Import(Imp::Close),
                            Message::Lib(crate::library::Lib::Created(
                                r.map_err(|e| e.to_string()),
                            )),
                            Message::Toast(format!("{count} titres importés")),
                        ])
                    },
                )
            }
        }
    }

    /// Paquet suivant de morceaux à chercher sur YouTube Music.
    fn next_chunk(&mut self) -> Task<Message> {
        let Some(catalog) = self.catalog() else {
            return Task::none();
        };
        let Some(Import {
            phase: Phase::Matching(found),
            generation,
            ..
        }) = &self.import
        else {
            return Task::none();
        };
        let start = found.found.len();
        let chunk: Vec<SpotifyTrack> =
            found.source[start..(start + CHUNK).min(found.source.len())].to_vec();
        let generation = *generation;
        Task::perform(
            async move { catalog.match_spotify(&chunk).await },
            move |r| Message::Import(Imp::Matched(generation, r)),
        )
    }
}

/// Fenêtre de l'import (par-dessus la page).
pub fn panel(app: &App) -> Option<El<'_>> {
    let import = app.import.as_ref()?;
    let accent = app.accent;
    let title = text("Importer depuis Spotify").size(20).font(BOLD);
    let link_row = row![
        text_input("Lien d'une playlist Spotify publique", &import.link)
            .id(LINK_INPUT)
            .on_input(|l| Message::Import(Imp::Link(l)))
            .on_submit(Message::Import(Imp::Fetch))
            .size(14)
            .padding(Padding::from([8, 12]))
            .style(style::search_input),
        button(text("Lire").size(14).font(SEMIBOLD))
            .padding(Padding::from([8, 18]))
            .style(style::pill(accent))
            .on_press(Message::Import(Imp::Fetch)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let hint = muted(
        "Ou dépose ici le fichier CSV d'une playlist exportée avec Exportify (exportify.net) : \
         toute la playlist, même privée ou de plus de 100 titres.",
    );
    let body: El<'_> = match &import.phase {
        Phase::Idle => column![link_row, hint].spacing(12).into(),
        Phase::Failed(e) => column![
            link_row,
            text(e.as_str()).size(13).color(iced::color!(0xff6b6b)),
            hint
        ]
        .spacing(12)
        .into(),
        Phase::Fetching => muted("Lecture de la playlist…"),
        Phase::Matching(found) => column![
            text(format!("« {} »", found.name)).size(15).font(SEMIBOLD),
            muted(format!(
                "Recherche sur YouTube Music… {} / {}",
                found.found.len(),
                found.source.len()
            )),
            progress_bar(0.0..=found.source.len() as f32, found.found.len() as f32)
                .girth(6)
                .style(move |_| iced::widget::progress_bar::Style {
                    background: style::surface_hi().into(),
                    bar: accent.into(),
                    border: iced::Border {
                        radius: 3.0.into(),
                        ..Default::default()
                    },
                }),
        ]
        .spacing(10)
        .into(),
        Phase::Ready(found) => ready(app, found),
        Phase::Creating => muted("Création de la playlist sur YouTube Music…"),
    };
    Some(column![title, body].spacing(16).into())
}

fn ready<'a>(app: &App, found: &'a Found) -> El<'a> {
    let accent = app.accent;
    let hits = found.found.iter().flatten().count();
    let missing: Vec<&SpotifyTrack> = found
        .source
        .iter()
        .zip(&found.found)
        .filter(|(_, f)| f.is_none())
        .map(|(s, _)| s)
        .collect();
    let mut body = column![
        text_input("Nom de la playlist", &found.name)
            .on_input(|n| Message::Import(Imp::Name(n)))
            .on_submit(Message::Import(Imp::Create))
            .size(14)
            .padding(Padding::from([8, 12]))
            .style(style::search_input),
        text(format!(
            "{hits} morceaux trouvés sur {}",
            found.source.len()
        ))
        .size(14)
        .font(SEMIBOLD),
    ]
    .spacing(10);
    if found.maybe_truncated {
        body = body.push(muted(
            "Un lien ne donne que les 100 premiers titres : pour la suite, passe par un export CSV.",
        ));
    }
    if !missing.is_empty() {
        let names: Vec<String> = missing
            .iter()
            .take(6)
            .map(|t| format!("{} — {}", t.title, t.artists))
            .collect();
        let more = missing.len().saturating_sub(6);
        let mut list = format!("Introuvables : {}", names.join(" · "));
        if more > 0 {
            list.push_str(&format!(" · et {more} autres"));
        }
        body = body.push(text(list).size(12).color(style::TEXT_DIM));
    }
    body.push(
        row![
            Space::new().width(Length::Fill),
            button(text("Annuler").size(14).font(SEMIBOLD))
                .padding(Padding::from([8, 16]))
                .style(style::pill_ghost)
                .on_press(Message::Import(Imp::Close)),
            button(text("Créer la playlist").size(14).font(SEMIBOLD))
                .padding(Padding::from([8, 18]))
                .style(style::pill(accent))
                .on_press_maybe(
                    (hits > 0 && !found.name.trim().is_empty())
                        .then_some(Message::Import(Imp::Create))
                ),
        ]
        .spacing(8),
    )
    .into()
}
