//! Optional install assets. Public mount paths are resolved by the browser, not
//! inferred from untrusted forwarding headers. The proxy strips its mount.
use axum::{
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};

pub fn enabled() -> bool {
    !std::env::var("AGENTWORKS_PWA").is_ok_and(|value| value == "0")
}

pub async fn index() -> Html<String> {
    let metadata = if enabled() {
        concat!(
            "<meta name=\"agentworks-pwa\" content=\"enabled\">",
            "<meta name=\"theme-color\" content=\"#101214\">",
            "<link rel=\"manifest\" href=\"./manifest.webmanifest\">",
            "<link rel=\"apple-touch-icon\" href=\"./icon-192.png\">"
        )
    } else {
        "<meta name=\"agentworks-pwa\" content=\"disabled\">"
    };
    Html(include_str!("../web/index.html").replace("<!-- PWA_METADATA -->", metadata))
}

pub async fn asset(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    let (mime, body): (&str, &[u8]) = match name.as_str() {
        "hosting.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/hosting.js"),
        ),
        "pwa.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/pwa.js"),
        ),
        "manifest.webmanifest" if enabled() => (
            "application/manifest+json",
            include_bytes!("../web/manifest.webmanifest"),
        ),
        "service-worker.js" if enabled() => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/service-worker.js"),
        ),
        "icon-192.png" if enabled() => ("image/png", include_bytes!("../web/icon-192.png")),
        "icon-512.png" if enabled() => ("image/png", include_bytes!("../web/icon-512.png")),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, mime)], body).into_response()
}
