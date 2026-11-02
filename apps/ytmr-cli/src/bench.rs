//! Mesure du démarrage à froid (`ytmr-cli bench-cold`).
//!
//! En clair : on prend des morceaux jamais résolus et, pour chacun, on refait
//! exactement ce que fait le moteur quand on clique sur un titre inconnu :
//! résolution rapide (`StartMode::Fast`, priorité urgente), puis `loadfile`
//! dans mpv, jusqu'à ce que le son sorte (`playback-restart`). Aucun cache
//! d'adresses ni cache audio : chaque morceau part de zéro. mpv joue à
//! volume 0. Rien n'est signalé à YouTube, le compte n'est que lu.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::broadcast::error::RecvError;
use ytmr_catalog::Catalog;
use ytmr_core::{AppPaths, Config};
use ytmr_player::resolve::itag_param;
use ytmr_player::session::SessionCookie;
use ytmr_player::worker;
use ytmr_player::{
    Dependencies, MediaSource, MpvPlayer, Player, PlayerEvent, Priority, Resolver, ResolverOptions,
    StartMode, YtDlpResolver,
};

/// Requêtes variées : le premier titre de chacune sert de morceau de test.
const QUERIES: &[&str] = &[
    "radiohead weird fishes",
    "stromae alors on danse",
    "kendrick lamar humble",
    "aphex twin xtal",
    "fleetwood mac dreams",
    "nujabes aruarian dance",
    "daft punk veridis quo",
    "billie eilish birds of a feather",
    "angele balance ton quoi",
    "tame impala let it happen",
    "frank ocean nights",
    "yoasobi idol",
    "massive attack teardrop",
    "christine and the queens tilted",
    "boards of canada roygbiv",
    "pnl au dd",
    "the weeknd blinding lights",
    "bjork joga",
    "justice genesis",
    "lorde ribs",
    "sade no ordinary love",
    "mf doom accordion",
    "air la femme d'argent",
    "beach house space song",
    "burial archangel",
    "phoenix 1901",
    "jamiroquai virtual insanity",
    "four tet baby",
    "orelsan la quête",
    "portishead roads",
];

const START_TIMEOUT: Duration = Duration::from_secs(40);

pub struct Options {
    pub account: bool,
    pub client: Option<String>,
    pub count: usize,
    pub skip: usize,
}

struct Sample {
    resolve: Duration,
    open: Option<Duration>,
    sound: Duration,
    total: Duration,
    itag: Option<u32>,
    fallback: bool,
}

