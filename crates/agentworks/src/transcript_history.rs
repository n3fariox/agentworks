//! Older transcript pages behind a bounded session snapshot. The snapshot keeps only
//! the recent event window; every stored event row stays in the database, so earlier
//! history is paged straight from it with an exclusive `before` sequence cursor.
//! Reading never changes retention.
use crate::{ApiResult, App, fail, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::json;

const DEFAULT_PAGE: usize = 500;
const MAX_PAGE: usize = 2000;
/// Byte budget per page, well under the 4 MiB snapshot window.
const MAX_PAGE_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    before: u64,
    limit: Option<usize>,
}

/// `events` are oldest first and all have seq < `before`. `earliestSeq` is the cursor for
/// the next page (null when nothing was returned); `hasMore: false` means the beginning
/// of the stored conversation has been reached.
pub async fn older(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> ApiResult {
    let limit = page.limit.unwrap_or(DEFAULT_PAGE);
    if page.before == 0 || !(1..=MAX_PAGE).contains(&limit) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("before must be a positive sequence and limit 1–{MAX_PAGE}"),
        ));
    }
    let s = session(&app, &id)?;
    let (events, more) = s
        .store
        .lock()
        .unwrap()
        .older_events(&id, page.before, limit, MAX_PAGE_BYTES)
        .map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not read saved transcript history",
            )
        })?;
    let earliest = events.first().map(|e| e["seq"].clone());
    Ok(Json(
        json!({"before":page.before,"events":events,"hasMore":more,"earliestSeq":earliest}),
    ))
}
