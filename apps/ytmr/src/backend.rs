//! Démarrage et arrêt de tout ce qui tourne derrière la fenêtre.
//!
//! En clair : on lance mpv, on se connecte au catalogue, on démarre le moteur
//! de lecture et le serveur local qui sert l'interface et l'API. La fenêtre
//! n'a plus qu'à afficher l'adresse de ce serveur.

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
    pub url: String,
    core: CoreHandle,
    core_task: JoinHandle<()>,
    server_task: JoinHandle<std::io::Result<()>>,
    stop_server: oneshot::Sender<()>,
}

impl Backend {
    pub async fn start(config: &Config, paths: &AppPaths) -> Result<Self> {
        let addr = SocketAddr::new(config.api.bind, config.api.port);
        // D'abord le port : s'il est pris, ytmr est sans doute déjà ouvert.
        let listener = TcpListener::bind(addr).await.with_context(|| {
            format!("le port {addr} est déjà utilisé (ytmr est peut-être déjà ouvert)")
        })?;

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
        let state = AppState::new(core.clone(), catalog, covers, guard);
        let (stop_server, stopped) = oneshot::channel();
        let server_task = tokio::spawn(ytmr_api::serve(listener, state, async {
            let _ = stopped.await;
        }));

        // La fenêtre passe toujours par la boucle locale, même si l'API écoute sur le LAN.
        let url = format!("http://127.0.0.1:{}/", config.api.port);
        Ok(Self {
            url,
            core,
            core_task,
            server_task,
            stop_server,
        })
    }

    /// Coupe le son, arrête mpv puis le serveur.
    pub async fn shutdown(self) {
        if let Err(e) = self.core.send(Command::Shutdown).await {
            tracing::warn!(error = %e, "arrêt du moteur");
        }
        let _ = self.core_task.await;
        let _ = self.stop_server.send(());
        match self.server_task.await {
            Ok(Err(e)) => tracing::warn!(error = %e, "arrêt du serveur"),
            Err(e) => tracing::warn!(error = %e, "tâche du serveur"),
            Ok(Ok(())) => {}
        }
    }
}
