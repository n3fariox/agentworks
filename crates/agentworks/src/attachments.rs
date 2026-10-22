//! Chat prompt attachments.
//!
//! Uploads are stored per session under the server's data directory, never in the
//! workspace, and reach the harness only as embedded ACP content blocks with a
//! synthetic `agentworks://attachments/...` URI. Host paths are never sent.
//!
//! Each attachment is consumed by at most one prompt (bound to its request ID).
//! Retries with the same request ID are duplicates; nothing is replayed after
//! cancellation, queueing, quota continuation or restart.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};
use std::sync::OnceLock;

pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_PER_PROMPT: usize = 4;
pub const MAX_PENDING: usize = 8;
const MAX_NAME_CHARS: usize = 120;
/// Unsent uploads older than this are pruned on the session's next upload.
const PENDING_TTL_MS: u64 = 24 * 3600 * 1000;
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
/// Image input is enabled only where the harness passes image blocks to a model
/// that reads them. OpenCode is limited to free models whose vision support is unverified.
const IMAGE_PROVIDERS: &[&str] = &["claude", "codex", "demo"];
const TEXT_PROVIDERS: &[&str] = &["claude", "codex", "opencode", "demo"];

static ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn init(root: PathBuf) {
    let _ = ROOT.set(root);
}
fn root() -> Result<&'static PathBuf, (StatusCode, String)> {
    ROOT.get().ok_or_else(|| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Attachment storage unavailable",
        )
    })
}
fn session_dir(root: &FsPath, session_id: &str) -> PathBuf {
    root.join(session_id)
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Meta {
    pub id: String,
    pub name: String,
    pub mime: String,
    /// `image` or `text`.
    pub kind: String,
    pub size: usize,
    pub created: u64,
    /// Request ID of the prompt that consumed this attachment.
    #[serde(default)]
    pub consumed_by: Option<String>,
}
impl Meta {
    pub fn public(&self) -> Value {
        json!({"id":self.id,"name":self.name,"mime":self.mime,"kind":self.kind,"size":self.size})
    }
    fn uri(&self) -> String {
        let name: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("agentworks://attachments/{}/{}", self.id, name)
    }
}

/// Content handed to the runtime; built into ACP blocks there.
#[derive(Debug)]
pub enum Payload {
    Image {
        mime: String,
        base64: String,
    },
    Text {
        uri: String,
        mime: String,
        text: String,
    },
}

/// What this session can accept, derived from the provider and the capabilities
/// the adapter advertised at initialization. Unknown until the agent has started.
pub fn policy(provider: &str, caps: &Value) -> Value {
    let known = caps.is_object();
    let image = known && caps["image"] == true && IMAGE_PROVIDERS.contains(&provider);
    let text = known && caps["embeddedContext"] == true && TEXT_PROVIDERS.contains(&provider);
    let reason = |ok: bool, kind: &str| -> Value {
        if ok {
            Value::Null
        } else if !known {
            json!("Available once the agent has started")
        } else if kind == "image" && provider == "opencode" {
            json!("OpenCode's free models are not verified to read images")
        } else {
            json!(format!("This agent does not accept {kind} attachments"))
        }
    };
    json!({"image":image,"text":text,"imageReason":reason(image,"image"),"textReason":reason(text,"text"),
        "imageTypes":IMAGE_TYPES,"maxImageBytes":MAX_IMAGE_BYTES,"maxTextBytes":MAX_TEXT_BYTES,
        "maxPerPrompt":MAX_PER_PROMPT,
        "unsupported":"PDFs and other binary files are not supported: no harness here passes their content to the model."})
}
pub fn session_policy(s: &Session) -> Value {
    policy(&s.provider, &s.data.lock().unwrap().prompt_capabilities)
}

fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}
fn text_mime(name: &str) -> &'static str {
    match name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        _ => "text/plain",
    }
}
/// Classify uploaded bytes by content, never by the browser-claimed type.
pub fn classify(name: &str, bytes: &[u8]) -> Result<(&'static str, &'static str), String> {
    if bytes.is_empty() {
        return Err("The file is empty".into());
    }
    if let Some(mime) = sniff_image(bytes) {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "Images are limited to {} MB",
                MAX_IMAGE_BYTES / 1024 / 1024
            ));
        }
        return Ok(("image", mime));
    }
    if bytes.starts_with(b"%PDF") {
        return Err("PDFs are not supported: no harness here passes PDF content to the model. Paste the relevant text instead.".into());
    }
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|t| !t.contains('\0'));
    let Some(_) = text else {
        return Err(
            "Unsupported file type. Attach PNG, JPEG, GIF or WebP images, or UTF-8 text files."
                .into(),
        );
    };
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(format!(
            "Text files are limited to {} KB",
            MAX_TEXT_BYTES / 1024
        ));
    }
    Ok(("text", text_mime(name)))
}
/// Display name only: no directories, control characters or path separators.
pub fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let name: String = base
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let name = name.trim().trim_start_matches('.').trim().to_owned();
    if name.is_empty() {
        "attachment".into()
    } else {
        name
    }
}
fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|u| u.hyphenated().to_string() == id)
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn write_private(path: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)
}
fn create_dir(dir: &FsPath) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}
fn read_meta(dir: &FsPath, id: &str) -> Option<Meta> {
    if !valid_id(id) {
        return None;
    }
    serde_json::from_slice(&std::fs::read(dir.join(format!("{id}.json"))).ok()?).ok()
}
fn save_meta(dir: &FsPath, meta: &Meta) -> std::io::Result<()> {
    write_private(
        &dir.join(format!("{}.json", meta.id)),
        &serde_json::to_vec(meta).unwrap_or_default(),
    )
}
fn list(dir: &FsPath) -> Vec<Meta> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            read_meta(dir, name.strip_suffix(".json")?)
        })
        .collect()
}
fn remove(dir: &FsPath, id: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{id}.bin")));
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
}

