//! Migration des données de ytmr (ancien nom) vers oxytone, au premier lancement.
//!
//! En clair : l'appli s'appelait ytmr et rangeait ses réglages, son cache
//! (dont plusieurs Go de morceaux téléchargés), sa session et ses statistiques
//! dans des dossiers « ytmr ». Au premier lancement d'oxytone, ces dossiers
//! sont **déplacés** (renommés, pas copiés : instantané, sans place en plus)
//! vers les dossiers « oxytone ». Rien n'est jamais effacé :
//! - ytmr ouvert : on ne déplace rien (ses fichiers sont en cours d'usage),
//!   l'interface demande de le fermer ;
//! - dossier oxytone déjà là : on le garde, celui de ytmr n'est pas touché ;
//! - un déplacement échoue : ce dossier-là reste celui de ytmr, utilisé tel
//!   quel ; on réessaiera au lancement suivant.
//!
//! Chaque dossier (réglages, données, cache) est traité à part. Les étapes
//! sont rendues au lanceur, qui les écrit au journal une fois celui-ci prêt
//! (le dossier du journal dépend justement de la migration).

use std::path::{Path, PathBuf};

use crate::AppPaths;

/// Ancien nom de l'appli (dossiers, fichiers).
pub const LEGACY_NAME: &str = "ytmr";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Rien à déplacer (déjà fait, ou jamais eu ytmr).
    Nothing,
    /// Tout ce qui était à déplacer l'a été.
    Moved,
    /// Au moins un dossier n'a pas pu être déplacé : il reste utilisé à
    /// l'ancien emplacement.
    Partial,
    /// ytmr est ouvert : rien n'a été déplacé.
    Blocked,
}

/// Une étape, à écrire au journal (`ok = false` : avertissement).
#[derive(Debug, Clone)]
pub struct Step {
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Migration {
    /// Dossiers à utiliser pour ce lancement.
    pub paths: AppPaths,
    pub outcome: Outcome,
    pub steps: Vec<Step>,
}

/// Des dossiers de ytmr attendent d'être déplacés.
pub fn pending(old: &AppPaths, new: &AppPaths) -> bool {
    leaves(old, new)
        .iter()
        .any(|(o, n)| o.exists() && !n.exists())
}

/// Déplace les dossiers de `old` (ytmr) vers `new` (oxytone).
///
/// `legacy_running` : ytmr est ouvert (rien n'est alors déplacé).
pub fn migrate(old: &AppPaths, new: &AppPaths, legacy_running: bool) -> Migration {
    let mut steps = Vec::new();
    let mut paths = new.clone();
    if !pending(old, new) {
        return Migration {
            paths,
            outcome: Outcome::Nothing,
            steps,
        };
    }
    if legacy_running {
        // Chaque dossier : le nouveau s'il existe déjà, sinon l'ancien.
        for (slot, (o, n)) in slots(&mut paths).into_iter().zip(leaves(old, new)) {
            if !n.exists() && o.exists() {
                *slot = o;
            }
        }
        steps.push(Step {
            ok: false,
            message: "ytmr est ouvert : migration remise au prochain lancement".into(),
        });
        return Migration {
            paths,
            outcome: Outcome::Blocked,
            steps,
        };
    }

    let mut failed = false;
    for (slot, (o, n)) in slots(&mut paths).into_iter().zip(leaves(old, new)) {
        if !o.exists() {
            continue;
        }
        if n.exists() {
            steps.push(Step {
                ok: true,
                message: format!(
                    "{} existe déjà : gardé, {} laissé tel quel",
                    n.display(),
                    o.display()
                ),
            });
            continue;
        }
        match move_dir(&o, &n) {
            Ok(()) => steps.push(Step {
                ok: true,
                message: format!("déplacé : {} → {}", o.display(), n.display()),
            }),
            Err(e) => {
                failed = true;
                steps.push(Step {
                    ok: false,
                    message: format!(
                        "déplacement impossible ({e}) : {} reste utilisé à cet emplacement",
                        o.display()
                    ),
                });
                *slot = o;
            }
        }
    }
    rename_worker_marker(&paths.cache_dir, &mut steps);
    for root in legacy_roots(old) {
        clean_legacy_root(&root, &mut steps);
    }
    Migration {
        paths,
        outcome: if failed {
            Outcome::Partial
        } else {
            Outcome::Moved
        },
        steps,
    }
}

/// (ancien, nouveau) pour les trois dossiers, dans l'ordre de [`slots`].
fn leaves(old: &AppPaths, new: &AppPaths) -> [(PathBuf, PathBuf); 3] {
    [
        (old.config_dir.clone(), new.config_dir.clone()),
        (old.data_dir.clone(), new.data_dir.clone()),
        (old.cache_dir.clone(), new.cache_dir.clone()),
    ]
}

fn slots(paths: &mut AppPaths) -> [&mut PathBuf; 3] {
    [
        &mut paths.config_dir,
        &mut paths.data_dir,
        &mut paths.cache_dir,
    ]
}

/// Renommage seul : même volume, instantané, rien n'est copié. Un échec
/// (fichier ouvert ailleurs, droits, autre disque) laisse tout en place.
fn move_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)
}

