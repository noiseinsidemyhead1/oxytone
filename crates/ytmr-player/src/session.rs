//! Session YouTube capturée par la fenêtre de connexion de ytmr.
//!
//! En clair : la liste des cookies de connexion à YouTube (nom, valeur,
//! domaine…). Elle sert au compte (en-tête `Cookie`) et à yt-dlp (qualité
//! Premium), sans dépendre d'un navigateur en particulier.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCookie {
    pub name: String,
    pub value: String,
    /// `.youtube.com`, `music.youtube.com`…
    pub domain: String,
    pub path: String,
    pub secure: bool,
    /// Expiration (secondes Unix) ; `None` : cookie de session.
    pub expires: Option<i64>,
}

/// En-tête `Cookie` pour music.youtube.com (un nom n'apparaît qu'une fois).
pub fn cookie_header(cookies: &[SessionCookie]) -> String {
    let mut seen = std::collections::HashSet::new();
    cookies
        .iter()
        .filter(|c| {
            let domain = c.domain.trim_start_matches('.');
            domain == "youtube.com" || domain.ends_with(".youtube.com")
        })
        .filter(|c| seen.insert(c.name.clone()))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Session utilisable : les cookies de connexion de Google sont là.
pub fn is_signed_in(cookies: &[SessionCookie]) -> bool {
    cookies
        .iter()
        .any(|c| c.name == "SAPISID" || c.name == "__Secure-3PAPISID")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, domain: &str) -> SessionCookie {
        SessionCookie {
            name: name.into(),
            value: "v".into(),
            domain: domain.into(),
            path: "/".into(),
            secure: true,
            expires: None,
        }
    }

    #[test]
    fn header_keeps_youtube_cookies_once() {
        let cookies = vec![
            cookie("SAPISID", ".youtube.com"),
            cookie("SAPISID", "music.youtube.com"),
            cookie("NID", ".google.com"),
        ];
        assert_eq!(cookie_header(&cookies), "SAPISID=v");
        assert!(is_signed_in(&cookies));
        assert!(!is_signed_in(&[cookie("NID", ".google.com")]));
    }
}
