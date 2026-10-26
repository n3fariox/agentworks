//! Platform PTY layer: a process on a Unix PTY or Windows ConPTY, with owned
//! output/input workers and whole-session cleanup. Used by the session
//! terminal and by interactive SSH enrollment.
//!
//! Follows the shape of morsh's `portable-pty` integration (reader thread,
//! bounded output channel, separate exit observation for ConPTY), with
//! Agentworks' own lifecycle: real exit status, bounded input queue, and
//! process-tree cleanup that never signals a reused PID.
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
use std::{
    io::{self, Read, Write},
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

const READ_CHUNK: usize = 16384;
const INPUT_LIMIT: usize = 1024 * 1024;

pub enum Event {
    Output(Vec<u8>),
    /// The output side closed: every holder of the terminal has gone.
    Closed,
    Failed(String),
}

/// Where the output worker delivers events. A bounded channel makes the
/// worker wait, and the child then blocks on a full PTY.
pub trait Sink: Clone + Send + 'static {
    /// Returns false once the receiver has gone.
    fn deliver(&self, event: Event) -> bool;
}
impl Sink for mpsc::SyncSender<Event> {
    fn deliver(&self, event: Event) -> bool {
        self.send(event).is_ok()
    }
}
impl Sink for tokio::sync::mpsc::Sender<Event> {
    // Workers are plain threads, never inside the async runtime.
    fn deliver(&self, event: Event) -> bool {
        self.blocking_send(event).is_ok()
    }
}

pub struct Pty {
    child: Box<dyn Child + Send + Sync>,
    // Taken on Windows once the child exits: closing the pseudoconsole flushes
    // and closes its output pipe, which ConPTY otherwise keeps open.
    master: Option<Box<dyn MasterPty + Send>>,
    input: Option<mpsc::Sender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    reaped: bool,
    #[cfg(windows)]
    job: windows::Job,
}

/// Start `command` as the session leader of a new PTY. Its program, arguments,
/// working directory and environment changes are used; stdio is the PTY.
/// Without `echo`, input is neither echoed nor line-buffered from the start,
/// before the program sets its own modes (Unix only; ConPTY leaves this to the
/// program's console mode).
pub fn spawn(
    command: &Command,
    cols: u16,
    rows: u16,
    echo: bool,
    events: impl Sink,
) -> io::Result<Pty> {
    let command = builder(command)?;
    let pair = native_pty_system()
        .openpty(size(cols, rows))
        .map_err(io::Error::other)?;
    #[cfg(unix)]
    if !echo {
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("The PTY has no descriptor"))?;
        let mut term = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut term) } < 0 {
            return Err(io::Error::last_os_error());
        }
        term.c_lflag &= !(libc::ECHO | libc::ICANON);
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(windows)]
    let _ = echo;
    // Acquire every handle before starting the child, so a setup failure
    // never leaves a process running without an owner.
    let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
    let mut writer = pair.master.take_writer().map_err(io::Error::other)?;
    #[cfg(windows)]
    let job = windows::Job::new()?;
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(io::Error::other)?;
    // The child holds its own copy; ours would keep the output open forever.
    drop(pair.slave);
    let failed = events.clone();
    let (input, chunks) = mpsc::channel::<Vec<u8>>();
    let queued = Arc::new(AtomicUsize::new(0));
    // From here on, dropping the Pty cleans up after any failure.
    let pty = Pty {
        child,
        master: Some(pair.master),
        input: Some(input),
        queued: queued.clone(),
        reaped: false,
        #[cfg(windows)]
        job,
    };
    // Window: processes the child starts before this call are outside the job.
    #[cfg(windows)]
    if let Some(handle) = pty.child.as_raw_handle() {
        pty.job.assign(handle)?;
    }
    // Not joined: a process that escapes the session can hold the PTY open,
    // and shutdown must not wait for it. The thread ends when the PTY closes.
    std::thread::Builder::new()
        .name("pty-output".into())
        .spawn(move || {
            let mut buf = vec![0; READ_CHUNK];
            loop {
                let event = match reader.read(&mut buf) {
                    Ok(0) => Event::Closed,
                    Ok(n) => Event::Output(buf[..n].to_vec()),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if closed(&e) => Event::Closed,
                    Err(e) => Event::Failed(format!("Terminal read failed: {e}")),
                };
                let last = !matches!(event, Event::Output(_));
                if !events.deliver(event) || last {
                    return;
                }
            }
        })?;
    std::thread::Builder::new()
        .name("pty-input".into())
        .spawn(move || {
            for chunk in chunks {
                if let Err(e) = writer.write_all(&chunk).and_then(|()| writer.flush()) {
                    failed.deliver(Event::Failed(format!("Terminal write failed: {e}")));
                    return;
                }
                queued.fetch_sub(chunk.len(), Ordering::AcqRel);
            }
        })?;
    Ok(pty)
}

