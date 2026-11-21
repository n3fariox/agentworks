//! Compacts a Codex session's native rollout file.
//!
//! Codex appends every event of a conversation to one `rollout-*.jsonl` file and
//! never shortens it. Each time Codex compacts its context it appends a
//! `compacted` checkpoint whose `replacement_history` is the model's whole
//! context from then on, and resuming rebuilds the context from the latest
//! checkpoint plus the lines after it. Everything earlier only feeds Codex's own
//! history views, while resuming still reads and returns it all.
//!
//! Compacting keeps the `session_meta` header, the latest checkpoint and what
//! follows, and replaces inline base64 images with a 1×1 PNG so every line keeps
//! its shape. The original stays beside it as a hard-linked backup until deleted.
//! It runs only while no harness is attached, holding the worker slot so none
//! can start and append to the file being replaced.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path as FsPath, PathBuf};

const BACKUP: &str = ".agentworks-backup-";
/// A transparent 1×1 PNG: an image part stays an image part, without the bytes.
const TINY_IMAGE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
/// Shorter base64 payloads are left alone: they cost less than the edit is worth.
const MIN_IMAGE: usize = 1024;

/// Where Codex homes may be: the inherited `CODEX_HOME` or `~/.codex`, and the
/// home Agentworks gives API-key sessions.
fn homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        homes.push(PathBuf::from(home));
    }
    if let Some(user) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        homes.push(PathBuf::from(user).join(".codex"));
    }
    homes.push(
        agentworks_runtime::provider::runtime_root()
            .join(".agentworks")
            .join("codex"),
    );
    homes.dedup();
    homes
}

/// The rollout file of a Codex thread: `sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`.
fn find(thread: &str) -> Option<PathBuf> {
    if thread.is_empty()
        || !thread
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    let suffix = format!("-{thread}.jsonl");
    let entries = |dir: &FsPath| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    for home in homes() {
        // Newest days first: an active conversation's file is usually recent.
        let mut days: Vec<PathBuf> = entries(&home.join("sessions"))
            .iter()
            .flat_map(|y| entries(y))
            .flat_map(|m| entries(&m))
            .collect();
        days.sort_unstable_by(|a, b| b.cmp(a));
        for day in days {
            if let Some(file) = entries(&day).into_iter().find(|f| {
                f.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.starts_with("rollout-") && n.ends_with(&suffix) && !n.contains(BACKUP)
                })
            }) {
                return Some(file);
            }
        }
    }
    None
}

/// What a rollout holds, read without loading it into memory.
#[derive(Debug, Default)]
struct Scan {
    bytes: u64,
    header: Vec<u8>,
    checkpoints: usize,
    /// Byte offset of the latest checkpoint line.
    latest: Option<u64>,
}

fn line_type(line: &[u8]) -> Option<String> {
    // Lines are `{"timestamp":…,"type":…,"payload":…}`; parse only short-listed ones.
    let head = &line[..line.len().min(160)];
    let marker = b"\"type\":\"";
    let at = head.windows(marker.len()).position(|w| w == marker)? + marker.len();
    let end = head[at..].iter().position(|&b| b == b'"')? + at;
    Some(String::from_utf8_lossy(&head[at..end]).into_owned())
}

fn scan(path: &FsPath) -> io::Result<Scan> {
    let mut reader = BufReader::with_capacity(1 << 20, fs::File::open(path)?);
    let mut scan = Scan::default();
    let mut line = Vec::new();
    let mut offset = 0u64;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if offset == 0 {
            if line_type(&line).as_deref() != Some("session_meta") {
                return Err(io::Error::other(
                    "The file does not start with a Codex session header",
                ));
            }
            scan.header = line.clone();
        } else if line_type(&line).as_deref() == Some("compacted") {
            let parsed: Value = serde_json::from_slice(&line)
                .map_err(|_| io::Error::other("A compaction checkpoint could not be read"))?;
            if parsed["type"] == "compacted" && parsed["payload"]["replacement_history"].is_array()
            {
                scan.checkpoints += 1;
                scan.latest = Some(offset);
            }
        }
        offset += read as u64;
    }
    scan.bytes = offset;
    Ok(scan)
}

