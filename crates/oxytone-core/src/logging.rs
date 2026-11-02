//! Initialisation des logs `tracing`, partagée par tous les binaires.
//!
//! En clair : les messages vont dans le terminal (stderr) et, si on donne un
//! dossier, aussi dans un fichier qui change chaque jour.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Doit rester en vie jusqu'à la fin du programme, sinon les derniers logs
/// du fichier sont perdus.
#[must_use = "les logs fichier s'arrêtent quand le guard est libéré"]
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

/// Installe le subscriber global. `RUST_LOG` a priorité sur `default_filter`.
pub fn init(default_filter: &str, log_dir: Option<&Path>) -> LogGuard {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_filter))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr = fmt::layer().with_writer(std::io::stderr).with_target(true);

    let (file, guard) = match log_dir {
        Some(dir) => {
            let appender = tracing_appender::rolling::daily(dir, "oxytone.log");
            let (writer, guard) = tracing_appender::non_blocking(appender);
            (
                Some(fmt::layer().with_writer(writer).with_ansi(false)),
                Some(guard),
            )
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file)
        .init();

    LogGuard { _file: guard }
}