/// Store one upload; prunes stale unsent uploads first.
pub fn store(
    root: &FsPath,
    session_id: &str,
    name: &str,
    bytes: &[u8],
    allowed: &Value,
) -> Result<Meta, (StatusCode, String)> {
    let name = clean_name(name);
    let (kind, mime) =
        classify(&name, bytes).map_err(|e| fail(StatusCode::UNSUPPORTED_MEDIA_TYPE, e))?;
    if allowed[kind] != true {
        let reason = allowed[format!("{kind}Reason")]
            .as_str()
            .unwrap_or("Not supported by this agent");
        return Err(fail(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "{} attachments are unavailable: {reason}",
                if kind == "image" { "Image" } else { "Text" }
            ),
        ));
    }
    let dir = session_dir(root, session_id);
    let now = now_ms();
    let mut pending = 0;
    for meta in list(&dir) {
        if meta.consumed_by.is_none() {
            if now.saturating_sub(meta.created) > PENDING_TTL_MS {
                remove(&dir, &meta.id);
            } else {
                pending += 1;
            }
        }
    }
    if pending >= MAX_PENDING {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("Up to {MAX_PENDING} unsent attachments per session; remove some first"),
        ));
    }
    let meta = Meta {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        mime: mime.into(),
        kind: kind.into(),
        size: bytes.len(),
        created: now,
        consumed_by: None,
    };
    let saved = create_dir(&dir)
        .and_then(|_| write_private(&dir.join(format!("{}.bin", meta.id)), bytes))
        .and_then(|_| save_meta(&dir, &meta));
    if saved.is_err() {
        remove(&dir, &meta.id);
        return Err(fail(
            StatusCode::INSUFFICIENT_STORAGE,
            "Could not save the attachment",
        ));
    }
    Ok(meta)
}

