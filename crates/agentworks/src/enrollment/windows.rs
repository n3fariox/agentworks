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
    format!("{RELEASES}{id}")
}
const RELEASES: &str = "AppData\\Local\\agentworks\\releases\\";

/// cmd.exe expands %USERPROFILE% whichever shell sshd starts, and its exit code
/// is the worker's.
pub(super) fn command(id: &str) -> String {
    format!(
        "cmd.exe /d /c \"%USERPROFILE%\\{}\\launch.cmd\"",
        release(id)
    )
}

/// The release a saved command launches, if this server installed it.
pub(super) fn release_id(command: &str) -> Option<&str> {
    let id = command
        .strip_prefix("cmd.exe /d /c \"%USERPROFILE%\\")?
        .strip_prefix(RELEASES)?
        .strip_suffix("\\launch.cmd\"")?;
    uuid::Uuid::parse_str(id).is_ok().then_some(id)
}

/// Best effort: the replaced worker has exited once its successor holds the host lock.
pub(super) async fn remove_release(host: &Host, id: &str, out: &Output) -> Result<()> {
    let script = format!(
        "Remove-Item -Recurse -Force -ErrorAction SilentlyContinue (Join-Path $env:USERPROFILE {})",
        literal(&release(id))
    );
    run(&mut managed(host, &powershell(&script)), out).await
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
        r#"if (-not (Test-Path "$env:SystemRoot\System32\tar.exe")) {{
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

/// Prints the host's CPU, e.g. `AMD64` or `ARM64`. The 64-bit PowerShell that
/// sshd starts reports the native architecture.
pub(super) fn architecture() -> String {
    powershell("$env:PROCESSOR_ARCHITECTURE")
}

async fn bundle(dir: &Path, worker: &Path, out: &Output) -> Result<PathBuf> {
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
pub(super) async fn install(
    host: &Host,
    dir: &Path,
    id: &str,
    worker: &Path,
    out: &Output,
) -> Result<String> {
    let release = release(id);
    let bundle = bundle(dir, worker, out).await?;
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
    Ok(command(id))
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
    fn saved_commands_name_their_release() {
        let id = "3f1c2d4e-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
        assert_eq!(
            command(id),
            format!(
                "cmd.exe /d /c \"%USERPROFILE%\\AppData\\Local\\agentworks\\releases\\{id}\\launch.cmd\""
            )
        );
        assert_eq!(release_id(&command(id)), Some(id));
        assert_eq!(release_id(&command("..\\x")), None);
        assert_eq!(release_id("agentworks"), None);
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
