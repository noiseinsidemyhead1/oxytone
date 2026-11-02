//! yt-dlp gardé en mémoire dans un processus Python permanent.
//!
//! En clair : lancer `yt-dlp.exe` pour chaque morceau coûte cher (l'exécutable
//! se décompresse, relit les cookies du navigateur, refait ses connexions).
//! Ici on démarre une fois un petit programme Python qui importe la
//! bibliothèque yt-dlp et attend des demandes. Mesuré le 2026-10-29 (Premium,
//! client `web_music`) : ~2,8 s par morceau contre ~5,3 s avec l'exécutable.
//!
//! yt-dlp s'installe tout seul dans un environnement Python privé (dossier de
//! cache de ytmr), sans rien toucher au Python du système. Sans Python, ytmr
//! continue simplement avec l'exécutable.
//!
//! Au lancement ([`launch`]), le worker démarre tout de suite avec
//! l'environnement existant (~0,5 s) ; la mise à jour quotidienne de yt-dlp
//! ([`update_if_due`]) se fait ensuite en fond. Avant, elle passait d'abord :
//! ~1 s de plus chaque jour, 11 à 14 s les jours de nouvelle version (mesuré
//! le 2026-11-02).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::oneshot;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(40);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// yt-dlp est mis à jour au plus une fois par jour (YouTube change souvent).
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Fils de résolution dans le processus (2 urgents + 2 de fond côté Rust).
const THREADS: usize = 4;

/// Programme Python exécuté par le processus permanent.
const SCRIPT: &str = r#"
import json, sys, threading
from concurrent.futures import ThreadPoolExecutor
import yt_dlp, yt_dlp.version

opts = json.loads(sys.argv[1])
if "cookiesfrombrowser" in opts:
    opts["cookiesfrombrowser"] = tuple(opts["cookiesfrombrowser"])
# Session capturée par la fenêtre de connexion de ytmr (cookies déjà lus).
session = opts.pop("ytmr_cookies", None) or []
has_account = "cookiesfrombrowser" in opts or bool(session)
local = threading.local()
out = threading.Lock()

# Premium détecté une fois pour toutes. yt-dlp lit la page « initial data »
# (~1 s par morceau) seulement pour savoir si le compte est Premium ; la
# réponse ne change pas d'un morceau à l'autre. Une fois connue, on la
# redonne et la requête est sautée. Si yt-dlp change ces noms, rien n'est
# modifié (même comportement qu'avant, juste plus lent).
premium = {"known": None}
try:
    from yt_dlp.extractor.youtube._video import YoutubeIE as _IE
    _orig_premium = _IE._is_premium_subscriber
    _orig_initial = _IE._download_initial_data

    def _is_premium(self, initial_data):
        if initial_data is None and premium["known"] is not None:
            return premium["known"]
        result = _orig_premium(self, initial_data)
        if initial_data is not None and self.is_authenticated:
            premium["known"] = result
        return result

    def _initial_data(self, *args, **kwargs):
        if premium["known"] is not None:
            return None
        return _orig_initial(self, *args, **kwargs)

    _IE._is_premium_subscriber = _is_premium
    _IE._download_initial_data = _initial_data
except Exception:
    pass

def ydl():
    if not hasattr(local, "ydl"):
        y = yt_dlp.YoutubeDL(opts)
        if session:
            import http.cookiejar
            for c in session:
                domain = c["domain"]
                y.cookiejar.set_cookie(http.cookiejar.Cookie(
                    0, c["name"], c["value"], None, False,
                    domain, True, domain.startswith("."),
                    c.get("path") or "/", True, bool(c.get("secure")),
                    c.get("expires"), False, None, None, {},
                ))
        local.ydl = y
    return local.ydl

def emit(obj):
    with out:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()

def browser_cookie(spec):
    from yt_dlp.cookies import extract_cookies_from_browser
    name, _, profile = spec.partition(":")
    jar = extract_cookies_from_browser(name, profile or None)
    values = {}
    for c in jar:
        domain = c.domain.lstrip(".")
        if domain == "youtube.com" or domain.endswith(".youtube.com"):
            values[c.name] = c.value
    return "; ".join(f"{k}={v}" for k, v in values.items())