/// Replaces each inline base64 image of at least `MIN_IMAGE` characters. Base64
/// and the data-URL prefix contain no quote or backslash, so JSON stays valid.
fn strip_images(line: &[u8]) -> (Vec<u8>, usize) {
    let prefix = b"data:image/";
    let mut out = Vec::with_capacity(line.len());
    let (mut i, mut count) = (0, 0);
    while i < line.len() {
        if line[i..].starts_with(prefix) {
            let mime_end = line[i + prefix.len()..]
                .iter()
                .position(|b| !(b.is_ascii_alphanumeric() || b"+.-".contains(b)))
                .map(|n| i + prefix.len() + n);
            if let Some(end) = mime_end
                && line[end..].starts_with(b";base64,")
            {
                let data = end + b";base64,".len();
                let len = line[data..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
                    .count();
                if len >= MIN_IMAGE {
                    out.extend_from_slice(TINY_IMAGE.as_bytes());
                    i = data + len;
                    count += 1;
                    continue;
                }
            }
        }
        out.push(line[i]);
        i += 1;
    }
    (out, count)
}

fn backups(path: &FsPath) -> Vec<(PathBuf, u64)> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return vec![];
    };
    let prefix = format!("{name}{BACKUP}");
    let mut found: Vec<(PathBuf, u64)> = fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with(&prefix))
                })
                .map(|e| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// Keeps the original under a name no other backup has. Neither the hard link nor
/// the copy fallback ever replaces an existing file, so a second compaction in the
/// same second cannot overwrite the first, full backup.
fn keep_backup(path: &FsPath, name: &str, now: u64) -> io::Result<PathBuf> {
    for n in 0..1000u32 {
        let suffix = if n == 0 {
            now.to_string()
        } else {
            format!("{now}-{n}")
        };
        let backup = path.with_file_name(format!("{name}{BACKUP}{suffix}"));
        let linked = match fs::hard_link(path, &backup) {
            Ok(()) => return Ok(backup),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => e,
        };
        // Filesystems without hard links get a copy instead.
        match copy_private(path, &backup) {
            Ok(()) => return Ok(backup),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io::Error::other(format!("{linked}; {e}"))),
        }
    }
    Err(io::Error::other("too many backups with the same time"))
}

/// Copies `source` to a new file at `target`, private like the rollout. An existing
/// `target` is never replaced (`AlreadyExists`), and any later failure removes the
/// partial copy.
fn copy_private(source: &FsPath, target: &FsPath) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(target)?;
    let copied = fs::File::open(source)
        .and_then(|mut source| io::copy(&mut source, &mut file))
        .and_then(|_| file.sync_all());
    if copied.is_err() {
        drop(file);
        let _ = fs::remove_file(target);
    }
    copied.map(|_| ())
}

#[derive(Debug)]
struct Outcome {
    before: u64,
    after: u64,
    images: usize,
    backup: PathBuf,
}

/// Rewrites the rollout from its latest checkpoint. The original is hard-linked
/// to a backup first, then the new file replaces it in one rename, so a crash
/// leaves either file complete.
fn compact(path: &FsPath, now: u64) -> io::Result<Outcome> {
    let scan = scan(path)?;
    let Some(latest) = scan.latest else {
        return Err(io::Error::other(
            "Codex has not compacted this conversation yet, so there is no checkpoint to keep",
        ));
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("rollout.jsonl");
    let temp = path.with_file_name(format!(".{name}.agentworks-tmp"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> io::Result<(u64, usize)> {
        let mut out = io::BufWriter::new(options.open(&temp)?);
        let mut source = fs::File::open(path)?;
        source.seek(SeekFrom::Start(latest))?;
        let mut reader = BufReader::with_capacity(1 << 20, source.take(scan.bytes - latest));
        let (header, mut images) = strip_images(&scan.header);
        out.write_all(&header)?;
        let mut written = header.len() as u64;
        let mut line = Vec::new();
        let mut first = true;
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let (kept, count) = strip_images(&line);
            let body = kept.strip_suffix(b"\n").unwrap_or(&kept);
            // Nothing is written that Codex could not read back.
            if !body.iter().all(u8::is_ascii_whitespace) {
                let value: Value = serde_json::from_slice(body).map_err(|_| {
                    io::Error::other("A kept line is not valid JSON; nothing was changed")
                })?;
                if first && value["type"] != "compacted" {
                    return Err(io::Error::other(
                        "The checkpoint moved while compacting; nothing was changed",
                    ));
                }
                first = false;
            }
            images += count;
            out.write_all(&kept)?;
            written += kept.len() as u64;
        }
        let file = out.into_inner().map_err(|e| e.into_error())?;
        // Codex orders conversations by their file's time; keep it.
        if let Ok(modified) = fs::metadata(path).and_then(|m| m.modified()) {
            let _ = file.set_modified(modified);
        }
        file.sync_all()?;
        Ok((written, images))
    })();
    let (after, images) = match result {
        Ok(done) => done,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
    };
    let backup = match keep_backup(path, name, now) {
        Ok(backup) => backup,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(io::Error::other(format!(
                "Could not keep a backup ({error}); nothing was changed"
            )));
        }
    };
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        let _ = fs::remove_file(&backup);
        return Err(error);
    }
    if let Some(dir) = path.parent().and_then(|d| fs::File::open(d).ok()) {
        let _ = dir.sync_all();
    }
    Ok(Outcome {
        before: scan.bytes,
        after,
        images,
        backup,
    })
}

