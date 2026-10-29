//! Interface graphique native (iced) et assemblage de l'application.
//!
//! En clair : la fenêtre de ytmr. Elle démarre tout de suite, puis lance le
//! backend (mpv, catalogue, moteur, API du deck) en tâche de fond ; l'écran de
//! démarrage laisse place à l'interface dès qu'il est prêt. Aucune logique
//! métier ici : l'interface envoie des commandes au moteur et affiche ce qu'il
//! publie, au même titre que l'API.

mod app;
mod backend;
mod chrome;
mod collection;
mod context;
mod icons;
mod library;
mod settings;
mod smooth;
mod style;
mod views;

use std::sync::Arc;

use iced::{Size, window};
use ytmr_core::{AppPaths, Config};

pub use backend::Backend;

pub fn run(config: Config, paths: AppPaths) -> iced::Result {
    let boot = Arc::new((config, paths));
    iced::application(
        move || app::App::boot(Arc::clone(&boot)),
        app::App::update,
        views::view,
    )
    .title(app::App::title)
    .theme(app::App::theme)
    .subscription(app::App::subscription)
    .window(window::Settings {
        size: Size::new(1320.0, 840.0),
        min_size: Some(Size::new(900.0, 580.0)),
        icon: app_icon(),
        // Fermer la fenêtre doit d'abord arrêter mpv proprement.
        exit_on_close_request: false,
        // Barre de titre dessinée par ytmr (voir `chrome`).
        decorations: false,
        ..window::Settings::default()
    })
    .antialiasing(true)
    .run()
}

/// Icône dessinée à la volée : un disque dégradé rose-violet percé au centre.
fn app_icon() -> Option<window::Icon> {
    const SIZE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let center = (SIZE as f32 - 1.0) / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - center, y as f32 - center);
            let d = (dx * dx + dy * dy).sqrt() / center;
            let t = y as f32 / SIZE as f32;
            let edge = ((1.0 - d) * center).clamp(0.0, 1.0);
            let hole = ((d - 0.2) * center).clamp(0.0, 1.0);
            let alpha = (edge * hole * 255.0) as u8;
            rgba.extend_from_slice(&[
                (255.0 - 60.0 * t) as u8,
                (60.0 + 10.0 * t) as u8,
                (110.0 + 110.0 * t) as u8,
                alpha,
            ]);
        }
    }
    window::icon::from_rgba(rgba, SIZE, SIZE).ok()
}
