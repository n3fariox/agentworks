//! Derive the public root origin from the reverse proxy's forwarded headers.
use axum::http::{HeaderMap, Uri};

/// Return the canonical forwarded origin when both proxy headers are singular
/// and valid. The deployment must keep the backend private and have its proxy
/// overwrite these headers.
pub fn forwarded_origin(headers: &HeaderMap) -> Option<String> {
    let forwarded_scheme = single_header(headers, "x-forwarded-proto")?;
    let scheme = match forwarded_scheme {
        "http" | "ws" => "http",
        "https" | "wss" => "https",
        _ => return None,
    };
    let authority = single_header(headers, "x-forwarded-host")?;
    let authority: axum::http::uri::Authority = authority.parse().ok()?;
    if authority.as_str().contains('@') || authority.as_str().contains(',') {
        return None;
    }
    let authority = authority.as_str().to_ascii_lowercase();
    let default_port = if scheme == "https" { ":443" } else { ":80" };
    let authority = authority.strip_suffix(default_port).unwrap_or(&authority);
    Some(format!("{scheme}://{authority}"))
}

pub fn origin_matches_forwarded(headers: &HeaderMap, origin: &str) -> bool {
    let Some(expected) = forwarded_origin(headers) else {
        return false;
    };
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    let Some(scheme) = uri.scheme_str() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    if !matches!(scheme, "http" | "https")
        || authority.as_str().contains('@')
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
        || origin.contains('#')
    {
        return false;
    }
    let authority = authority.as_str().to_ascii_lowercase();
    let default_port = if scheme == "https" { ":443" } else { ":80" };
    let authority = authority.strip_suffix(default_port).unwrap_or(&authority);
    expected == format!("{scheme}://{authority}")
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() || value.is_empty() || value.contains(',') {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy_headers(scheme: &str, host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", scheme.parse().unwrap());
        headers.insert("x-forwarded-host", host.parse().unwrap());
        headers
    }

    #[test]
    fn forwarded_origin_is_derived_from_proxy_headers() {
        let headers = proxy_headers("https", "AgentBox.example:443");
        assert_eq!(
            forwarded_origin(&headers).as_deref(),
            Some("https://agentbox.example")
        );
        assert!(origin_matches_forwarded(
            &headers,
            "https://agentbox.example/"
        ));
        assert!(!origin_matches_forwarded(&headers, "https://other.example"));
        assert!(!origin_matches_forwarded(
            &headers,
            "http://agentbox.example"
        ));

        let websocket_headers = proxy_headers("wss", "AgentBox.example:443");
        assert_eq!(
            forwarded_origin(&websocket_headers).as_deref(),
            Some("https://agentbox.example")
        );
        assert!(origin_matches_forwarded(
            &websocket_headers,
            "https://agentbox.example"
        ));
    }

    #[test]
    fn rejects_ambiguous_or_invalid_proxy_headers() {
        for (scheme, host) in [
            ("https,http", "agentbox.example"),
            ("https", "agentbox.example,other.example"),
            ("javascript", "agentbox.example"),
            ("https", "user@agentbox.example"),
            ("https", "agentbox.example/path"),
        ] {
            assert!(forwarded_origin(&proxy_headers(scheme, host)).is_none());
        }
        let mut headers = proxy_headers("https", "agentbox.example");
        headers.append("x-forwarded-proto", "http".parse().unwrap());
        assert!(forwarded_origin(&headers).is_none());
    }
}
