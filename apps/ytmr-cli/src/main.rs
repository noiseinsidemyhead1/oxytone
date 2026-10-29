//! Binaire headless de ytmr : debug et usage sans interface graphique.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast::error::RecvError;
use ytmr_catalog::Catalog;
use ytmr_core::{AppPaths, Config, Track};
use ytmr_player::{Dependencies, EndReason, MpvPlayer, Player, PlayerEvent};

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
    /// Joue le premier morceau trouvé.
    Play {
        #[arg(required = true)]
        query: Vec<String>,
    },
}

const HELP: &str = "commandes : [entrée] ou p = pause/reprise · s 1:30 = aller à · \
                    + / - = volume · v 50 = volume · i = état · q = quitter";

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::discover()?;
    let config = Config::load_or_create(&paths.config_file())
        .with_context(|| format!("chargement de {}", paths.config_file().display()))?;
    let _log = ytmr_core::logging::init(&config.log.filter, None);

    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async {
        match cli.command {
            Command::Doctor => doctor(&config).await,
            Command::Search { query } => search(&query.join(" ")).await,
            Command::Play { query } => play(&config, &query.join(" ")).await,
        }
    });
    // La lecture de stdin bloque un thread jusqu'à la prochaine ligne : sans
    // ça, quitter (mpv mort, fin du morceau) attendrait un appui sur Entrée.
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

async fn play(config: &Config, query: &str) -> Result<()> {
    // Vérifier les outils d'abord : inutile de chercher si on ne peut pas jouer.
    let deps = detect(config).await?;
    let catalog = Catalog::new_unauthenticated().await?;
    let track = catalog
        .search_songs(query)
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("aucun morceau trouvé pour « {query} »"))?;

    let player =
        MpvPlayer::spawn(&deps, config.player.volume, &config.player.ytdl_raw_options).await?;
    let mut events = player.subscribe();
    println!("♪ {}  [{}]", describe(&track), fmt_opt(track.duration));
    println!("{HELP}");
    player.load(&track.watch_url()).await?;

    let result = control_loop(&player, &mut events, config.player.volume).await;
    player.shutdown().await;
    result
}

#[derive(Default)]
struct Status {
    position: Duration,
    duration: Option<Duration>,
    paused: bool,
    volume: u8,
}

impl Status {
    fn line(&self) -> String {
        format!(
            "{} {} / {}  vol {}",
            if self.paused { "⏸" } else { "▶" },
            fmt(self.position),
            fmt_opt(self.duration),
            self.volume
        )
    }
}

async fn control_loop(
    player: &MpvPlayer,
    events: &mut tokio::sync::broadcast::Receiver<PlayerEvent>,
    volume: u8,
) -> Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_open = true;
    let mut status = Status {
        volume,
        ..Status::default()
    };

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            line = stdin.next_line(), if stdin_open => {
                // stdin fermé ou illisible : on continue à jouer jusqu'à la fin du morceau.
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
                if let Err(e) = handle_input(player, &mut status, line.trim()).await {
                    match e {
                        InputOutcome::Quit => return Ok(()),
                        InputOutcome::Error(msg) => println!("✗ {msg}"),
                    }
                }
            }
            event = events.recv() => match event {
                Ok(PlayerEvent::Position(p)) => status.position = p,
                Ok(PlayerEvent::Duration(d)) => status.duration = Some(d),
                Ok(PlayerEvent::Volume(v)) => status.volume = v,
                Ok(PlayerEvent::Paused(p)) => {
                    status.paused = p;
                    println!("{}", status.line());
                }
                Ok(PlayerEvent::Loaded) => println!("lecture lancée"),
                Ok(PlayerEvent::Ended(EndReason::Eof)) => {
                    println!("morceau terminé");
                    return Ok(());
                }
                Ok(PlayerEvent::Ended(EndReason::Error(e))) => bail!("lecture impossible : {e}"),
                Ok(PlayerEvent::Ended(EndReason::Stopped) | PlayerEvent::Idle) => {}
                Ok(PlayerEvent::Exited) => bail!("mpv s'est arrêté"),
                Err(RecvError::Lagged(n)) => tracing::debug!(n, "événements sautés"),
                Err(RecvError::Closed) => bail!("mpv s'est arrêté"),
            }
        }
    }
}

enum InputOutcome {
    Quit,
    Error(String),
}

async fn handle_input(
    player: &MpvPlayer,
    status: &mut Status,
    input: &str,
) -> Result<(), InputOutcome> {
    let err = |e: ytmr_player::PlayerError| InputOutcome::Error(e.to_string());
    let (cmd, arg) = input.split_once(' ').unwrap_or((input, ""));
    match cmd {
        "" | "p" => player.toggle_pause().await.map_err(err),
        "q" => Err(InputOutcome::Quit),
        "i" => {
            println!("{}", status.line());
            Ok(())
        }
        "+" | "-" => {
            let v = if cmd == "+" {
                status.volume.saturating_add(5).min(100)
            } else {
                status.volume.saturating_sub(5)
            };
            player.set_volume(v).await.map_err(err)?;
            // Sans attendre la confirmation de mpv, pour enchaîner « v 40 » puis « - ».
            status.volume = v;
            println!("vol {v}");
            Ok(())
        }
        "v" => {
            let v: u8 = arg
                .trim()
                .parse()
                .map_err(|_| InputOutcome::Error(format!("volume invalide : « {arg} »")))?;
            player.set_volume(v).await.map_err(err)?;
            status.volume = v;
            Ok(())
        }
        "s" => {
            let pos = ytmr_core::track::parse_duration(arg)
                .ok_or_else(|| InputOutcome::Error(format!("position invalide : « {arg} »")))?;
            player.seek(pos).await.map_err(err)?;
            println!("→ {}", fmt(pos));
            Ok(())
        }
        _ => Err(InputOutcome::Error(format!("commande inconnue. {HELP}"))),
    }
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
