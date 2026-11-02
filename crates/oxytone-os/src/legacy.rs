//! Traces de ytmr (ancien nom) dans le registre Windows, reprises par oxytone.
//!
//! En clair : ytmr avait déclaré son nom de notifications (AUMID
//! `ytmr.player`) et, si on l'avait choisi, son lancement avec Windows (valeur
//! `ytmr` de la clé Run). Au lancement d'oxytone : le démarrage avec Windows
//! est gardé s'il était actif (nouvelle valeur `oxytone` vers l'exe
//! d'oxytone), puis les deux traces de ytmr sont retirées. Le nouvel AUMID,
//! `oxytone.player`, est déclaré par `notify` comme avant.

use std::path::Path;

use oxytone_core::migrate::{LEGACY_NAME, Step};
use winreg::HKCU;
use winreg::enums::{KEY_READ, KEY_SET_VALUE};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const AUMID_PARENT: &str = r"Software\Classes\AppUserModelId";

/// Reprend le démarrage avec Windows et retire l'AUMID de ytmr.
/// `exe` : l'exécutable d'oxytone en cours.
pub fn migrate_registry(exe: &Path) -> Vec<Step> {
    migrate_at(RUN_KEY, AUMID_PARENT, exe)
}

fn migrate_at(run_key: &str, aumid_parent: &str, exe: &Path) -> Vec<Step> {
    let mut steps = Vec::new();
    if let Ok(run) = HKCU.open_subkey_with_flags(run_key, KEY_READ | KEY_SET_VALUE)
        && run.get_value::<String, _>(LEGACY_NAME).is_ok()
    {
        let keep = if run.get_value::<String, _>("oxytone").is_ok() {
            Ok(())
        } else {
            run.set_value("oxytone", &format!("\"{}\"", exe.display()))
        };
        match keep {
            Ok(()) => {
                let removed = run.delete_value(LEGACY_NAME);
                steps.push(Step {
                    ok: removed.is_ok(),
                    message: match removed {
                        Ok(()) => "démarrage avec Windows repris par oxytone".into(),
                        Err(e) => format!("valeur Run de ytmr non retirée : {e}"),
                    },
                });
            }
            // Sans la nouvelle valeur, on garde l'ancienne (le réglage n'est
            // pas perdu ; nouvel essai au prochain lancement).
            Err(e) => steps.push(Step {
                ok: false,
                message: format!("démarrage avec Windows non repris : {e}"),
            }),
        }
    }
    let aumid = format!(r"{aumid_parent}\{LEGACY_NAME}.player");
    if HKCU.open_subkey(&aumid).is_ok() {
        let removed = HKCU.delete_subkey_all(&aumid);
        steps.push(Step {
            ok: removed.is_ok(),
            message: match removed {
                Ok(()) => "nom de notifications de ytmr retiré du registre".into(),
                Err(e) => format!("nom de notifications de ytmr non retiré : {e}"),
            },
        });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dans une clé de test à part (`HKCU\Software\oxytone-test-<pid>`), jamais
    /// dans les vraies clés.
    #[test]
    #[ignore = "écrit dans HKCU (clé de test, effacée à la fin)"]
    fn moves_autostart_and_removes_the_old_aumid() {
        let root = format!(r"Software\oxytone-test-{}", std::process::id());
        let run_key = format!(r"{root}\Run");
        let aumid_parent = format!(r"{root}\AppUserModelId");
        let (run, _) = HKCU.create_subkey(&run_key).unwrap();
        run.set_value("ytmr", &r#""C:\ytmr\ytmr.exe""#.to_owned())
            .unwrap();
        let (aumid, _) = HKCU
            .create_subkey(format!(r"{aumid_parent}\ytmr.player"))
            .unwrap();
        aumid.set_value("DisplayName", &"ytmr".to_owned()).unwrap();
        drop(aumid);

        let steps = migrate_at(
            &run_key,
            &aumid_parent,
            Path::new(r"C:\oxytone\oxytone.exe"),
        );
        let run_after = run.get_value::<String, _>("oxytone");
        let old_gone = run.get_value::<String, _>("ytmr").is_err();
        let aumid_gone = HKCU
            .open_subkey(format!(r"{aumid_parent}\ytmr.player"))
            .is_err();
        // Second passage : plus rien à faire.
        let again = migrate_at(
            &run_key,
            &aumid_parent,
            Path::new(r"C:\oxytone\oxytone.exe"),
        );
        drop(run);
        HKCU.delete_subkey_all(&root).unwrap();

        assert!(steps.iter().all(|s| s.ok), "{steps:?}");
        assert_eq!(run_after.unwrap(), r#""C:\oxytone\oxytone.exe""#);
        assert!(old_gone);
        assert!(aumid_gone);
        assert!(again.is_empty());
    }

    #[test]
    #[ignore = "écrit dans HKCU (clé de test, effacée à la fin)"]
    fn without_autostart_nothing_is_added() {
        let root = format!(r"Software\oxytone-test-b-{}", std::process::id());
        let run_key = format!(r"{root}\Run");
        let (run, _) = HKCU.create_subkey(&run_key).unwrap();
        let steps = migrate_at(&run_key, &format!(r"{root}\AppUserModelId"), Path::new("x"));
        let added = run.get_value::<String, _>("oxytone").is_ok();
        drop(run);
        HKCU.delete_subkey_all(&root).unwrap();
        assert!(steps.is_empty());
        assert!(!added);
    }
}
