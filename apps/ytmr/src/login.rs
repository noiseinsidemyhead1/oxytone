//! Fenêtre de connexion à YouTube (`ytmr --login`, Windows).
//!
//! En clair : une petite fenêtre affiche la page de connexion Google (moteur
//! web d'Edge, WebView2, déjà présent dans Windows 11). Dès que la connexion
//! aboutit sur music.youtube.com, ses cookies sont écrits sur la sortie
//! standard (une ligne JSON) pour l'instance de ytmr qui a ouvert la fenêtre,
//! puis la fenêtre se ferme. Navigation privée : rien n'est gardé par le
//! moteur web lui-même.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::{WebContext, WebView, WebViewBuilder};

const START: &str = "https://accounts.google.com/ServiceLogin?service=youtube&hl=fr&passive=true&continue=https%3A%2F%2Fmusic.youtube.com%2F";
/// Vérification de la connexion, deux fois par seconde.
const POLL: Duration = Duration::from_millis(500);

/// Ouvre la fenêtre ; ne rend jamais la main (le processus s'arrête à la
/// connexion, code 0, ou à la fermeture de la fenêtre, code 1).
pub fn run(data_dir: PathBuf) -> i32 {
    let event_loop = EventLoop::new();
    let window = match WindowBuilder::new()
        .with_title("Connexion à YouTube · ytmr")
        .with_inner_size(LogicalSize::new(480.0, 720.0))
        .build(&event_loop)
    {
        Ok(window) => window,
        Err(e) => {
            eprintln!("fenêtre de connexion : {e}");
            return 2;
        }
    };
    let mut context = WebContext::new(Some(data_dir));
    let webview = match WebViewBuilder::new_with_web_context(&mut context)
        .with_url(START)
        .with_incognito(true)
        .build(&window)
    {
        Ok(webview) => webview,
        Err(e) => {
            eprintln!("moteur web (WebView2) : {e}");
            return 2;
        }
    };
    event_loop.run(move |event, _, control_flow| {
        // Gardés vivants jusqu'à la fin.
        let _ = (&window, &context);
        *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL);
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => std::process::exit(1),
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(json) = signed_in(&webview) {
                    let mut out = std::io::stdout();
                    let _ = writeln!(out, "{json}");
                    let _ = out.flush();
                    std::process::exit(0);
                }
            }
            _ => {}
        }
    })
}

/// Cookies de la session, en JSON, une fois connecté sur music.youtube.com.
fn signed_in(webview: &WebView) -> Option<String> {
    let url = webview.url().ok()?;
    if !url.starts_with("https://music.youtube.com") {
        return None;
    }
    let mut cookies = webview.cookies_for_url("https://music.youtube.com/").ok()?;
    cookies.extend(
        webview
            .cookies_for_url("https://www.youtube.com/")
            .unwrap_or_default(),
    );
    let signed = cookies
        .iter()
        .any(|c| c.name() == "SAPISID" || c.name() == "__Secure-3PAPISID");
    if !signed {
        return None;
    }
    let list: Vec<serde_json::Value> = cookies
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name(),
                "value": c.value(),
                "domain": c.domain().map_or_else(|| ".youtube.com".to_owned(), |d| {
                    if d.starts_with('.') { d.to_owned() } else { format!(".{d}") }
                }),
                "path": c.path().unwrap_or("/"),
                "secure": c.secure().unwrap_or(true),
                "expires": c.expires_datetime().map(|t| t.unix_timestamp()),
            })
        })
        .collect();
    serde_json::to_string(&list).ok()
}
