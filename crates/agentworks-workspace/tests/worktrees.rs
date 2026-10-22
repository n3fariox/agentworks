//! Exercise the extracted library against real repositories without an HTTP server.
use agentworks_workspace::{
    Error, cleanup, diff, git,
    worktrees::{self, Context, Mode, Policy, Workspace},
};
use http::StatusCode;
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    reservations: AtomicUsize,
    reject_reservation: bool,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("agentworks-workspace-{}", uuid::Uuid::new_v4()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let fixture = Self {
            root,
            repo,
            reservations: AtomicUsize::new(0),
            reject_reservation: false,
        };
        fixture.git(&["init", "-b", "main"]);
        std::fs::write(fixture.repo.join("tracked.txt"), "committed\n").unwrap();
        fixture.git(&["add", "."]);
        fixture.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "-m",
            "fixture",
        ]);
        fixture
    }

    fn git(&self, args: &[&str]) {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .current_dir(&self.repo)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn workspace(&self) -> Workspace {
        Workspace {
            mode: Mode::NewWorktree,
            branch: Some("feature/test".into()),
            base_ref: None,
            path: Some(self.root.join("checkout").to_str().unwrap().into()),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Context for Fixture {
    fn roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }
    fn checked(&self, path: &str) -> Result<PathBuf, Error> {
        let path =
            std::fs::canonicalize(path).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        worktrees::allowed(self, &path)?;
        Ok(path)
    }
    fn arm(&self, key: Option<&str>) -> Result<(), Error> {
        assert_eq!(key, Some("creation"));
        self.reservations.fetch_add(1, Ordering::SeqCst);
        if self.reject_reservation {
            Err((
                StatusCode::INSUFFICIENT_STORAGE,
                "reservation failed".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn safe_error(&self, message: &str) -> String {
        message.to_owned()
    }
}

#[tokio::test]
async fn owned_checkout_requires_current_confirmation_and_restores_retained_branch() {
    let fixture = Fixture::new();
    let (path, owned) = worktrees::prepare_in(
        &fixture,
        Some(&fixture.workspace()),
        Policy::default(),
        false,
        fixture.repo.clone(),
        Some("creation"),
    )
    .await
    .unwrap();
    let mut owned = owned.unwrap();
    assert_eq!(fixture.reservations.load(Ordering::SeqCst), 1);
    assert_eq!(
        worktrees::list_in(&fixture, &fixture.repo).await.unwrap()["worktrees"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let clean = cleanup::inspect_in(&fixture, Some(owned.clone())).await;
    assert!(clean.blocker.is_none());
    assert!(!clean.dirty.as_ref().unwrap().any());

    std::fs::write(path.join("tracked.txt"), "changed\n").unwrap();
    let request = serde_json::from_str(r#"{"file":"tracked.txt"}"#).unwrap();
    let comparison = diff::compare(&path, &fixture.root, &request).await.unwrap();
    assert!(comparison["patch"].as_str().unwrap().contains("+changed"));
    let stale = cleanup::remove_in(&fixture, &owned, clean.fingerprint.as_deref(), true)
        .await
        .unwrap_err();
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert!(path.exists());
    let dirty = cleanup::inspect_in(&fixture, Some(owned.clone())).await;
    assert!(dirty.dirty.as_ref().unwrap().any());
    assert!(
        cleanup::remove_in(&fixture, &owned, dirty.fingerprint.as_deref(), false)
            .await
            .is_err()
    );
    cleanup::remove_in(&fixture, &owned, dirty.fingerprint.as_deref(), true)
        .await
        .unwrap();
    assert!(!path.exists());
    owned.removed = Some(1);
    cleanup::restore_in(&fixture, &owned).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "committed\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("tracked.txt")).unwrap(),
        "committed\n"
    );
}

#[tokio::test]
async fn failed_durable_reservation_prevents_directory_and_branch_creation() {
    let mut fixture = Fixture::new();
    fixture.reject_reservation = true;
    let error = worktrees::prepare_in(
        &fixture,
        Some(&fixture.workspace()),
        Policy::default(),
        false,
        fixture.repo.clone(),
        Some("creation"),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.0, StatusCode::INSUFFICIENT_STORAGE);
    assert!(!fixture.root.join("checkout").exists());
    let branches = git::git(
        &fixture.repo,
        &fixture.root,
        &["branch", "--list", "feature/test"],
    )
    .await
    .unwrap();
    assert!(branches.1.is_empty());
}

#[tokio::test]
async fn git_inspection_preserves_raw_paths_and_enforces_output_limits() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.join("space name.txt"), "new").unwrap();
    let raw = git::git_bounded_raw(
        &fixture.repo,
        &fixture.root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        1024,
    )
    .await
    .unwrap();
    assert_eq!(raw.1, "space name.txt\0");
    assert!(
        git::git_bounded_raw(&fixture.repo, &fixture.root, &["ls-files"], 1)
            .await
            .is_err()
    );
}
