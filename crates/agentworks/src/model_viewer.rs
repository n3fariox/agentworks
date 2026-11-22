//! Assets exist only in builds that opt into model previews.
use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

pub async fn asset(Path(name): Path<String>) -> Response {
    let result: Option<(&str, &[u8])> =
        include!(concat!(env!("OUT_DIR"), "/model_viewer_assets.rs"));
    match result {
        Some((mime, body)) => ([(header::CONTENT_TYPE, mime)], body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
