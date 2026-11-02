//! Démarrage et arrêt de tout ce qui tourne derrière la fenêtre.
//!
//! En clair : on lance mpv, on se connecte au catalogue, on démarre le moteur
//! de lecture et, si la config le demande, le serveur local pour le deck CYD.
//! L'interface parle directement au moteur, sans passer par le réseau.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use ytmr_api::{AppState, Guard};
use ytmr_catalog::Catalog;
use ytmr_catalog::account::Account;
use ytmr_core::engine::{self, CoreOptions};
use ytmr_core::{AppPaths, Command, Config, CoreHandle};
use ytmr_media::Covers;
use ytmr_player::worker::{self, Worker};
use ytmr_player::{Dependencies, MpvPlayer, ResolverOptions, YtDlpResolver};

/// Connexion au compte YouTube (playlists, likes).
#[derive(Clone)]
pub enum AccountState {
    Connecting,
    Ready(Arc<Account>),
    Unavailable(String),
}

pub struct Backend {
    pub core: CoreHandle,
    pub catalog: Arc<Catalog>,
    pub covers: Arc<Covers>,
    pub account: watch::Receiver<AccountState>,
    /// Cache disque de la bibliothèque (listes de playlists, contenus).
    pub library_cache: std::path::PathBuf,
    resolver: Arc<YtDlpResolver>,
    core_task: JoinHandle<()>,
    server: Option<(JoinHandle<std::io::Result<()>>, oneshot::Sender<()>)>,
}

impl Backend {
    /// Résolveur yt-dlp (lecture de la session d'un navigateur).
    pub fn resolver(&self) -> Arc<YtDlpResolver> {
        Arc::clone(&self.resolver)
    }
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

impl Backend {
    pub async fn start(config: &Config, paths: &AppPaths) -> Result<Self> {
        let addr = SocketAddr::new(config.api.bind, config.api.port);
        // D'abord le port : s'il est pris, ytmr est sans doute déjà ouvert.
        let listener = if config.api.enabled {
            Some(TcpListener::bind(addr).await.with_context(|| {
                format!("le port {addr} est déjà utilisé (ytmr est peut-être déjà ouvert)")
            })?)
        } else {
            None
        };

        let deps = Dependencies::detect(
            config.player.mpv_path.as_deref(),
            config.player.ytdlp_path.as_deref(),
        )
        .await?;
        // Session capturée par « Se connecter » (prioritaire sur le navigateur).
        let session = load_session(&session_file(paths));
        // Avec un compte, pas de démarrage rapide : les clients rapides
        // n'acceptent pas les cookies (et perdraient la qualité Premium).
        let fast_client = config
            .player
            .fast_start_client()
            .filter(|_| session.is_none());
        let ytdl_options = config.player.ytdl_options();

        // Le catalogue se connecte à sa première utilisation : pas de réseau
        // requis pour ouvrir l'appli.
        let player = MpvPlayer::spawn(&deps, config.player.volume, fast_client, &ytdl_options)
            .await
            .context("démarrage de mpv")?;
        let catalog = Arc::new(Catalog::new());
        let resolver = Arc::new(YtDlpResolver::new(
            &deps,
            ResolverOptions {
                fast_client: fast_client.map(str::to_owned),
                cookies_from_browser: config.player.cookies_from_browser().map(str::to_owned),
                session,
                raw_options: config.player.ytdl_raw_options.clone(),
                cache_file: Some(paths.cache_dir.join("streams.json")),
                audio_cache: ytmr_player::AudioCache::new(
                    paths.cache_dir.join("audio"),
                    config.player.audio_cache_mb,
                ),
            },
        ));
        let (account_tx, account) = watch::channel(AccountState::Connecting);
        start_worker(&resolver, paths.cache_dir.join("ytdlp-env"), account_tx);
        let covers = Arc::new(
            Covers::new(paths.cache_dir.join("covers"))
                .context("dossier de cache des jaquettes")?,
        );

        let (core, core_task) = engine::spawn(
            player,
            Arc::clone(&resolver),
            Arc::clone(&catalog),
            CoreOptions {
                volume: config.player.volume,
                autoplay: config.playback.autoplay,
                session_file: Some(paths.data_dir.join("session.json")),
                stats_file: Some(paths.data_dir.join("plays.jsonl")),
            },
        );

        let guard = Guard {
            loopback_only: config.api.bind.is_loopback(),
            port: config.api.port,
            token: config
                .api
                .token_required()
                .then(|| config.api.token.clone())
                .flatten(),
        };
        let server = listener.map(|listener| {
            let state = AppState::new(
                core.clone(),
                Arc::clone(&catalog),
                Arc::clone(&covers),
                guard,
            );
            let (stop, stopped) = oneshot::channel();
            let task = tokio::spawn(ytmr_api::serve(listener, state, async {
                let _ = stopped.await;
            }));
            (task, stop)
        });

        Ok(Self {
            core,
            catalog,
            covers,
            account,
            library_cache: paths.cache_dir.join("library"),
            resolver: Arc::clone(&resolver),
            core_task,
            server,
        })
    }

    /// Nouvelle connexion au compte (session du navigateur relue) : sert à
    /// « se reconnecter », par exemple après avoir changé de compte dans Firefox.
    pub fn reconnect(&self) -> impl Future<Output = AccountState> + Send + 'static {
        let resolver = Arc::clone(&self.resolver);
        async move { connect_account(&resolver).await }
    }