fn builder(command: &Command) -> io::Result<CommandBuilder> {
    let mut builder = CommandBuilder::new(command.get_program());
    builder.args(command.get_args());
    // CommandBuilder substitutes the home directory for an unset or missing
    // directory; keep Command's meaning of both instead.
    let cwd = match command.get_current_dir() {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()?,
    };
    if !cwd.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", cwd.display()),
        ));
    }
    builder.cwd(cwd);
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => builder.env(key, value),
            None => builder.env_remove(key),
        }
    }
    Ok(builder)
}

/// The user's interactive shell in `cwd`, with launcher state removed.
pub fn shell(cwd: &Path) -> Command {
    let mut command = default_shell();
    command.current_dir(cwd);
    shell_environment(&mut command, std::env::vars_os().map(|(key, _)| key));
    command
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

// Linux reports a closed slave as EIO; Windows reports a closed pipe.
fn closed(error: &io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::EIO) {
        return true;
    }
    error.kind() == io::ErrorKind::BrokenPipe
}

impl Pty {
    #[cfg(target_os = "linux")]
    fn pid(&self) -> u32 {
        self.child.process_id().unwrap_or(0)
    }

    /// Queue bytes for the child in order. Writes happen on the input worker,
    /// so a child that stops reading cannot stall the caller.
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.queued.load(Ordering::Acquire) + bytes.len() > INPUT_LIMIT {
            return Err("Terminal input queue exceeded its limit".into());
        }
        self.queued.fetch_add(bytes.len(), Ordering::AcqRel);
        let input = self.input.as_ref().ok_or("The terminal input has closed")?;
        input
            .send(bytes)
            .map_err(|_| "The terminal input has closed".to_string())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        match &self.master {
            Some(master) => master.resize(size(cols, rows)).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// Observe exit without reaping where the platform allows it.
    pub fn exited(&mut self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            // The shell PID/session ID stays reserved until cleanup has
            // finished. A numeric ID from a reaped child is unsafe to use for
            // finding or signaling terminal jobs.
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { info.si_pid() } != 0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let exited = self.child.try_wait()?.is_some();
            #[cfg(windows)]
            if exited {
                self.master = None;
            }
            Ok(exited)
        }
    }

    /// Stop everything the child started, then collect its exit code. A child
    /// ended by a signal has no exit code.
    pub fn finish(&mut self) -> io::Result<Option<i32>> {
        self.kill_jobs();
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status
            .signal()
            .is_none()
            .then_some(status.exit_code() as i32))
    }

    fn kill_jobs(&self) {
        #[cfg(target_os = "linux")]
        for job in self.jobs() {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    job.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
        #[cfg(windows)]
        self.job.terminate();
    }

    #[cfg(target_os = "linux")]
    fn jobs(&self) -> Vec<std::fs::File> {
        if self.reaped {
            return Vec::new();
        }
        let sid = self.pid() as i32;
        // Fail closed for an unexpected session or a /proc mount from another
        // PID namespace. Never translate /proc IDs into process-group signals.
        let own = unsafe { libc::getpid() };
        let proc_pid = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|stat| stat.split_whitespace().next()?.parse::<i32>().ok());
        if sid <= 1 || sid == unsafe { libc::getsid(0) } || proc_pid != Some(own) {
            return Vec::new();
        }
        std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
                if pid <= 1 || pid == own || unsafe { libc::getsid(pid) } != sid {
                    return None;
                }
                let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
                if fd < 0 {
                    return None;
                }
                let handle = unsafe { std::fs::File::from_raw_fd(fd as i32) };
                // Recheck after opening: the handle pins the target identity, and
                // the unreaped session leader prevents session-ID reuse.
                (unsafe { libc::getsid(pid) } == sid).then_some(handle)
            })
            .collect()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.kill_jobs();
        // portable-pty's kill signals the numeric PID, which is only safe while
        // the child is unreaped. No negative PID or group kill is used.
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Close input last so its worker's end-of-file write reaches no shell.
        self.input = None;
    }
}

#[cfg(unix)]
fn default_shell() -> Command {
    let program = std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/') && Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/sh".into());
    let mut command = Command::new(program);
    command.arg("-i");
    command
}

#[cfg(windows)]
fn default_shell() -> Command {
    // PowerShell 7 when installed, otherwise the Windows PowerShell every
    // supported Windows ships with.
    let pwsh = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("pwsh.exe"))
            .find(|candidate| candidate.is_file())
    });
    let mut command = Command::new(pwsh.unwrap_or_else(|| "powershell.exe".into()));
    command.arg("-NoLogo");
    command
}

