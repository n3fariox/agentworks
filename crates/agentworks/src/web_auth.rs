//! Opt-in browser sign-in with the host's PAM stack. Only offered over HTTPS,
//! direct or through a trusted proxy, so passwords and session cookies never
//! cross the network in clear.
use crate::{App, browser_origin, browser_origin::TrustedProxies, pam, tls::ClientAddr};
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
const FAILURE_DELAY: Duration = Duration::from_secs(2);
/// Cookies are named per public app URL and scoped to its mount, so several
/// instances on one domain keep separate sign-ins.
const COOKIE_PREFIX: &str = "__Secure-agentworks-";
/// Served without a session: the sign-in page and what the browser fetches
/// without credentials while installing the app.
const PUBLIC: &[&str] = &[
    "/api/auth/login",
    "/login.js",
    "/shell.css",
    "/manifest.webmanifest",
    "/service-worker.js",
    "/icon-192.png",
    "/icon-512.png",
];

#[derive(Default)]
pub struct WebAuth {
    /// The listener serves HTTPS itself.
    pub secure: bool,
    pub proxies: TrustedProxies,
    pam: Option<PamLogin>,
}

struct PamLogin {
    service: String,
    /// Replaces /etc/pam.d (tests).
    confdir: Option<PathBuf>,
    users: Vec<String>,
    path: PathBuf,
    /// SHA-256 of the cookie token → grant. Only hashes are written to disk.
    grants: Mutex<HashMap<String, Grant>>,
    /// One PAM conversation at a time; failures hold it for FAILURE_DELAY.
    attempts: tokio::sync::Mutex<()>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    user: String,
    expires: u64,
    /// Name and path of the cookie holding this grant, for sign-out.
    cookie: String,
    mount: String,
}

impl WebAuth {
    /// `grants_path` keeps sign-ins across service restarts.
    pub fn configure(secure: bool, grants_path: PathBuf) -> Result<Self, String> {
        let proxies = TrustedProxies::from_env()?;
        let Some(service) = std::env::var("AGENTWORKS_PAM_SERVICE")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return Ok(Self {
                secure,
                proxies,
                pam: None,
            });
        };
        if !secure && !proxies.configured() {
            return Err("AGENTWORKS_PAM_SERVICE requires HTTPS: set AGENTWORKS_TLS_CERT and AGENTWORKS_TLS_KEY, or AGENTWORKS_TRUSTED_PROXIES for a TLS proxy".into());
        }
        let confdir = std::env::var_os("AGENTWORKS_PAM_CONFDIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        pam::check_service(&service, confdir.as_deref())?;
        // Every user who signs in controls the agents running as this server's user.
        let users = match std::env::var("AGENTWORKS_PAM_USERS") {
            Ok(list) => list
                .split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned)
                .collect(),
            Err(_) => pam::current_user().into_iter().collect::<Vec<_>>(),
        };
        if users.is_empty() {
            return Err("AGENTWORKS_PAM_USERS names no users".into());
        }
        let grants = load(&grants_path);
        Ok(Self {
            secure,
            proxies,
            pam: Some(PamLogin {
                service,
                confdir,
                users,
                path: grants_path,
                grants: Mutex::new(grants),
                attempts: Default::default(),
            }),
        })
    }

    pub fn describe(&self) -> Option<String> {
        self.pam.as_ref().map(|p| {
            format!(
                "PAM sign-in through service {:?} for {}",
                p.service,
                p.users.join(", ")
            )
        })
    }

    /// `None` lets the request through; otherwise the sign-in page for app
    /// pages and 401 for everything else.
    pub fn deny(
        &self,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        peer: Option<IpAddr>,
    ) -> Option<Response> {
        let pam = self.pam.as_ref()?;
        // Without our own TLS, only HTTPS requests relayed by a trusted proxy may
        // carry passwords or cookies; a direct plain-HTTP client gets nothing.
        if !self.secure && !(self.proxies.trusts(peer) && browser_origin::forwarded_https(headers))
        {
            return Some(
                (
                    StatusCode::FORBIDDEN,
                    "Sign-in requires HTTPS through a trusted proxy",
                )
                    .into_response(),
            );
        }
        if PUBLIC.contains(&path) || pam.signed_in(headers).is_some() {
            return None;
        }
        if method == Method::GET && matches!(path, "/" | "/dashboard") {
            return Some(
                (
                    StatusCode::UNAUTHORIZED,
                    Html(include_str!("../../../web/login.html")),
                )
                    .into_response(),
            );
        }
        // The browser reloads into the sign-in page when it sees this header.
        Some(
            (
                StatusCode::UNAUTHORIZED,
                [("x-agentworks-auth", "required")],
                "Sign in required",
            )
                .into_response(),
        )
    }

    /// The browser's address: the proxy's last `X-Forwarded-For` hop when the
    /// peer is a configured proxy, otherwise the peer itself.
    fn client(&self, headers: &HeaderMap, peer: IpAddr) -> IpAddr {
        if self.proxies.configured() && self.proxies.trusts(Some(peer)) {
            browser_origin::forwarded_client(headers).unwrap_or(peer)
        } else {
            peer
        }
    }
}

