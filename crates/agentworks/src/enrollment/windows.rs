//! Windows hosts. OpenSSH's default shell may be cmd.exe or PowerShell, so every
//! step runs as an encoded Windows PowerShell 5.1 command line, which both parse
//! the same way. PowerShell takes over redirected stdin, so the bundle travels by
//! scp (SFTP) instead of being piped like on Unix.
use super::{Output, Result, checksum, managed, output, run, stage};
use crate::{catalog::Host, hosts};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Home-relative, because SFTP and `%USERPROFILE%` both resolve it without asking
/// the host for its (possibly redirected) `%LOCALAPPDATA%`.
fn release(id: &str) -> String {
    format!("AppData\\Local\\agentworks\\releases\\{id}")
}

fn powershell(script: &str) -> String {
    let script = format!(
        "$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'\n{script}"
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}",
        BASE64.encode(utf16)
    )
}

fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Installs the dedicated key where sshd will look for it. Administrators use
/// the shared `administrators_authorized_keys` under the default sshd_config.
pub(super) fn bootstrap(public: &str) -> String {
    powershell(&format!(
        r#"if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {{
  [Console]::Error.WriteLine("This server installs the x86_64 Windows worker; this host is $env:PROCESSOR_ARCHITECTURE."); exit 1
}}
if (-not (Test-Path "$env:SystemRoot\System32\tar.exe")) {{
  [Console]::Error.WriteLine('Windows 10 version 1803 or later is required (tar.exe is missing).'); exit 1
}}
$key = {key}
$admin = [Security.Principal.WindowsIdentity]::GetCurrent().Groups -contains [Security.Principal.SecurityIdentifier]'S-1-5-32-544'
$config = "$env:ProgramData\ssh\sshd_config"
if ($admin -and (Test-Path $config) -and (Select-String -Path $config -Pattern '^\s*Match\s+Group\s+administrators\b' -Quiet)) {{
  $file = "$env:ProgramData\ssh\administrators_authorized_keys"
}} else {{
  $dir = Join-Path $env:USERPROFILE '.ssh'
  New-Item -ItemType Directory -Force $dir | Out-Null
  $file = Join-Path $dir 'authorized_keys'
}}
$created = -not (Test-Path $file)
$lines = @()
if (-not $created) {{ $lines = @(Get-Content $file) }}
if ($lines -notcontains $key) {{
  # Rewriting keeps the file's ACL; UTF-8 without BOM is what sshd reads reliably.
  [IO.File]::WriteAllLines($file, [string[]]($lines + $key), (New-Object Text.UTF8Encoding $false))
}}
if ($created -and $file.StartsWith($env:ProgramData)) {{
  icacls $file /inheritance:r /grant '*S-1-5-18:F' '*S-1-5-32-544:F' | Out-Null
}}
"`nDedicated key installed in $file."
"#,
        key = literal(public)
    ))
}

/// The Windows worker built beside this server (`<exe>-worker.exe`), e.g. by
/// `npm run build:windows-worker`. It cannot be probed here; install checks it.
pub fn worker_executable() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut name = exe.file_name()?.to_os_string();
    name.push("-worker.exe");
    Some(exe.with_file_name(name)).filter(|p| p.is_file())
}

async fn bundle(dir: &Path, out: &Output) -> Result<PathBuf> {
    let worker = worker_executable().ok_or(
        "No Windows worker beside this server. Build it with `npm run build:windows-worker`.",
    )?;
    let staging = dir.join("runtime");
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::copy(worker, staging.join("agentworks.exe"))
        .await
        .map_err(|e| e.to_string())?;
    // cmd.exe needs CRLF line endings, whatever the checkout's line endings are.
    let launch = include_str!("../../../../scripts/remote-runtime/launch.cmd")
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    tokio::fs::write(staging.join("launch.cmd"), launch)
        .await
        .map_err(|e| e.to_string())?;
    let archive = dir.join("bundle.tar.gz");
    run(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .args(["agentworks.exe", "launch.cmd"]),
        out,
    )
    .await?;
    Ok(archive)
}

