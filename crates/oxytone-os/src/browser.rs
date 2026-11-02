//! Navigateur par défaut du système.
//!
//! En clair : pour se connecter sans retaper son mot de passe, oxytone essaie de
//! reprendre la session YouTube du navigateur par défaut. Ce module dit lequel
//! c'est, sous le nom que yt-dlp comprend (`firefox`, `chrome`, `edge`…).
//! Sous Windows, seule la famille Firefox est lisible : Chrome, Edge et Brave
//! chiffrent leurs cookies pour eux seuls (yt-dlp, ticket 10927).

/// Nom yt-dlp du navigateur par défaut (`None` : inconnu ou non géré).
pub fn default_browser() -> Option<&'static str> {
    imp::program_id().and_then(|id| to_ytdlp(&id))
}

/// Ce navigateur laisse-t-il lire sa session ? (Windows : famille Firefox.)
pub fn session_readable(browser: &str) -> bool {
    !cfg!(windows) || browser == "firefox"
}

fn to_ytdlp(id: &str) -> Option<&'static str> {
    let id = id.to_ascii_lowercase();
    [
        ("firefox", "firefox"),
        ("chrome", "chrome"),
        ("msedge", "edge"),
        ("microsoft-edge", "edge"),
        ("brave", "brave"),
        ("opera", "opera"),
        ("vivaldi", "vivaldi"),
        ("chromium", "chromium"),
    ]
    .into_iter()
    .find(|(key, _)| id.contains(key))
    .map(|(_, name)| name)
}

#[cfg(windows)]
mod imp {
    /// ProgId du gestionnaire des liens https (`FirefoxURL-…`, `MSEdgeHTM`, `ChromeHTML`…).
    pub fn program_id() -> Option<String> {
        winreg::HKCU
            .open_subkey(
                r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
            )
            .ok()?
            .get_value("ProgId")
            .ok()
    }
}

#[cfg(not(windows))]
mod imp {
    /// Fichier `.desktop` du navigateur par défaut (`firefox.desktop`…).
    pub fn program_id() -> Option<String> {
        let output = std::process::Command::new("xdg-settings")
            .args(["get", "default-web-browser"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_program_ids() {
        assert_eq!(to_ytdlp("FirefoxURL-308046B0AF4A39CB"), Some("firefox"));
        assert_eq!(to_ytdlp("MSEdgeHTM"), Some("edge"));
        assert_eq!(to_ytdlp("ChromeHTML"), Some("chrome"));
        assert_eq!(to_ytdlp("firefox.desktop"), Some("firefox"));
        assert_eq!(to_ytdlp("zen.desktop"), None);
    }
}
