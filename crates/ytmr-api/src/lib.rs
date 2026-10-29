//! Serveur HTTP + WebSocket (axum) pour le deck CYD et les autres télécommandes.
//!
//! En clair : une porte d'entrée réseau qui traduit les requêtes en ordres pour
//! le core. Aucune logique de lecture ici : tout passe par [`CoreHandle`].

mod dto;
mod security;
mod ws;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use ytmr_catalog::Catalog;
use ytmr_core::{Command, CoreError, CoreHandle, QueueId};
use ytmr_media::{CoverFormat, Covers, MediaError};

pub use security::Guard;

use crate::dto::{
    AlbumDto, ArtistDto, QueueDto, SearchDto, StateDto, TrackDto, is_browse_id, is_video_id,
    parse_repeat, tracks_from_dto,
};

const MAX_QUEUE_PAGE: usize = 200;
const MAX_QUERY_LEN: usize = 200;
const MAX_VOLUME: u8 = 100;

#[derive(Clone)]
pub struct AppState {
    core: CoreHandle,
    catalog: Arc<Catalog>,
    covers: Arc<Covers>,
    accents: Arc<Mutex<HashMap<String, String>>>,
    guard: Arc<Guard>,
}

impl AppState {
    pub fn new(core: CoreHandle, catalog: Arc<Catalog>, covers: Arc<Covers>, guard: Guard) -> Self {
        Self {
            core,
            catalog,
            covers,
            accents: Arc::default(),
            guard: Arc::new(guard),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/state", get(get_state))
        .route(
            "/toggle",
            post(|s: State<AppState>| command(s, Command::TogglePause)),
        )
        .route(
            "/play",
            post(|s: State<AppState>| command(s, Command::Play)),
        )
        .route(
            "/pause",
            post(|s: State<AppState>| command(s, Command::Pause)),
        )
        .route(
            "/next",
            post(|s: State<AppState>| command(s, Command::Next)),
        )
        .route(
            "/prev",
            post(|s: State<AppState>| command(s, Command::Previous)),
        )
        .route("/seek", post(seek))
        .route("/volume", post(volume))
        .route("/shuffle", post(shuffle))
        .route("/repeat", post(repeat))
        .route("/autoplay", post(autoplay))
        .route("/like", post(like))
        .route("/queue", get(get_queue))
        .route("/queue/play", post(queue_play))
        .route("/queue/add", post(queue_add))
        .route("/queue/next", post(queue_next))
        .route("/queue/jump", post(queue_jump))
        .route("/queue/remove", post(queue_remove))
        .route("/queue/move", post(queue_move))
        .route(
            "/queue/clear",
            post(|s: State<AppState>| command(s, Command::ClearUpcoming)),
        )
        .route("/prefetch", post(prefetch))
        .route("/search", get(search))
        .route("/album/{id}", get(album))
        .route("/artist/{id}", get(artist))
        .route("/cover/{id}", get(cover))
        .route("/cover/{id}/accent", get(accent))
        .route("/ws", get(ws::handler));

    Router::new()
        .nest("/api/v1", api)
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            security::guard,
        ))
        .with_state(state)
}

/// Écoute sur `addr` jusqu'à ce que `shutdown` se termine.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "API prête");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

// ----- erreurs -----

struct ApiError(StatusCode, String);

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        let status = match &e {
            CoreError::NotInQueue => StatusCode::NOT_FOUND,
            CoreError::NothingToPlay => StatusCode::CONFLICT,
            CoreError::Invalid(_) => StatusCode::BAD_REQUEST,
            CoreError::Player(_) => StatusCode::BAD_GATEWAY,
            CoreError::Stopped => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self(status, e.to_string())
    }
}

impl From<ytmr_catalog::CatalogError> for ApiError {
    fn from(e: ytmr_catalog::CatalogError) -> Self {
        Self(StatusCode::BAD_GATEWAY, e.to_string())
    }
}