/// The session's rollout, when it can be compacted from here.
fn rollout(s: &Session) -> Result<PathBuf, String> {
    if agentworks_runtime::provider::cli_provider(&s.provider) != "codex" {
        return Err("Only Codex sessions have a Codex transcript".into());
    }
    if s.host_id != "local" {
        return Err("Not available for sessions on remote hosts yet".into());
    }
    let thread = s.data.lock().unwrap().provider_session_id.clone();
    let thread = thread.ok_or("This session has no Codex conversation yet")?;
    find(&thread).ok_or_else(|| "The Codex transcript file was not found".into())
}

fn harness_running(s: &Session) -> bool {
    s.worker
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|w| !w.is_finished())
}

fn describe(path: &FsPath, running: bool) -> io::Result<Value> {
    let scan = scan(path)?;
    let estimate = scan
        .latest
        .map(|latest| scan.header.len() as u64 + scan.bytes - latest);
    let backups: Vec<Value> = backups(path)
        .into_iter()
        .map(|(path, bytes)| json!({"path":path,"bytes":bytes}))
        .collect();
    Ok(
        json!({"available":true,"path":path,"bytes":scan.bytes,"checkpoints":scan.checkpoints,
        "compactedBytes":estimate,"backups":backups,"running":running}),
    )
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, (StatusCode, String)> + Send + 'static,
) -> Result<T, (StatusCode, String)> {
    tokio::task::spawn_blocking(work).await.map_err(|_| {
        fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Codex transcript task failed",
        )
    })?
}

/// Size of the Codex transcript and what compacting would keep.
pub async fn info(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let path = match rollout(&s) {
        Ok(path) => path,
        Err(reason) => return Ok(Json(json!({"available":false,"reason":reason}))),
    };
    let running = harness_running(&s);
    blocking(move || {
        describe(&path, running)
            .map(Json)
            .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })
    .await
}