/// Load attachments for a prompt without changing state. Each must be unsent or
/// already bound to this same request ID (a retry), and still allowed.
pub fn prepare(
    root: &FsPath,
    session_id: &str,
    ids: &[String],
    request_id: &str,
    allowed: &Value,
) -> Result<Vec<(Meta, Payload)>, (StatusCode, String)> {
    if ids.len() > MAX_PER_PROMPT {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("Attach up to {MAX_PER_PROMPT} files per message"),
        ));
    }
    let dir = session_dir(root, session_id);
    let mut out = Vec::with_capacity(ids.len());
    for (i, id) in ids.iter().enumerate() {
        if ids[..i].contains(id) {
            return Err(fail(StatusCode::BAD_REQUEST, "Duplicate attachment"));
        }
        let meta = read_meta(&dir, id).ok_or_else(|| {
            fail(
                StatusCode::NOT_FOUND,
                "Attachment not found; attach it again",
            )
        })?;
        if meta.consumed_by.as_deref().is_some_and(|r| r != request_id) {
            return Err(fail(
                StatusCode::CONFLICT,
                "Attachment was already sent with another message; attach it again to resend",
            ));
        }
        if allowed[meta.kind.as_str()] != true {
            return Err(fail(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("{} is no longer supported by this agent", meta.name),
            ));
        }
        let bytes = std::fs::read(dir.join(format!("{id}.bin")))
            .map_err(|_| fail(StatusCode::NOT_FOUND, "Attachment content is missing"))?;
        // Re-validate stored bytes: the type and size must still match the record.
        match classify(&meta.name, &bytes) {
            Ok((kind, mime))
                if kind == meta.kind && mime == meta.mime && bytes.len() == meta.size => {}
            _ => {
                return Err(fail(
                    StatusCode::CONFLICT,
                    "Attachment content changed; attach it again",
                ));
            }
        }
        let payload = if meta.kind == "image" {
            Payload::Image {
                mime: meta.mime.clone(),
                base64: base64(&bytes),
            }
        } else {
            Payload::Text {
                uri: meta.uri(),
                mime: meta.mime.clone(),
                text: String::from_utf8(bytes).unwrap_or_default(),
            }
        };
        out.push((meta, payload));
    }
    Ok(out)
}
/// Bind attachments to the accepted request before dispatch; persisted, so an
/// interrupted or cancelled turn never resends them.
pub fn consume(
    root: &FsPath,
    session_id: &str,
    metas: &[Meta],
    request_id: &str,
) -> std::io::Result<()> {
    let dir = session_dir(root, session_id);
    for meta in metas {
        let mut meta = meta.clone();
        meta.consumed_by = Some(request_id.to_owned());
        save_meta(&dir, &meta)?;
    }
    Ok(())
}
pub fn prepare_for(
    s: &Session,
    ids: &[String],
    request_id: &str,
) -> Result<Vec<(Meta, Payload)>, (StatusCode, String)> {
    prepare(root()?, &s.id, ids, request_id, &session_policy(s))
}
pub fn consume_for(
    s: &Session,
    metas: &[Meta],
    request_id: &str,
) -> Result<(), (StatusCode, String)> {
    consume(root()?, &s.id, metas, request_id).map_err(|_| {
        fail(
            StatusCode::INSUFFICIENT_STORAGE,
            "Could not record the attachments; nothing was sent",
        )
    })
}
/// Session deletion removes all of its attachments.
pub fn delete_session(session_id: &str) {
    let safe =
        !session_id.is_empty() && !session_id.contains(['/', '\\']) && !session_id.starts_with('.');
    if let (true, Ok(root)) = (safe, root()) {
        let _ = std::fs::remove_dir_all(session_dir(root, session_id));
    }
}
/// Startup: drop storage for sessions that no longer exist.
pub fn prune_orphans(root: &FsPath, live: &std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let orphan = entry
            .file_name()
            .to_str()
            .is_some_and(|name| !live.contains(name));
        if orphan && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Deserialize)]
