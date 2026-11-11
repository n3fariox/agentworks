//! Replaces the worker on a host this server installed. The new release goes
//! into its own directory beside the old one, so a failed upload or install
//! leaves the host on its current worker. Only after it installs does the host
//! switch to it and reconnect; the old release is then removed.
use super::{Os, Output, connected, install, managed, run, unix_release, unix_release_id, windows};
use crate::{ApiResult, App, fail, hosts};
use axum::{
    Json,
    extract::{Path, State, ws::Message},
    http::StatusCode,
};
use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};
use tokio::sync::mpsc;
use uuid::Uuid;

static UPDATING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
/// Enough of the copy/install output to explain a failure.
const TAIL: usize = 2000;

struct Updating(String);
impl Drop for Updating {
    fn drop(&mut self) {
        UPDATING.lock().unwrap().remove(&self.0);
    }
}

pub async fn update(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    // A closed browser tab must not abandon a half-finished switch.
    tokio::spawn(update_worker(app, id)).await.map_err(|_| {
        fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The worker update stopped unexpectedly",
        )
    })?
}

fn release(command: &str) -> Option<(Os, &str)> {
    unix_release_id(command)
        .map(|id| (Os::Unix, id))
        .or_else(|| windows::release_id(command).map(|id| (Os::Windows, id)))
}

async fn update_worker(app: App, id: String) -> ApiResult {
    let host = app
        .catalog
        .lock()
        .unwrap()
        .data
        .hosts
        .iter()
        .find(|h| h.id == id)
        .cloned()
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Host not found"))?;
    let (Some(key), Some((os, old))) = (host.identity_file.as_deref(), release(&host.command))
    else {
        return Err(fail(
            StatusCode::CONFLICT,
            "This host runs a worker you set up yourself. Update the Agentworks binary on the host, then reconnect.",
        ));
    };
    let old = old.to_owned();
    if !UPDATING.lock().unwrap().insert(id.clone()) {
        return Err(fail(
            StatusCode::CONFLICT,
            "This host's worker is already updating",
        ));
    }
    let _updating = Updating(id.clone());
    // Staged beside the host's key, in its private directory.
    let dir = key
        .parent()
        .ok_or_else(|| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Host key directory is missing",
            )
        })?
        .join(format!("update-{}", Uuid::new_v4()));
    let (out, log) = transcript();
    let release = Uuid::new_v4().to_string();
    let installed = async {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| e.to_string())?;
        install(&host, os, &dir, &release, &out).await
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    let command = match installed {
        Ok(command) => command,
        Err(e) => {
            drop(out);
            let tail = log.await.unwrap_or_default();
            return Err(fail(
                StatusCode::BAD_GATEWAY,
                format!(
                    "Could not install the new worker; the host keeps its current one. {}\n{tail}",
                    // There is no terminal here; the output tail follows instead.
                    e.replace(" See the terminal output above.", "")
                )
                .trim_end()
                .to_owned(),
            ));
        }
    };
    let saved = app
        .catalog
        .lock()
        .unwrap()
        .set_host_command(&id, &command)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    tracing::info!(host = %saved.name, "Worker updated; reconnecting");
    app.hosts.start(saved.clone());
    connected(&app, &id, "Worker updated")
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    // Connected means the new worker holds the host lock, so the old one has exited.
    let removed = match os {
        Os::Unix => {
            run(
                &mut managed(&saved, &format!("rm -rf {}", unix_release(&old))),
                &out,
            )
            .await
        }
        Os::Windows => windows::remove_release(&saved, &old, &out).await,
    };
    if removed.is_err() {
        tracing::warn!(host = %saved.name, "Could not remove the replaced worker release");
    }
    Ok(Json(hosts::view(&app.hosts, &saved)))
}

/// Keeps the tail of ssh's output; nothing is persisted.
fn transcript() -> (Output, tokio::task::JoinHandle<String>) {
    let (out, mut rx) = mpsc::channel::<Message>(32);
    let log = tokio::spawn(async move {
        let mut text = String::new();
        while let Some(message) = rx.recv().await {
            if let Message::Binary(bytes) = message {
                text.push_str(&String::from_utf8_lossy(&bytes));
                if text.len() > 2 * TAIL {
                    let cut = text.ceil_char_boundary(text.len() - TAIL);
                    text.drain(..cut);
                }
            }
        }
        let cut = text.ceil_char_boundary(text.len().saturating_sub(TAIL));
        text[cut..].trim().to_owned()
    });
    (out, log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_releases_this_server_installed_are_updated() {
        let id = "3f1c2d4e-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
        let unix = format!("{}/launch", unix_release(id));
        assert_eq!(
            unix,
            format!("\"$HOME/.local/share/agentworks/releases/{id}\"/launch")
        );
        assert!(matches!(release(&unix), Some((Os::Unix, i)) if i == id));
        assert!(matches!(release(&windows::command(id)), Some((Os::Windows, i)) if i == id));
        for manual in [
            "agentworks",
            "~/agentworks/target/release/agentworks",
            &unix.replace(id, "x; rm -rf ~"),
        ] {
            assert!(release(manual).is_none(), "{manual}");
        }
    }
}