pub async fn compact_now(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let path = rollout(&s).map_err(|reason| fail(StatusCode::CONFLICT, reason))?;
    blocking(move || {
        // Held throughout: a harness cannot start while its file is replaced.
        let worker = s.worker.lock().unwrap();
        if worker.as_ref().is_some_and(|w| !w.is_finished()) {
            return Err(fail(
                StatusCode::CONFLICT,
                "Suspend the harness first: Codex writes to this file while it runs",
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let outcome = compact(&path, now).map_err(|e| fail(StatusCode::CONFLICT, e.to_string()))?;
        drop(worker);
        tracing::info!(session = %s.id, before = outcome.before, after = outcome.after,
            images = outcome.images, "Compacted Codex transcript");
        let mut info = describe(&path, false)
            .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        info["result"] = json!({"before":outcome.before,"after":outcome.after,"images":outcome.images,"backup":outcome.backup});
        Ok(Json(info))
    })
    .await
}

pub async fn delete_backups(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let path = rollout(&s).map_err(|reason| fail(StatusCode::CONFLICT, reason))?;
    let running = harness_running(&s);
    blocking(move || {
        for (backup, _) in backups(&path) {
            fs::remove_file(&backup).map_err(|e| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Could not delete {}: {e}", backup.display()),
                )
            })?;
        }
        describe(&path, running)
            .map(Json)
            .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: &str, payload: Value) -> String {
        json!({"timestamp":"2026-01-01T00:00:00Z","type":kind,"payload":payload}).to_string() + "\n"
    }

    #[test]
    fn compacting_keeps_the_header_latest_checkpoint_and_after_with_a_backup() {
        let dir = std::env::temp_dir().join(format!("aw-codex-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-01-01T00-00-00-abc.jsonl");
        let image = format!("data:image/png;base64,{}", "A".repeat(4000));
        let small = "data:image/png;base64,AAAA";
        let content = [
            line("session_meta", json!({"id":"abc"})),
            line("response_item", json!({"type":"message","text":"old"})),
            line("compacted", json!({"message":"","replacement_history":[{"old":1}]})),
            line("response_item", json!({"type":"message","text":"older"})),
            line("compacted", json!({"message":"","replacement_history":[{"image_url":image}]})),
            line("response_item", json!({"type":"custom_tool_call_output","output":[{"type":"input_image","image_url":image},{"image_url":small}]})),
        ]
        .concat();
        fs::write(&path, &content).unwrap();
        let scan = scan(&path).unwrap();
        assert_eq!(scan.checkpoints, 2);
        let outcome = compact(&path, 7).unwrap();
        assert_eq!(outcome.before, content.len() as u64);
        assert_eq!(outcome.images, 2);
        let kept = fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = kept
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["session_meta", "compacted", "response_item"]);
        assert_eq!(
            lines[1]["payload"]["replacement_history"][0]["image_url"],
            TINY_IMAGE
        );
        assert_eq!(lines[2]["payload"]["output"][0]["image_url"], TINY_IMAGE);
        assert_eq!(
            lines[2]["payload"]["output"][1]["image_url"], small,
            "small images stay"
        );
        assert_eq!(outcome.after, kept.len() as u64);
        // The original is kept whole, and compacting again keeps the same lines.
        assert_eq!(fs::read_to_string(&outcome.backup).unwrap(), content);
        assert_eq!(backups(&path).len(), 1);
        // Compacting again in the same second keeps the first, full backup.
        let again = compact(&path, 7).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), kept);
        assert_eq!(again.images, 0);
        assert_ne!(again.backup, outcome.backup);
        assert_eq!(fs::read_to_string(&outcome.backup).unwrap(), content);
        assert_eq!(fs::read_to_string(&again.backup).unwrap(), kept);
        assert_eq!(backups(&path).len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_copy_fallback_is_private_and_never_replaces_or_leaves_a_partial_backup() {
        let dir = std::env::temp_dir().join(format!("aw-codex-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let (source, backup) = (dir.join("rollout.jsonl"), dir.join("backup"));
        fs::write(&source, "full").unwrap();
        copy_private(&source, &backup).unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), "full");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&backup).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::write(&source, "compacted").unwrap();
        let taken = copy_private(&source, &backup).unwrap_err();
        assert_eq!(taken.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&backup).unwrap(), "full");
        let missing = dir.join("other-backup");
        assert!(copy_private(&dir.join("gone.jsonl"), &missing).is_err());
        assert!(!missing.exists(), "a failed copy leaves no empty backup");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn without_a_checkpoint_or_header_nothing_changes() {
        let dir = std::env::temp_dir().join(format!("aw-codex-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-x-abc.jsonl");
        let content = line("session_meta", json!({})) + &line("response_item", json!({}));
        fs::write(&path, &content).unwrap();
        assert!(compact(&path, 1).is_err());
        fs::write(&path, line("response_item", json!({}))).unwrap();
        assert!(scan(&path).is_err());
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "no temp or backup left"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn thread_ids_cannot_name_other_files() {
        assert!(find("../etc").is_none());
        assert!(find("").is_none());
        assert!(find("a/b").is_none());
    }

    /// Compacts a copy of a real rollout: `AW_ROLLOUT=/copy.jsonl cargo test -- --ignored compact_rollout_copy --nocapture`.
    #[test]
    #[ignore]
    fn compact_rollout_copy() {
        let path = PathBuf::from(std::env::var("AW_ROLLOUT").unwrap());
        let started = std::time::Instant::now();
        let outcome = compact(&path, 1).unwrap();
        eprintln!("{outcome:?} in {:?}", started.elapsed());
    }
}
