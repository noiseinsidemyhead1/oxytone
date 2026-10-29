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
    core_task: JoinHandle<()>,
    server: Option<(JoinHandle<std::io::Result<()>>, oneshot::Sender<()>)>,
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
        let fast_client = config.player.fast_start_client();
        let ytdl_options = config.player.ytdl_options();

        let (catalog, player) = tokio::try_join!(
            async {
                Catalog::new_unauthenticated()
                    .await
                    .context("connexion à YouTube Music")
            },
            async {
                MpvPlayer::spawn(&deps, config.player.volume, fast_client, &ytdl_options)
                    .await
                    .context("démarrage de mpv")
            },
        )?;
        let catalog = Arc::new(catalog);
        let resolver = Arc::new(YtDlpResolver::new(
            &deps,
            ResolverOptions {
                fast_client: fast_client.map(str::to_owned),
                cookies_from_browser: config.player.cookies_from_browser().map(str::to_owned),
                raw_options: config.player.ytdl_raw_options.clone(),
                cache_file: Some(paths.cache_dir.join("streams.json")),
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
            core_task,
            server,
        })
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
        let worker = match worker::ensure_env(&env_dir).await {
            Ok(python) => Worker::spawn(&python, &options).await,
            Err(e) => Err(e),
        };
        match worker {
            Ok(w) => {
                resolver.attach_worker(w);
                tracing::info!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "worker yt-dlp branché"
                );
            }
            Err(e) => {
                tracing::warn!("worker yt-dlp indisponible : {e}");
                account.send_replace(AccountState::Unavailable(e));
                return;
            }
        }
        let state = match resolver.browser_cookie().await {
            Ok(cookie) => match Account::connect(&cookie).await {
                Ok(a) => {
                    tracing::info!("compte YouTube connecté");
                    AccountState::Ready(Arc::new(a))
                }
                Err(e) => AccountState::Unavailable(format!("connexion au compte : {e}")),
            },
            Err(e) => AccountState::Unavailable(e),
        };
        if let AccountState::Unavailable(e) = &state {
            tracing::warn!("compte indisponible : {e}");
        }
        account.send_replace(state);
    });
}
