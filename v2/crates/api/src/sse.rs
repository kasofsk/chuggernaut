//! NATS stream → SSE bridge with Last-Event-ID replay (spec §6.4).
//!
//! Each connection gets an ephemeral JetStream consumer on the `job-events`
//! stream, filtered to the project (or job) subject space, starting after the
//! client's `Last-Event-ID` (the NATS stream sequence). Events are forwarded
//! verbatim — the dispatcher already embeds `event_type` in every payload.

use crate::SharedState;
use crate::routes::{ApiError, Auth};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use std::convert::Infallible;

fn last_event_id(headers: &HeaderMap) -> u64 {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

async fn bridge(
    state: &SharedState,
    filter: String,
    after_seq: u64,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>> + use<>>, ApiError> {
    let sub = state
        .store
        .subscribe_stream(store::buckets::STREAM_JOB_EVENTS, &filter, after_seq)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let stream = futures::stream::unfold(sub, |mut sub| async move {
        let (seq, _subject, payload) = sub.next().await?;
        let event = Event::default()
            .id(seq.to_string())
            .data(String::from_utf8_lossy(&payload));
        Some((Ok(event), sub))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub async fn project_events(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    auth::authorize(
        &identity,
        &auth::Action::ReadProject {
            project: format!("{owner}/{project}"),
        },
    )
    .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "insufficient role"))?;
    let filter = format!("job.events.{owner}.{project}.>");
    bridge(&state, filter, last_event_id(&headers)).await
}

pub async fn job_events(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    auth::authorize(
        &identity,
        &auth::Action::ReadProject {
            project: format!("{owner}/{project}"),
        },
    )
    .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "insufficient role"))?;
    let filter = format!("job.events.{owner}.{project}.{seq}.>");
    bridge(&state, filter, last_event_id(&headers)).await
}