impl From<MediaError> for ApiError {
    fn from(e: MediaError) -> Self {
        let status = match &e {
            MediaError::UnknownCover => StatusCode::NOT_FOUND,
            MediaError::InvalidSize(_) => StatusCode::BAD_REQUEST,
            MediaError::Download(_) | MediaError::Decode(_) => StatusCode::BAD_GATEWAY,
            MediaError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self(status, e.to_string())
    }
}

type ApiResult<T = Response> = Result<T, ApiError>;

/// Corps JSON avec un message d'erreur en français si la forme est mauvaise.
struct Body<T>(T);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> axum::extract::FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, ApiError> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(v)| Body(v))
            .map_err(|e| ApiError::bad_request(format!("corps JSON invalide : {}", e.body_text())))
    }
}

fn ok() -> Response {
    axum::Json(json!({ "ok": true })).into_response()
}

// ----- lecture -----

async fn get_state(State(s): State<AppState>) -> Response {
    axum::Json(StateDto::new(&s.core.state(), &s.covers)).into_response()
}

async fn command(State(s): State<AppState>, command: Command) -> ApiResult {
    s.core.send(command).await?;
    Ok(ok())
}

#[derive(Deserialize)]
struct SeekBody {
    position_ms: u64,
}

async fn seek(State(s): State<AppState>, Body(b): Body<SeekBody>) -> ApiResult {
    // La borne haute (durée du morceau) est vérifiée par le core.
    command(
        State(s),
        Command::Seek(Duration::from_millis(b.position_ms)),
    )
    .await
}

#[derive(Deserialize)]
struct ValueBody {
    value: u8,
}

async fn volume(State(s): State<AppState>, Body(b): Body<ValueBody>) -> ApiResult {
    if b.value > MAX_VOLUME {
        return Err(ApiError::bad_request("volume entre 0 et 100"));
    }
    command(State(s), Command::SetVolume(b.value)).await
}

#[derive(Deserialize)]
struct OnBody {
    on: bool,
}

async fn shuffle(State(s): State<AppState>, Body(b): Body<OnBody>) -> ApiResult {
    command(State(s), Command::SetShuffle(b.on)).await
}

async fn autoplay(State(s): State<AppState>, Body(b): Body<OnBody>) -> ApiResult {
    command(State(s), Command::SetAutoplay(b.on)).await
}

#[derive(Deserialize)]
struct RepeatBody {
    mode: String,
}

async fn repeat(State(s): State<AppState>, Body(b): Body<RepeatBody>) -> ApiResult {
    let mode =
        parse_repeat(&b.mode).ok_or_else(|| ApiError::bad_request("mode : off, all ou one"))?;
    command(State(s), Command::SetRepeat(mode)).await
}

async fn like() -> ApiResult {
    Err(ApiError(
        StatusCode::NOT_IMPLEMENTED,
        "les likes arrivent avec l'authentification (phase 6)".into(),
    ))
}

// ----- file d'attente -----

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

async fn get_queue(State(s): State<AppState>, Query(q): Query<PageQuery>) -> Response {
    let limit = q.limit.unwrap_or(MAX_QUEUE_PAGE).min(MAX_QUEUE_PAGE);
    axum::Json(QueueDto::new(&s.core.queue(), q.offset, limit, &s.covers)).into_response()
}

#[derive(Deserialize)]
struct PlayBody {
    tracks: Vec<TrackDto>,
    #[serde(default)]
    start: usize,
}

async fn queue_play(State(s): State<AppState>, Body(b): Body<PlayBody>) -> ApiResult {
    let tracks = tracks_from_dto(b.tracks).map_err(ApiError::bad_request)?;
    if b.start >= tracks.len() {
        return Err(ApiError::bad_request("start hors de la liste"));
    }
    command(
        State(s),
        Command::PlayTracks {
            tracks,
            start: b.start,
        },
    )
    .await
}

#[derive(Deserialize)]
struct TracksBody {
    tracks: Vec<TrackDto>,
}

async fn queue_add(State(s): State<AppState>, Body(b): Body<TracksBody>) -> ApiResult {
    let tracks = tracks_from_dto(b.tracks).map_err(ApiError::bad_request)?;
    command(State(s), Command::Enqueue(tracks)).await
}

