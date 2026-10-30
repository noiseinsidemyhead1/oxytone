//! Page « Statistiques » : ce que tu écoutes, calculé en local.
//!
//! En clair : le moteur note chaque écoute dans `plays.jsonl` ; cette page le
//! relit, le résume pour la période choisie (temps d'écoute, artistes et
//! titres les plus écoutés, jours d'écoute) et l'affiche. Rien ne quitte la
//! machine.

use std::path::PathBuf;

use iced::widget::{Space, button, column, container, row, text};
use iced::{Alignment, Length, Padding, Task};
use ytmr_core::stats::{Play, Summary};

use crate::app::{App, Message, THUMB};
use crate::style;
use crate::views::{BOLD, El, SEMIBOLD, cover, ellipsize, muted, page_scroll, section_title};

/// Entrées des classements.
const TOP: usize = 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Period {
    Week,
    #[default]
    Month,
    Year,
    All,
}

impl Period {
    fn days(self) -> u32 {
        match self {
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
            Self::All => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Stats {
    Loaded(Vec<Play>),
    Period(Period),
}

#[derive(Debug, Default)]
pub struct StatsState {
    pub file: Option<PathBuf>,
    plays: Option<Vec<Play>>,
    pub period: Period,
    summary: Option<Summary>,
}

impl App {
    /// Ouverture de la page : relit le journal des écoutes (en tâche de fond).
    pub fn load_stats(&mut self) -> Task<Message> {
        let Some(path) = self.stats.file.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || ytmr_core::stats::load(&path))
                    .await
                    .unwrap_or_default()
            },
            |plays| Message::Stats(Stats::Loaded(plays)),
        )
    }

    pub fn update_stats(&mut self, message: Stats) -> Task<Message> {
        match message {
            Stats::Loaded(plays) => {
                self.stats.plays = Some(plays);
            }
            Stats::Period(period) => self.stats.period = period,
        }
        self.summarize_stats()
    }

    fn summarize_stats(&mut self) -> Task<Message> {
        let Some(plays) = &self.stats.plays else {
            return Task::none();
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let summary = ytmr_core::stats::summarize(
            plays,
            self.stats.period.days(),
            TOP,
            now,
            crate::views::utc_offset_secs(),
        );
        let tracks: Vec<ytmr_core::Track> = summary
            .top_tracks
            .iter()
            .map(|t| t.track.clone())
            .chain(summary.top_artists.iter().map(|a| a.sample.clone()))
            .collect();
        self.stats.summary = Some(summary);
        self.want_track_covers(&tracks, THUMB)
    }
}

/// « 12 h 34 min », « 45 min ».
fn duration_label(seconds: u64) -> String {
    let minutes = seconds / 60;
    match minutes / 60 {
        0 => format!("{minutes} min"),
        hours => format!("{hours} h {:02} min", minutes % 60),
    }
}

fn tile<'a>(value: String, label: &'a str, accent: iced::Color) -> El<'a> {
    container(
        column![
            text(value).size(26).font(BOLD).color(accent),
            text(label).size(12).color(style::TEXT_DIM),
        ]
        .spacing(4),
    )
    .padding(Padding::from([14, 18]))
    .width(Length::Fill)
    .style(style::panel)
    .into()
}