def work(req):
    try:
        if req.get("op") == "warm":
            ydl()
            return
        if req.get("op") == "premium":
            # Au lancement, en tâche de fond : le premier vrai morceau profite
            # déjà de la détection Premium.
            if has_account and premium["known"] is None:
                ydl().extract_info(
                    "https://music.youtube.com/watch?v=dQw4w9WgXcQ", download=False
                )
            return
        if req.get("op") == "cookie":
            emit({"id": req["id"], "ok": {"cookie": browser_cookie(req["browser"])}})
            return
        y = ydl()
        fmt = req.get("format")
        # yt-dlp fige son sélecteur de format à la création : on le refait
        # quand la qualité demandée change (Paramètres).
        if fmt and getattr(local, "fmt", None) != fmt:
            y.params["format"] = fmt
            y.format_selector = y.build_format_selector(fmt)
            local.fmt = fmt
        info = y.extract_info(req["url"], download=False)
        emit({"id": req["id"], "ok": {
            "url": info.get("url"),
            "vcodec": info.get("vcodec"),
            "available_at": info.get("available_at"),
            "http_headers": info.get("http_headers") or {},
        }})
    except Exception as e:
        emit({"id": req.get("id"), "error": str(e)})

pool = ThreadPoolExecutor(int(sys.argv[2]))
for _ in range(int(sys.argv[2])):
    pool.submit(work, {"op": "warm"})
pool.submit(work, {"op": "premium"})
emit({"ready": True, "version": yt_dlp.version.__version__})
for line in sys.stdin:
    line = line.strip()
    if line:
        pool.submit(work, json.loads(line))
"#;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

pub struct Worker {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    /// Version de yt-dlp chargée par ce processus.
    version: String,
    _child: Child,
}

impl Worker {
    /// Démarre le processus et attend qu'il soit prêt. `options` : options
    /// `YoutubeDL` (cookies, moteur JS, client YouTube…).
    pub async fn spawn(python: &Path, options: &Value) -> Result<Self, String> {
        let mut cmd = crate::process::command(python);
        cmd.args(["-u", "-c", SCRIPT])
            .arg(options.to_string())
            .arg(THREADS.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::process::below_normal_priority(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("lancement de Python : {e}"))?;
        let stdin = child.stdin.take().ok_or("stdin du worker indisponible")?;
        let stdout = child.stdout.take().ok_or("stdout du worker indisponible")?;
        let mut lines = BufReader::new(stdout).lines();

        let ready = tokio::time::timeout(READY_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| "le worker yt-dlp ne démarre pas".to_owned())?
            .map_err(|e| e.to_string())?
            .ok_or("le worker yt-dlp s'est arrêté au démarrage")?;
        let ready: Value = serde_json::from_str(&ready).map_err(|e| e.to_string())?;
        let version = ready["version"].as_str().unwrap_or_default().to_owned();
        tracing::info!(%version, "worker yt-dlp prêt");

        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(read_loop(lines, Arc::clone(&pending), Arc::clone(&alive)));
        Ok(Self {
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            alive,
            version,
            _child: child,
        })
    }

    /// Version de yt-dlp chargée par ce processus.
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Infos yt-dlp (`url`, `vcodec`, `available_at`, `http_headers`) pour `url`.
    /// `format` : sélecteur yt-dlp (qualité choisie dans les Paramètres).
    pub async fn extract(&self, url: &str, format: &str) -> Result<Value, String> {
        self.request(json!({ "url": url, "format": format })).await
    }

    /// En-tête `Cookie` YouTube de la session du navigateur (`firefox`,
    /// `firefox:<profil>`). Reste en mémoire : rien n'est écrit sur le disque.
    pub async fn browser_cookie(&self, browser: &str) -> Result<String, String> {
        let value = self
            .request(json!({ "op": "cookie", "browser": browser }))
            .await?;
        value["cookie"]
            .as_str()
            .filter(|c| c.contains("SAPISID="))
            .map(str::to_owned)
            .ok_or_else(|| format!("pas de session YouTube dans {browser} (es-tu connecté ?)"))
    }

    async fn request(&self, mut body: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        body["id"] = json!(id);
        let mut line = body.to_string();
        line.push('\n');
        let written = {
            let mut stdin = self.stdin.lock().await;
            match stdin.write_all(line.as_bytes()).await {
                Ok(()) => stdin.flush().await,
                Err(e) => Err(e),
            }
        };
        if let Err(e) = written {
            lock(&self.pending).remove(&id);
            self.alive.store(false, Ordering::Relaxed);
            return Err(format!("worker yt-dlp injoignable : {e}"));
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("worker yt-dlp arrêté".into()),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err("worker yt-dlp bloqué".into())
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn read_loop(
    mut lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
) {
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(tx) = lock(&pending).remove(&id) else {
            continue;
        };
        let result = match (msg.get("ok"), msg.get("error")) {
            (Some(ok), _) => Ok(ok.clone()),
            (_, Some(err)) => Err(err.as_str().unwrap_or("erreur yt-dlp").to_owned()),
            _ => Err("réponse du worker illisible".into()),
        };
        let _ = tx.send(result);
    }
    alive.store(false, Ordering::Relaxed);
    lock(&pending).clear();
    tracing::warn!("worker yt-dlp arrêté");
}

/// Démarre le worker au plus vite.
///
/// Environnement présent : le worker part tout de suite, sans vérification ni
/// mise à jour (c'est son démarrage qui vérifie). S'il ne démarre pas,
/// l'environnement est réparé (yt-dlp réinstallé de force : un simple
/// `--upgrade` ne fait rien si ses métadonnées sont restées) ; en dernier
/// recours, ou si Python lui-même ne répond plus, il est recréé de zéro.
/// Environnement absent : création complète ([`ensure_env`]).
pub async fn launch(dir: &Path, options: &Value) -> Result<Worker, String> {
    let python = venv_python(dir);
    if python.exists() {
        match Worker::spawn(&python, options).await {
            Ok(worker) => return Ok(worker),
            Err(e) => tracing::warn!("environnement yt-dlp inutilisable ({e}) : réparation"),
        }
        if run(&python, ["-c", "import sys"], false).await.is_ok() {
            match run(&python, PIP_REPAIR, false).await {
                Ok(()) => {
                    let _ = std::fs::write(dir.join(MARKER), b"");
                    match Worker::spawn(&python, options).await {
                        Ok(worker) => return Ok(worker),
                        Err(e) => tracing::warn!("worker toujours en échec après réparation : {e}"),
                    }
                }
                Err(e) => tracing::warn!("réinstallation de yt-dlp : {e}"),
            }
        }
        // Python de l'environnement cassé (interpréteur déplacé ou
        // désinstallé) ou réparation sans effet : on le recrée de zéro. Garde :
        // seulement un dossier qui est bien un environnement Python.
        if !dir.join("pyvenv.cfg").exists() {
            return Err(format!(
                "{} n'est pas un environnement Python",
                dir.display()
            ));
        }
        tracing::warn!(dir = %dir.display(), "environnement Python recréé");
        std::fs::remove_dir_all(dir)
            .map_err(|e| format!("suppression de {} : {e}", dir.display()))?;
    }
    let python = ensure_env(dir).await?;
    Worker::spawn(&python, options).await
}

/// Mise à jour quotidienne de yt-dlp, à lancer en fond une fois le worker
/// prêt (priorité basse). Le worker en cours garde sa version en mémoire.
/// Rend la version installée si elle diffère de `running` (le worker est
/// alors à relancer pour en profiter).
pub async fn update_if_due(dir: &Path, running: &str) -> Result<Option<String>, String> {
    if !update_due(dir) {
        return Ok(None);
    }
    let python = venv_python(dir);
    remove_pip_leftovers(dir);
    tracing::info!("mise à jour de yt-dlp en fond");
    let started = std::time::Instant::now();
    run(&python, PIP_UPGRADE, true).await?;
    let _ = std::fs::write(dir.join(MARKER), b"");
    let installed = installed_version(&python).await?;
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis(),
        %installed,
        running,
        "mise à jour de yt-dlp terminée"
    );
    Ok((installed != running).then_some(installed))
}

/// Sous Windows, pip ne peut pas effacer une bibliothèque compilée (`.pyd`)
/// chargée par le worker en cours : il la renomme `~…` et la laisse (vérifié
/// le 2026-11-02 avec pycryptodomex). À la mise à jour suivante, plus aucun
/// processus ne la tient : on l'efface.
fn remove_pip_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir.join("Lib").join("site-packages")) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('~') {
            let path = entry.path();
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = removed {
                tracing::debug!(path = %path.display(), error = %e, "reste de pip");
            }
        }
    }
}

