use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::Event as SseEvent;
use axum::response::{IntoResponse, Sse};
use futures_util::stream::{self, StreamExt as _};
use kiliax_core::session::SessionId;
use tokio::sync::broadcast;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn list_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(list_events)
}

pub(in crate::http) fn stream_sse_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(stream_events_sse)
}

pub(in crate::http) fn stream_ws_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(stream_events_ws)
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    after: Option<u64>,
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/events",
    tags = ["Events"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("limit" = Option<usize>, Query, description = "Max number of items to return."),
        ("after" = Option<u64>, Query, description = "Return events after this event id (exclusive).")
    ),
    responses(
        (status = 200, body = crate::api::EventListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn list_events(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let limit = q.limit.unwrap_or(50);
    let out = state.list_events(&id, limit, q.after).await?;
    Ok(axum::Json(out))
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    #[serde(default)]
    after_event_id: Option<u64>,
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/events/stream",
    tags = ["Events"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("Last-Event-ID" = Option<String>, Header, description = "Resume from last event id (exclusive)."),
        ("after_event_id" = Option<u64>, Query, description = "Start streaming after this event id (exclusive).")
    ),
    responses(
        (status = 200, content_type = "text/event-stream", body = String),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn stream_events_sse(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let after = headers
        .get("Last-Event-ID")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or(q.after_event_id)
        .unwrap_or(0);

    let live = state.ensure_live(&id).await?;
    let backlog = state.events_backlog_after(&id, after, usize::MAX).await?;
    let rx = live.subscribe_events();
    let shutdown = state.shutdown.clone();

    let backlog_stream = stream::iter(
        backlog
            .into_iter()
            .map(|e| Ok::<_, std::convert::Infallible>(e)),
    );
    let live_stream = stream::unfold(
        LiveSseState {
            rx,
            live,
            session_id: id.to_string(),
            shutdown,
            done: false,
        },
        |mut st| async move {
            if st.done {
                return None;
            }
            tokio::select! {
                _ = st.shutdown.notified() => None,
                received = st.rx.recv() => match received {
                    Ok(ev) => Some((Ok::<_, std::convert::Infallible>(ev), st)),
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let last_event_id = st.live.last_event_id().await;
                        let ev = events_lagged_event(&st.session_id, missed, last_event_id);
                        st.done = true;
                        Some((Ok::<_, std::convert::Infallible>(ev), st))
                    }
                    Err(_) => None,
                }
            }
        },
    );

    let out = backlog_stream.chain(live_stream).map(|item| {
        let ev = item.unwrap();
        let json = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
        Ok::<SseEvent, std::convert::Infallible>(
            SseEvent::default()
                .id(ev.event_id.to_string())
                .event(ev.event_type.clone())
                .data(json),
        )
    });

    Ok(Sse::new(out).keep_alive(axum::response::sse::KeepAlive::default()))
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/events/ws",
    tags = ["Events"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("after_event_id" = Option<u64>, Query, description = "Start streaming after this event id (exclusive).")
    ),
    responses(
        (status = 101, description = "Switching Protocols"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn stream_events_ws(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<StreamQuery>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let after = q.after_event_id.unwrap_or(0);
    let live = state.ensure_live(&id).await?;
    let backlog = state.events_backlog_after(&id, after, usize::MAX).await?;
    let mut rx = live.subscribe_events();
    let session_id = id.to_string();
    let shutdown = state.shutdown.clone();

    Ok(ws.on_upgrade(move |mut socket| async move {
        for ev in backlog {
            if let Ok(text) = serde_json::to_string(&ev) {
                let _ = socket
                    .send(axum::extract::ws::Message::Text(text.into()))
                    .await;
            }
        }
        loop {
            tokio::select! {
                _ = shutdown.notified() => break,
                received = rx.recv() => match received {
                    Ok(ev) => {
                        if let Ok(text) = serde_json::to_string(&ev) {
                            if socket
                                .send(axum::extract::ws::Message::Text(text.into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let last_event_id = live.last_event_id().await;
                        let ev = events_lagged_event(&session_id, missed, last_event_id);
                        if let Ok(text) = serde_json::to_string(&ev) {
                            let _ = socket
                                .send(axum::extract::ws::Message::Text(text.into()))
                                .await;
                        }
                        break;
                    }
                    Err(_) => break,
                }
            };
        }
    }))
}

struct LiveSseState {
    rx: broadcast::Receiver<crate::api::Event>,
    live: Arc<crate::state::LiveSession>,
    session_id: String,
    shutdown: Arc<tokio::sync::Notify>,
    done: bool,
}

fn events_lagged_event(session_id: &str, missed: u64, last_event_id: u64) -> crate::api::Event {
    crate::api::Event {
        event_id: last_event_id,
        ts: now_rfc3339(),
        session_id: session_id.to_string(),
        run_id: None,
        event_type: "events_lagged".to_string(),
        data: serde_json::json!({
            "missed": missed,
            "last_event_id": last_event_id,
        }),
    }
}

fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
