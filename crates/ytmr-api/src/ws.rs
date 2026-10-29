//! WebSocket `/api/v1/ws` : l'état poussé en direct.
//!
//! En clair : au lieu de demander « où en est-on ? » sans arrêt, le client
//! garde une connexion ouverte et le serveur lui écrit dès que quelque chose
//! change. Messages JSON, chacun avec un champ `type` :
//! - `state` : l'état complet (à la connexion, puis à chaque vrai changement) ;
//! - `position` : une fois par seconde pendant la lecture ;
//! - `track_changed`, `queue_changed`, `error`.

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use ytmr_core::CoreEvent;

use crate::AppState;
use crate::dto::{StateDto, TrackDto};

const POSITION_INTERVAL: Duration = Duration::from_secs(1);

pub async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| session(socket, state))
}

async fn session(socket: WebSocket, app: AppState) {
    let (mut tx, mut rx) = socket.split();
    let mut state = app.core.watch_state();
    let mut events = app.core.subscribe();
    let mut ticker = tokio::time::interval(POSITION_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut last = StateDto::new(&state.borrow_and_update(), &app.covers);
    if send(&mut tx, typed("state", &last)).await.is_err() {
        return;
    }

    loop {
        let message = tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                let now = StateDto::new(&state.borrow_and_update(), &app.covers);
                let message = (!now.same_except_position(&last)).then(|| typed("state", &now));
                last = now;
                message
            }
            _ = ticker.tick() => last.playing.then(|| {
                json!({
                    "type": "position",
                    "position_ms": last.position_ms,
                    "duration_ms": last.duration_ms,
                })
            }),
            event = events.recv() => match event {
                Ok(CoreEvent::TrackChanged(entry)) => Some(json!({
                    "type": "track_changed",
                    "track": entry.map(|e| TrackDto::new(&e.track, &app.covers)),
                })),
                Ok(CoreEvent::QueueChanged) => Some(json!({ "type": "queue_changed" })),
                Ok(CoreEvent::Error(message)) => Some(json!({ "type": "error", "message": message })),
                Err(RecvError::Lagged(_)) => None,
                Err(RecvError::Closed) => break,
            },
            incoming = rx.next() => match incoming {
                // Le client n'envoie rien d'utile : on ne lit que pour voir la fermeture.
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => None,
            },
        };
        if let Some(message) = message
            && send(&mut tx, message).await.is_err()
        {
            break;
        }
    }
}

fn typed(kind: &str, payload: &impl serde::Serialize) -> Value {
    let mut value = serde_json::to_value(payload).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("type".into(), kind.into());
    }
    value
}

async fn send(
    tx: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: Value,
) -> Result<(), axum::Error> {
    tx.send(Message::Text(value.to_string().into())).await
}