async fn queue_next(State(s): State<AppState>, Body(b): Body<TracksBody>) -> ApiResult {
    let tracks = tracks_from_dto(b.tracks).map_err(ApiError::bad_request)?;
    command(State(s), Command::PlayNext(tracks)).await
}

#[derive(Deserialize)]
struct IdBody {
    id: u64,
}

async fn queue_jump(State(s): State<AppState>, Body(b): Body<IdBody>) -> ApiResult {
    command(State(s), Command::JumpTo(QueueId(b.id))).await
}

async fn queue_remove(State(s): State<AppState>, Body(b): Body<IdBody>) -> ApiResult {
    command(State(s), Command::Remove(QueueId(b.id))).await
}

#[derive(Deserialize)]
struct MoveBody {
    id: u64,
    to: usize,
}

async fn queue_move(State(s): State<AppState>, Body(b): Body<MoveBody>) -> ApiResult {
    command(
        State(s),
        Command::Move {
            id: QueueId(b.id),
            to: b.to,
        },
    )
    .await
}

#[derive(Deserialize)]
struct PrefetchBody {
    video_id: String,
}

async fn prefetch(State(s): State<AppState>, Body(b): Body<PrefetchBody>) -> ApiResult {
    if !is_video_id(&b.video_id) {
        return Err(ApiError::bad_request("video_id invalide"));
    }
    command(State(s), Command::Prefetch(b.video_id)).await
}

// ----- catalogue -----

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(State(s): State<AppState>, Query(q): Query<SearchQuery>) -> ApiResult {
    let query = q.q.trim();
    if query.is_empty() || query.len() > MAX_QUERY_LEN {
        return Err(ApiError::bad_request("recherche vide ou trop longue"));
    }
    let results = s.catalog.search_all(query).await?;
    Ok(axum::Json(SearchDto::new(&results, &s.covers)).into_response())
}

async fn album(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult {
    if !is_browse_id(&id) {
        return Err(ApiError::bad_request("identifiant d'album invalide"));
    }
    let album = s.catalog.album(&id).await?;
    Ok(axum::Json(AlbumDto::new(&album, &s.covers)).into_response())
}

async fn artist(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult {
    if !is_browse_id(&id) {
        return Err(ApiError::bad_request("identifiant d'artiste invalide"));
    }
    let artist = s.catalog.artist(&id).await?;
    Ok(axum::Json(ArtistDto::new(&artist, &s.covers)).into_response())
}

// ----- jaquettes -----

#[derive(Deserialize)]
struct CoverQuery {
    size: Option<u32>,
    fmt: Option<String>,
}

async fn cover(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<CoverQuery>,
) -> ApiResult {
    let format = match q.fmt.as_deref().unwrap_or("jpg") {
        "jpg" | "jpeg" => CoverFormat::Jpeg,
        "rgb565" => CoverFormat::Rgb565,
        _ => return Err(ApiError::bad_request("fmt : jpg ou rgb565")),
    };
    let size = q.size.unwrap_or(120);
    let data = s.covers.get(&id, size, format).await?;
    let side = HeaderValue::from(size);
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(format.content_type()),
            ),
            // Une variante (id, taille, format) ne change jamais.
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
            (header::HeaderName::from_static("x-width"), side.clone()),
            (header::HeaderName::from_static("x-height"), side),
        ],
        data,
    )
        .into_response())
}

async fn accent(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult {
    if let Some(hex) = s.accents.lock().ok().and_then(|m| m.get(&id).cloned()) {
        return Ok(axum::Json(json!({ "accent": hex })).into_response());
    }
    let [r, g, b] = s.covers.accent(&id).await?;
    let hex = format!("#{r:02x}{g:02x}{b:02x}");
    if let Ok(mut map) = s.accents.lock() {
        map.insert(id, hex.clone());
    }
    Ok((
        [(header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
        axum::Json(json!({ "accent": hex })),
    )
        .into_response())
}

// ----- divers -----

async fn not_found() -> Response {
    ApiError(StatusCode::NOT_FOUND, "route inconnue".into()).into_response()
}
