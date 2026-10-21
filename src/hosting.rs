//! Optional install assets. Public mount paths are resolved by the browser, not
//! inferred from untrusted forwarding headers. The proxy strips its mount.
use axum::{
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};

pub fn enabled() -> bool {
    !std::env::var("AGENTWORKS_PWA").is_ok_and(|value| value == "0")
}

fn metadata() -> &'static str {
    if enabled() {
        concat!(
            "<meta name=\"agentworks-pwa\" content=\"enabled\">",
            "<meta name=\"theme-color\" content=\"#101214\">",
            "<link rel=\"manifest\" href=\"./manifest.webmanifest\">",
            "<link rel=\"apple-touch-icon\" href=\"./icon-192.png\">"
        )
    } else {
        "<meta name=\"agentworks-pwa\" content=\"disabled\">"
    }
}

pub async fn index() -> Html<String> {
    Html(include_str!("../web/index.html").replace("<!-- PWA_METADATA -->", metadata()))
}

pub async fn asset(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    let (mime, body): (&str, &[u8]) = match name.as_str() {
        "dashboard" => {
            let page = include_str!("../web/dashboard.html");
            return Html(page.replace("<!-- PWA_METADATA -->", metadata())).into_response();
        }
        "dashboard.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/dashboard.js"),
        ),
        "dashboard.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../web/dashboard.css"),
        ),
        "review-queue.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/review-queue.js"),
        ),
        "terminal.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/terminal.js"),
        ),
        "hosting.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/hosting.js"),
        ),
        "worktree-ownership.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/worktree-ownership.js"),
        ),
        "controllers.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/controllers.js"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, extract::Path};

    #[tokio::test]
    async fn dashboard_assets_are_served_with_explicit_mime_types() {
        for (name, mime, marker) in [
            (
                "dashboard",
                "text/html; charset=utf-8",
                "Operations dashboard",
            ),
            ("dashboard.js", "text/javascript; charset=utf-8", "appUrl"),
            (
                "dashboard.css",
                "text/css; charset=utf-8",
                ".operations-dashboard",
            ),
        ] {
            let response = asset(Path(name.to_string())).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], mime);
            let body = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
            assert!(std::str::from_utf8(&body).unwrap().contains(marker));
        }
        assert_eq!(
            asset(Path("dashboard-private".into())).await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn dashboard_links_keep_the_public_proxy_mount() {
        let page = index().await.0;
        assert!(page.contains("id=\"nav-dashboard\""));
        assert!(page.contains("href=\"./dashboard\""));
        let dashboard = include_str!("../web/dashboard.html");
        assert!(dashboard.contains("src=\"./dashboard.js\""));
        assert!(dashboard.contains("href=\"./dashboard.css\""));
        assert!(dashboard.contains("href=\"./shell.css\""));
        assert!(page.contains("href=\"./shell.css\""));
        assert!(!dashboard.contains("<base"));
    }
}