    /// Coupe le son, arrête mpv puis le serveur.
    pub async fn shutdown(self) {
        if let Err(e) = self.core.send(Command::Shutdown).await {
            tracing::warn!(error = %e, "arrêt du moteur");
        }
        let _ = self.core_task.await;
        if let Some((task, stop)) = self.server {
            let _ = stop.send(());
            match task.await {
                Ok(Err(e)) => tracing::warn!(error = %e, "arrêt du serveur"),
                Err(e) => tracing::warn!(error = %e, "tâche du serveur"),
                Ok(Ok(())) => {}
            }
        }
    }
}

/// Fichier de la session YouTube (chiffrée par Windows).
pub fn session_file(paths: &AppPaths) -> std::path::PathBuf {
    paths.data_dir.join("youtube-session.bin")
}

/// Session enregistrée par « Se connecter » (`None` : aucune, ou illisible).
pub fn load_session(path: &std::path::Path) -> Option<Vec<ytmr_player::session::SessionCookie>> {
    let sealed = std::fs::read(path).ok()?;
    let clear = match ytmr_os::secret::unprotect(&sealed) {
        Ok(clear) => clear,
        Err(e) => {
            tracing::warn!("session YouTube illisible : {e}");
            return None;
        }
    };
    serde_json::from_slice(&clear)
        .ok()
        .filter(|c: &Vec<_>| ytmr_player::session::is_signed_in(c))
}

/// `ytmr --login` : la fenêtre de connexion, dont la sortie est la session.
pub fn login_command(exe: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--login")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Enregistre la session (chiffrée ; fichier réservé à l'utilisateur).
pub fn save_session(
    path: &std::path::Path,
    cookies: &[ytmr_player::session::SessionCookie],
) -> Result<(), String> {
    let clear = serde_json::to_vec(cookies).map_err(|e| e.to_string())?;
    let sealed = ytmr_os::secret::protect(&clear)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, sealed).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Installe (au besoin) puis démarre le yt-dlp permanent, en tâche de fond
/// (en attendant, le résolveur utilise l'exécutable), puis se connecte au
/// compte YouTube avec la session du navigateur.
fn start_worker(
    resolver: &Arc<YtDlpResolver>,
    env_dir: std::path::PathBuf,
    account: watch::Sender<AccountState>,
) {
    let Some(options) = resolver.worker_options() else {
        account.send_replace(AccountState::Unavailable(
            "options yt-dlp personnalisées : compte non géré".into(),
        ));
        return;
    };
    let resolver = Arc::clone(resolver);
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        match worker::launch(&env_dir, &options).await {
            Ok(w) => {
                let version = w.version().to_owned();
                resolver.attach_worker(w);
                tracing::info!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "worker yt-dlp branché"
                );
                update_worker(Arc::clone(&resolver), env_dir, options, version);
            }
            Err(e) => {
                tracing::warn!("worker yt-dlp indisponible : {e}");
                account.send_replace(AccountState::Unavailable(e));
                return;
            }
        }
        account.send_replace(connect_account(&resolver).await);
    });
}

/// Mise à jour quotidienne de yt-dlp en fond. Si une nouvelle version arrive,
/// un nouveau worker la charge et remplace l'ancien : les résolutions en
/// cours finissent sur l'ancien, qui s'arrête ensuite de lui-même (dernière
/// référence libérée). Un correctif de yt-dlp sert donc dès ce lancement.
fn update_worker(
    resolver: Arc<YtDlpResolver>,
    env_dir: std::path::PathBuf,
    options: serde_json::Value,
    running: String,
) {
    tokio::spawn(async move {
        match worker::update_if_due(&env_dir, &running).await {
            Ok(None) => {}
            Ok(Some(version)) => {
                match Worker::spawn(&worker::python_path(&env_dir), &options).await {
                    Ok(w) => {
                        resolver.attach_worker(w);
                        tracing::info!(%version, "worker yt-dlp relancé sur la nouvelle version");
                    }
                    Err(e) => tracing::warn!("relance du worker après mise à jour : {e}"),
                }
            }
            Err(e) => tracing::warn!("mise à jour de yt-dlp en fond : {e}"),
        }
    });
}

/// Lit la session YouTube du navigateur et s'y connecte. Un réseau qui se
/// réveille (sortie de veille, Wi-Fi qui se reconnecte) ne doit pas laisser le
/// compte « indisponible » : on réessaie pendant ~5 min en espaçant les essais
/// (l'interface affiche « Connexion… » pendant ce temps).
async fn connect_account(resolver: &YtDlpResolver) -> AccountState {
    const DELAYS_S: [u64; 11] = [0, 1, 3, 5, 10, 15, 20, 30, 45, 60, 90];
    let mut last = String::new();
    for delay in DELAYS_S {
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        let cookie = match resolver.browser_cookie().await {
            Ok(cookie) => cookie,
            Err(e) => {
                // Pas de session dans le navigateur : inutile de réessayer.
                tracing::warn!("compte indisponible : {e}");
                return AccountState::Unavailable(e);
            }
        };
        match Account::connect(&cookie).await {
            Ok(a) => {
                tracing::info!("compte YouTube connecté");
                return AccountState::Ready(Arc::new(a));
            }
            Err(e) => {
                // La cause exacte (DNS, connexion refusée, TLS…) est dans la chaîne.
                let mut cause = e.to_string();
                let mut source = std::error::Error::source(&e);
                while let Some(inner) = source {
                    cause.push_str(&format!(" ← {inner}"));
                    source = inner.source();
                }
                tracing::warn!(error = %cause, "connexion au compte, nouvel essai");
                last = format!("connexion au compte : {e}");
            }
        }
    }
    AccountState::Unavailable(last)
}