/// Python de l'environnement privé (qu'il existe ou non).
pub fn python_path(dir: &Path) -> PathBuf {
    venv_python(dir)
}

/// Marqueur de la dernière mise à jour de yt-dlp.
const MARKER: &str = "ytmr-updated";

const PIP_UPGRADE: [&str; 7] = [
    "-m",
    "pip",
    "install",
    "--quiet",
    "--disable-pip-version-check",
    "--upgrade",
    "yt-dlp[default]",
];

const PIP_REPAIR: [&str; 8] = [
    "-m",
    "pip",
    "install",
    "--quiet",
    "--disable-pip-version-check",
    "--upgrade",
    "--force-reinstall",
    "yt-dlp[default]",
];

/// Dernière mise à jour il y a plus d'un jour (ou jamais).
fn update_due(dir: &Path) -> bool {
    std::fs::metadata(dir.join(MARKER))
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() > UPDATE_EVERY)
        .unwrap_or(true)
}

async fn installed_version(python: &Path) -> Result<String, String> {
    let mut cmd = crate::process::command(python);
    cmd.args([
        "-c",
        "import yt_dlp.version; print(yt_dlp.version.__version__)",
    ])
    .stdin(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    let output = tokio::time::timeout(INSTALL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "version de yt-dlp : trop long".to_owned())?
        .map_err(|e| format!("version de yt-dlp : {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Python de l'environnement privé, créé et rempli au premier appel.
///
/// Peut prendre une minute la première fois (téléchargement de yt-dlp) : à
/// appeler en tâche de fond. Met yt-dlp à jour une fois par jour.
pub async fn ensure_env(dir: &Path) -> Result<PathBuf, String> {
    let python = venv_python(dir);
    let marker = dir.join(MARKER);
    if !python.exists() {
        tracing::info!(dir = %dir.display(), "création de l'environnement Python pour yt-dlp");
        let (launcher, args) = system_python()
            .await
            .ok_or("Python 3 introuvable : ytmr continue avec yt-dlp.exe (plus lent)".to_owned())?;
        run(
            &launcher,
            args.iter()
                .map(String::as_str)
                .chain(["-m", "venv"])
                .chain([dir.to_str().unwrap_or_default()]),
            false,
        )
        .await?;
    }
    if update_due(dir) || !has_yt_dlp(&python).await {
        tracing::info!("installation / mise à jour de yt-dlp dans l'environnement privé");
        run(&python, PIP_UPGRADE, false).await?;
        let _ = std::fs::write(&marker, b"");
    }
    Ok(python)
}

fn venv_python(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.join("Scripts").join("python.exe")
    } else {
        dir.join("bin").join("python")
    }
}

async fn has_yt_dlp(python: &Path) -> bool {
    run(python, ["-c", "import yt_dlp"], false).await.is_ok()
}

/// Lanceur Python du système : `py -3` sous Windows, `python3` ailleurs.
async fn system_python() -> Option<(PathBuf, Vec<String>)> {
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("py", &["-3"]), ("python", &[])]
    } else {
        &[("python3", &[]), ("python", &[])]
    };
    for (program, args) in candidates {
        let check = args.iter().copied().chain(["-c", "import venv"]);
        if run(Path::new(program), check, false).await.is_ok() {
            return Some((
                PathBuf::from(program),
                args.iter().map(|s| (*s).to_owned()).collect(),
            ));
        }
    }
    None
}

/// `low` : priorité processeur basse (travail de fond).
async fn run<'a>(
    program: &Path,
    args: impl IntoIterator<Item = &'a str>,
    low: bool,
) -> Result<(), String> {
    let mut cmd = crate::process::command(program);
    if low {
        crate::process::below_normal_priority(&mut cmd);
    }
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(INSTALL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{} trop long", program.display()))?
        .map_err(|e| format!("{} : {e}", program.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{} a échoué : {}",
            program.display(),
            stderr.lines().last().unwrap_or("sans message")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lancement réel sur un environnement jetable :
    /// `YTMR_TEST_ENV=<dossier> cargo test -p ytmr-player --release -- --ignored --nocapture launch_then_update`.
    /// Pendant la mise à jour, l'ancien worker résout un morceau ; après une
    /// nouvelle version, un nouveau worker est lancé et résout à son tour.
    #[tokio::test]
    #[ignore = "réseau, installe yt-dlp dans YTMR_TEST_ENV"]
    async fn launch_then_update() {
        let dir = PathBuf::from(std::env::var("YTMR_TEST_ENV").expect("YTMR_TEST_ENV"));
        let options = json!({ "quiet": true, "no_warnings": true, "noplaylist": true,
                              "skip_download": true, "js_runtimes": { "node": {} } });
        let url = "https://music.youtube.com/watch?v=dQw4w9WgXcQ";
        let started = std::time::Instant::now();
        let worker = launch(&dir, &options).await.expect("launch");
        println!(
            "worker prêt : {} ms (yt-dlp {})",
            started.elapsed().as_millis(),
            worker.version()
        );

        let started = std::time::Instant::now();
        let (update, during) = tokio::join!(
            update_if_due(&dir, worker.version()),
            worker.extract(url, "bestaudio/best"),
        );
        println!(
            "mise à jour : {} ms -> {:?} ; résolution pendant la mise à jour : {}",
            started.elapsed().as_millis(),
            update,
            if during.is_ok() { "ok" } else { "échec" }
        );
        if let Ok(Some(_)) = update {
            let started = std::time::Instant::now();
            let fresh = Worker::spawn(&python_path(&dir), &options)
                .await
                .expect("relance");
            let resolved = fresh.extract(url, "bestaudio/best").await;
            println!(
                "relance : yt-dlp {} prêt et résolution {} en {} ms ; ancien worker vivant : {}",
                fresh.version(),
                if resolved.is_ok() { "ok" } else { "en échec" },
                started.elapsed().as_millis(),
                worker.is_alive()
            );
        }
    }
}
