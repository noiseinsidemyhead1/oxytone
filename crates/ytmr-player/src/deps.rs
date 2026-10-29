//! Détection des programmes externes : mpv, yt-dlp et un moteur JavaScript.
//!
//! En clair : avant de lancer quoi que ce soit, on vérifie que les outils sont
//! installés et on explique comment les installer s'ils manquent.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::PlayerError;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Tool {
    pub path: PathBuf,
    /// Première ligne de `--version`.
    pub version: String,
}

/// Moteur JS utilisé par yt-dlp pour résoudre les défis de YouTube.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsRuntime {
    /// Activé par défaut dans yt-dlp, rien à configurer.
    Deno,
    /// À activer explicitement (`--js-runtimes node`).
    Node,
}

#[derive(Debug, Clone)]
pub struct Dependencies {
    pub mpv: Tool,
    pub ytdlp: Tool,
    /// `None` : yt-dlp fonctionnera peut-être, mais beaucoup de morceaux échoueront.
    pub js_runtime: Option<JsRuntime>,
}

impl Dependencies {
    /// `mpv` et `ytdlp` : chemins explicites de la config, sinon recherche dans le PATH.
    pub async fn detect(mpv: Option<&Path>, ytdlp: Option<&Path>) -> Result<Self, PlayerError> {
        let mpv = probe(mpv.unwrap_or(Path::new("mpv")))
            .await
            .ok_or(PlayerError::MissingTool {
                name: "mpv",
                hint: install_hint("mpv"),
            })?;
        let ytdlp =
            probe(ytdlp.unwrap_or(Path::new("yt-dlp")))
                .await
                .ok_or(PlayerError::MissingTool {
                    name: "yt-dlp",
                    hint: install_hint("yt-dlp"),
                })?;

        let js_runtime = if probe(Path::new("deno")).await.is_some() {
            Some(JsRuntime::Deno)
        } else if probe(Path::new("node")).await.is_some() {
            Some(JsRuntime::Node)
        } else {
            tracing::warn!(
                "ni deno ni node trouvé : yt-dlp risque d'échouer sur YouTube (installer deno)"
            );
            None
        };

        Ok(Self {
            mpv,
            ytdlp,
            js_runtime,
        })
    }
}

/// Lance `<program> --version` ; `None` si absent ou en échec.
async fn probe(program: &Path) -> Option<Tool> {
    let output = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(VERSION_TIMEOUT, output)
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    Some(Tool {
        path: program.to_path_buf(),
        version,
    })
}

fn install_hint(tool: &str) -> String {
    if cfg!(windows) {
        let bucket = if tool == "mpv" {
            "scoop bucket add extras && "
        } else {
            ""
        };
        format!(
            "installer avec `{bucket}scoop install {tool}`, ou renseigner player.{}_path dans config.toml",
            tool.replace('-', "")
        )
    } else {
        format!(
            "installer avec `sudo pacman -S {tool}`, ou renseigner player.{}_path dans config.toml",
            tool.replace('-', "")
        )
    }
}
