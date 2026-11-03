//! Derive the public root origin from the reverse proxy's forwarded headers.
use axum::http::{HeaderMap, Uri};
use std::net::IpAddr;

/// Peers whose forwarded headers are honoured, from `AGENTWORKS_TRUSTED_PROXIES`
/// (comma-separated addresses or CIDR ranges). Unset keeps the original contract:
/// any peer is trusted, so the backend must be reachable only by its proxy.
#[derive(Default)]
pub struct TrustedProxies(Option<Vec<(IpAddr, u8)>>);

impl TrustedProxies {
    pub fn from_env() -> Result<Self, String> {
        match std::env::var("AGENTWORKS_TRUSTED_PROXIES") {
            Ok(list) if !list.trim().is_empty() => Self::parse(&list),
            _ => Ok(Self(None)),
        }
    }

    pub fn parse(list: &str) -> Result<Self, String> {
        let entry = |item: &str| {
            let (ip, bits) = item.split_once('/').unwrap_or((item, ""));
            let ip = ip.parse::<IpAddr>().ok()?.to_canonical();
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let bits = if bits.is_empty() {
                max
            } else {
                bits.parse().ok()?
            };
            (bits <= max).then_some((ip, bits))
        };
        list.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| {
                entry(item)
                    .ok_or_else(|| format!("AGENTWORKS_TRUSTED_PROXIES: invalid entry {item:?}"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|list| Self(Some(list)))
    }

    pub fn configured(&self) -> bool {
        self.0.is_some()
    }

    pub fn trusts(&self, peer: Option<IpAddr>) -> bool {
        let Some(list) = &self.0 else {
            return true;
        };
        let Some(peer) = peer.map(|ip| ip.to_canonical()) else {
            return false;
        };
        list.iter().any(|&(net, bits)| match (net, peer) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - bits as u32).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - bits as u32).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        })
    }
}

/// The proxy reported an HTTPS (or WSS) browser connection.
pub fn forwarded_https(headers: &HeaderMap) -> bool {
    matches!(
        single_header(headers, "x-forwarded-proto"),
        Some("https" | "wss")
    )
}

/// The browser's address as appended by the proxy: the last `X-Forwarded-For`
/// hop. Earlier hops come from the client and are not trusted.
pub fn forwarded_client(headers: &HeaderMap) -> Option<IpAddr> {
    let last = headers.get_all("x-forwarded-for").iter().next_back()?;
    let hop = last.to_str().ok()?.rsplit(',').next()?.trim();
    hop.parse().ok()
}

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
    fn trusted_proxies_match_addresses_and_ranges() {
        let ip = |s: &str| Some(s.parse().unwrap());
        let proxies = TrustedProxies::parse("127.0.0.1, 10.1.0.0/16, fd00::/8").unwrap();
        assert!(proxies.configured());
        assert!(proxies.trusts(ip("127.0.0.1")));
        assert!(proxies.trusts(ip("::ffff:127.0.0.1")));
        assert!(!proxies.trusts(ip("127.0.0.2")));
        assert!(proxies.trusts(ip("10.1.200.3")));
        assert!(!proxies.trusts(ip("10.2.0.1")));
        assert!(proxies.trusts(ip("fd12::1")));
        assert!(!proxies.trusts(ip("::1")));
        assert!(!proxies.trusts(None));
        assert!(
            TrustedProxies::parse("0.0.0.0/0")
                .unwrap()
                .trusts(ip("192.0.2.1"))
        );
        assert!(TrustedProxies::default().trusts(None));
        for bad in ["localhost", "10.0.0.0/33", "::1/129", "10.0.0.1/x"] {
            assert!(TrustedProxies::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn forwarded_client_is_the_proxy_appended_hop() {
        let mut headers = HeaderMap::new();
        assert_eq!(forwarded_client(&headers), None);
        headers.append("x-forwarded-for", "6.6.6.6, 192.0.2.7".parse().unwrap());
        assert_eq!(forwarded_client(&headers), "192.0.2.7".parse().ok());
        headers.append("x-forwarded-for", "198.51.100.9".parse().unwrap());
        assert_eq!(forwarded_client(&headers), "198.51.100.9".parse().ok());
        assert!(forwarded_https(&proxy_headers("https", "a")));
        assert!(!forwarded_https(&proxy_headers("http", "a")));
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
