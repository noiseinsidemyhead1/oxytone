//! Binaire headless d'oxytone : debug et usage sans interface graphique.
//!
//! En clair : une télécommande en mode texte. Toute la logique (file, préchargement,
//! radio) est dans `oxytone-core` ; ici on ne fait que traduire le clavier en commandes.

mod bench;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use oxytone_catalog::Catalog;
use oxytone_core::engine::{self, CoreOptions};
use oxytone_core::{AppPaths, Command as Cmd, Config, CoreEvent, CoreHandle, Status, Track};
use oxytone_player::{Dependencies, MpvPlayer, ResolverOptions, StreamQuality, YtDlpResolver};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast::error::RecvError;

#[derive(Parser)]
#[command(version, about = "Client YouTube Music en ligne de commande")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Vérifie que mpv, yt-dlp et un moteur JS sont installés.
    Doctor,
    /// Cherche des morceaux.
    Search {
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Joue le premier morceau trouvé, puis enchaîne (radio si l'autoplay est actif).
    Play {
        /// Met tous les résultats de la recherche dans la file.
        #[arg(long)]
        all: bool,
        /// Désactive l'autoplay (radio) pour cette session.
        #[arg(long)]
        no_autoplay: bool,
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Mesure le démarrage à froid (morceaux jamais résolus, mpv à volume 0).
    BenchCold {
        /// Avec la session « Se connecter » de l'appli.
        #[arg(long)]
        account: bool,
        /// Client rapide sans compte (défaut : celui de la config).
        #[arg(long)]
        client: Option<String>,
        #[arg(long, default_value_t = 10)]
        count: usize,
        /// Saute les N premiers morceaux de la liste (pour en prendre d'autres).
        #[arg(long, default_value_t = 0)]
        skip: usize,
    },
}

const HELP: &str = "[entrée]/p pause · n suivant · b précédent · s 1:30 aller à · +/- ou v 50 volume\n\
                    z shuffle · r repeat · a autoplay · l file · i état · q quitter";

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::discover()?;
    let config = Config::load_or_create(&paths.config_file())
        .with_context(|| format!("chargement de {}", paths.config_file().display()))?;
    let _log = oxytone_core::logging::init(&config.log.filter, None);

    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async {
        match cli.command {
            Command::Doctor => doctor(&config).await,
            Command::Search { query } => search(&query.join(" ")).await,
            Command::Play {
                all,
                no_autoplay,
                query,
            } => play(&config, &query.join(" "), all, !no_autoplay).await,
            Command::BenchCold {
                account,
                client,
                count,
                skip,
            } => {
                bench::run(
                    &config,
                    &paths,
                    bench::Options {
                        account,
                        client,
                        count,
                        skip,
                    },
                )
                .await
            }
        }
    });
    // La lecture de stdin bloque un thread jusqu'à la prochaine ligne : sans
    // ça, quitter (mpv mort, fin de file) attendrait un appui sur Entrée.
    runtime.shutdown_background();
    result
}

async fn detect(config: &Config) -> Result<Dependencies> {
    Ok(Dependencies::detect(
        config.player.mpv_path.as_deref(),
        config.player.ytdlp_path.as_deref(),
    )
    .await?)
}

async fn doctor(config: &Config) -> Result<()> {
    let deps = detect(config).await?;
    println!(
        "mpv     : {} ({})",
        deps.mpv.version,
        deps.mpv.path.display()
    );
    println!(
        "yt-dlp  : {} ({})",
        deps.ytdlp.version,
        deps.ytdlp.path.display()
    );
    match deps.js_runtime {
        Some(rt) => println!("JS      : {rt:?}"),
        None => println!("JS      : aucun (installer deno conseillé)"),
    }
    Ok(())
}

async fn search(query: &str) -> Result<()> {
    let catalog = Catalog::new_unauthenticated().await?;
    let tracks = catalog.search_songs(query).await?;
    if tracks.is_empty() {
        println!("aucun résultat");
    }
    for (i, t) in tracks.iter().enumerate() {
        println!(
            "{:>2}. {}  [{}]  {}",
            i + 1,
            describe(t),
            fmt_opt(t.duration),
            t.video_id
        );
    }
    Ok(())
}

async fn play(config: &Config, query: &str, all: bool, autoplay: bool) -> Result<()> {
    // Vérifier les outils d'abord : inutile de chercher si on ne peut pas jouer.
    let deps = detect(config).await?;
    let fast_client = config.player.fast_start_client();
    let ytdl_options = config.player.ytdl_options();
    let (catalog, player) = tokio::try_join!(
        async { Ok::<_, anyhow::Error>(Catalog::new_unauthenticated().await?) },
        async {
            Ok(MpvPlayer::spawn(&deps, config.player.volume, fast_client, &ytdl_options).await?)
        },
    )?;
    let catalog = Arc::new(catalog);
    let resolver = Arc::new(YtDlpResolver::new(
        &deps,
        ResolverOptions {
            fast_client: fast_client.map(str::to_owned),
            cookies_from_browser: config.player.cookies_from_browser().map(str::to_owned),
            session: None,
            raw_options: config.player.ytdl_raw_options.clone(),
            cache_file: None,
            audio_cache: None,
        },
    ));

    let mut tracks = catalog.search_songs(query).await?;
    anyhow::ensure!(!tracks.is_empty(), "aucun morceau trouvé pour « {query} »");
    if !all {
        tracks.truncate(1);
    }

    let (core, task) = engine::spawn(
        player,
        resolver,
        Arc::clone(&catalog),
        CoreOptions {
            volume: config.player.volume,
            autoplay,
            // La CLI est un outil de test : elle ne touche pas à la session de l'appli.
            session_file: None,
            stats_file: None,
        },
    );
    println!("{HELP}");
    core.send(Cmd::PlayTracks { tracks, start: 0 }).await?;

    let result = control_loop(&core).await;
    core.send(Cmd::Shutdown).await.ok();
    task.await.ok();
    result
}