pub async fn run(config: &Config, paths: &AppPaths, opts: Options) -> Result<()> {
    let deps = Dependencies::detect(
        config.player.mpv_path.as_deref(),
        config.player.ytdlp_path.as_deref(),
    )
    .await?;
    let session = if opts.account {
        Some(
            load_session(&paths.data_dir.join("youtube-session.bin"))
                .context("aucune session YouTube lisible (se connecter d'abord dans l'appli)")?,
        )
    } else {
        None
    };
    // Comme `ytmr_gui::backend` : pas de client rapide avec un compte.
    let fast_client = if session.is_some() {
        None
    } else {
        opts.client
            .clone()
            .or_else(|| config.player.fast_start_client().map(str::to_owned))
    };
    let ytdl_options = config.player.ytdl_options();
    let player = MpvPlayer::spawn(&deps, 0, fast_client.as_deref(), &ytdl_options).await?;
    let resolver = Arc::new(YtDlpResolver::new(
        &deps,
        ResolverOptions {
            fast_client: fast_client.clone(),
            cookies_from_browser: config.player.cookies_from_browser().map(str::to_owned),
            session,
            raw_options: config.player.ytdl_raw_options.clone(),
            cache_file: None,
            audio_cache: None,
        },
    ));
    // Le worker comme dans l'appli (branché bien avant le premier clic).
    if let Some(options) = resolver.worker_options() {
        let started = Instant::now();
        let w = worker::launch(&paths.cache_dir.join("ytdlp-env"), &options)
            .await
            .map_err(anyhow::Error::msg)?;
        println!("worker prêt en {} ms", started.elapsed().as_millis());
        resolver.attach_worker(w);
        // Détection Premium du worker, faite en fond au lancement.
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    let catalog = Catalog::new_unauthenticated().await?;
    let mut ids = Vec::new();
    for query in QUERIES.iter().skip(opts.skip) {
        if ids.len() >= opts.count {
            break;
        }
        match catalog.search_songs(query).await {
            Ok(tracks) => {
                if let Some(t) = tracks.into_iter().next() {
                    ids.push((t.video_id, format!("{} — {}", t.artist, t.title)));
                }
            }
            Err(e) => tracing::warn!(error = %e, query, "recherche"),
        }
    }

    println!(
        "compte : {}  client rapide : {}  morceaux : {}",
        if opts.account { "oui" } else { "non" },
        fast_client.as_deref().unwrap_or("(aucun)"),
        ids.len()
    );
    let mut samples = Vec::new();
    for (video_id, label) in &ids {
        match measure(&player, resolver.as_ref(), video_id).await {
            Ok(s) => {
                println!(
                    "{:<11}  résolution {:>5} ms  mpv ouverture {:>5}  mpv son {:>5} ms  total {:>5} ms  itag {:<4} {}{}",
                    video_id,
                    s.resolve.as_millis(),
                    s.open
                        .map(|d| format!("{} ms", d.as_millis()))
                        .unwrap_or_else(|| "?".into()),
                    s.sound.as_millis(),
                    s.total.as_millis(),
                    s.itag.map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
                    if s.fallback { "[repli mpv] " } else { "" },
                    label
                );
                samples.push(s);
            }
            Err(e) => println!("{video_id:<11}  échec : {e}  {label}"),
        }
        let _ = player.stop().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    println!();
    summary("résolution", samples.iter().map(|s| s.resolve).collect());
    summary(
        "mpv ouverture",
        samples.iter().filter_map(|s| s.open).collect(),
    );
    summary("mpv son", samples.iter().map(|s| s.sound).collect());
    summary("total", samples.iter().map(|s| s.total).collect());
    player.shutdown().await;
    Ok(())
}

async fn measure(player: &MpvPlayer, resolver: &YtDlpResolver, video_id: &str) -> Result<Sample> {
    let mut events = player.subscribe();
    let began = Instant::now();
    let (source, fallback) = match resolver
        .resolve(video_id, StartMode::Fast, Priority::Urgent)
        .await
    {
        Ok(source) => (source, false),
        Err(e) => {
            // Même repli que le moteur.
            tracing::warn!(error = %e, video_id, "résolution échouée, repli sur mpv");
            (
                MediaSource::via_mpv_hook(format!("https://music.youtube.com/watch?v={video_id}")),
                true,
            )
        }
    };
    let resolve = began.elapsed();
    let entry = player.load(&source).await?;
    let mut started = false;
    let mut open = None;
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .context("mpv ne démarre pas")?;
        match event {
            Ok(PlayerEvent::Started(e)) if e == entry => started = true,
            Ok(PlayerEvent::Loaded) if started => open = Some(began.elapsed() - resolve),
            Ok(PlayerEvent::Restarted) if started => break,
            Ok(PlayerEvent::Ended {
                entry: Some(e),
                reason,
            }) if e == entry => {
                anyhow::bail!("lecture terminée avant le son : {reason:?}")
            }
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => anyhow::bail!("mpv arrêté"),
        }
    }
    let total = began.elapsed();
    Ok(Sample {
        resolve,
        open,
        sound: total - resolve,
        total,
        itag: itag_param(&source.url),
        fallback,
    })
}

/// Médiane et p90 (rang le plus proche).
fn summary(name: &str, mut values: Vec<Duration>) {
    if values.is_empty() {
        println!("{name:<14} : aucune mesure");
        return;
    }
    values.sort();
    let rank = |p: f64| {
        let i = ((p * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1;
        values[i].as_millis()
    };
    let median = if values.len().is_multiple_of(2) {
        (values[values.len() / 2 - 1] + values[values.len() / 2]).as_millis() / 2
    } else {
        values[values.len() / 2].as_millis()
    };
    println!(
        "{name:<14} : médiane {median:>5} ms   p90 {:>5} ms   min {:>5}   max {:>5}   (n = {})",
        rank(0.9),
        values[0].as_millis(),
        values[values.len() - 1].as_millis(),
        values.len()
    );
}

/// Session enregistrée par « Se connecter » (même lecture que `ytmr_gui::backend`).
fn load_session(path: &std::path::Path) -> Option<Vec<SessionCookie>> {
    let sealed = std::fs::read(path).ok()?;
    let clear = ytmr_os::secret::unprotect(&sealed)
        .inspect_err(|e| tracing::warn!("session YouTube illisible : {e}"))
        .ok()?;
    serde_json::from_slice(&clear)
        .ok()
        .filter(|c: &Vec<SessionCookie>| ytmr_player::session::is_signed_in(c))
}