/// Le marqueur de mise à jour du worker a changé de nom : sans lui, yt-dlp
/// serait juste remis à jour en fond, mais autant le garder.
fn rename_worker_marker(cache_dir: &Path, steps: &mut Vec<Step>) {
    let env = cache_dir.join("ytdlp-env");
    let old = env.join(format!("{LEGACY_NAME}-updated"));
    let new = env.join("oxytone-updated");
    if old.exists() && !new.exists() && std::fs::rename(&old, &new).is_ok() {
        steps.push(Step {
            ok: true,
            message: "marqueur de mise à jour de yt-dlp renommé".into(),
        });
    }
}

/// Dossiers racine de ytmr (`…\ytmr` sous Windows, qui contient `config`,
/// `data` ou `cache`) : nettoyés une fois vidés. Sous Linux, chaque dossier
/// est lui-même la racine (`~/.config/ytmr`) et a déjà été déplacé.
fn legacy_roots(old: &AppPaths) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = [&old.config_dir, &old.data_dir, &old.cache_dir]
        .into_iter()
        .filter_map(|leaf| leaf.parent())
        .filter(|p| p.file_name().is_some_and(|n| n == LEGACY_NAME))
        .map(Path::to_path_buf)
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// Efface l'icône des notifications de ytmr (fichier régénéré, propre à
/// l'appli) puis le dossier s'il est vide. Jamais rien d'autre.
fn clean_legacy_root(root: &Path, steps: &mut Vec<Step>) {
    let icon = root.join(format!("{LEGACY_NAME}-icon.png"));
    if icon.is_file() {
        let _ = std::fs::remove_file(&icon);
    }
    if root.is_dir() && std::fs::remove_dir(root).is_ok() {
        steps.push(Step {
            ok: true,
            message: format!("dossier vide retiré : {}", root.display()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disposition de Windows : `Roaming\<nom>\{config,data}`, `Local\<nom>\cache`.
    fn layout(root: &Path, name: &str) -> AppPaths {
        AppPaths {
            config_dir: root.join("Roaming").join(name).join("config"),
            data_dir: root.join("Roaming").join(name).join("data"),
            cache_dir: root.join("Local").join(name).join("cache"),
        }
    }

    fn fill(paths: &AppPaths) {
        for (dir, file, body) in [
            (&paths.config_dir, "config.toml", "volume = 70"),
            (&paths.data_dir, "plays.jsonl", "{}"),
            (&paths.data_dir, "youtube-session.bin", "secret"),
            (&paths.cache_dir, "streams.json", "{}"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        }
        let audio = paths.cache_dir.join("audio");
        std::fs::create_dir_all(&audio).unwrap();
        std::fs::write(audio.join("abc.audio"), vec![7u8; 4096]).unwrap();
        std::fs::write(audio.join("pinned.json"), "[\"abc\"]").unwrap();
        let env = paths.cache_dir.join("ytdlp-env");
        std::fs::create_dir_all(&env).unwrap();
        std::fs::write(env.join("ytmr-updated"), "").unwrap();
        // Icône des notifications, à côté du cache.
        if let Some(local) = paths.cache_dir.parent() {
            std::fs::write(local.join("ytmr-icon.png"), "png").unwrap();
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn moves_everything_on_first_launch() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (layout(tmp.path(), "ytmr"), layout(tmp.path(), "oxytone"));
        fill(&old);
        assert!(pending(&old, &new));

        let m = migrate(&old, &new, false);
        assert_eq!(m.outcome, Outcome::Moved, "{:?}", m.steps);
        assert_eq!(m.paths.data_dir, new.data_dir);
        assert_eq!(read(&new.config_dir.join("config.toml")), "volume = 70");
        assert_eq!(read(&new.data_dir.join("youtube-session.bin")), "secret");
        assert_eq!(
            std::fs::read(new.cache_dir.join("audio").join("abc.audio"))
                .unwrap()
                .len(),
            4096
        );
        assert!(
            new.cache_dir
                .join("ytdlp-env")
                .join("oxytone-updated")
                .exists()
        );
        // Racines de ytmr vidées puis retirées.
        assert!(!tmp.path().join("Roaming").join("ytmr").exists());
        assert!(!tmp.path().join("Local").join("ytmr").exists());
        // Second lancement : plus rien à faire.
        assert!(!pending(&old, &new));
        assert_eq!(migrate(&old, &new, false).outcome, Outcome::Nothing);
    }

    #[test]
    fn touches_nothing_while_ytmr_is_open() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (layout(tmp.path(), "ytmr"), layout(tmp.path(), "oxytone"));
        fill(&old);

        let m = migrate(&old, &new, true);
        assert_eq!(m.outcome, Outcome::Blocked);
        assert_eq!(m.paths.config_dir, old.config_dir);
        assert_eq!(m.paths.data_dir, old.data_dir);
        assert_eq!(m.paths.cache_dir, old.cache_dir);
        assert!(old.data_dir.join("youtube-session.bin").exists());
        assert!(!tmp.path().join("Roaming").join("oxytone").exists());
        assert!(pending(&old, &new));
    }

    #[test]
    fn keeps_an_existing_oxytone_folder_and_leaves_ytmr_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (layout(tmp.path(), "ytmr"), layout(tmp.path(), "oxytone"));
        fill(&old);
        std::fs::create_dir_all(&new.config_dir).unwrap();
        std::fs::write(new.config_dir.join("config.toml"), "volume = 30").unwrap();

        let m = migrate(&old, &new, false);
        assert_eq!(m.outcome, Outcome::Moved, "{:?}", m.steps);
        // Réglages : ceux d'oxytone gardés, ceux de ytmr intacts.
        assert_eq!(read(&new.config_dir.join("config.toml")), "volume = 30");
        assert_eq!(read(&old.config_dir.join("config.toml")), "volume = 70");
        // Le reste a bien été déplacé.
        assert!(new.data_dir.join("plays.jsonl").exists());
        assert!(new.cache_dir.join("streams.json").exists());
        // La racine Roaming de ytmr n'est pas vide (config) : gardée.
        assert!(tmp.path().join("Roaming").join("ytmr").exists());
    }

    #[test]
    fn a_failed_move_keeps_the_old_folder_in_use() {
        let tmp = tempfile::tempdir().unwrap();
        let old = layout(tmp.path(), "ytmr");
        let mut new = layout(tmp.path(), "oxytone");
        fill(&old);
        // Le parent du nouveau dossier de données est un fichier : impossible
        // d'y créer un dossier.
        let blocker = tmp.path().join("bloque");
        std::fs::write(&blocker, "").unwrap();
        new.data_dir = blocker.join("data");

        let m = migrate(&old, &new, false);
        assert_eq!(m.outcome, Outcome::Partial);
        assert!(m.steps.iter().any(|s| !s.ok));
        // Données : toujours à l'ancien emplacement, intactes, et utilisées.
        assert_eq!(m.paths.data_dir, old.data_dir);
        assert_eq!(read(&old.data_dir.join("youtube-session.bin")), "secret");
        assert_eq!(read(&old.data_dir.join("plays.jsonl")), "{}");
        // Les autres dossiers ont été déplacés.
        assert_eq!(m.paths.config_dir, new.config_dir);
        assert!(new.cache_dir.join("audio").join("pinned.json").exists());
        // Encore à faire au prochain lancement.
        assert!(pending(&old, &new));
    }
}
