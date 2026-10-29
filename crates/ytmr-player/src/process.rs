//! Lancement des programmes externes (mpv, yt-dlp, moteur JS).
//!
//! En clair : sous Windows, un programme « console » lancé depuis une appli
//! fenêtrée ouvre sa propre fenêtre noire (c'est le cas de yt-dlp et des
//! raccourcis de scoop). On les lance donc toujours sans fenêtre.

use std::ffi::OsStr;

use tokio::process::Command;

/// `CREATE_NO_WINDOW` : pas de console pour le processus fils.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}