/// scp resolves relative paths against the remote home, for `host:path` and
/// `scp://host:port/path` alike.
fn copy(host: &Host, source: &Path, path: &str) -> Command {
    let mut cmd = Command::new(std::env::var("AGENTWORKS_SCP").unwrap_or_else(|_| "scp".into()));
    cmd.args(["-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20"])
        .args(hosts::managed_ssh_args(host))
        .arg("--")
        .arg(source);
    let path = path.replace('\\', "/");
    cmd.arg(match host.destination.strip_prefix("ssh://") {
        Some(rest) => format!("scp://{}/{path}", rest.trim_end_matches('/')),
        None => format!("{}:{path}", host.destination),
    });
    cmd
}

/// Uploads and installs the worker; returns the saved worker command.
pub(super) async fn install(host: &Host, dir: &Path, id: &str, out: &Output) -> Result<String> {
    let release = release(id);
    let bundle = bundle(dir, out).await?;
    run(
        &mut managed(
            host,
            &powershell(&format!(
                "New-Item -ItemType Directory -Force (Join-Path $env:USERPROFILE {}) | Out-Null",
                literal(&release)
            )),
        ),
        out,
    )
    .await?;
    run(
        &mut copy(host, &bundle, &format!("{release}\\bundle.tar.gz")),
        out,
    )
    .await?;
    let checksum = checksum(&bundle).await?;
    stage(out, "Installing the worker…").await?;
    let script = format!(
        r#"Set-Location (Join-Path $env:USERPROFILE {release})
if ((Get-FileHash -Algorithm SHA256 bundle.tar.gz).Hash.ToLower() -ne '{checksum}') {{
  [Console]::Error.WriteLine('Bundle checksum mismatch'); exit 1
}}
# System32's bsdtar: a Git for Windows tar on PATH would read C: as a remote host.
& "$env:SystemRoot\System32\tar.exe" -xzf bundle.tar.gz
if ($LASTEXITCODE) {{ exit 1 }}
Remove-Item bundle.tar.gz
# Fail here for a mismatched build, before saving this host.
$reply = & .\agentworks.exe --worker-protocol | ConvertFrom-Json
if ($reply.workerProtocol -ne {protocol}) {{
  [Console]::Error.WriteLine("Worker protocol $($reply.workerProtocol) does not match this server ({protocol})."); exit 1
}}
"`nWorker installed. Reconnects use its absolute launch path."
"#,
        release = literal(&release),
        protocol = agentworks_runtime::remote::PROTOCOL,
    );
    run(&mut managed(host, &powershell(&script)), out).await?;
    output(out, b"\r\n").await?;
    // cmd.exe expands %USERPROFILE% whichever shell sshd starts, and its exit code
    // is the worker's.
    Ok(format!(
        "cmd.exe /d /c \"%USERPROFILE%\\{release}\\launch.cmd\""
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_encoded_for_any_default_shell() {
        let command = powershell("'it''s'");
        let encoded = command.rsplit(' ').next().unwrap();
        // Base64 carries nothing cmd.exe or PowerShell would interpret.
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        );
        let bytes = BASE64.decode(encoded).unwrap();
        let utf16: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert!(String::from_utf16(&utf16).unwrap().ends_with("\n'it''s'"));
        assert_eq!(literal("a'b"), "'a''b'");
    }

    #[test]
    fn copies_resolve_against_the_remote_home() {
        let mut host = Host {
            id: "h".into(),
            name: "Box".into(),
            destination: "me@box".into(),
            identity_file: None,
            known_hosts_file: None,
            command: String::new(),
            roots: Vec::new(),
        };
        let target = |host: &Host| {
            let cmd = copy(host, Path::new("/b.tgz"), "AppData\\Local\\x\\b.tgz");
            cmd.as_std()
                .get_args()
                .last()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(target(&host), "me@box:AppData/Local/x/b.tgz");
        host.destination = "ssh://me@box:2222".into();
        assert_eq!(target(&host), "scp://me@box:2222/AppData/Local/x/b.tgz");
    }
}