pub struct UploadQuery {
    name: String,
}
pub async fn upload(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult {
    let s = session(&app, &id)?;
    let allowed = session_policy(&s);
    let root = root()?;
    Ok(Json(store(root, &s.id, &q.name, &body, &allowed)?.public()))
}
pub async fn discard(
    State(app): State<App>,
    Path((id, attachment)): Path<(String, String)>,
) -> ApiResult {
    let s = session(&app, &id)?;
    let _run = app.run_lock.lock().await;
    let dir = session_dir(root()?, &s.id);
    match read_meta(&dir, &attachment) {
        Some(meta) if meta.consumed_by.is_none() => {
            remove(&dir, &attachment);
            Ok(Json(json!({"ok":true})))
        }
        // Sent attachments stay with the transcript until the session is deleted.
        Some(_) => Err(fail(StatusCode::CONFLICT, "Attachment was already sent")),
        None => Ok(Json(json!({"ok":true}))),
    }
}
pub async fn content(
    State(app): State<App>,
    Path((id, attachment)): Path<(String, String)>,
) -> Result<Response, (StatusCode, String)> {
    let s = session(&app, &id)?;
    let dir = session_dir(root()?, &s.id);
    let meta = read_meta(&dir, &attachment)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Attachment not found"))?;
    let bytes = tokio::fs::read(dir.join(format!("{}.bin", meta.id)))
        .await
        .map_err(|_| fail(StatusCode::NOT_FOUND, "Attachment not found"))?;
    let mut headers = HeaderMap::new();
    let mime = if meta.kind == "image" {
        meta.mime.clone()
    } else {
        "text/plain; charset=utf-8".into()
    };
    headers.insert(header::CONTENT_TYPE, mime.parse().unwrap());
    let ascii: String = meta
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let disposition = if meta.kind == "image" {
        "inline"
    } else {
        "attachment"
    };
    if let Ok(value) = format!("{disposition}; filename=\"{ascii}\"").parse() {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    Ok((headers, bytes).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentworks-attach-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn all() -> Value {
        policy("claude", &json!({"image":true,"embeddedContext":true}))
    }

    #[test]
    fn policy_requires_advertised_capability_and_verified_provider() {
        let caps = json!({"image":true,"embeddedContext":true});
        assert_eq!(policy("claude", &caps)["image"], true);
        assert_eq!(policy("codex", &caps)["text"], true);
        let opencode = policy("opencode", &caps);
        assert_eq!(opencode["image"], false);
        assert_eq!(opencode["text"], true);
        assert!(
            opencode["imageReason"]
                .as_str()
                .unwrap()
                .contains("OpenCode")
        );
        let demo = policy("demo", &json!({"image":true}));
        assert_eq!(
            (demo["image"].clone(), demo["text"].clone()),
            (json!(true), json!(false))
        );
        let unknown = policy("claude", &Value::Null);
        assert_eq!(
            (unknown["image"].clone(), unknown["text"].clone()),
            (json!(false), json!(false))
        );
        assert_eq!(policy("future", &caps)["image"], false);
    }

    #[test]
    fn classification_uses_content_and_limits() {
        assert_eq!(classify("x.txt", PNG).unwrap(), ("image", "image/png"));
        assert_eq!(
            classify("photo.png", b"hello").unwrap(),
            ("text", "text/plain")
        );
        assert_eq!(classify("a.json", b"{}").unwrap().1, "application/json");
        assert!(classify("a.pdf", b"%PDF-1.7").unwrap_err().contains("PDF"));
        assert!(classify("a.bin", b"\0\x01\x02").is_err());
        assert!(classify("a.bin", &[0xff, 0xfe, 0x00]).is_err());
        assert!(classify("a.txt", b"").is_err());
        assert!(
            classify("a.txt", &vec![b'a'; MAX_TEXT_BYTES + 1])
                .unwrap_err()
                .contains("KB")
        );
        let mut big = PNG.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert!(classify("a.png", &big).unwrap_err().contains("MB"));
    }

    #[test]
    fn names_never_carry_paths() {
        assert_eq!(clean_name("/home/user/secret/report.md"), "report.md");
        assert_eq!(clean_name("C:\\Users\\me\\a.txt"), "a.txt");
        assert_eq!(clean_name("../.."), "attachment");
        assert_eq!(clean_name("a\u{0}b\nc.txt"), "abc.txt");
        let meta = Meta {
            id: "id".into(),
            name: "my file (1).txt".into(),
            mime: "text/plain".into(),
            kind: "text".into(),
            size: 1,
            created: 0,
            consumed_by: None,
        };
        assert_eq!(meta.uri(), "agentworks://attachments/id/my_file__1_.txt");
    }

    #[test]
    fn base64_matches_reference() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }

    #[test]
    fn attachments_are_consumed_once_per_request() {
        let root = temp();
        let sid = "s1";
        let text = store(&root, sid, "/tmp/notes.md", b"# hi", &all()).unwrap();
        let image = store(&root, sid, "pic.png", PNG, &all()).unwrap();
        let ids = vec![text.id.clone(), image.id.clone()];
        let prepared = prepare(&root, sid, &ids, "r1", &all()).unwrap();
        match &prepared[0].1 {
            Payload::Text { uri, text, mime } => {
                assert!(uri.starts_with("agentworks://attachments/") && !uri.contains("/tmp"));
                assert_eq!((text.as_str(), mime.as_str()), ("# hi", "text/markdown"));
            }
            _ => panic!("expected text"),
        }
        assert!(matches!(&prepared[1].1, Payload::Image { mime, .. } if mime == "image/png"));
        let metas: Vec<Meta> = prepared.into_iter().map(|(m, _)| m).collect();
        consume(&root, sid, &metas, "r1").unwrap();
        // A retry of the same request may load them; any other request may not.
        assert!(prepare(&root, sid, &ids, "r1", &all()).is_ok());
        assert_eq!(
            prepare(&root, sid, &ids, "r2", &all()).unwrap_err().0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            prepare(
                &root,
                sid,
                &[text.id.clone(), text.id.clone()],
                "r1",
                &all()
            )
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            prepare(&root, "other", &[text.id.clone()], "r1", &all())
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            prepare(&root, sid, &["../x".into()], "r1", &all())
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        let five: Vec<String> = (0..5).map(|_| uuid::Uuid::new_v4().to_string()).collect();
        assert_eq!(
            prepare(&root, sid, &five, "r3", &all()).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upload_policy_and_pending_limits() {
        let root = temp();
        let text_only = policy("opencode", &json!({"image":true,"embeddedContext":true}));
        let err = store(&root, "s", "a.png", PNG, &text_only).unwrap_err();
        assert_eq!(err.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(err.1.contains("OpenCode"));
        let image = store(&root, "s", "pic.png", PNG, &all()).unwrap();
        // Later capability loss blocks sending an already uploaded file.
        assert_eq!(
            prepare(&root, "s", &[image.id], "r", &text_only)
                .unwrap_err()
                .0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        for _ in 1..MAX_PENDING {
            store(&root, "s", "a.txt", b"x", &all()).unwrap();
        }
        assert_eq!(
            store(&root, "s", "a.txt", b"x", &all()).unwrap_err().0,
            StatusCode::CONFLICT
        );
        let live = std::collections::HashSet::new();
        prune_orphans(&root, &live);
        assert!(!root.join("s").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