impl PamLogin {
    /// Any of our cookies may arrive (a root mount's cookie also reaches nested
    /// mounts), so look for one whose token this server issued.
    fn current(&self, headers: &HeaderMap) -> Option<(String, Grant)> {
        let grants = self.grants.lock().unwrap();
        let time = now();
        cookies(headers).find_map(|token| {
            let key = digest(token);
            let grant = grants.get(&key).filter(|g| g.expires > time)?;
            Some((key, grant.clone()))
        })
    }

    fn signed_in(&self, headers: &HeaderMap) -> Option<String> {
        self.current(headers).map(|(_, grant)| grant.user)
    }

    /// Returns the token and its `Set-Cookie` value for app URL `base`.
    fn grant(&self, user: &str, base: &str) -> Result<(String, HeaderValue), String> {
        let mount = mount(base)?;
        let cookie = format!("{COOKIE_PREFIX}{}", &digest(base)[..16]);
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut grants = self.grants.lock().unwrap();
        let time = now();
        grants.retain(|_, g| g.expires > time);
        grants.insert(
            digest(&token),
            Grant {
                user: user.into(),
                expires: time + LIFETIME.as_millis() as u64,
                cookie: cookie.clone(),
                mount: mount.into(),
            },
        );
        save(&self.path, &grants)?;
        let header = set_cookie(&cookie, mount, &token, LIFETIME.as_secs());
        Ok((token, header))
    }

    /// Removes the request's grant; returns the `Set-Cookie` that clears it.
    fn revoke(&self, headers: &HeaderMap) -> Result<Option<HeaderValue>, String> {
        let Some((key, grant)) = self.current(headers) else {
            return Ok(None);
        };
        let mut grants = self.grants.lock().unwrap();
        grants.remove(&key);
        save(&self.path, &grants)?;
        Ok(Some(set_cookie(&grant.cookie, &grant.mount, "", 0)))
    }
}

fn set_cookie(name: &str, mount: &str, token: &str, max_age: u64) -> HeaderValue {
    // Lax keeps links from other sites working; writes still require a same-origin Origin.
    format!("{name}={token}; Path={mount}; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}")
        .parse()
        .unwrap()
}

/// The path of the browser's public app URL (`https://host/mount/`). The proxy
/// strips the mount, so only the browser knows it.
fn mount(base: &str) -> Result<&str, String> {
    let path = base
        .strip_prefix("https://")
        .and_then(|rest| rest.find('/').map(|i| &rest[i..]))
        .ok_or("App URL must be an https URL")?;
    let safe = path.len() <= 512
        && path.starts_with('/')
        && path.ends_with('/')
        && path
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b";,\"\\?#".contains(&b));
    safe.then_some(path).ok_or_else(|| "Invalid app URL".into())
}

#[derive(Deserialize)]
pub struct Credentials {
    username: String,
    password: String,
    /// The browser's public app URL, which names and scopes the cookie.
    base: String,
}

