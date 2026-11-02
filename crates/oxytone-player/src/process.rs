//! Lancement des programmes externes (mpv, yt-dlp, Python, moteur JS).
//!
//! En clair : sous Windows, un programme « console » lancé depuis une appli
//! fenêtrée ouvre sa propre fenêtre noire (c'est le cas de yt-dlp et des
//! raccourcis de scoop). On les lance donc toujours sans fenêtre. Le travail
//! d'anticipation tourne en plus en priorité basse, pour ne jamais voler de
//! temps processeur à l'interface.

use std::ffi::OsStr;

use tokio::process::Command;

/// `CREATE_NO_WINDOW` : pas de console pour le processus fils.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `BELOW_NORMAL_PRIORITY_CLASS`.
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY: u32 = 0x0000_4000;

pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Priorité processeur basse (Windows ; sans effet ailleurs pour l'instant).
pub fn below_normal_priority(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY);
    #[cfg(not(windows))]
    let _ = cmd;
}
