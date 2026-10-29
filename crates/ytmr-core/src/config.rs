//! Configuration persistante de ytmr.
//!
//! En clair : un fichier `config.toml` rangé au bon endroit selon l'OS
//! (`~/.config/ytmr` sous Linux, `%APPDATA%\ytmr\config` sous Windows).
//! S'il n'existe pas, on l'écrit avec les valeurs par défaut au premier lancement.

use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Nom du fichier de configuration dans [`AppPaths::config_dir`].
pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("impossible de déterminer le dossier personnel de l'utilisateur")]
    NoHomeDir,
    #[error("lecture/écriture de {path} : {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config invalide dans {path} : {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("sérialisation de la config : {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("valeur invalide : {0}")]
    Invalid(String),
}

/// Dossiers propres à l'application, résolus selon les conventions de l'OS.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, ConfigError> {
        let dirs = ProjectDirs::from("", "", "ytmr").ok_or(ConfigError::NoHomeDir)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE)
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub api: ApiConfig,
    pub player: PlayerConfig,
    pub playback: PlaybackConfig,
    pub log: LogConfig,
}

/// Serveur HTTP/WebSocket pour le deck CYD et les autres télécommandes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    pub enabled: bool,
    pub bind: IpAddr,
    pub port: u16,
    pub require_token: bool,
    /// Généré au premier lancement de l'API (phase 3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 26540,
            require_token: false,
            token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerConfig {
    /// Chemin explicite vers mpv ; sinon recherche dans le PATH.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpv_path: Option<PathBuf>,
    /// Chemin explicite vers yt-dlp ; sinon recherche dans le PATH.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ytdlp_path: Option<PathBuf>,
    /// Volume initial, 0 à 100.
    pub volume: u8,
    /// Options `clé=valeur` ajoutées à chaque appel de yt-dlp (ex. `force-ipv4=`).
    /// C'est ici qu'on s'adapte quand YouTube change ses règles, sans recompiler.
    pub ytdl_raw_options: Vec<String>,
    /// Client YouTube de yt-dlp pour un démarrage à froid : son immédiat mais
    /// qualité moindre (AAC ~96 kb/s). Vide (défaut) = toujours la meilleure
    /// qualité, quelques secondes d'attente au premier morceau seulement.
    /// Ignoré quand `cookies_from_browser` est renseigné.
    pub fast_start_client: String,
    /// Navigateur où YouTube est connecté (`firefox`, ou `firefox:<dossier du profil>`
    /// pour un dérivé comme Zen). Avec un compte Premium, yt-dlp obtient l'audio
    /// ~256 kb/s (Opus 774 / AAC 141). Les cookies sont lus à chaque résolution,
    /// jamais copiés. Chrome et Edge ne marchent pas sous Windows (cookies chiffrés).
    pub cookies_from_browser: String,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            mpv_path: None,
            ytdlp_path: None,
            volume: 70,
            ytdl_raw_options: Vec::new(),
            fast_start_client: String::new(),
            cookies_from_browser: String::new(),
        }
    }
}

impl PlayerConfig {
    pub fn cookies_from_browser(&self) -> Option<&str> {
        Some(self.cookies_from_browser.trim()).filter(|c| !c.is_empty())
    }

    /// Le mode rapide n'a pas de sens avec un compte : les clients rapides
    /// n'acceptent pas les cookies et perdraient la qualité Premium.
    pub fn fast_start_client(&self) -> Option<&str> {
        if self.cookies_from_browser().is_some() {
            return None;
        }
        Some(self.fast_start_client.trim()).filter(|c| !c.is_empty())
    }

    /// Options yt-dlp communes à la résolution et au repli de mpv.
    pub fn ytdl_options(&self) -> Vec<String> {
        let mut options = self.ytdl_raw_options.clone();
        if let Some(browser) = self.cookies_from_browser() {
            options.push(format!("cookies-from-browser={browser}"));
        }
        options
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackConfig {
    /// En fin de file, ajoute des morceaux similaires (radio YouTube Music).
    pub autoplay: bool,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self { autoplay: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// Filtre au format `tracing_subscriber::EnvFilter` (ex. `info,ytmr_player=debug`).
    /// La variable d'environnement `RUST_LOG` a priorité.
    pub filter: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: "info".to_owned(),
        }
    }
}

impl Config {
    /// Charge la config depuis `path`, ou l'écrit avec les valeurs par défaut si absente.
    pub fn load_or_create(path: &Path) -> Result<Self, ConfigError> {
        let config = match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                config.save(path)?;
                tracing::info!(path = %path.display(), "config par défaut créée");
                config
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let io_err = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_err)?;
        }
        fs::write(path, toml::to_string_pretty(self)?).map_err(io_err)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.player.volume > 100 {
            return Err(ConfigError::Invalid(format!(
                "player.volume = {} (attendu 0 à 100)",
                self.player.volume
            )));
        }
        if self.api.port == 0 {
            return Err(ConfigError::Invalid("api.port ne peut pas valoir 0".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_default_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(CONFIG_FILE);

        let config = Config::load_or_create(&path).unwrap();

        assert_eq!(config, Config::default());
        assert!(path.exists());
        assert_eq!(Config::load_or_create(&path).unwrap(), config);
    }

    #[test]
    fn partial_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        fs::write(&path, "[api]\nport = 9000\n").unwrap();

        let config = Config::load_or_create(&path).unwrap();

        assert_eq!(config.api.port, 9000);
        assert!(config.api.bind.is_loopback());
        assert_eq!(config.player, PlayerConfig::default());
    }

    #[test]
    fn rejects_out_of_range_volume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        fs::write(&path, "[player]\nvolume = 150\n").unwrap();

        assert!(matches!(
            Config::load_or_create(&path),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn cookies_disable_fast_start() {
        let mut player = PlayerConfig {
            fast_start_client: "android".into(),
            ..PlayerConfig::default()
        };
        assert_eq!(player.fast_start_client(), Some("android"));
        player.cookies_from_browser = "firefox".into();
        assert_eq!(player.fast_start_client(), None);
        assert_eq!(player.ytdl_options(), ["cookies-from-browser=firefox"]);
    }

    #[test]
    fn rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        fs::write(&path, "[api]\nprot = 1\n").unwrap();

        assert!(matches!(
            Config::load_or_create(&path),
            Err(ConfigError::Parse { .. })
        ));
    }
}
