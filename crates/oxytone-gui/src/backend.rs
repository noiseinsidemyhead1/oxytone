//! Démarrage et arrêt de tout ce qui tourne derrière la fenêtre.
//!
//! En clair : on lance mpv, on se connecte au catalogue, on démarre le moteur
//! de lecture et, si la config le demande, le serveur local pour le deck CYD.
//! L'interface parle directement au moteur, sans passer par le réseau.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use oxytone_api::{AppState, Guard};
use oxytone_catalog::Catalog;
use oxytone_catalog::account::Account;
use oxytone_core::engine::{self, CoreOptions};
use oxytone_core::{AppPaths, Command, Config, CoreHandle};
use oxytone_media::Covers;
use oxytone_player::worker::{self, Worker};
use oxytone_player::{Dependencies, MpvPlayer, ResolverOptions, YtDlpResolver};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

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
        // D'abord le port : s'il est pris, oxytone est sans doute déjà ouvert.
        let listener = if config.api.enabled {
            Some(TcpListener::bind(addr).await.with_context(|| {
                format!("le port {addr} est déjà utilisé (oxytone est peut-être déjà ouvert)")
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
                audio_cache: oxytone_player::AudioCache::new(
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
            let task = tokio::spawn(oxytone_api::serve(listener, state, async {
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
pub fn load_session(path: &std::path::Path) -> Option<Vec<oxytone_player::session::SessionCookie>> {
    let sealed = std::fs::read(path).ok()?;
    let clear = match oxytone_os::secret::unprotect(&sealed) {
        Ok(clear) => clear,
        Err(e) => {
            tracing::warn!("session YouTube illisible : {e}");
            return None;
        }
    };
    serde_json::from_slice(&clear)
        .ok()
        .filter(|c: &Vec<_>| oxytone_player::session::is_signed_in(c))
}

/// `oxytone --login` : la fenêtre de connexion, dont la sortie est la session.
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
    cookies: &[oxytone_player::session::SessionCookie],
) -> Result<(), String> {
    let clear = serde_json::to_vec(cookies).map_err(|e| e.to_string())?;
    let sealed = oxytone_os::secret::protect(&clear)?;
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

#[cfg(test)]
mod migration_tests {
    use std::path::{Path, PathBuf};

    use oxytone_core::AppPaths;
    use oxytone_core::migrate::{Outcome, migrate, pending};

    fn layout(root: &Path, name: &str) -> AppPaths {
        AppPaths {
            config_dir: root.join("Roaming").join(name).join("config"),
            data_dir: root.join("Roaming").join(name).join("data"),
            cache_dir: root.join("Local").join(name).join("cache"),
        }
    }

    /// Nombre de fichiers et octets sous les trois dossiers.
    fn inventory(paths: &AppPaths) -> (u64, u64) {
        fn walk(dir: &Path, acc: &mut (u64, u64)) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, acc);
                } else if let Ok(meta) = entry.metadata() {
                    acc.0 += 1;
                    acc.1 += meta.len();
                }
            }
        }
        let mut acc = (0, 0);
        for dir in [&paths.config_dir, &paths.data_dir, &paths.cache_dir] {
            walk(dir, &mut acc);
        }
        acc
    }

    /// Migration sur une **copie** des vrais dossiers, jamais sur les vrais :
    /// `OXYTONE_TEST_MIGRATION=<dossier>` qui contient `Roaming\ytmr\{config,data}`
    /// et `Local\ytmr\cache` (copiés avec robocopy).
    #[tokio::test]
    #[ignore = "copie des vraies données dans OXYTONE_TEST_MIGRATION"]
    async fn migrates_a_copy_of_real_data() {
        let root = PathBuf::from(
            oxytone_core::env::var("TEST_MIGRATION").expect("OXYTONE_TEST_MIGRATION"),
        );
        let (old, new) = (layout(&root, "ytmr"), layout(&root, "oxytone"));
        assert!(pending(&old, &new), "rien à migrer dans {}", root.display());
        let before = inventory(&old);
        println!("avant : {} fichiers, {} Mo", before.0, before.1 >> 20);

        // 1. ytmr ouvert : rien ne bouge.
        let m = migrate(&old, &new, true);
        assert_eq!(m.outcome, Outcome::Blocked);
        assert_eq!(inventory(&old), before);
        assert!(!root.join("Roaming").join("oxytone").exists());
        println!("1. ytmr ouvert : {:?}, rien déplacé", m.outcome);

        // 2. Échec en cours de route : un fichier des données ouvert sans
        // partage (comme un programme qui le tient) empêche son déplacement.
        let locked = old.data_dir.join("plays.jsonl");
        #[cfg(windows)]
        let handle = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&locked)
                .expect("verrou")
        };
        let m = migrate(&old, &new, false);
        #[cfg(windows)]
        {
            assert_eq!(m.outcome, Outcome::Partial, "{:?}", m.steps);
            assert_eq!(m.paths.data_dir, old.data_dir);
            assert!(locked.exists());
            // La session reste lisible là où elle est.
            assert!(super::load_session(&m.paths.data_dir.join("youtube-session.bin")).is_some());
            drop(handle);
        }
        for s in &m.steps {
            println!("2. {} {}", if s.ok { "ok " } else { "!! " }, s.message);
        }

        // 3. Lancement suivant : le reste est déplacé, les dossiers d'oxytone
        // déjà présents sont gardés.
        let m = migrate(&old, &new, false);
        for s in &m.steps {
            println!("3. {} {}", if s.ok { "ok " } else { "!! " }, s.message);
        }
        assert_eq!(m.paths.data_dir, new.data_dir);
        assert!(!pending(&old, &new));
        assert_eq!(migrate(&old, &new, false).outcome, Outcome::Nothing);

        // Rien de perdu : mêmes fichiers, mêmes octets.
        let after = inventory(&new);
        println!("après : {} fichiers, {} Mo", after.0, after.1 >> 20);
        assert_eq!(after, before);
        assert!(!root.join("Roaming").join("ytmr").exists());
        assert!(!root.join("Local").join("ytmr").exists());

        // Session (DPAPI : liée à l'utilisateur Windows, pas au chemin).
        let session = super::load_session(&new.data_dir.join("youtube-session.bin"));
        println!(
            "session déchiffrée après déplacement : {}",
            session.as_ref().map_or(0, Vec::len)
        );
        assert!(session.is_some());
        for file in ["session.json", "plays.jsonl"] {
            assert!(new.data_dir.join(file).exists(), "{file}");
        }
        assert!(new.cache_dir.join("streams.json").exists());

        // Worker : l'environnement Python déplacé démarre (ou est recréé).
        let started = std::time::Instant::now();
        let options =
            serde_json::json!({ "quiet": true, "no_warnings": true, "skip_download": true });
        let worker = oxytone_player::worker::launch(&new.cache_dir.join("ytdlp-env"), &options)
            .await
            .expect("worker");
        println!(
            "worker prêt en {} ms (yt-dlp {}) depuis l'environnement déplacé",
            started.elapsed().as_millis(),
            worker.version()
        );
    }
}
