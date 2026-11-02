//! Détection des programmes externes : mpv, yt-dlp et un moteur JavaScript.
//!
//! En clair : avant de lancer quoi que ce soit, on vérifie que les outils sont
//! installés et on explique comment les installer s'ils manquent.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

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
        let mpv = find("mpv", mpv).await.ok_or(PlayerError::MissingTool {
            name: "mpv",
            hint: install_hint("mpv"),
        })?;
        let ytdlp = find("yt-dlp", ytdlp)
            .await
            .ok_or(PlayerError::MissingTool {
                name: "yt-dlp",
                hint: install_hint("yt-dlp"),
            })?;

        // node d'abord : mesuré un peu plus rapide que deno pour les défis de
        // YouTube (1,45 s contre 1,64 s par morceau, 2026-10-30). yt-dlp
        // n'active que deno de lui-même : node est alors demandé explicitement.
        let js_runtime = if find("node", None).await.is_some() {
            Some(JsRuntime::Node)
        } else if find("deno", None).await.is_some() {
            Some(JsRuntime::Deno)
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

/// Le programme `name` : chemin de la config, sinon le PATH, sinon les
/// emplacements habituels (dossiers de scoop, Program Files…). Le PATH hérité
/// peut être ancien (Windows : lancé par l'installateur ou l'Explorateur
/// ouvert avant l'installation de l'outil).
async fn find(name: &str, explicit: Option<&Path>) -> Option<Tool> {
    if let Some(path) = explicit {
        return probe(path).await;
    }
    if let Some(tool) = probe(Path::new(name)).await {
        return Some(tool);
    }
    for candidate in usual_places(name) {
        if candidate.exists()
            && let Some(tool) = probe(&candidate).await
        {
            tracing::info!(path = %candidate.display(), "{name} trouvé hors du PATH");
            return Some(tool);
        }
    }
    tracing::warn!(
        path = %std::env::var("PATH").unwrap_or_default(),
        "{name} introuvable (ni dans le PATH, ni aux emplacements habituels)"
    );
    None
}

/// Emplacements d'installation courants d'un outil.
fn usual_places(name: &str) -> Vec<PathBuf> {
    let env = |key: &str| std::env::var_os(key).map(PathBuf::from);
    let mut out = Vec::new();
    if cfg!(windows) {
        let exe = format!("{name}.exe");
        if let Some(home) = env("USERPROFILE") {
            out.push(home.join("scoop").join("shims").join(&exe));
            out.push(
                home.join("scoop")
                    .join("apps")
                    .join(name)
                    .join("current")
                    .join(&exe),
            );
        }
        for base in [
            env("ProgramFiles"),
            env("LOCALAPPDATA").map(|l| l.join("Programs")),
        ]
        .into_iter()
        .flatten()
        {
            out.push(base.join(name).join(&exe));
            out.push(base.join("nodejs").join(&exe));
        }
    } else {
        for dir in ["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
            out.push(Path::new(dir).join(name));
        }
        if let Some(home) = env("HOME") {
            out.push(home.join(".local").join("bin").join(name));
        }
    }
    out
}

/// Lance `<program> --version` ; `None` si absent ou en échec (raison au journal).
async fn probe(program: &Path) -> Option<Tool> {
    let output = crate::process::command(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(VERSION_TIMEOUT, output).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            // Absent du PATH : normal (on essaie ailleurs) ; sinon c'est à noter.
            if program.is_absolute() {
                tracing::warn!(program = %program.display(), error = %e, "outil non lancé");
            } else {
                tracing::debug!(program = %program.display(), error = %e, "outil non lancé");
            }
            return None;
        }
        Err(_) => {
            tracing::warn!(program = %program.display(), "outil trop lent à répondre (--version)");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(program = %program.display(), status = %output.status, "outil en échec (--version)");
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