async fn control_loop(core: &CoreHandle) -> Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_open = true;
    let mut events = core.subscribe();
    let mut state = core.watch_state();
    let mut last = state.borrow().clone();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            line = stdin.next_line(), if stdin_open => {
                // stdin fermé ou illisible : on continue à jouer.
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        stdin_open = false;
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "entrée clavier illisible, commandes désactivées");
                        stdin_open = false;
                        continue;
                    }
                };
                match handle_input(core, line.trim()).await {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(e) => println!("✗ {e}"),
                }
            }
            event = events.recv() => match event {
                Ok(CoreEvent::TrackChanged(Some(entry))) => {
                    println!("♪ {}  [{}]", describe(&entry.track), fmt_opt(entry.track.duration));
                }
                Ok(CoreEvent::Error(message)) => println!("✗ {message}"),
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Ok(()),
            },
            changed = state.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let now = state.borrow_and_update().clone();
                if now.quality != last.quality && let Some(q) = now.quality {
                    println!("  qualité : {}", quality_label(q));
                }
                if now.next_ready && !last.next_ready {
                    println!("  suivant prêt (préchargé)");
                }
                if now.status != last.status && matches!(now.status, Status::Playing | Status::Paused) {
                    println!("{}", status_line(&now));
                }
                if now.status == Status::Idle
                    && matches!(last.status, Status::Playing | Status::Paused | Status::Loading)
                    && !now.autoplay
                    && now.current.is_some()
                {
                    println!("file terminée");
                    return Ok(());
                }
                last = now;
            }
        }
    }
}

/// Rend `true` pour quitter.
async fn handle_input(core: &CoreHandle, input: &str) -> Result<bool> {
    let (cmd, arg) = input.split_once(' ').unwrap_or((input, ""));
    let state = core.state();
    let command = match cmd {
        "" | "p" => Cmd::TogglePause,
        "n" => Cmd::Next,
        "b" => Cmd::Previous,
        "q" => return Ok(true),
        "z" => Cmd::SetShuffle(!state.shuffle),
        "r" => Cmd::SetRepeat(state.repeat.cycle()),
        "a" => Cmd::SetAutoplay(!state.autoplay),
        "+" => Cmd::SetVolume(state.volume.saturating_add(5).min(100)),
        "-" => Cmd::SetVolume(state.volume.saturating_sub(5)),
        "v" => Cmd::SetVolume(
            arg.trim()
                .parse()
                .with_context(|| format!("volume invalide : « {arg} »"))?,
        ),
        "s" => Cmd::Seek(
            oxytone_core::track::parse_duration(arg)
                .with_context(|| format!("position invalide : « {arg} »"))?,
        ),
        "i" => {
            println!("{}", status_line(&state));
            return Ok(false);
        }
        "l" => {
            print_queue(core);
            return Ok(false);
        }
        _ => anyhow::bail!("commande inconnue.\n{HELP}"),
    };
    let echo = match &command {
        Cmd::SetShuffle(on) => Some(format!("shuffle {}", on_off(*on))),
        Cmd::SetRepeat(r) => Some(format!("repeat {r:?}")),
        Cmd::SetAutoplay(on) => Some(format!("autoplay {}", on_off(*on))),
        Cmd::SetVolume(v) => Some(format!("vol {v}")),
        Cmd::Seek(p) => Some(format!("→ {}", fmt(*p))),
        _ => None,
    };
    core.send(command).await?;
    if let Some(echo) = echo {
        println!("{echo}");
    }
    Ok(false)
}

fn print_queue(core: &CoreHandle) {
    let queue = core.queue();
    let current = queue.current.unwrap_or(0);
    let from = current.saturating_sub(2);
    for (i, entry) in queue.entries.iter().enumerate().skip(from).take(12) {
        let marker = if Some(i) == queue.current { "▶" } else { " " };
        println!(
            "{marker} {:>3}. {}  [{}]",
            i + 1,
            describe(&entry.track),
            fmt_opt(entry.track.duration)
        );
    }
    println!("  ({} morceaux dans la file)", queue.entries.len());
}

fn status_line(s: &oxytone_core::PlaybackState) -> String {
    let icon = match s.status {
        Status::Playing => "▶",
        Status::Paused => "⏸",
        Status::Loading => "…",
        Status::Idle => "■",
    };
    format!(
        "{icon} {} / {}  vol {}  shuffle {}  repeat {:?}  autoplay {}",
        fmt(s.position),
        fmt_opt(s.duration),
        s.volume,
        on_off(s.shuffle),
        s.repeat,
        on_off(s.autoplay)
    )
}

fn quality_label(q: StreamQuality) -> &'static str {
    match q {
        StreamQuality::High => "haute (audio seul, meilleur format du compte)",
        StreamQuality::Low => "rapide (AAC ~96 kb/s, démarrage à froid)",
        StreamQuality::Unknown => "inconnue (résolue par mpv)",
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn describe(t: &Track) -> String {
    match &t.album {
        Some(album) => format!("{} — {} ({album})", t.artist, t.title),
        None => format!("{} — {}", t.artist, t.title),
    }
}

fn fmt(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_opt(d: Option<Duration>) -> String {
    d.map(fmt).unwrap_or_else(|| "?:??".to_owned())
}
