//! Démarrage et arrêt de tout ce qui tourne derrière la fenêtre.
//!
//! En clair : on lance mpv, on se connecte au catalogue, on démarre le moteur
//! de lecture et, si la config le demande, le serveur local pour le deck CYD.
//! L'interface parle directement au moteur, sans passer par le réseau.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use ytmr_api::{AppState, Guard};
use ytmr_catalog::Catalog;
use ytmr_core::engine::{self, CoreOptions};
use ytmr_core::{AppPaths, Command, Config, CoreHandle};
use ytmr_media::Covers;
use ytmr_player::{Dependencies, MpvPlayer, YtDlpResolver};

pub struct Backend {
    pub core: CoreHandle,
    pub catalog: Arc<Catalog>,
    pub covers: Arc<Covers>,
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
            fast_client.map(str::to_owned),
            &ytdl_options,
        ));
        let covers = Arc::new(
            Covers::new(paths.cache_dir.join("covers"))
                .context("dossier de cache des jaquettes")?,
        );

        let (core, core_task) = engine::spawn(
            player,
            resolver,
            Arc::clone(&catalog),
            CoreOptions {
                volume: config.player.volume,
                autoplay: config.playback.autoplay,
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
