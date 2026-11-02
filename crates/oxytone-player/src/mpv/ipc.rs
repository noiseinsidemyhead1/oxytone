//! Protocole JSON IPC de mpv.
//!
//! En clair : on écrit à mpv une ligne JSON par commande (`{"command": [...],
//! "request_id": 3}`) et il répond sur le même canal, en mélangeant les réponses
//! (repérées par `request_id`) et des événements spontanés (`{"event": ...}`).
//! Une tâche de fond lit ce canal et trie les deux.
//!
//! Le canal est un named pipe sous Windows et un socket Unix ailleurs ; c'est le
//! seul endroit du crate qui dépend de l'OS.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::PlayerError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Adresse du canal IPC, propre à ce processus pour ne pas croiser une autre instance.
pub fn ipc_path() -> PathBuf {
    let name = format!("oxytone-mpv-{}", std::process::id());
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\{name}"))
    } else {
        std::env::temp_dir().join(format!("{name}.sock"))
    }
}

/// Connexion établie avec mpv.
pub struct IpcClient {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Pending,
    next_id: AtomicU64,
}

impl IpcClient {
    /// Se connecte (en réessayant le temps que mpv crée le canal) et démarre la
    /// tâche de lecture. Les événements bruts de mpv arrivent sur `events`.
    pub async fn connect(
        path: &Path,
        events: mpsc::UnboundedSender<Value>,
    ) -> Result<Self, PlayerError> {
        let (reader, writer) = tokio::time::timeout(CONNECT_TIMEOUT, connect_retry(path))
            .await
            .map_err(|_| PlayerError::ConnectTimeout)??;

        let pending: Pending = Arc::default();
        tokio::spawn(read_loop(reader, Arc::clone(&pending), events));

        Ok(Self {
            writer: Mutex::new(writer),
            pending,
            next_id: AtomicU64::new(1),
        })
    }

    /// Envoie une commande et attend la réponse de mpv.
    pub async fn command(&self, args: Value) -> Result<Value, PlayerError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);

        let mut line = json!({ "command": &args, "request_id": id }).to_string();
        line.push('\n');
        tracing::trace!(%line, "→ mpv");

        let written = {
            let mut writer = self.writer.lock().await;
            match writer.write_all(line.as_bytes()).await {
                Ok(()) => writer.flush().await,
                Err(e) => Err(e),
            }
        };
        if let Err(e) = written {
            lock(&self.pending).remove(&id);
            return Err(PlayerError::Ipc(e));
        }

        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(PlayerError::CommandTimeout(args.to_string()))
            }
            Ok(Err(_)) => Err(PlayerError::Closed),
            Ok(Ok(Err(message))) => Err(PlayerError::Command {
                command: args.to_string(),
                message,
            }),
            Ok(Ok(Ok(data))) => Ok(data),
        }
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Un panic pendant qu'on tient le verrou ne rend pas la table incohérente.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn read_loop(
    reader: Box<dyn AsyncRead + Send + Unpin>,
    pending: Pending,
    events: mpsc::UnboundedSender<Value>,
) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "lecture IPC mpv interrompue");
                break;
            }
        };
        tracing::trace!(%line, "← mpv");
        let mut msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, %line, "ligne IPC mpv illisible");
                continue;
            }
        };

        if let Some(id) = msg.get("request_id").and_then(Value::as_u64) {
            let Some(tx) = lock(&pending).remove(&id) else {
                continue;
            };
            let result = match msg.get("error").and_then(Value::as_str) {
                Some("success") => Ok(msg.get_mut("data").map(Value::take).unwrap_or(Value::Null)),
                Some(err) => Err(err.to_owned()),
                None => Err("réponse sans champ error".to_owned()),
            };
            let _ = tx.send(result);
        } else if msg.get("event").is_some() && events.send(msg).is_err() {
            break;
        }
    }
    // Canal fermé : mpv est mort ou en train de quitter.
    lock(&pending).clear();
}

type Halves = (
    Box<dyn AsyncRead + Send + Unpin>,
    Box<dyn AsyncWrite + Send + Unpin>,
);

#[cfg(windows)]
async fn connect_retry(path: &Path) -> Result<Halves, PlayerError> {
    use tokio::net::windows::named_pipe::ClientOptions;
    // ERROR_PIPE_BUSY : le pipe existe mais aucune instance n'est libre.
    const ERROR_PIPE_BUSY: i32 = 231;
    loop {
        match ClientOptions::new().open(path) {
            Ok(pipe) => {
                let (r, w) = tokio::io::split(pipe);
                return Ok((Box::new(r), Box::new(w)));
            }
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(ERROR_PIPE_BUSY) =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(PlayerError::Ipc(e)),
        }
    }
}

#[cfg(unix)]
async fn connect_retry(path: &Path) -> Result<Halves, PlayerError> {
    use tokio::net::UnixStream;
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => {
                let (r, w) = stream.into_split();
                return Ok((Box::new(r), Box::new(w)));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(PlayerError::Ipc(e)),
        }
    }
}
