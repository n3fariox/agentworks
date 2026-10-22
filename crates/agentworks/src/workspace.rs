use crate::{ApiResult, App, FileQuery, checked_cwd, fail};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::Response,
};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path as FsPath, PathBuf};
use tokio::io::AsyncReadExt;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterLocation {
    cwd: String,
    project_id: Option<String>,
}
pub async fn register_location(
    State(app): State<App>,
    Json(req): Json<RegisterLocation>,
) -> ApiResult {
    let cwd = checked_cwd(&app, &req.cwd)?;
    let mut catalog = app.catalog.lock().unwrap();
    if req
        .project_id
        .as_deref()
        .is_some_and(|id| !catalog.contains(id))
    {
        return Err(fail(StatusCode::BAD_REQUEST, "Project not found"));
    }
    let location = catalog.location(&cwd).map_err(|_| {
        fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save directory",
        )
    })?;
    if let Some(id) = req.project_id {
        catalog.attach_location(&id, &location.id).map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not save project directory",
            )
        })?;
    }
    Ok(Json(json!(location)))
}
fn root(app: &App, id: &str) -> Result<PathBuf, (StatusCode, String)> {
    let path = app
        .catalog
        .lock()
        .unwrap()
        .data
        .locations
        .iter()
        .find(|l| l.id == id && l.host_id == "local")
        .map(|l| l.path.clone())
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Directory not found"))?;
    checked_cwd(app, &path.to_string_lossy())
}
async fn resolve(root: &FsPath, path: &str) -> Result<PathBuf, (StatusCode, String)> {
    let requested = PathBuf::from(path);
    let path = tokio::fs::canonicalize(if requested.is_absolute() {
        requested
    } else {
        root.join(requested)
    })
    .await
    .map_err(|_| fail(StatusCode::NOT_FOUND, "File or directory not found"))?;
    if !path.starts_with(root) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Path is outside this project directory",
        ));
    }
    Ok(path)
}
#[derive(Deserialize)]
pub struct DirectoryQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    hidden: bool,
}
pub async fn list_files(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(req): Query<DirectoryQuery>,
) -> ApiResult {
    let root = root(&app, &id)?;
    let path = resolve(&root, &req.path).await?;
    let mut reader = tokio::fs::read_dir(&path)
        .await
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "Directory cannot be read"))?;
    let mut entries = Vec::new();
    let mut scanned = 0;
    let mut truncated = false;
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "Directory read failed"))?
    {
        scanned += 1;
        if scanned > 5000 {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !req.hidden && name.starts_with('.') {
            continue;
        }
        let canonical = tokio::fs::canonicalize(entry.path()).await.ok();
        let blocked = canonical.as_ref().is_none_or(|p| !p.starts_with(&root));
        let meta = if blocked {
            None
        } else {
            tokio::fs::metadata(entry.path()).await.ok()
        };
        let kind = match meta.as_ref() {
            Some(m) if m.is_dir() => "directory",
            Some(m) if m.is_file() => "file",
            _ => "blocked",
        };
        entries.push(json!({"name":name,"path":entry.path().strip_prefix(&root).unwrap().to_string_lossy(),"kind":kind,"size":meta.filter(|m|m.is_file()).map(|m|m.len())}));
    }
    entries.sort_by(|a, b| {
        let ad = a["kind"] == "directory";
        let bd = b["kind"] == "directory";
        bd.cmp(&ad).then_with(|| {
            a["name"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .cmp(&b["name"].as_str().unwrap().to_lowercase())
        })
    });
    Ok(Json(
        json!({"path":path.strip_prefix(&root).unwrap().to_string_lossy(),"root":root,"entries":entries,"truncated":truncated}),
    ))
}
pub async fn read_file(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(req): Query<FileQuery>,
) -> Result<Response, (StatusCode, String)> {
    serve_file(&root(&app, &id)?, req).await
}
pub async fn serve_file(root: &FsPath, req: FileQuery) -> Result<Response, (StatusCode, String)> {
    let path = resolve(root, &req.path).await?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| fail(StatusCode::NOT_FOUND, "File unavailable"))?;
    const LIMIT: u64 = 10 * 1024 * 1024;
    if !meta.is_file() || meta.len() > LIMIT {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Preview/download limit: 10 MB",
        ));
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| fail(StatusCode::NOT_FOUND, "File cannot be read"))?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| fail(StatusCode::NOT_FOUND, "File cannot be read"))?;
    if bytes.len() as u64 > LIMIT {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "File grew beyond the 10 MB limit",
        ));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ if !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok() => {
            "text/plain; charset=utf-8"
        }
        _ => "application/octet-stream",
    };
    let attachment = req.download == Some(true)
        || matches!(mime, "application/pdf" | "application/octet-stream");
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        if mime == "application/pdf" {
            "application/octet-stream"
        } else {
            mime
        }
        .parse()
        .unwrap(),
    );
    if attachment {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, "attachment".parse().unwrap());
    }
    Ok(response)
}