pub async fn login(
    State(app): State<App>,
    ConnectInfo(ClientAddr(peer)): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> Response {
    let Some(pam) = &app.web_auth.pam else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // The guard has matched Origin to this server's public origin.
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let base_path = credentials
        .base
        .strip_prefix(origin)
        .filter(|path| !origin.is_empty() && path.starts_with('/'));
    if base_path.is_none() || mount(&credentials.base).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid app URL").into_response();
    }
    let peer = app.web_auth.client(&headers, peer.ip());
    let user = credentials.username.clone();
    let logged: String = user.chars().take(64).collect();
    let _attempt = pam.attempts.lock().await;
    let result = if credentials.username.len() > 256 || credentials.password.len() > 4096 {
        Err("credentials too long".to_string())
    } else if !pam.users.contains(&credentials.username) {
        Err("user is not in AGENTWORKS_PAM_USERS".to_string())
    } else {
        let service = pam.service.clone();
        let confdir = pam.confdir.clone();
        let rhost = peer.to_string();
        tokio::task::spawn_blocking(move || {
            pam::authenticate(
                &service,
                confdir.as_deref(),
                &credentials.username,
                &credentials.password,
                Some(&rhost),
            )
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
    };
    if let Err(e) = result {
        eprintln!("Web sign-in failed for {logged:?} from {peer}: {e}");
        tokio::time::sleep(FAILURE_DELAY).await;
        return (StatusCode::UNAUTHORIZED, "Sign-in failed").into_response();
    }
    match pam.grant(&user, &credentials.base) {
        Ok((_, cookie)) => {
            eprintln!("Web sign-in for {logged:?} from {peer}");
            ([(header::SET_COOKIE, cookie)], Json(json!({"user":user}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn logout(State(app): State<App>, headers: HeaderMap) -> Response {
    let Some(pam) = &app.web_auth.pam else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match pam.revoke(&headers) {
        Ok(Some(clear)) => ([(header::SET_COOKIE, clear)], Json(json!({}))).into_response(),
        Ok(None) => Json(json!({})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn status(State(app): State<App>, headers: HeaderMap) -> Json<serde_json::Value> {
    let user = app
        .web_auth
        .pam
        .as_ref()
        .and_then(|p| p.signed_in(&headers));
    Json(json!({"secure":app.web_auth.secure,"signIn":app.web_auth.pam.is_some(),"user":user}))
}

fn cookies(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key.starts_with(COOKIE_PREFIX) && !value.is_empty()).then_some(value)
        })
}

fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn load(path: &PathBuf) -> HashMap<String, Grant> {
    let time = now();
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HashMap<String, Grant>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, g)| g.expires > time)
        .collect()
}

fn save(path: &PathBuf, grants: &HashMap<String, Grant>) -> Result<(), String> {
    use std::io::Write;
    let temp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let write = || -> std::io::Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(&serde_json::to_vec(grants)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    };
    write().map_err(|e| format!("Cannot save sign-ins to {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://agents.example/a1b918dd/";

    fn login(path: PathBuf) -> PamLogin {
        PamLogin {
            service: "login".into(),
            confdir: None,
            users: vec!["alice".into()],
            path,
            grants: Default::default(),
            attempts: Default::default(),
        }
    }

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("agentworks-grants-{}.json", uuid::Uuid::new_v4()))
    }

    /// The `name=value` a browser would send back for a `Set-Cookie` header.
    fn sent(set_cookie: &HeaderValue) -> String {
        set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    fn with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, value.parse().unwrap());
        headers
    }

    #[test]
    fn grants_survive_restart_and_revoke() {
        let path = temp();
        let pam = login(path.clone());
        let (token, set) = pam.grant("alice", BASE).unwrap();
        let text = set.to_str().unwrap();
        assert!(text.starts_with(COOKIE_PREFIX));
        assert!(
            text.contains("; Path=/a1b918dd/;")
                && text.contains("Secure")
                && text.contains("HttpOnly")
        );
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains(&token), "only token hashes are stored");
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&path).unwrap().permissions()
            ) & 0o777,
            0o600
        );
        let headers = with_cookie(&format!("theme=dark; {}", sent(&set)));
        assert_eq!(pam.signed_in(&headers).as_deref(), Some("alice"));
        assert_eq!(pam.signed_in(&with_cookie(&format!("other={token}"))), None);
        assert_eq!(
            pam.signed_in(&with_cookie(&format!("{COOKIE_PREFIX}x={token}x"))),
            None
        );

        let restarted = PamLogin {
            grants: Mutex::new(load(&path)),
            ..login(path.clone())
        };
        assert_eq!(restarted.signed_in(&headers).as_deref(), Some("alice"));
        let clear = restarted.revoke(&headers).unwrap().unwrap();
        let clear = clear.to_str().unwrap();
        assert!(clear.starts_with(&format!("{}=;", text.split('=').next().unwrap())));
        assert!(clear.contains("Path=/a1b918dd/;") && clear.contains("Max-Age=0"));
        assert_eq!(restarted.signed_in(&headers), None);
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mounts_get_separate_cookies() {
        let path = temp();
        let pam = login(path.clone());
        let (_, first) = pam.grant("alice", BASE).unwrap();
        let (_, second) = pam
            .grant("alice", "https://agents.example/second/")
            .unwrap();
        let (_, root) = pam.grant("alice", "https://agents.example/").unwrap();
        let name = |v: &HeaderValue| sent(v).split('=').next().unwrap().to_string();
        assert_ne!(name(&first), name(&second));
        assert_ne!(name(&first), name(&root));
        assert!(root.to_str().unwrap().contains("Path=/;"));
        // A nested mount also receives the root's cookie; signing out clears one grant at a time.
        let both = with_cookie(&format!("{}; {}", sent(&root), sent(&first)));
        pam.revoke(&both).unwrap();
        assert_eq!(pam.signed_in(&both).as_deref(), Some("alice"));
        pam.revoke(&both).unwrap();
        assert_eq!(pam.signed_in(&both), None);
        assert_eq!(
            pam.signed_in(&with_cookie(&sent(&second))).as_deref(),
            Some("alice")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn app_urls_are_validated() {
        assert_eq!(mount("https://h/"), Ok("/"));
        assert_eq!(mount("https://h:4424/a%20b/"), Ok("/a%20b/"));
        for bad in [
            "http://h/",
            "https://h",
            "https://h/a",
            "https://h/a;Domain=evil/",
            "https://h/a,b/",
            "https://h/a b/",
            "https://h/?x/",
        ] {
            assert!(mount(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn expired_grants_are_rejected_and_pruned() {
        let path = temp();
        let pam = login(path.clone());
        let (_, set) = pam.grant("alice", BASE).unwrap();
        pam.grants
            .lock()
            .unwrap()
            .values_mut()
            .for_each(|g| g.expires = 1);
        assert_eq!(pam.signed_in(&with_cookie(&sent(&set))), None);
        save(&path, &pam.grants.lock().unwrap()).unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn signed_out_requests_get_the_sign_in_page_or_401() {
        let auth = WebAuth {
            secure: true,
            proxies: Default::default(),
            pam: Some(login(temp())),
        };
        let empty = HeaderMap::new();
        let page = auth.deny(&Method::GET, "/", &empty, None).unwrap();
        assert_eq!(page.status(), StatusCode::UNAUTHORIZED);
        assert!(page.headers().get("x-agentworks-auth").is_none());
        let api = auth
            .deny(&Method::GET, "/api/status", &empty, None)
            .unwrap();
        assert_eq!(api.headers()["x-agentworks-auth"], "required");
        assert!(
            auth.deny(&Method::POST, "/api/sessions/x/prompt", &empty, None)
                .is_some()
        );
        assert!(auth.deny(&Method::GET, "/app.js", &empty, None).is_some());
        assert!(
            auth.deny(&Method::POST, "/api/auth/login", &empty, None)
                .is_none()
        );
        assert!(auth.deny(&Method::GET, "/login.js", &empty, None).is_none());
        assert!(
            WebAuth::default()
                .deny(&Method::GET, "/api/status", &empty, None)
                .is_none()
        );
    }

    #[test]
    fn plain_http_sign_in_needs_https_from_a_trusted_proxy() {
        let auth = WebAuth {
            secure: false,
            proxies: TrustedProxies::parse("127.0.0.1").unwrap(),
            pam: Some(login(temp())),
        };
        let proxy = "127.0.0.1".parse().ok();
        let mut https = HeaderMap::new();
        https.insert("x-forwarded-proto", "https".parse().unwrap());
        https.insert("x-forwarded-for", "6.6.6.6, 192.0.2.7".parse().unwrap());
        let mut http = HeaderMap::new();
        http.insert("x-forwarded-proto", "http".parse().unwrap());
        let none = HeaderMap::new();
        let forbidden = |headers, peer| {
            auth.deny(&Method::POST, "/api/auth/login", headers, peer)
                .is_some_and(|r| r.status() == StatusCode::FORBIDDEN)
        };
        assert!(!forbidden(&https, proxy));
        assert!(forbidden(&http, proxy));
        assert!(forbidden(&none, proxy));
        assert!(forbidden(&https, "192.0.2.7".parse().ok()));
        assert!(forbidden(&https, None));
        assert_eq!(
            auth.client(&https, proxy.unwrap()),
            "192.0.2.7".parse::<IpAddr>().unwrap()
        );
        let other = "192.0.2.50".parse().unwrap();
        assert_eq!(auth.client(&https, other), other);
    }

    #[test]
    fn pam_requires_https_or_a_trusted_proxy() {
        // Process-wide variables; no other test reads them.
        unsafe { std::env::set_var("AGENTWORKS_PAM_SERVICE", "login") };
        let plain = WebAuth::configure(false, temp());
        unsafe { std::env::remove_var("AGENTWORKS_PAM_SERVICE") };
        assert!(plain.err().unwrap().contains("requires HTTPS"));
    }
}
