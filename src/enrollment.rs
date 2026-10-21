//! Interactive SSH bootstrap. Credentials travel only through a local PTY and
//! the browser WebSocket; neither terminal output nor input is persisted.
use crate::{App, catalog::Host, hosts};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::PermissionsExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, unix::AsyncFd},
    process::Command,
    sync::{Semaphore, mpsc},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, String>;
type Output = mpsc::Sender<Message>;
static SLOTS: OnceLock<Semaphore> = OnceLock::new();

#[derive(Deserialize)]
struct Start {
    name: String,
    destination: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Input {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
}

pub async fn upgrade(State(app): State<App>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(16 * 1024)
        .on_upgrade(move |socket| session(app, socket))
}
async fn event(out: &Output, value: serde_json::Value) -> Result<()> {
    out.send(Message::Text(value.to_string().into()))
        .await
        .map_err(|_| "Terminal disconnected".into())
}
async fn stage(out: &Output, message: &str) -> Result<()> {
    event(out, json!({"type":"stage", "message":message})).await
}
async fn output(out: &Output, bytes: &[u8]) -> Result<()> {
    out.send(Message::Binary(bytes.to_vec().into()))
        .await
        .map_err(|_| "Terminal disconnected".into())
}
async fn session(app: App, mut socket: WebSocket) {
    let Ok(_slot) = SLOTS.get_or_init(|| Semaphore::new(2)).try_acquire() else {
        let _ = socket.send(Message::Text(json!({"type":"error","message":"Two hosts are already enrolling. Try again when one finishes."}).to_string().into())).await;
        return;
    };
    let request = match tokio::time::timeout(Duration::from_secs(30), socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<Start>(&text).ok(),
        _ => None,
    };
    let Some(request) = request else {
        return;
    };
    let (mut sink, mut stream) = socket.split();
    let (out, mut outgoing) = mpsc::channel(32);
    let (input, mut incoming) = mpsc::channel(32);
    let writer = async {
        while let Some(message) = outgoing.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    };
    let reader = async {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Text(text) => {
                    if let Ok(value) = serde_json::from_str::<Input>(&text) {
                        // A full queue cancels setup instead of blocking disconnect detection.
                        if input.try_send(value).is_err() {
                            break;
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    let dir = std::path::absolute(
        app.catalog
            .lock()
            .unwrap()
            .credentials_dir()
            .join(Uuid::new_v4().to_string()),
    )
    .unwrap();
    tokio::pin!(writer, reader);
    let mut writer_finished = false;
    {
        let work = async {
            let result = tokio::time::timeout(
                Duration::from_secs(15 * 60),
                enroll(&app, request, &dir, &out, &mut incoming),
            )
            .await;
            match result {
                Ok(Ok(host)) => {
                    let _ = event(
                        &out,
                        json!({"type":"complete","host":hosts::view(&app.hosts, &host)}),
                    )
                    .await;
                }
                other => {
                    let message = match other {
                        Ok(Err(e)) => e,
                        _ => "Enrollment timed out after 15 minutes".into(),
                    };
                    let _ = event(&out, json!({"type":"error","message":message})).await;
                }
            }
        };
        // Closing the browser cancels setup and drops every kill-on-drop subprocess.
        tokio::pin!(work);
        tokio::select! { _ = &mut work => {}, _ = &mut reader => {}, _ = &mut writer => { writer_finished = true; } }
    }
    // Drain the final completion/error message before closing the WebSocket.
    drop(out);
    if !writer_finished {
        tokio::select! { _ = &mut writer => {}, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
    }
    // Keep managed keys only if the host was committed. On cancellation a public
    // authorized_keys entry may remain; it is labelled by this directory's UUID.
    let canonical_dir = tokio::fs::canonicalize(&dir)
        .await
        .unwrap_or_else(|_| dir.clone());
    let committed =
        app.catalog.lock().unwrap().data.hosts.iter().any(|h| {
            h.identity_file.as_deref() == Some(canonical_dir.join("id_ed25519").as_path())
        });
    if !committed {
        let _ = tokio::fs::remove_dir_all(&dir).await;
    } else {
        let _ = tokio::fs::remove_file(dir.join("bundle.tar.gz")).await;
        let _ = tokio::fs::remove_dir_all(dir.join("runtime")).await;
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn ssh() -> Command {
    Command::new(std::env::var("AGENTWORKS_SSH").unwrap_or_else(|_| "ssh".into()))
}
fn managed(host: &Host, command: &str) -> Command {
    let mut cmd = ssh();
    cmd.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=20",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ])
    .args(hosts::managed_ssh_args(host))
    .arg("--")
    .arg(&host.destination)
    .arg(command);
    cmd
}
fn platform_check() -> String {
    let os = match std::env::consts::OS {
        "macos" => "Darwin",
        "linux" => "Linux",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" if os == "Darwin" => "arm64",
        other => other,
    };
    format!(
        "test \"$(uname -s)\" = {} && test \"$(uname -m)\" = {} || {{ echo 'This server can only enroll a matching OS/CPU. Use manual setup for other platforms.' >&2; exit 1; }};",
        quote(os),
        quote(arch)
    )
}
async fn enroll(
    app: &App,
    req: Start,
    dir: &Path,
    out: &Output,
    input: &mut mpsc::Receiver<Input>,
) -> Result<Host> {
    let mut host =
        app.catalog
            .lock()
            .unwrap()
            .prepare_host(&req.name, &req.destination, "agentworks")?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|e| e.to_string())?;
    let dir = tokio::fs::canonicalize(dir)
        .await
        .map_err(|e| e.to_string())?;
    let key = dir.join("id_ed25519");
    let known = dir.join("known_hosts");
    let tag = format!("agentworks:{}", dir.file_name().unwrap().to_string_lossy());
    stage(out, "Generating a dedicated SSH key…").await?;
    run(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", &tag, "-f"])
            .arg(&key),
        out,
    )
    .await?;
    let public = tokio::fs::read_to_string(key.with_extension("pub"))
        .await
        .map_err(|e| e.to_string())?;
    let public = format!("restrict {}", public.trim());
    let bootstrap = format!(
        "set -eu; {} command -v tar >/dev/null; mise_bin=$(command -v mise || true); if [ -z \"$mise_bin\" ]; then mise_bin=\"$HOME/.local/bin/mise\"; fi; test -x \"$mise_bin\" || {{ echo 'mise is required. Install it on this host: https://mise.jdx.dev/getting-started.html' >&2; exit 1; }}; umask 077; mkdir -p \"$HOME/.ssh\"; chmod 700 \"$HOME/.ssh\"; touch \"$HOME/.ssh/authorized_keys\"; chmod 600 \"$HOME/.ssh/authorized_keys\"; grep -qxF {} \"$HOME/.ssh/authorized_keys\" || printf '\\n%s\\n' {} >> \"$HOME/.ssh/authorized_keys\"; printf '\\nDedicated key installed.\\n'",
        platform_check(),
        quote(&public),
        quote(&public)
    );
    stage(
        out,
        "Connect: confirm the host fingerprint and enter your SSH password if prompted.",
    )
    .await?;
    let mut cmd = ssh();
    cmd.args([
        "-T",
        "-o",
        "StrictHostKeyChecking=ask",
        "-o",
        "ConnectTimeout=20",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "BatchMode=no",
        "-o",
    ])
    .arg(hosts::known_hosts_option(&known))
    .arg("--")
    .arg(&host.destination)
    .arg(format!("sh -c {}", quote(&bootstrap)));
    interactive(&mut cmd, out, input).await?;
    host.identity_file = Some(key);
    host.known_hosts_file = Some(known);
    stage(out, "Verifying the new key…").await?;
    run(&mut managed(&host, "true"), out).await?;
    let release = format!(
        ".local/share/agentworks/releases/{}",
        dir.file_name().unwrap().to_string_lossy()
    );
    let remote = format!("\"$HOME/{release}\"");
    stage(out, "Preparing and copying worker and adapter packages…").await?;
    let bundle = bundle(&dir, out).await?;
    let command = format!("umask 077; mkdir -p {remote} && cat > {remote}/bundle.tar.gz");
    let mut upload = managed(&host, &command);
    upload.stdin(Stdio::from(File::open(&bundle).map_err(|e| e.to_string())?));
    run(&mut upload, out).await?;
    // SHA-256 is checked before extraction; only our own source files are bundled.
    let bytes = tokio::fs::read(&bundle).await.map_err(|e| e.to_string())?;
    let checksum = tokio::task::spawn_blocking(move || crate::sha256::hex(&bytes))
        .await
        .map_err(|e| e.to_string())?;
    stage(
        out,
        "Installing Node with mise and locked adapter dependencies…",
    )
    .await?;
    let install = format!(
        "set -eu; cd {remote}; if command -v sha256sum >/dev/null; then actual=$(sha256sum bundle.tar.gz); else actual=$(shasum -a 256 bundle.tar.gz); fi; test \"${{actual%% *}}\" = {checksum} || {{ echo 'Bundle checksum mismatch' >&2; exit 1; }}; tar -xzf bundle.tar.gz; rm bundle.tar.gz; sh install.sh"
    );
    run(
        &mut managed(&host, &format!("sh -c {}", quote(&install))),
        out,
    )
    .await?;
    let command = format!("{remote}/launch");
    stage(out, "Connecting the worker…").await?;
    let saved = app.catalog.lock().unwrap().add_managed_host(
        &host.name,
        &host.destination,
        &command,
        host.identity_file,
        host.known_hosts_file,
    )?;
    app.hosts.start(saved.clone());
    // Report connection/provider availability from the actual worker handshake.
    for _ in 0..600 {
        let link = app.hosts.link(&saved.id);
        if link.status == "connected" {
            return Ok(saved);
        }
        if link.status == "offline" {
            return Err(format!(
                "Host installed and saved, but the worker could not connect: {}. Use Reconnect after fixing it.",
                link.detail
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("Host installed and saved; worker connection is taking longer than expected. Check its status in Hosts.".into())
}

async fn bundle(dir: &Path, out: &Output) -> Result<PathBuf> {
    let root = crate::runtime_root();
    let staging = dir.join("runtime");
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::copy(
        std::env::current_exe().map_err(|e| e.to_string())?,
        staging.join("agentworks"),
    )
    .await
    .map_err(|e| e.to_string())?;
    for (file, bytes) in [
        (
            "mise.toml",
            include_bytes!("../scripts/remote-runtime/mise.toml").as_slice(),
        ),
        (
            "launch",
            include_bytes!("../scripts/remote-runtime/launch").as_slice(),
        ),
        (
            "install.sh",
            include_bytes!("../scripts/remote-runtime/install.sh").as_slice(),
        ),
    ] {
        tokio::fs::write(staging.join(file), bytes)
            .await
            .map_err(|e| e.to_string())?;
    }
    let archive = dir.join("bundle.tar.gz");
    run(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .args(["agentworks", "mise.toml", "launch", "install.sh"])
            .arg("-C")
            .arg(&root)
            .args([
                "package.json",
                "package-lock.json",
                "vendor/codex-acp",
                "scripts/demo-agent.mjs",
            ]),
        out,
    )
    .await?;
    Ok(archive)
}

/// Stream both pipes with bounded backpressure. The browser never receives input
/// or a transcript of it; ssh reads passwords directly from its controlling TTY.
async fn run(cmd: &mut Command, out: &Output) -> Result<()> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start setup command: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let pump = |mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>| async move {
        let mut buf = [0; 8192];
        loop {
            let n = reader.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            output(out, &buf[..n]).await?;
        }
        Ok::<_, String>(())
    };
    let (_, _, status) = tokio::try_join!(
        pump(Box::new(&mut stdout)),
        pump(Box::new(&mut stderr)),
        async { child.wait().await.map_err(|e| e.to_string()) }
    )?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Setup command failed ({status}). See the terminal output above."
        ))
    }
}

struct Pty(AsyncFd<File>);
impl Pty {
    fn open(cmd: &mut Command) -> io::Result<Self> {
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for file in [&master, &slave] {
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // Keep terminal input unbuffered and unechoed even before ssh disables
        // echo for a password prompt. Output keeps normal CRLF terminal handling.
        let mut term = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut term) } < 0 {
            return Err(io::Error::last_os_error());
        }
        term.c_lflag &= !(libc::ECHO | libc::ICANON);
        if unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &term) } < 0 {
            return Err(io::Error::last_os_error());
        }
        cmd.stdin(slave.try_clone()?)
            .stdout(slave.try_clone()?)
            .stderr(slave);
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        // The master is owned exclusively here and all I/O goes through this registration.
        Ok(Self(unsafe { AsyncFd::register(master)? }))
    }
    async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.0.readable().await?;
            match ready.try_io(|fd| {
                let n = unsafe {
                    libc::read(fd.get_ref().as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                if n < 0 {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error() == Some(libc::EIO) {
                        Ok(0)
                    } else {
                        Err(e)
                    }
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(r) => return r,
                Err(_) => continue,
            }
        }
    }
    async fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut bytes = bytes;
        while !bytes.is_empty() {
            let mut ready = self.0.writable().await?;
            match ready.try_io(|fd| {
                let n = unsafe {
                    libc::write(fd.get_ref().as_raw_fd(), bytes.as_ptr().cast(), bytes.len())
                };
                if n <= 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(r) => bytes = &bytes[r?..],
                Err(_) => continue,
            }
        }
        Ok(())
    }
    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows.clamp(5, 100),
            ws_col: cols.clamp(20, 240),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.0.get_ref().as_raw_fd(), libc::TIOCSWINSZ as _, &size);
        }
    }
}
async fn interactive(
    cmd: &mut Command,
    out: &Output,
    input: &mut mpsc::Receiver<Input>,
) -> Result<()> {
    let pty = Pty::open(cmd).map_err(|e| format!("Could not open SSH terminal: {e}"))?;
    let mut child = cmd.kill_on_drop(true).spawn().map_err(|e| e.to_string())?;
    // Drop Command's copies of the slave, so EOF arrives when ssh exits.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut buf = [0; 8192];
    loop {
        tokio::select! {
            result = pty.read(&mut buf) => { let n = result.map_err(|e| e.to_string())?; if n == 0 { break; } output(out, &buf[..n]).await?; }
            value = input.recv() => match value {
                Some(Input::Input { data }) => pty.write(data.as_bytes()).await.map_err(|e| e.to_string())?,
                Some(Input::Resize { cols, rows }) => pty.resize(cols, rows),
                None => return Err("Terminal disconnected".into()),
            }
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "SSH enrollment failed ({status}). Check the terminal output and try again."
        ))
    }
}
