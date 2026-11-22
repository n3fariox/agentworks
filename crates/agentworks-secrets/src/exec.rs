//! `run_with_secrets` as the MCP facade runs it on the session's host. The service only
//! authorizes the call and releases its values; placeholders are expanded after the
//! command is fixed, so the model never sees a value.
use crate::redact::{self, Forms};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use zeroize::Zeroizing;

const MAX_REQUEST_BYTES: usize = 32 * 1024;
const OUTPUT_LIMIT: usize = 1024 * 1024;
const DEFAULT_TIMEOUT: u64 = 300;
const MAX_TIMEOUT: u64 = 3600;
/// Bridge credentials stay with the facade, never the command it runs.
const WITHHELD: [&str; 4] = [
    "AGENTWORKS_MCP_ENDPOINT",
    "AGENTWORKS_MCP_ENDPOINT_FILE",
    "AGENTWORKS_MCP_TOKEN",
    "AGENTWORKS_MCP_TOKEN_FILE",
];

/// The tool's arguments.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Call {
    argv: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    files: BTreeMap<String, String>,
    stdin: Option<String>,
    cwd: Option<String>,
    timeout_seconds: Option<u64>,
}

#[derive(Debug, PartialEq)]
enum Piece<'a> {
    Text(&'a str),
    Secret(&'a str),
    File(&'a str),
}
/// Splits a template once, so placeholder-like text inside a value is never expanded.
fn parse(template: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut pieces = Vec::new();
    let mut rest = template;
    loop {
        let found = [("{{secret:", true), ("{{file:", false)]
            .into_iter()
            .filter_map(|(open, secret)| Some((rest.find(open)?, open, secret)))
            .min_by_key(|(at, ..)| *at);
        let Some((at, open, secret)) = found else {
            if !rest.is_empty() {
                pieces.push(Piece::Text(rest));
            }
            return Ok(pieces);
        };
        if at > 0 {
            pieces.push(Piece::Text(&rest[..at]));
        }
        let body = &rest[at + open.len()..];
        let end = body
            .find("}}")
            .ok_or_else(|| format!("Unclosed placeholder {open}"))?;
        let name = &body[..end];
        if secret {
            let mut chars = name.chars();
            if !chars
                .next()
                .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
                || !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!("Invalid secret name in {{{{secret:{name}}}}}"));
            }
            pieces.push(Piece::Secret(name));
        } else {
            pieces.push(Piece::File(name));
        }
        rest = &body[end + 2..];
    }
}
fn expand(
    pieces: &[Piece<'_>],
    values: &BTreeMap<String, Zeroizing<String>>,
    files: &BTreeMap<String, PathBuf>,
) -> Result<Zeroizing<String>, String> {
    let mut out = Zeroizing::new(String::new());
    for piece in pieces {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Secret(name) => out.push_str(
                values
                    .get(*name)
                    .ok_or_else(|| format!("{name} was not resolved"))?,
            ),
            Piece::File(key) => out.push_str(&files[*key].to_string_lossy()),
        }
    }
    Ok(out)
}
/// A private directory removed when the call ends, however it ends.
struct Private(PathBuf);
impl Private {
    fn create() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("agentworks-secret-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
    fn write(&self, index: usize, contents: &str) -> std::io::Result<PathBuf> {
        use std::io::Write;
        let path = self.0.join(format!("file-{index}"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path)?.write_all(contents.as_bytes())?;
        Ok(path)
    }
}
impl Drop for Private {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl Call {
    pub fn from_arguments(arguments: Map<String, Value>) -> Result<Self, String> {
        serde_json::from_value(Value::Object(arguments))
            .map_err(|e| format!("Invalid arguments: {e}"))
    }
    /// What the service needs to authorize the call: the names it uses and the shape of
    /// the command, never a value.
    pub fn release_request(&self, plan: &Plan<'_>) -> Value {
        json!({"names":plan.names,"argv":self.argv,"envKeys":self.env.keys().collect::<Vec<_>>(),
            "fileKeys":self.files.keys().collect::<Vec<_>>(),"cwd":self.cwd})
    }
}
/// A checked call with its templates split.
pub struct Plan<'a> {
    pub names: BTreeSet<String>,
    argv: Vec<Vec<Piece<'a>>>,
    env: Vec<(&'a str, Vec<Piece<'a>>)>,
    files: Vec<(&'a str, Vec<Piece<'a>>)>,
    stdin: Option<Vec<Piece<'a>>>,
}
/// Checks the call and finds the secrets it uses.
pub fn prepare(call: &Call) -> Result<Plan<'_>, String> {
    let size = call.argv.iter().map(String::len).sum::<usize>()
        + call
            .env
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
        + call.files.keys().map(String::len).sum::<usize>();
    if call.argv.is_empty() || call.argv[0].is_empty() {
        return Err("argv needs a program to run".into());
    }
    if size > MAX_REQUEST_BYTES {
        return Err("argv and env are limited to 32 KiB".into());
    }
    if call
        .timeout_seconds
        .is_some_and(|t| t == 0 || t > MAX_TIMEOUT)
    {
        return Err("timeoutSeconds must be 1–3600".into());
    }
    for key in call.env.keys() {
        if key.is_empty() || key.contains(['=', '\0']) {
            return Err(format!("Invalid environment name {key:?}"));
        }
    }
    let argv = call
        .argv
        .iter()
        .map(|t| parse(t))
        .collect::<Result<Vec<_>, _>>()?;
    let env = call
        .env
        .iter()
        .map(|(k, v)| Ok((k.as_str(), parse(v)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let files = call
        .files
        .iter()
        .map(|(k, v)| Ok((k.as_str(), parse(v)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let stdin = call.stdin.as_deref().map(parse).transpose()?;
    let contents = || files.iter().map(|(_, p)| p).chain(stdin.iter());
    if contents().flatten().any(|p| matches!(p, Piece::File(_))) {
        return Err("{{file:KEY}} is only allowed in argv and env".into());
    }
    let mut names = BTreeSet::new();
    for piece in argv
        .iter()
        .chain(env.iter().map(|(_, p)| p))
        .chain(contents())
        .flatten()
    {
        match piece {
            Piece::Secret(name) => {
                names.insert((*name).to_owned());
            }
            Piece::File(key) if !call.files.contains_key(*key) => {
                return Err(format!("{{{{file:{key}}}}} has no matching entry in files"));
            }
            _ => {}
        }
    }
    if names.is_empty() {
        return Err("No {{secret:NAME}} placeholders; run this command normally".into());
    }
    Ok(Plan {
        names,
        argv,
        env,
        files,
        stdin,
    })
}

/// The session directory or below it.
fn working_directory(session: &Path, requested: Option<&str>) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(session)
        .map_err(|_| "The session directory is not available on this host".to_owned())?;
    let Some(requested) = requested else {
        return Ok(root);
    };
    let path = std::fs::canonicalize(root.join(requested))
        .map_err(|_| format!("{requested} does not exist"))?;
    if !path.starts_with(&root) || !path.is_dir() {
        return Err("cwd must be a directory inside the session directory".into());
    }
    Ok(path)
}
/// Captured bytes, and whether more were discarded.
type Captured = Arc<Mutex<(Vec<u8>, bool)>>;
/// Keeps the first bytes for the result and drains the rest so the command never blocks.
async fn capture(mut stream: impl AsyncRead + Unpin, limit: usize, into: Captured) {
    let mut chunk = [0u8; 8192];
    while let Ok(read) = stream.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        let mut buffer = into.lock().unwrap();
        let room = limit.saturating_sub(buffer.0.len());
        buffer.0.extend_from_slice(&chunk[..read.min(room)]);
        buffer.1 |= read > room;
    }
}
fn finish(buffer: &Mutex<(Vec<u8>, bool)>, forms: &Forms) -> (String, bool) {
    let (bytes, more) = &*buffer.lock().unwrap();
    let mut text = redact::replace(forms, &String::from_utf8_lossy(bytes));
    let mut truncated = *more;
    if *more {
        // The capture stopped mid-stream, possibly inside a value. Redaction shrinks the
        // text, so that partial value could otherwise land inside the part we keep.
        while text.ends_with(char::REPLACEMENT_CHARACTER) {
            text.pop();
        }
        for (form, _) in forms {
            let form = form.as_bytes();
            if let Some(partial) = (1..form.len())
                .rev()
                .find(|len| text.as_bytes().ends_with(&form[..*len]))
            {
                text.truncate(text.len() - partial);
            }
        }
    }
    if text.len() > OUTPUT_LIMIT {
        let mut end = OUTPUT_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    (text, truncated)
}
/// Runs the command without a shell in `session` (or `cwd` below it) and returns its
/// exit status and redacted output.
pub async fn execute(
    call: &Call,
    parsed: &Plan<'_>,
    values: &BTreeMap<String, Zeroizing<String>>,
    session: &Path,
) -> Result<Value, String> {
    let cwd = working_directory(session, call.cwd.as_deref())?;
    let forms = redact::forms_for(values.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let private =
        Private::create().map_err(|e| format!("Could not create a private directory: {e}"))?;
    let mut paths = BTreeMap::new();
    for (index, (key, pieces)) in parsed.files.iter().enumerate() {
        let contents = expand(pieces, values, &paths)?;
        let path = private
            .write(index, &contents)
            .map_err(|e| format!("Could not write a private file: {e}"))?;
        paths.insert((*key).to_owned(), path);
    }
    let argv = parsed
        .argv
        .iter()
        .map(|pieces| expand(pieces, values, &paths))
        .collect::<Result<Vec<_>, _>>()?;
    let mut command = Command::new(argv[0].as_str());
    command
        .args(argv[1..].iter().map(|arg| arg.as_str()))
        .current_dir(&cwd)
        .stdin(if parsed.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in WITHHELD {
        command.env_remove(key);
    }
    for (key, pieces) in &parsed.env {
        command.env(key, expand(pieces, values, &paths)?.as_str());
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", call.argv[0]))?;
    if let (Some(pieces), Some(mut stdin)) = (&parsed.stdin, child.stdin.take()) {
        let input = expand(pieces, values, &paths)?;
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
        });
    }
    let longest = forms.iter().map(|(form, _)| form.len()).max().unwrap_or(0);
    let limit = OUTPUT_LIMIT + longest;
    let (stdout, stderr): (Captured, Captured) = Default::default();
    let out = tokio::spawn(capture(child.stdout.take().unwrap(), limit, stdout.clone()));
    let err = tokio::spawn(capture(child.stderr.take().unwrap(), limit, stderr.clone()));
    let timeout = Duration::from_secs(call.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT));
    let group = child.id();
    let status = tokio::time::timeout(timeout, child.wait()).await;
    let timed_out = status.is_err();
    if timed_out {
        #[cfg(unix)]
        if let Some(group) = group {
            // SAFETY: signals only the process group this call created.
            unsafe { libc::kill(-(group as i32), libc::SIGKILL) };
        }
        #[cfg(not(unix))]
        let _ = group;
        let _ = child.kill().await;
    }
    // A background process may hold the pipes open after the command exits.
    let (mut out, mut err) = (out, err);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        let _ = (&mut out).await;
        let _ = (&mut err).await;
    })
    .await;
    out.abort();
    err.abort();
    let (stdout, stdout_truncated) = finish(&stdout, &forms);
    let (stderr, stderr_truncated) = finish(&stderr, &forms);
    let code = status.ok().and_then(|s| s.ok()).and_then(|s| s.code());
    Ok(
        json!({"exitCode":code,"stdout":stdout,"stderr":stderr,"stdoutTruncated":stdout_truncated,
        "stderrTruncated":stderr_truncated,"timedOut":timed_out}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_expand_once_and_reject_bad_placeholders() {
        assert_eq!(
            parse("a {{secret:TOKEN}} {{file:RC}}{{other}}").unwrap(),
            [
                Piece::Text("a "),
                Piece::Secret("TOKEN"),
                Piece::Text(" "),
                Piece::File("RC"),
                Piece::Text("{{other}}"),
            ]
        );
        assert!(parse("{{secret:lower}}").is_err());
        assert!(parse("{{secret:OPEN").is_err());
        let values = BTreeMap::from([("A".to_owned(), Zeroizing::new("{{secret:B}}".to_owned()))]);
        let pieces = parse("{{secret:A}}").unwrap();
        assert_eq!(
            expand(&pieces, &values, &BTreeMap::new()).unwrap().as_str(),
            "{{secret:B}}"
        );
    }

    fn call(value: Value) -> Call {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn calls_need_a_program_a_secret_and_known_files() {
        let names = |c: Call| prepare(&c).map(|p| p.names.into_iter().collect::<Vec<_>>());
        assert_eq!(
            names(call(json!({"argv":["x","{{secret:A}}"],"env":{"K":"{{secret:B}}"},"files":{"F":"{{secret:C}}"},"stdin":"{{secret:A}}"}))).unwrap(),
            ["A", "B", "C"]
        );
        assert!(names(call(json!({"argv":[]}))).is_err());
        assert!(
            names(call(json!({"argv":["echo","plain"]})))
                .unwrap_err()
                .contains("normally")
        );
        assert!(names(call(json!({"argv":["x","{{secret:A}}","{{file:NOPE}}"]}))).is_err());
        assert!(
            names(call(
                json!({"argv":["x","{{secret:A}}"],"stdin":"{{file:F}}","files":{"F":"x"}})
            ))
            .is_err()
        );
        assert!(
            names(call(
                json!({"argv":["x","{{secret:A}}"],"files":{"F":"{{file:F}}"}})
            ))
            .is_err()
        );
        assert!(names(call(json!({"argv":["x","{{secret:A}}"],"env":{"A=B":"x"}}))).is_err());
        assert!(
            names(call(
                json!({"argv":["x","{{secret:A}}"],"timeoutSeconds":0})
            ))
            .is_err()
        );
    }

    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentworks-exec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn commands_get_values_with_output_redacted_and_private_files_removed() {
        let dir = workspace();
        let values = BTreeMap::from([(
            "TOKEN".to_owned(),
            Zeroizing::new("fixture-value".to_owned()),
        )]);
        let input = call(
            json!({"argv":["sh","-c","echo \"$1 $K\"; cat \"$2\"; cat; pwd; echo \"[$AGENTWORKS_MCP_TOKEN]\"; echo \"$2\" >&2","sh","{{secret:TOKEN}}","{{file:F}}"],
            "env":{"K":"{{secret:TOKEN}}"},"files":{"F":"file {{secret:TOKEN}}\n"},"stdin":"in {{secret:TOKEN}}\n","cwd":"inner"}),
        );
        let parsed = prepare(&input).unwrap();
        let result = execute(&input, &parsed, &values, &dir).await.unwrap();
        let label = "[redacted:TOKEN]";
        let stdout = result["stdout"].as_str().unwrap();
        assert_eq!(result["exitCode"], 0);
        assert!(
            stdout.starts_with(&format!("{label} {label}\nfile {label}\nin {label}\n")),
            "{stdout}"
        );
        assert!(
            stdout.contains("/inner\n[]\n"),
            "bridge credentials are withheld: {stdout}"
        );
        assert!(!result.to_string().contains("fixture-value"));
        let file = PathBuf::from(result["stderr"].as_str().unwrap().trim());
        assert!(file.starts_with(std::env::temp_dir()));
        assert!(!file.exists(), "private files are removed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeouts_kill_the_command_and_keep_redacted_partial_output() {
        let dir = workspace();
        let values = BTreeMap::from([(
            "TOKEN".to_owned(),
            Zeroizing::new("fixture-value".to_owned()),
        )]);
        let input = call(
            json!({"argv":["sh","-c","echo {{secret:TOKEN}}; sleep 30 & wait"],"timeoutSeconds":1}),
        );
        let parsed = prepare(&input).unwrap();
        let started = std::time::Instant::now();
        let result = execute(&input, &parsed, &values, &dir).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(result["timedOut"], true);
        assert_eq!(result["exitCode"], Value::Null);
        assert_eq!(result["stdout"], "[redacted:TOKEN]\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_is_capped_without_leaking_a_value_at_the_cut() {
        let dir = workspace();
        let value = "fixture-value-at-the-boundary";
        let values = BTreeMap::from([("TOKEN".to_owned(), Zeroizing::new(value.to_owned()))]);
        let script = format!(
            "head -c {} /dev/zero | tr '\\0' x; echo -n {{{{secret:TOKEN}}}}; head -c 4096 /dev/zero | tr '\\0' y",
            OUTPUT_LIMIT - 5
        );
        let input = call(json!({"argv":["sh","-c",script]}));
        let parsed = prepare(&input).unwrap();
        let result = execute(&input, &parsed, &values, &dir).await.unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert_eq!(result["stdoutTruncated"], true);
        assert_eq!(stdout.len(), OUTPUT_LIMIT);
        assert!(
            !stdout.contains("fixture"),
            "no part of the value survives the cut"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_value_cut_off_at_the_end_of_the_capture_is_removed() {
        let forms = redact::forms_for([("TOKEN", "fixture-value")].into_iter());
        let buffer = Mutex::new((b"fixture-value keep fixture-va".to_vec(), true));
        assert_eq!(
            finish(&buffer, &forms),
            ("[redacted:TOKEN] keep ".to_owned(), true)
        );
        let complete = Mutex::new((b"keep fixture-va".to_vec(), false));
        assert_eq!(
            finish(&complete, &forms).0,
            "keep fixture-va",
            "only a cut stream is trimmed"
        );
    }

    #[test]
    fn the_working_directory_stays_inside_the_session() {
        let dir = workspace();
        assert_eq!(working_directory(&dir, None).unwrap(), dir);
        assert_eq!(
            working_directory(&dir, Some("inner")).unwrap(),
            dir.join("inner")
        );
        assert!(working_directory(&dir, Some("..")).is_err());
        assert!(working_directory(&dir, Some("/")).is_err());
        assert!(working_directory(&dir, Some("missing")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
