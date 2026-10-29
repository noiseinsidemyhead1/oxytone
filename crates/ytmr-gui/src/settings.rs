//! Page Paramètres : égaliseur, volume homogène, cache audio.
//!
//! En clair : les réglages de son. Ils sont envoyés au moteur (qui les garde
//! dans la session et les passe à mpv) ; la page ne fait qu'afficher et
//! transmettre. Les curseurs bougent librement et le son n'est mis à jour
//! qu'au relâchement (reconfigurer les filtres de mpv à chaque pixel grésille).

use std::path::{Path, PathBuf};

use iced::widget::{Space, button, column, container, row, text, toggler, vertical_slider};
use iced::{Alignment, Length, Padding, Task};
use ytmr_core::Command;
use ytmr_player::AudioEffects;
use ytmr_player::effects::{EQ_BANDS, EQ_MAX_DB, EQ_PRESETS};

use crate::app::{App, Message};
use crate::style;
use crate::views::{BOLD, El, SEMIBOLD, muted, page_scroll, section_title};

#[derive(Debug, Clone)]
pub enum Settings {
    /// Curseur d'une bande déplacé (pas encore appliqué).
    Band(usize, i8),
    /// Curseur relâché : on applique.
    Release,
    Preset([i8; 10]),
    Normalize(bool),
    CacheSize(u64),
    ClearCache,
}

#[derive(Debug, Default)]
pub struct SettingsState {
    /// Gains affichés pendant un glissement, avant envoi au moteur.
    pub draft: Option<[i8; 10]>,
    pub audio_cache_bytes: Option<u64>,
    pub audio_dir: Option<PathBuf>,
}

impl App {
    pub fn update_settings(&mut self, message: Settings) -> Task<Message> {
        match message {
            Settings::Band(band, gain) => {
                let mut eq = self.settings.draft.unwrap_or(self.state.effects.eq);
                if let Some(slot) = eq.get_mut(band) {
                    *slot = gain;
                }
                self.settings.draft = Some(eq);
                Task::none()
            }
            Settings::Release => match self.settings.draft.take() {
                Some(eq) => self.apply_effects(AudioEffects {
                    eq,
                    ..self.state.effects
                }),
                None => Task::none(),
            },
            Settings::Preset(eq) => {
                self.settings.draft = None;
                self.apply_effects(AudioEffects {
                    eq,
                    ..self.state.effects
                })
            }
            Settings::Normalize(normalize) => self.apply_effects(AudioEffects {
                normalize,
                ..self.state.effects
            }),
            Settings::CacheSize(bytes) => {
                self.settings.audio_cache_bytes = Some(bytes);
                Task::none()
            }
            Settings::ClearCache => {
                let Some(dir) = self.settings.audio_dir.clone() else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            clear_audio(&dir);
                            dir_size(&dir)
                        })
                        .await
                        .unwrap_or(0)
                    },
                    |bytes| Message::Settings(Settings::CacheSize(bytes)),
                )
            }
        }
    }

    fn apply_effects(&mut self, effects: AudioEffects) -> Task<Message> {
        // Affiché tout de suite ; le moteur confirme par son état.
        self.state.effects = effects;
        self.send(Command::SetEffects(effects))
    }

    /// À l'ouverture de la page : place prise par le cache audio.
    pub fn open_settings(&mut self) -> Task<Message> {
        let Some(dir) = self.settings.audio_dir.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || dir_size(&dir))
                    .await
                    .unwrap_or(0)
            },
            |bytes| Message::Settings(Settings::CacheSize(bytes)),
        )
    }
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn clear_audio(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|e| e == "audio") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn card(content: El<'_>) -> El<'_> {
    container(content)
        .padding(20)
        .width(Length::Fill)
        .style(style::panel)
        .into()
}

fn band_label(freq: u32) -> String {
    if freq >= 1000 {
        format!("{}k", freq / 1000)
    } else {
        freq.to_string()
    }
}

pub fn settings_page(app: &App) -> El<'_> {
    let effects = app.state.effects;
    let eq = app.settings.draft.unwrap_or(effects.eq);
    let accent = app.page_accent;

    let bands = row(EQ_BANDS
        .iter()
        .zip(eq)
        .enumerate()
        .map(|(i, (&freq, gain))| {
            column![
                text(format!("{gain:+}"))
                    .size(12)
                    .font(SEMIBOLD)
                    .color(if gain == 0 { style::TEXT_FAINT } else { accent }),
                // i16 : le curseur d'iced exige `From<u8>`, que i8 n'a pas.
                vertical_slider(
                    i16::from(-EQ_MAX_DB)..=i16::from(EQ_MAX_DB),
                    i16::from(gain),
                    move |g| Message::Settings(Settings::Band(i, i8::try_from(g).unwrap_or(0))),
                )
                .on_release(Message::Settings(Settings::Release))
                .height(Length::Fixed(180.0)),
                text(band_label(freq)).size(12).color(style::TEXT_DIM),
            ]
            .spacing(8)
            .align_x(Alignment::Center)
            .width(Length::Fixed(44.0))
            .into()
        }))
    .spacing(6);

    let presets = row(EQ_PRESETS.iter().map(|(name, gains)| {
        let active = *gains == eq;
        button(text(*name).size(13).font(SEMIBOLD))
            .padding(Padding::from([7, 14]))
            .style(move |theme, status| {
                if active {
                    style::pill(accent)(theme, status)
                } else {
                    style::pill_ghost(theme, status)
                }
            })
            .on_press(Message::Settings(Settings::Preset(*gains)))
            .into()
    }))
    .spacing(8)
    .wrap();

    let normalize = row![
        column![
            text("Volume homogène").size(15).font(SEMIBOLD),
            muted("Rapproche le volume des morceaux entre eux (utile en radio ou en aléatoire)."),
        ]
        .spacing(4)
        .width(Length::Fill),
        toggler(effects.normalize)
            .on_toggle(|on| Message::Settings(Settings::Normalize(on)))
            .size(22),
    ]
    .spacing(16)
    .align_y(Alignment::Center);

    let cache_text = match app.settings.audio_cache_bytes {
        Some(bytes) => format!(
            "{:.0} Mo utilisés. Les morceaux déjà écoutés repartent sans attendre.",
            bytes as f64 / 1_048_576.0
        ),
        None => "Morceaux déjà écoutés gardés sur le disque.".to_owned(),
    };
    let cache = row![
        column![
            text("Cache audio").size(15).font(SEMIBOLD),
            muted(cache_text),
        ]
        .spacing(4)
        .width(Length::Fill),
        button(text("Vider").size(13).font(SEMIBOLD))
            .padding(Padding::from([7, 16]))
            .style(style::pill_ghost)
            .on_press(Message::Settings(Settings::ClearCache)),
    ]
    .spacing(16)
    .align_y(Alignment::Center);

    page_scroll(
        column![
            text("Paramètres").size(30).font(BOLD),
            section_title("Égaliseur"),
            card(
                column![presets, Space::new().height(Length::Fixed(8.0)), bands]
                    .spacing(12)
                    .into()
            ),
            section_title("Son"),
            card(normalize.into()),
            section_title("Stockage"),
            card(cache.into()),
        ]
        .spacing(16)
        .padding(Padding {
            top: 28.0,
            right: 28.0,
            bottom: 32.0,
            left: 28.0,
        }),
    )
}
