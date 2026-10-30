//! Page Paramètres : égaliseur, volume homogène, cache audio.
//!
//! En clair : les réglages de son. Ils sont envoyés au moteur (qui les garde
//! dans la session et les passe à mpv) ; la page ne fait qu'afficher et
//! transmettre. Les curseurs bougent librement et le son n'est mis à jour
//! qu'au relâchement (reconfigurer les filtres de mpv à chaque pixel grésille).

use std::path::{Path, PathBuf};

use iced::widget::{Space, button, column, container, pick_list, row, text, vertical_slider};
use iced::{Alignment, Color, Length, Padding, Task, color};
use ytmr_core::Command;
use ytmr_player::effects::{EQ_BANDS, EQ_MAX_DB, EQ_PRESETS};
use ytmr_player::{AudioEffects, QualityPreference};

use crate::anim::Key;
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
    Notifications(bool),
    Quality(QualityPreference),
    AudioDevice(String),
    OfflineMode(bool),
}

/// Sortie audio dans la liste déroulante (nom lisible, identifiant mpv).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceChoice {
    name: String,
    label: String,
}

impl std::fmt::Display for DeviceChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
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
            Settings::Quality(quality) => {
                // Affichée tout de suite ; le moteur confirme par son état.
                self.state.quality_pref = quality;
                self.send(Command::SetQuality(quality))
            }
            Settings::AudioDevice(name) => {
                self.state.audio_device = name.clone();
                self.send(Command::SetAudioDevice(name))
            }
            Settings::OfflineMode(on) => {
                self.lib.offline_mode = on;
                if on {
                    // Plus rien n'arrivera du réseau : la barre de chargement s'arrête.
                    self.pending_route = None;
                    self.loading_since = None;
                    self.lib.streaming = None;
                    self.lyrics
                        .retain(|_, l| matches!(l, crate::app::Load::Ready(_)));
                }
                self.save_ui();
                self.apply_offline_mode();
                let toast = self.show_toast(if on {
                    "Mode hors ligne : seulement ce qui est téléchargé".into()
                } else {
                    "Mode hors ligne désactivé".into()
                });
                let reload = if on {
                    Task::none()
                } else {
                    // Retour du réseau : compte et playlists se remettent à jour.
                    self.load_playlists()
                };
                Task::batch([
                    toast,
                    reload,
                    self.send(Command::SetOffline(on)),
                    self.scan_offline(),
                ])
            }
            Settings::Notifications(on) => {
                self.notify
                    .enabled
                    .store(on, std::sync::atomic::Ordering::Relaxed);
                self.save_ui();
                Task::none()
            }
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