const SHELL_VARIABLES: [(&str, &str); 4] = [
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("TERM_PROGRAM", "agentworks"),
    ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
];

pub(crate) fn shell_environment(
    command: &mut Command,
    keys: impl IntoIterator<Item = std::ffi::OsString>,
) {
    for key in keys {
        if transient_shell_variable(&key) {
            command.env_remove(key);
        }
    }
    command.envs(SHELL_VARIABLES);
}

// A terminal is a new interactive context, even when its service was launched
// by an agent, CI runner, desktop terminal or service manager. Keep the user's
// credentials, paths, locale and desktop/SSH integration; remove only launcher
// controls and process-specific state. NO_BROWSER is intentionally preserved.
fn transient_shell_variable(key: &std::ffi::OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    if key.starts_with("AGENTWORKS_") {
        return true;
    }
    if ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"]
        .iter()
        .any(|prefix| {
            key.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit())
            })
        })
    {
        return true;
    }
    matches!(
        key,
        "NO_COLOR"
            | "FORCE_COLOR"
            | "CLICOLOR"
            | "CLICOLOR_FORCE"
            | "CI"
            | "CONTINUOUS_INTEGRATION"
            | "CODEX_CI"
            | "CODEX_SESSION_ID"
            | "CODEX_THREAD_ID"
            | "CODEX_VERSION"
            | "CODEX_MANAGED_BY_NPM"
            | "CODEX_MANAGED_PACKAGE_ROOT"
            | "CODEX_SANDBOX_NETWORK_DISABLED"
            | "CLAUDECODE"
            | "CLAUDE_CODE_ENTRYPOINT"
            | "CLAUDE_CODE_SESSION_ID"
            | "PAGER"
            | "GIT_PAGER"
            | "GH_PAGER"
            | "NOTIFY_SOCKET"
            | "LISTEN_PID"
            | "LISTEN_FDS"
            | "LISTEN_FDNAMES"
            | "WATCHDOG_PID"
            | "WATCHDOG_USEC"
            | "INVOCATION_ID"
            | "JOURNAL_STREAM"
            | "SYSTEMD_EXEC_PID"
            | "MANAGERPID"
            | "MANAGERPIDFDID"
            | "MEMORY_PRESSURE_WATCH"
            | "MEMORY_PRESSURE_WRITE"
            | "TERM_PROGRAM"
            | "TERM_PROGRAM_VERSION"
            | "TERM_SESSION_ID"
            | "TERMCAP"
            | "LINES"
            | "COLUMNS"
            | "COLORFGBG"
            | "TTY"
            | "SSH_TTY"
            | "TMUX"
            | "TMUX_PANE"
            | "STY"
            | "WINDOW"
            | "WINDOWID"
            | "WT_SESSION"
            | "WT_PROFILE_ID"
            | "VTE_VERSION"
            | "KONSOLE_VERSION"
            | "KONSOLE_DBUS_SESSION"
            | "KONSOLE_DBUS_SERVICE"
            | "KONSOLE_DBUS_WINDOW"
            | "ITERM_SESSION_ID"
            | "ITERM_PROFILE"
            | "ALACRITTY_SOCKET"
            | "ALACRITTY_WINDOW_ID"
            | "PWD"
            | "OLDPWD"
            | "SHLVL"
            | "_"
            | "BASHOPTS"
            | "SHELLOPTS"
            | "GIT_DIR"
            | "GIT_WORK_TREE"
            | "GIT_INDEX_FILE"
            | "GIT_COMMON_DIR"
            | "GIT_OBJECT_DIRECTORY"
            | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
            | "GIT_NAMESPACE"
            | "GIT_PREFIX"
            | "GIT_CONFIG"
            | "GIT_CONFIG_COUNT"
            | "GIT_CONFIG_PARAMETERS"
    )
}