/// Barres du temps d'écoute (par jour, ou par semaine sur une longue période).
fn chart<'a>(per_day: &[u64], accent: iced::Color) -> El<'a> {
    const HEIGHT: f32 = 120.0;
    let buckets: Vec<u64> = if per_day.len() > 60 {
        per_day.chunks(7).map(|w| w.iter().sum()).collect()
    } else {
        per_day.to_vec()
    };
    let max = buckets.iter().copied().max().unwrap_or(0).max(1) as f32;
    let bars = row(buckets.iter().map(|&secs| {
        let h = (secs as f32 / max * HEIGHT).max(if secs > 0 { 3.0 } else { 1.0 });
        column![
            Space::new().height(Length::Fill),
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fixed(h))
                .style(move |_| iced::widget::container::Style {
                    background: Some(
                        if secs > 0 {
                            accent
                        } else {
                            style::alpha(iced::Color::WHITE, 0.08)
                        }
                        .into()
                    ),
                    border: iced::Border {
                        radius: 3.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        ]
        .width(Length::Fill)
        .height(Length::Fixed(HEIGHT))
        .into()
    }))
    .spacing(3);
    let unit = if per_day.len() > 60 {
        "par semaine"
    } else {
        "par jour"
    };
    column![
        bars,
        row![
            muted(format!("Temps d'écoute {unit}")),
            Space::new().width(Length::Fill),
            muted("aujourd'hui"),
        ],
    ]
    .spacing(8)
    .into()
}

pub fn stats_page(app: &App) -> El<'_> {
    let accent = app.accent;
    let pill = |label: &'static str, period: Period| -> El<'_> {
        let active = app.stats.period == period;
        button(text(label).size(13).font(SEMIBOLD))
            .padding(Padding::from([7, 14]))
            .style(move |theme, status| {
                if active {
                    style::pill(accent)(theme, status)
                } else {
                    style::pill_ghost(theme, status)
                }
            })
            .on_press(Message::Stats(Stats::Period(period)))
            .into()
    };
    let header = row![
        text("Statistiques").size(30).font(BOLD).width(Length::Fill),
        row![
            pill("7 jours", Period::Week),
            pill("30 jours", Period::Month),
            pill("12 mois", Period::Year),
            pill("Tout", Period::All),
        ]
        .spacing(6),
    ]
    .align_y(Alignment::Center);

    let body: El<'_> = match &app.stats.summary {
        None => muted("Chargement…"),
        Some(s) if s.plays == 0 => muted(
            "Aucune écoute sur cette période. Les statistiques se remplissent au fil de tes \
             écoutes (un morceau compte après 30 s).",
        ),
        Some(s) => {
            let tiles = row![
                tile(duration_label(s.seconds), "d'écoute", accent),
                tile(s.plays.to_string(), "écoutes", accent),
                tile(s.distinct_tracks.to_string(), "titres différents", accent),
                tile(s.distinct_artists.to_string(), "artistes", accent),
                tile(
                    format!("{} j", s.streak),
                    "d'affilée (jusqu'à aujourd'hui)",
                    accent
                ),
            ]
            .spacing(10);

            let mut artists = column![section_title("Artistes les plus écoutés")].spacing(4);
            for (rank, a) in s.top_artists.iter().enumerate() {
                let id = app.track_cover(&a.sample);
                artists = artists.push(
                    button(
                        row![
                            text(format!("{}", rank + 1))
                                .size(13)
                                .color(style::TEXT_FAINT)
                                .width(Length::Fixed(22.0)),
                            cover(app.cover(id.as_deref(), THUMB), 40.0, 20.0),
                            text(ellipsize(&a.name, 40).into_owned())
                                .size(14)
                                .font(SEMIBOLD)
                                .width(Length::Fill),
                            text(format!(
                                "{} écoutes · {}",
                                a.plays,
                                duration_label(a.seconds)
                            ))
                            .size(12)
                            .color(style::TEXT_DIM),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding(Padding::from([5, 10]))
                    .style(style::row(false, accent))
                    .on_press(Message::OpenArtist(a.artist_id.clone(), a.name.clone())),
                );
            }

            let mut tracks = column![section_title("Titres les plus écoutés")].spacing(4);
            for (rank, t) in s.top_tracks.iter().enumerate() {
                let id = app.track_cover(&t.track);
                tracks = tracks.push(
                    iced::widget::mouse_area(
                        container(
                            row![
                                text(format!("{}", rank + 1))
                                    .size(13)
                                    .color(style::TEXT_FAINT)
                                    .width(Length::Fixed(22.0)),
                                cover(app.cover(id.as_deref(), THUMB), 40.0, 6.0),
                                column![
                                    text(ellipsize(&t.track.title, 50).into_owned())
                                        .size(14)
                                        .font(SEMIBOLD),
                                    text(ellipsize(&t.track.artist, 60).into_owned())
                                        .size(12)
                                        .color(style::TEXT_DIM),
                                ]
                                .spacing(2)
                                .width(Length::Fill),
                                text(format!("{} écoutes", t.plays))
                                    .size(12)
                                    .color(style::TEXT_DIM),
                            ]
                            .spacing(12)
                            .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .padding(Padding::from([5, 10])),
                    )
                    // Double-clic : comme partout, lance le morceau.
                    .on_double_click(Message::Play(vec![t.track.clone()], 0))
                    .interaction(iced::mouse::Interaction::Pointer),
                );
            }

            column![
                tiles,
                container(chart(&s.per_day, accent))
                    .padding(18)
                    .style(style::panel),
                row![artists.width(Length::Fill), tracks.width(Length::Fill)].spacing(28),
            ]
            .spacing(24)
            .into()
        }
    };

    page_scroll(column![header, body].spacing(24).padding(Padding {
        top: 28.0,
        right: 28.0,
        bottom: 32.0,
        left: 28.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::duration_label;

    #[test]
    fn labels_durations() {
        assert_eq!(duration_label(59), "0 min");
        assert_eq!(duration_label(45 * 60), "45 min");
        assert_eq!(duration_label(3 * 3600 + 5 * 60), "3 h 05 min");
    }
}
