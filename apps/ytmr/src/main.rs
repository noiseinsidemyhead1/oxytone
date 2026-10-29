//! Binaire principal de ytmr : une fenêtre native qui affiche l'interface web.
//!
//! En clair : on démarre le backend (mpv, moteur, serveur local), puis on ouvre
//! une fenêtre avec le moteur web du système (WebView2 sous Windows, WebKitGTK
//! sous Linux) pointée sur ce serveur. Fermer la fenêtre arrête tout proprement.

// En release, pas de console noire derrière la fenêtre sous Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;

use std::time::Instant;

use anyhow::{Context, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::{Icon, Theme, WindowBuilder};
use wry::WebViewBuilder;
use ytmr_core::{AppPaths, Config};

use backend::Backend;

/// Fond de la fenêtre avant que la page s'affiche (évite un flash blanc).
const BACKGROUND: (u8, u8, u8, u8) = (11, 11, 14, 255);

fn main() -> Result<()> {
    let started = Instant::now();
    let paths = AppPaths::discover()?;
    let config_file = paths.config_file();
    let mut config = Config::load_or_create(&config_file)
        .with_context(|| format!("chargement de {}", config_file.display()))?;
    let _log = ytmr_core::logging::init(&config.log.filter, Some(&paths.log_dir()));
    if config.api.ensure_token()? {
        config.save(&config_file)?;
        tracing::info!("token d'API généré dans la config (écoute réseau)");
    }

    let runtime = tokio::runtime::Runtime::new()?;
    let backend = runtime.block_on(Backend::start(&config, &paths));
    let page = match &backend {
        Ok(backend) => Page::Url(backend.url.clone()),
        Err(e) => {
            tracing::error!("démarrage impossible : {e:#}");
            Page::Error(format!("{e:#}"))
        }
    };
    tracing::info!(elapsed_ms = started.elapsed().as_millis(), "backend prêt");

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("ytmr")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(720.0, 520.0))
        .with_theme(Some(Theme::Dark))
        .with_window_icon(app_icon())
        .build(&event_loop)
        .context("création de la fenêtre")?;

    let origin = page.origin();
    let builder = WebViewBuilder::new()
        .with_background_color(BACKGROUND)
        .with_devtools(cfg!(debug_assertions))
        .with_autoplay(true)
        // L'interface ne quitte jamais le serveur local.
        .with_navigation_handler(move |url| origin.as_ref().is_none_or(|o| url.starts_with(o)))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny);
    let builder = match &page {
        Page::Url(url) => builder.with_url(url),
        Page::Error(message) => builder.with_html(error_page(message)),
    };

    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window)?;
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .context("conteneur GTK de la fenêtre")?;
        builder.build_gtk(vbox)?
    };
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis(),
        "fenêtre ouverte"
    );

    let mut backend = backend.ok();
    let mut webview = Some(webview);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            // La page d'abord, pour que la fenêtre disparaisse sans attendre mpv.
            webview.take();
            if let Some(backend) = backend.take() {
                runtime.block_on(backend.shutdown());
            }
            *control_flow = ControlFlow::Exit;
        }
    });
}

enum Page {
    Url(String),
    Error(String),
}

impl Page {
    fn origin(&self) -> Option<String> {
        match self {
            Page::Url(url) => Some(url.trim_end_matches('/').to_owned()),
            Page::Error(_) => None,
        }
    }
}

fn error_page(message: &str) -> String {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>ytmr</title>\
         <body style=\"margin:0;height:100vh;display:grid;place-items:center;\
         background:#0b0b0e;color:#e8e8ea;font:15px/1.5 system-ui,sans-serif\">\
         <div style=\"max-width:560px;padding:32px\">\
         <h1 style=\"font-size:22px;margin:0 0 12px\">ytmr n'a pas pu démarrer</h1>\
         <pre style=\"white-space:pre-wrap;background:#18181d;padding:16px;border-radius:10px;\
         color:#ff9b9b\">{escaped}</pre>\
         <p style=\"color:#9a9aa3\">Corrige le problème puis relance ytmr. \
         Le détail est aussi dans le fichier de log.</p></div>"
    )
}

/// Icône dessinée à la volée : un disque dégradé rouge-violet avec un trou
/// central, comme un vinyle.
fn app_icon() -> Option<Icon> {
    const SIZE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let center = (SIZE as f32 - 1.0) / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - center, y as f32 - center);
            let d = (dx * dx + dy * dy).sqrt() / center;
            let t = y as f32 / SIZE as f32;
            let alpha = if (0.18..=1.0).contains(&d) {
                ((1.0 - d) * center).clamp(0.0, 1.0) * 255.0
            } else {
                0.0
            };
            rgba.extend_from_slice(&[
                (255.0 - 60.0 * t) as u8,
                (40.0 + 20.0 * t) as u8,
                (90.0 + 120.0 * t) as u8,
                alpha as u8,
            ]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}