/// A kill-on-close Job Object owning the shell and everything it starts.
#[cfg(windows)]
mod windows {
    use std::{io, os::windows::io::RawHandle};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
    };

    pub(super) struct Job(HANDLE);
    // The handle is an owned kernel object reference, usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub(super) fn new() -> io::Result<Self> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);
            let mut info = unsafe { std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        pub(super) fn assign(&self, process: RawHandle) -> io::Result<()> {
            if unsafe { AssignProcessToJobObject(self.0, process as HANDLE) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn terminate(&self) {
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::{collections::HashMap, process::Command};

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_pins_only_its_own_terminal_session_and_keeps_leader_unreaped() {
        let mut sentinel = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let (events, _output) = mpsc::sync_channel(64);
        let mut shell = spawn(&shell(&std::env::temp_dir()), 80, 24, true, events).unwrap();
        let sid = shell.pid() as i32;
        assert_ne!(sid, unsafe { libc::getsid(0) });
        assert_eq!(unsafe { libc::getsid(sid) }, sid);
        for job in shell.jobs() {
            let info =
                std::fs::read_to_string(format!("/proc/self/fdinfo/{}", job.as_raw_fd())).unwrap();
            let pid = info
                .lines()
                .find_map(|line| line.strip_prefix("Pid:")?.trim().parse::<i32>().ok())
                .unwrap();
            assert_ne!(pid, sentinel.id() as i32);
            assert_ne!(pid, unsafe { libc::getpid() });
            assert_eq!(unsafe { libc::getsid(pid) }, sid);
        }
        // Signal the still-unreaped child, then observe twice without
        // releasing its PID. portable-pty's own kill would reap it.
        assert_eq!(unsafe { libc::kill(sid, libc::SIGKILL) }, 0);
        for _ in 0..100 {
            if shell.exited().unwrap() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(shell.exited().unwrap());
        assert!(shell.exited().unwrap());
        assert_eq!(shell.finish().unwrap(), None);
        drop(shell);
        assert!(sentinel.try_wait().unwrap().is_none());
        sentinel.kill().unwrap();
        sentinel.wait().unwrap();
    }

    #[test]
    fn missing_directory_is_an_error_not_the_home_directory() {
        let missing = std::env::temp_dir().join(format!("aw-missing-{}", std::process::id()));
        let (events, _output) = mpsc::sync_channel(64);
        assert!(spawn(&shell(&missing), 80, 24, true, events).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn shell_environment_removes_launcher_state_preserves_user_settings_and_no_browser() {
        // Inject settings into this child only; never mutate the test runner's
        // environment or rely on whichever variables the runner inherited.
        let dropped = [
            "NO_COLOR",
            "FORCE_COLOR",
            "CI",
            "CODEX_CI",
            "CODEX_THREAD_ID",
            "CLAUDECODE",
            "PAGER",
            "GIT_PAGER",
            "GH_PAGER",
            "AGENTWORKS_DB_PATH",
            "AGENTWORKS_SUPERVISOR_TOKEN",
            "NOTIFY_SOCKET",
            "LISTEN_FDS",
            "WATCHDOG_PID",
            "TMUX",
            "SSH_TTY",
            "COLUMNS",
            "PWD",
            "GIT_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
        ];
        let kept = [
            ("NO_BROWSER", "1"),
            ("HOME", "/user/home"),
            ("PATH", "/user/bin"),
            ("LANG", "C.UTF-8"),
            ("XDG_CONFIG_HOME", "/user/config"),
            ("XDG_RUNTIME_DIR", "/user/run"),
            ("SSH_AUTH_SOCK", "/user/agent.sock"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/user/bus"),
            ("DISPLAY", ":0"),
            ("CODEX_HOME", "/user/codex"),
            ("OPENAI_API_KEY", "fixture-key"),
            ("ANTHROPIC_API_KEY", "fixture-key"),
            ("GIT_SSH_COMMAND", "fixture-ssh"),
            ("TERMINFO_DIRS", "/user/terminfo"),
        ];
        let mut command = Command::new("/usr/bin/env");
        command.arg("-0");
        for key in dropped {
            command.env(key, "launcher-only");
        }
        for (key, value) in kept {
            command.env(key, value);
        }
        command
            .env("TERM", "dumb")
            .env("COLORTERM", "")
            .env("TERM_PROGRAM", "parent-terminal");
        let keys = dropped
            .into_iter()
            .chain(kept.into_iter().map(|(key, _)| key))
            .chain(["TERM", "COLORTERM", "TERM_PROGRAM"])
            .map(std::ffi::OsString::from);
        shell_environment(&mut command, keys);
        let output = command.output().unwrap();
        assert!(output.status.success());
        let values: HashMap<_, _> = std::str::from_utf8(&output.stdout)
            .unwrap()
            .split('\0')
            .filter_map(|entry| entry.split_once('='))
            .collect();
        for key in dropped {
            assert!(!values.contains_key(key), "{key} leaked into shell");
        }
        for (key, value) in kept {
            assert_eq!(values.get(key), Some(&value), "{key} changed");
        }
        assert_eq!(values.get("TERM"), Some(&"xterm-256color"));
        assert_eq!(values.get("COLORTERM"), Some(&"truecolor"));
        assert_eq!(values.get("TERM_PROGRAM"), Some(&"agentworks"));
    }
}
