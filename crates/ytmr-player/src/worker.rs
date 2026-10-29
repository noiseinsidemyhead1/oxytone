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
local = threading.local()
out = threading.Lock()

def ydl():
    if not hasattr(local, "ydl"):
        local.ydl = yt_dlp.YoutubeDL(opts)
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
        if req.get("op") == "cookie":
            emit({"id": req["id"], "ok": {"cookie": browser_cookie(req["browser"])}})
            return
        info = ydl().extract_info(req["url"], download=False)
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
        tracing::info!(version = %ready["version"], "worker yt-dlp prêt");

        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(read_loop(lines, Arc::clone(&pending), Arc::clone(&alive)));
        Ok(Self {
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            alive,
            _child: child,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Infos yt-dlp (`url`, `vcodec`, `available_at`, `http_headers`) pour `url`.
    pub async fn extract(&self, url: &str) -> Result<Value, String> {
        self.request(json!({ "url": url })).await
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

/// Python de l'environnement privé, créé et rempli au premier appel.
///
/// Peut prendre une minute la première fois (téléchargement de yt-dlp) : à
/// appeler en tâche de fond. Met yt-dlp à jour une fois par jour.
pub async fn ensure_env(dir: &Path) -> Result<PathBuf, String> {
    let python = venv_python(dir);
    let marker = dir.join("ytmr-updated");
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
        )
        .await?;
    }
    let stale = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() > UPDATE_EVERY)
        .unwrap_or(true);
    if stale || !has_yt_dlp(&python).await {
        tracing::info!("installation / mise à jour de yt-dlp dans l'environnement privé");
        run(
            &python,
            [
                "-m",
                "pip",
                "install",
                "--quiet",
                "--disable-pip-version-check",
                "--upgrade",
                "yt-dlp[default]",
            ],
        )
        .await?;
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
    run(python, ["-c", "import yt_dlp"]).await.is_ok()
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
        if run(Path::new(program), check).await.is_ok() {
            return Some((
                PathBuf::from(program),
                args.iter().map(|s| (*s).to_owned()).collect(),
            ));
        }
    }
    None
}

async fn run<'a>(program: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut cmd = crate::process::command(program);
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
