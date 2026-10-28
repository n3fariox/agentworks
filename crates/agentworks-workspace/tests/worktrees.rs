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
    settings: agentworks_workspace::config::Settings,
}

impl Fixture {
    fn new() -> Self {
        Self::new_in(std::env::temp_dir())
    }
    fn new_in(parent: PathBuf) -> Self {
        let root = parent.join(format!("agentworks-workspace-{}", uuid::Uuid::new_v4()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let fixture = Self {
            root,
            repo,
            reservations: AtomicUsize::new(0),
            reject_reservation: false,
            settings: Default::default(),
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
    fn git_settings(&self) -> agentworks_workspace::config::Settings {
        self.settings.clone()
    }
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

#[tokio::test]
async fn repository_config_roundtrips_other_sections_and_refuses_stale_writes() {
    let fixture = Fixture::new();
    let first = worktrees::configuration_in(&fixture, &fixture.repo, None, None)
        .await
        .unwrap();
    let text = "version = 1\n[future]\nname = 'keep me'\n[git.worktrees]\ncopy = 'prefer-cow'\n";
    let saved = worktrees::configuration_in(
        &fixture,
        &fixture.repo,
        Some(text),
        first["fingerprint"].as_str(),
    )
    .await
    .unwrap();
    assert_eq!(saved["text"], text);
    assert_eq!(saved["effective"]["copy"], "prefer-cow");
    assert_eq!(
        worktrees::configuration_in(
            &fixture,
            &fixture.repo,
            Some(text),
            first["fingerprint"].as_str()
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );
    assert!(agentworks_workspace::config::parse("[git.worktrees]\nshare=['../outside']").is_err());
    assert!(
        agentworks_workspace::config::parse(
            "[git.hooks]\nenter=[{name='bad',command=[],timeout_seconds=0}]"
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(fixture.repo.join(".agentworks/config.toml")).unwrap();
        std::os::unix::fs::symlink(
            fixture.repo.join("tracked.txt"),
            fixture.repo.join(".agentworks/config.toml"),
        )
        .unwrap();
        assert!(
            worktrees::configuration_in(&fixture, &fixture.repo, None, None)
                .await
                .is_err()
        );
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cow_checkout_is_clean_isolated_and_restorable_with_a_dirty_source() {
    use agentworks_workspace::config::CopyMode;
    use std::os::unix::fs::MetadataExt;
    let mut fixture = Fixture::new_in(std::env::current_dir().unwrap().join("target"));
    fixture.settings.copy = CopyMode::RequireCow;
    fixture.settings.share = vec!["build".into()];
    std::fs::write(fixture.repo.join("large.bin"), vec![17_u8; 1024 * 1024]).unwrap();
    std::fs::write(fixture.repo.join(".gitignore"), "build/\n").unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@localhost",
        "commit",
        "-m",
        "large fixture",
    ]);
    std::fs::write(fixture.repo.join("tracked.txt"), "newer commit\n").unwrap();
    fixture.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@localhost",
        "commit",
        "-am",
        "newer commit",
    ]);
    std::fs::write(fixture.repo.join("tracked.txt"), "dirty source\n").unwrap();
    std::fs::create_dir(fixture.repo.join("build")).unwrap();
    std::fs::write(fixture.repo.join("build/cache"), vec![42; 8192]).unwrap();
    let mut workspace = fixture.workspace();
    workspace.base_ref = Some("HEAD~1".into());
    let result = worktrees::prepare_in(
        &fixture,
        Some(&workspace),
        Policy::default(),
        false,
        fixture.repo.clone(),
        Some("creation"),
    )
    .await;
    let (path, owned) = match result {
        Err((_, message)) if message.contains("do not support reflinks") => {
            eprintln!("Reflink filesystem unavailable: {message}");
            return;
        }
        other => other.unwrap(),
    };
    let mut owned = owned.unwrap();
    assert_eq!(owned.population.as_deref(), Some("cow"));
    let template_file = owned
        .common_dir
        .join("agentworks-cow")
        .join(&owned.base_commit)
        .join("large.bin");
    std::fs::File::open(&template_file)
        .unwrap()
        .sync_all()
        .unwrap();
    std::fs::File::open(path.join("large.bin"))
        .unwrap()
        .sync_all()
        .unwrap();
    if let Ok(extents) = Command::new("filefrag")
        .arg("-v")
        .arg(&template_file)
        .arg(path.join("large.bin"))
        .output()
    {
        assert!(extents.status.success());
        assert!(
            String::from_utf8_lossy(&extents.stdout)
                .matches("shared")
                .count()
                >= 2,
            "Template and actual checkout must retain physically shared blocks after Git index setup"
        );
    }

    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "committed\n"
    );
    assert_eq!(
        std::fs::read(path.join("build/cache")).unwrap(),
        vec![42; 8192]
    );
    assert_ne!(
        std::fs::metadata(path.join("large.bin")).unwrap().ino(),
        std::fs::metadata(fixture.repo.join("large.bin"))
            .unwrap()
            .ino()
    );
    let inspection = cleanup::inspect_in(&fixture, Some(owned.clone())).await;
    assert!(!inspection.dirty.unwrap().any());
    assert_eq!(
        worktrees::list_in(&fixture, &fixture.repo).await.unwrap()["worktrees"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    cleanup::remove_in(&fixture, &owned, inspection.fingerprint.as_deref(), false)
        .await
        .unwrap();
    owned.removed = Some(1);
    cleanup::restore_in(&fixture, &owned).await.unwrap();
    std::fs::write(path.join("large.bin"), "isolated").unwrap();
    assert_eq!(
        std::fs::metadata(fixture.repo.join("large.bin"))
            .unwrap()
            .len(),
        1024 * 1024
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("tracked.txt")).unwrap(),
        "dirty source\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn owned_hook_snapshot_uses_worktree_context_and_bounds_failures() {
    use agentworks_workspace::{
        config::{Hook, Hooks},
        hooks,
    };
    let mut fixture = Fixture::new();
    fixture.settings.run_hooks = true;
    std::fs::create_dir(fixture.repo.join(".agentworks")).unwrap();
    std::fs::write(fixture.repo.join(".agentworks/config.toml"), "[[git.hooks.enter]]\nname='setup'\ncommand=['sh','-c','printf %s $AGENTWORKS_BRANCH > hook-result']\ntimeout_seconds=10\n").unwrap();
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
    std::fs::write(
        fixture.repo.join(".agentworks/config.toml"),
        "[[git.hooks.enter]]\nname='edited'\ncommand=['false']\n",
    )
    .unwrap();
    hooks::run(&fixture, &owned, "enter", "create")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("hook-result")).unwrap(),
        "feature/test"
    );
    assert!(!fixture.repo.join("hook-result").exists());
    owned.hooks = Some(hooks::State {
        definitions: Hooks {
            enter: vec![Hook {
                name: "timeout".into(),
                command: vec!["sh".into(), "-c".into(), "sleep 20 & wait".into()],
                timeout_seconds: 1,
            }],
            exit: vec![],
        },
        ..Default::default()
    });
    let error = hooks::run(&fixture, &owned, "enter", "test")
        .await
        .unwrap_err();
    assert!(error.1.contains("timed out"));
    owned.hooks.as_mut().unwrap().definitions.enter[0].command =
        vec!["sh".into(), "-c".into(), "yes flooding-output".into()];
    assert!(
        hooks::run(&fixture, &owned, "enter", "test")
            .await
            .unwrap_err()
            .1
            .contains("output limit")
    );
}

#[tokio::test]
async fn prefer_cow_falls_back_and_require_cow_refuses_unsupported_checkout_features() {
    use agentworks_workspace::config::CopyMode;
    let mut fixture = Fixture::new();
    fixture.git(&["config", "core.splitIndex", "true"]);
    fixture.settings.copy = CopyMode::PreferCow;
    let (_, owned) = worktrees::prepare_in(
        &fixture,
        Some(&fixture.workspace()),
        Policy::default(),
        false,
        fixture.repo.clone(),
        Some("creation"),
    )
    .await
    .unwrap();
    assert!(
        owned
            .unwrap()
            .population
            .unwrap()
            .starts_with("plain: repository uses")
    );
    let mut workspace = fixture.workspace();
    workspace.branch = Some("feature/required".into());
    workspace.path = Some(fixture.root.join("required").to_str().unwrap().into());
    fixture.settings.copy = CopyMode::RequireCow;
    let error = worktrees::prepare_in(
        &fixture,
        Some(&workspace),
        Policy::default(),
        false,
        fixture.repo.clone(),
        Some("creation"),
    )
    .await
    .err()
    .unwrap();
    assert!(error.1.contains("CoW required"));
    let output = Command::new("git")
        .current_dir(&fixture.repo)
        .args(["branch", "--list", "feature/required"])
        .output()
        .unwrap();
    assert!(
        output.stdout.is_empty(),
        "Unsupported CoW must not register a branch"
    );
}
