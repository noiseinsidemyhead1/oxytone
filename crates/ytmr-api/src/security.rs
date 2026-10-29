//! Protection du serveur local.
//!
//! En clair : le serveur écoute sur la machine, mais n'importe quelle page web
//! ouverte dans un navigateur peut tenter de lui parler. Trois garde-fous :
//! - `Host` : en mode local, il doit valoir `127.0.0.1`/`localhost`. Ça bloque
//!   le « DNS rebinding » (un site qui fait pointer son propre nom vers 127.0.0.1).
//! - `Origin` : si le navigateur l'envoie, il doit être le nôtre. Ça bloque les
//!   requêtes qu'un autre site déclencherait (« suivant », « volume 100 »…).
//! - Token Bearer : obligatoire dès que le serveur est ouvert au réseau local.
//!   L'ESP32 n'envoie pas d'`Origin` et met son token dans `Authorization`.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;

#[derive(Debug, Clone)]
pub struct Guard {
    /// Mode local : seuls les noms de loopback sont acceptés dans `Host`.
    pub loopback_only: bool,
    pub port: u16,
    pub token: Option<String>,
}

impl Guard {
    fn host_allowed(&self, host: &str) -> bool {
        if !self.loopback_only {
            return true;
        }
        let port = self.port;
        [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ]
        .iter()
        .any(|allowed| allowed == host)
    }

    fn token_ok(&self, headers: &HeaderMap, query: Option<&str>) -> bool {
        let Some(expected) = &self.token else {
            return true;
        };
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        // `?token=` pour les clients qui ne peuvent pas poser d'en-tête (WebSocket du navigateur).
        let from_query = query.and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")));
        bearer
            .or(from_query)
            .is_some_and(|given| constant_time_eq(given.as_bytes(), expected.as_bytes()))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn reject(status: StatusCode, message: &str) -> Response {
    tracing::warn!(%status, message, "requête refusée");
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
}

pub async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let guard = &state.guard;
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !guard.host_allowed(host) {
        return reject(StatusCode::FORBIDDEN, "hôte non autorisé");
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let expected = format!("http://{host}");
        if origin.to_str().ok() != Some(expected.as_str()) {
            return reject(StatusCode::FORBIDDEN, "origine non autorisée");
        }
    }
    let is_api = request.uri().path().starts_with("/api/");
    if is_api && !guard.token_ok(headers, request.uri().query()) {
        return reject(StatusCode::UNAUTHORIZED, "token manquant ou invalide");
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn local() -> Guard {
        Guard {
            loopback_only: true,
            port: 26540,
            token: None,
        }
    }

    #[test]
    fn local_mode_accepts_only_loopback_hosts() {
        let g = local();
        assert!(g.host_allowed("127.0.0.1:26540"));
        assert!(g.host_allowed("localhost:26540"));
        assert!(!g.host_allowed("evil.example:26540"));
        assert!(!g.host_allowed("127.0.0.1:80"));
        assert!(!g.host_allowed(""));
    }

    #[test]
    fn token_from_header_or_query() {
        let g = Guard {
            token: Some("s3cret".into()),
            ..local()
        };
        let mut headers = HeaderMap::new();
        assert!(!g.token_ok(&headers, None));
        assert!(g.token_ok(&headers, Some("a=1&token=s3cret")));
        assert!(!g.token_ok(&headers, Some("token=s3cre")));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer s3cret"),
        );
        assert!(g.token_ok(&headers, None));
    }
}