/// Vide le cache audio, sauf les morceaux gardés hors ligne.
fn clear_audio(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let pinned: std::collections::HashSet<String> =
        std::fs::read_to_string(dir.join("pinned.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
    for entry in entries.flatten() {
        let path = entry.path();
        let kept = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|id| pinned.contains(id));
        if path.extension().is_some_and(|e| e == "audio") && !kept {
            let _ = std::fs::remove_file(path);
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

/// Interrupteur animé : la pastille glisse et les couleurs passent du gris à
/// la teinte du morceau (`level` : 0 = éteint, 1 = allumé, entre les deux
/// pendant le mouvement). Visible éteint : rail gris liseré, pastille claire.
fn switch<'a>(level: f32, accent: Color, message: Message) -> El<'a> {
    const W: f32 = 46.0;
    const KNOB: f32 = 18.0;
    const PAD: f32 = 4.0;
    // Marge égale tout autour : la pastille est centrée dans le rail (une
    // marge seulement à gauche et à droite la laissait collée en haut).
    const H: f32 = KNOB + 2.0 * PAD;
    let travel = W - KNOB - 2.0 * PAD;
    let rail = style::mix(color!(0x2c2c34), accent, level);
    let edge = style::mix(color!(0x5a5a64), accent, level);
    let knob = style::mix(style::TEXT_DIM, Color::WHITE, level);
    button(
        row![
            Space::new().width(Length::Fixed(travel * level)),
            container(Space::new())
                .width(Length::Fixed(KNOB))
                .height(Length::Fixed(KNOB))
                .style(move |_| container::Style {
                    background: Some(knob.into()),
                    border: iced::Border {
                        radius: (KNOB / 2.0).into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                }),
        ]
        .align_y(Alignment::Center),
    )
    .width(Length::Fixed(W))
    .height(Length::Fixed(H))
    .padding(PAD)
    .style(move |_, status| button::Style {
        background: Some(
            if matches!(status, button::Status::Hovered) {
                style::mix(rail, Color::WHITE, 0.08)
            } else {
                rail
            }
            .into(),
        ),
        border: iced::Border {
            radius: (H / 2.0).into(),
            width: 1.0,
            color: edge,
        },
        ..button::Style::default()
    })
    .on_press(message)
    .into()
}

pub fn settings_page(app: &App) -> El<'_> {
    let effects = app.state.effects;
    let eq = app.settings.draft.unwrap_or(effects.eq);
    let accent = app.accent;

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
                // Valeur affichée animée (un réglage tout prêt fait glisser les
                // curseurs) ; le gain envoyé reste un entier.
                vertical_slider(
                    f32::from(-EQ_MAX_DB)..=f32::from(EQ_MAX_DB),
                    app.anims.get(Key::Band(i), f32::from(gain)),
                    move |g: f32| Message::Settings(Settings::Band(i, g.round() as i8)),
                )
                .step(1.0_f32)
                .width(18.0)
                .style(style::eq_slider(accent))
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
            muted("Chaque morceau trop fort est baissé au niveau prévu par YouTube Music (un gain fixe par morceau, le son reste intact)."),
        ]
        .spacing(4)
        .width(Length::Fill),
        switch(
            app.anims.get(Key::Normalize, 0.0),
            accent,
            Message::Settings(Settings::Normalize(!effects.normalize)),
        ),
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
    // Qualité : trois choix, la description du choix courant en dessous.
    let current = app.state.quality_pref;
    let choice = |label: &'static str, value: QualityPreference| -> El<'_> {
        let active = current == value;
        button(text(label).size(13).font(SEMIBOLD))
            .padding(Padding::from([7, 14]))
            .style(move |theme, status| {
                if active {
                    style::pill(accent)(theme, status)
                } else {
                    style::pill_ghost(theme, status)
                }
            })
            .on_press(Message::Settings(Settings::Quality(value)))
            .into()
    };
    let quality = row![
        column![
            text("Qualité").size(15).font(SEMIBOLD),
            muted(match current {
                QualityPreference::Max => "La meilleure disponible (Opus ~256 kb/s avec Premium).",
                QualityPreference::High => "Jusqu'à ~160 kb/s.",
                QualityPreference::DataSaver => "Jusqu'à ~70 kb/s : économise les données mobiles.",
            }),
        ]
        .spacing(4)
        .width(Length::Fill),
        row![
            choice("Maximale", QualityPreference::Max),
            choice("Haute", QualityPreference::High),
            choice("Économe", QualityPreference::DataSaver),
        ]
        .spacing(6),
    ]
    .spacing(16)
    .align_y(Alignment::Center);

    // Sortie audio : la liste vient de mpv (mise à jour au branchement d'un casque…).
    let devices: Vec<DeviceChoice> = app
        .state
        .audio_devices
        .iter()
        .map(|d| DeviceChoice {
            name: d.name.clone(),
            label: if d.name == "auto" {
                "Sortie du système".to_owned()
            } else {
                d.description.clone()
            },
        })
        .collect();
    let wanted = if app.state.audio_device.is_empty() {
        "auto"
    } else {
        app.state.audio_device.as_str()
    };
    let selected = devices.iter().find(|d| d.name == wanted).cloned();
    let output = row![
        column![
            text("Sortie audio").size(15).font(SEMIBOLD),
            muted("Haut-parleurs, casque, écran… « Sortie du système » suit celle de Windows."),
        ]
        .spacing(4)
        .width(Length::Fill),
        pick_list(devices, selected, |d: DeviceChoice| {
            Message::Settings(Settings::AudioDevice(d.name))
        })
        .placeholder("Sortie du système")
        .text_size(13)
        .width(Length::Fixed(280.0)),
    ]
    .spacing(16)
    .align_y(Alignment::Center);

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
            card(column![normalize, quality, output].spacing(18).into()),
            section_title("Hors ligne"),
            card(
                row![
                    column![
                        text("Mode hors ligne").size(15).font(SEMIBOLD),
                        muted("ytmr n'accède plus à Internet : seuls les morceaux et playlists téléchargés sont affichés et lus."),
                    ]
                    .spacing(4)
                    .width(Length::Fill),
                    switch(
                        app.anims.get(Key::OfflineMode, 0.0),
                        accent,
                        Message::Settings(Settings::OfflineMode(!app.lib.offline_mode)),
                    ),
                ]
                .spacing(16)
                .align_y(Alignment::Center)
                .into()
            ),
            section_title("Notifications"),
            card(
                row![
                    column![
                        text("Nouveau morceau").size(15).font(SEMIBOLD),
                        muted("Une bulle du système quand un titre commence et que ytmr n'est pas au premier plan."),
                    ]
                    .spacing(4)
                    .width(Length::Fill),
                    switch(
                        app.anims.get(Key::Notifications, 0.0),
                        accent,
                        Message::Settings(Settings::Notifications(!app.notifications_on())),
                    ),
                ]
                .spacing(16)
                .align_y(Alignment::Center)
                .into()
            ),
            section_title("Stockage"),
            card(cache.into()),
            section_title("Compte"),
            card(
                row![
                    column![
                        text("Historique d'écoute").size(15).font(SEMIBOLD),
                        muted("Tous les titres écoutés récemment, à relancer d'un clic."),
                    ]
                    .spacing(4)
                    .width(Length::Fill),
                    button(text("Ouvrir").size(13).font(SEMIBOLD))
                        .padding(Padding::from([7, 16]))
                        .style(style::pill_ghost)
                        .on_press(Message::Navigate(crate::app::Route::History)),
                ]
                .spacing(16)
                .align_y(Alignment::Center)
                .into()
            ),
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
