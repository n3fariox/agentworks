//! Supervised interpreter worker: hard CPU/memory/wall bounds via a child process.
//!
//! In-process tick/heap limits are best-effort (starlark-rust documents that a
//! single native operation can allocate far past the heap limit between checks).
//! The worker applies RLIMIT_AS/RLIMIT_CPU before exec and the client kills it
//! on a wall-clock deadline. A dead worker yields a `Worker` diagnostic and is
//! respawned lazily; the host process is unaffected.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::engine::{Diagnostic, DiagnosticKind, Engine, Installation};

/// One request line on the worker's stdin.
#[derive(Serialize, Deserialize)]
pub struct Request {
    pub install: Installation,
    pub source: String,
    pub first: Json,
    pub second: Json,
}

/// One response line on the worker's stdout.
#[derive(Serialize, Deserialize)]
pub enum Response {
    Ok { value: Json, ticks: u64, micros: u64 },
    Err(Diagnostic),
}

/// Worker main loop. Compiled scripts are cached by content hash for the
/// worker's lifetime; each call still uses a fresh heap.
pub fn serve() {
    let engine = match Engine::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("worker: {e}");
            std::process::exit(2);
        }
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let response = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response::Err(Diagnostic {
                script: String::new(),
                hash: String::new(),
                kind: DiagnosticKind::Worker,
                message: format!("bad request: {e}"),
            }),
            Ok(req) => match engine
                .compile(&req.install, &req.source)
                .and_then(|c| engine.call(&c, &req.first, &req.second))
            {
                Ok((value, stats)) => Response::Ok {
                    value,
                    ticks: stats.ticks,
                    micros: stats.elapsed.as_micros() as u64,
                },
                Err(d) => Response::Err(d),
            },
        };
        if serde_json::to_writer(&mut out, &response).is_err() || out.write_all(b"\n").is_err() || out.flush().is_err()
        {
            break;
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkerLimits {
    pub address_space_bytes: u64,
    pub cpu_seconds: u64,
    pub wall: Duration,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        WorkerLimits {
            address_space_bytes: 512 * 1024 * 1024,
            cpu_seconds: 10,
            wall: Duration::from_secs(2),
        }
    }
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

pub struct WorkerClient {
    program: PathBuf,
    limits: WorkerLimits,
    running: Option<Running>,
    pub spawns: u32,
}

impl WorkerClient {
    pub fn new(program: impl Into<PathBuf>, limits: WorkerLimits) -> Self {
        WorkerClient {
            program: program.into(),
            limits,
            running: None,
            spawns: 0,
        }
    }

    fn spawn(&mut self) -> std::io::Result<&mut Running> {
        if self.running.is_none() {
            let mut command = Command::new(&self.program);
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let (address_space, cpu) = (self.limits.address_space_bytes, self.limits.cpu_seconds);
            // SAFETY: only async-signal-safe setrlimit/prctl calls between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    let set = |resource, value: u64| {
                        let limit = libc::rlimit {
                            rlim_cur: value as libc::rlim_t,
                            rlim_max: value as libc::rlim_t,
                        };
                        if libc::setrlimit(resource, &limit) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    };
                    set(libc::RLIMIT_AS, address_space)?;
                    set(libc::RLIMIT_CPU, cpu)?;
                    set(libc::RLIMIT_CORE, 0)?;
                    // Piped core_pattern handlers (systemd-coredump) ignore RLIMIT_CORE.
                    if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn()?;
            let stdin = child.stdin.take().expect("piped stdin");
            let stdout = child.stdout.take().expect("piped stdout");
            let (tx, lines) = channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            self.spawns += 1;
            self.running = Some(Running { child, stdin, lines });
        }
        Ok(self.running.as_mut().unwrap())
    }

    fn kill(&mut self) -> String {
        let Some(mut r) = self.running.take() else {
            return "not running".into();
        };
        let _ = r.child.kill();
        match r.child.wait() {
            Ok(status) => status.to_string(),
            Err(e) => e.to_string(),
        }
    }

    pub fn call(
        &mut self,
        install: &Installation,
        source: &str,
        first: &Json,
        second: &Json,
    ) -> Result<(Json, u64), Diagnostic> {
        let fail = |message: String| Diagnostic {
            script: install.name.clone(),
            hash: install.hash.clone(),
            kind: DiagnosticKind::Worker,
            message,
        };
        let request = Request {
            install: install.clone(),
            source: source.to_owned(),
            first: first.clone(),
            second: second.clone(),
        };
        let mut line = serde_json::to_string(&request).map_err(|e| fail(e.to_string()))?;
        line.push('\n');
        let wall = self.limits.wall;
        let running = self.spawn().map_err(|e| fail(format!("spawn failed: {e}")))?;
        if running
            .stdin
            .write_all(line.as_bytes())
            .and_then(|_| running.stdin.flush())
            .is_err()
        {
            let status = self.kill();
            return Err(fail(format!("worker exited ({status})")));
        }
        match running.lines.recv_timeout(wall) {
            Ok(line) => match serde_json::from_str::<Response>(&line) {
                Ok(Response::Ok { value, ticks, .. }) => Ok((value, ticks)),
                Ok(Response::Err(d)) => Err(d),
                Err(e) => {
                    self.kill();
                    Err(fail(format!("malformed worker response: {e}")))
                }
            },
            Err(RecvTimeoutError::Timeout) => {
                let status = self.kill();
                Err(fail(format!(
                    "worker exceeded {wall:?} wall time and was killed ({status})"
                )))
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = self.kill();
                Err(fail(format!("worker exited ({status})")))
            }
        }
    }
}

impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.kill();
    }
}
