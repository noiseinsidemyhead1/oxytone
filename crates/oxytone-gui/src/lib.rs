//! Interface graphique native (iced) et assemblage de l'application.
//!
//! En clair : la fenêtre d'oxytone. Elle démarre tout de suite, puis lance le
//! backend (mpv, catalogue, moteur, API du deck) en tâche de fond ; l'écran de
//! démarrage laisse place à l'interface dès qu'il est prêt. Aucune logique
//! métier ici : l'interface envoie des commandes au moteur et affiche ce qu'il
//! publie, au même titre que l'API.

mod anim;
mod app;
mod backend;
mod chrome;
mod collection;
mod context;
mod icons;
mod import;
mod instance;
mod library;
mod releases;
mod settings;
mod smooth;
mod stats_page;
mod style;
mod views;

use std::sync::Arc;

use iced::{Size, window};
use oxytone_core::{AppPaths, Config};

pub use backend::Backend;

/// Une seule instance : `false` si oxytone tourne déjà (sa fenêtre a été ramenée).
/// `wait` : redémarrage, l'instance précédente est en train de se fermer.
pub fn claim_instance(wait: bool) -> bool {
    instance::claim(wait)
}

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
        // Barre de titre dessinée par oxytone (voir `chrome`).
        decorations: false,
        ..window::Settings::default()
    })
    // Pas d'anticrénelage MSAA : iced lisse déjà texte, formes et images, et
    // l'appli ne dessine aucun tracé vectoriel. Le MSAA ×4 coûtait une image
    // quatre fois plus grande (~60 Mo en 2560 × 1440, pris dans la RAM avec
    // une carte graphique intégrée).
    .antialiasing(false)
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
