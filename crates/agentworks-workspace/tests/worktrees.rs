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
            pull_request: None,
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

#[cfg(target_os = "linux")]
fn assert_lfs_cow_sharing(owned: &cleanup::Owned) {
    use std::os::unix::fs::MetadataExt;
    if owned.population.as_deref() != Some("cow") {
        return;
    }
    let template = owned
        .common_dir
        .join("agentworks-cow/v2")
        .join(&owned.base_commit)
        .join("asset.bin");
    let target = owned.path.join("asset.bin");
    assert_ne!(
        std::fs::metadata(&template).unwrap().ino(),
        std::fs::metadata(&target).unwrap().ino()
    );
    for file in [&template, &target] {
        std::fs::File::open(file).unwrap().sync_all().unwrap();
    }
    if let Ok(extents) = Command::new("filefrag")
        .arg("-v")
        .arg(&template)
        .arg(&target)
        .output()
    {
        assert!(extents.status.success());
        assert!(
            String::from_utf8_lossy(&extents.stdout)
                .matches("shared")
                .count()
                >= 2,
            "Hydrated LFS template and checkout must share physical blocks: {}",
            String::from_utf8_lossy(&extents.stdout)
        );
    }
}

async fn lfs_checkout(copy: agentworks_workspace::config::CopyMode) {
    if !Command::new("git")
        .args(["lfs", "version"])
        .output()
        .is_ok_and(|out| out.status.success())
    {
        eprintln!("Git LFS unavailable; skipping LFS integration test");
        return;
    }
    let mut fixture = Fixture::new_in(std::env::current_dir().unwrap().join("target"));
    fixture.settings.copy = copy;
    fixture.git(&["lfs", "install", "--local", "--skip-repo"]);
    std::fs::write(
        fixture.repo.join(".gitattributes"),
        "*.bin filter=lfs -text\n",
    )
    .unwrap();
    let content = vec![42_u8; 8192];
    std::fs::write(fixture.repo.join("asset.bin"), &content).unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@localhost",
        "commit",
        "-m",
        "LFS fixture",
    ]);
    // Model the usual install: LFS filters exist only in global config, which
    // Agentworks deliberately ignores. Keep the object in the shared LFS store.
    for key in [
        "filter.lfs.clean",
        "filter.lfs.smudge",
        "filter.lfs.process",
        "filter.lfs.required",
    ] {
        fixture.git(&["config", "--local", "--unset-all", key]);
    }
    let pointer = git::git(&fixture.repo, &fixture.root, &["show", "HEAD:asset.bin"])
        .await
        .unwrap()
        .1;
    assert!(pointer.starts_with("version https://git-lfs.github.com/spec/v1\n"));
    if copy == agentworks_workspace::config::CopyMode::RequireCow {
        let commit = git::git(&fixture.repo, &fixture.root, &["rev-parse", "HEAD"])
            .await
            .unwrap()
            .1;
        let legacy = fixture.repo.join(".git/agentworks-cow").join(commit);
        fixture.git(&[
            "-c",
            "filter.lfs.process=",
            "-c",
            "filter.lfs.smudge=",
            "-c",
            "filter.lfs.required=false",
            "worktree",
            "add",
            "--detach",
            legacy.to_str().unwrap(),
            "HEAD",
        ]);
        assert_eq!(
            std::fs::read_to_string(legacy.join("asset.bin"))
                .unwrap()
                .trim_end(),
            pointer
        );
    }
    let result = worktrees::prepare_in(
        &fixture,
        Some(&fixture.workspace()),
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
    assert_eq!(std::fs::read(path.join("asset.bin")).unwrap(), content);
    #[cfg(target_os = "linux")]
    assert_lfs_cow_sharing(&owned);
    let inspection = cleanup::inspect_in(&fixture, Some(owned.clone())).await;
    assert!(!inspection.dirty.unwrap().any());
    cleanup::remove_in(&fixture, &owned, inspection.fingerprint.as_deref(), false)
        .await
        .unwrap();
    owned.removed = Some(1);
    cleanup::restore_in(&fixture, &owned).await.unwrap();
    owned.removed = None;
    assert_eq!(std::fs::read(path.join("asset.bin")).unwrap(), content);
    #[cfg(target_os = "linux")]
    assert_lfs_cow_sharing(&owned);
    assert!(
        !cleanup::inspect_in(&fixture, Some(owned.clone()))
            .await
            .dirty
            .unwrap()
            .any()
    );
    std::fs::write(path.join("asset.bin"), "edited LFS file").unwrap();
    assert_eq!(
        std::fs::read(fixture.repo.join("asset.bin")).unwrap(),
        content
    );
    if owned.population.as_deref() == Some("cow") {
        let template = owned
            .common_dir
            .join("agentworks-cow/v2")
            .join(&owned.base_commit)
            .join("asset.bin");
        assert_eq!(std::fs::read(template).unwrap(), content);
    }
}

#[tokio::test]
async fn plain_worktree_hydrates_lfs_on_create_and_restore() {
    lfs_checkout(agentworks_workspace::config::CopyMode::Plain).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cow_worktree_hydrates_lfs_on_create_and_restore() {
    lfs_checkout(agentworks_workspace::config::CopyMode::RequireCow).await;
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
        .join("v2")
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

#[tokio::test]
async fn pr_worktree_fetches_missing_pinned_head_from_matching_origin_only() {
    let fixture = Fixture::new();
    let upstream = fixture.root.join("upstream");
    let git_at = |cwd: &std::path::Path, args: &[&str]| {
        let out = Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git_at(
        &fixture.root,
        &[
            "clone",
            "--no-hardlinks",
            fixture.repo.to_str().unwrap(),
            upstream.to_str().unwrap(),
        ],
    );
    git_at(
        &upstream,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "--allow-empty",
            "-m",
            "PR head",
        ],
    );
    let sha = git_at(&upstream, &["rev-parse", "HEAD"]);
    git_at(&upstream, &["update-ref", "refs/pull/17/head", &sha]);
    fixture.git(&[
        "remote",
        "add",
        "origin",
        "https://ghe.example.com/team/repo.git",
    ]);
    fixture.git(&["config", "protocol.file.allow", "always"]);
    fixture.git(&[
        "config",
        &format!("url.file://{}.insteadOf", upstream.display()),
        "https://ghe.example.com/team/repo.git",
    ]);
    let mut workspace = fixture.workspace();
    workspace.base_ref = Some(sha.clone());
    workspace.pull_request = Some(worktrees::PullRequest {
        host: "wrong.example.com".into(),
        owner: "team".into(),
        repo: "repo".into(),
        number: 17,
        head_sha: sha.clone(),
    });
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
    assert!(error.1.contains("origin matches"));
    assert_eq!(fixture.reservations.load(Ordering::SeqCst), 0);
    workspace.pull_request.as_mut().unwrap().host = "ghe.example.com".into();
    let (path, _) = worktrees::prepare_in(
        &fixture,
        Some(&workspace),
        Policy::default(),
        false,
        fixture.repo.clone(),
        Some("creation"),
    )
    .await
    .unwrap();
    assert_eq!(git_at(&path, &["rev-parse", "HEAD"]), sha);
    assert_eq!(
        git_at(&fixture.repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
}
