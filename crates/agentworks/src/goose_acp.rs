//! ACP compatibility for the pinned Goose runtime, without modifying its sources.
//! Goose owns execution and storage. We adapt ACP system prompts and exact forks
//! to its public session APIs, retaining all other requests and notifications.
use agent_client_protocol::{
    Agent, ByteStreams, Client, ConnectionTo, Error, Stdio, UntypedMessage,
};
use goose::{
    acp::{
        server::AcpBuiltinSelection,
        server_factory::{AcpServer, AcpServerFactoryConfig},
    },
    agents::GoosePlatform,
    config::paths::Paths,
    conversation::{Conversation, message::MessageContent},
    session::SessionManager,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Result<T> = std::result::Result<T, Error>;
const PROMPT_KEY: &str = "agentworks.acp.systemPrompt";
fn failure(e: impl ToString) -> Error {
    Error::internal_error().data(e.to_string())
}
fn invalid(e: &str) -> Error {
    Error::invalid_params().data(e.to_owned())
}
fn message(method: &str, params: Value) -> UntypedMessage {
    UntypedMessage {
        method: method.into(),
        params,
    }
}
fn field<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p[key]
        .as_str()
        .ok_or_else(|| invalid(&format!("Missing {key}")))
}

// Accept append only: the capability must never promise replacement semantics
// while retaining Goose's own base prompt. Missing/empty clears our prior entry.
fn system_prompt(p: &Value) -> Result<Option<String>> {
    let Some(prompt) = p.get("systemPrompt") else {
        return Ok(None);
    };
    if prompt["mode"] != "append" {
        return Err(invalid("Goose supports append system prompts only"));
    }
    let blocks = prompt["content"]
        .as_array()
        .ok_or_else(|| invalid("Invalid system prompt content"))?;
    let mut text = Vec::new();
    for block in blocks {
        if block["type"] != "text" {
            return Err(invalid("Goose system prompts accept text content only"));
        }
        text.push(field(block, "text")?);
    }
    let text = text.join("\n\n");
    Ok((!text.trim().is_empty()).then_some(text))
}

fn fork_conversation(mut conversation: Conversation, boundary: &Value) -> Result<Conversation> {
    if !boundary.is_null() {
        if boundary["version"] != 1 {
            return Err(invalid("Unsupported fork boundary version"));
        }
        let id = field(boundary, "messageId")?;
        let mut matches = conversation
            .messages()
            .iter()
            .enumerate()
            .filter(|(_, m)| m.id.as_deref() == Some(id));
        let index = matches.next().map(|(i, _)| i).ok_or_else(|| {
            invalid("Fork boundary was not found; refusing to fork newer history")
        })?;
        if matches.next().is_some() {
            return Err(invalid(
                "Fork boundary ID is ambiguous; refusing to fork different history",
            ));
        }
        conversation.messages_mut().truncate(index + 1);
        if conversation.last().unwrap().role != rmcp::model::Role::Assistant {
            return Err(invalid("Fork boundary must be an assistant message"));
        }
        // Native history ends with an assistant reply. Goose's inference-input
        // validator expects a user message at the end, so do not normalize the
        // stored conversation through that validator or discard native metadata.
        let mut pending = HashSet::new();
        for message in conversation.messages() {
            for content in &message.content {
                match content {
                    MessageContent::ToolRequest(request) => {
                        pending.insert(&request.id);
                    }
                    MessageContent::ToolResponse(response) => {
                        pending.remove(&response.id);
                    }
                    _ => {}
                }
            }
        }
        if !pending.is_empty() {
            return Err(invalid("Fork boundary contains unfinished tool calls"));
        }
    }
    Ok(conversation)
}

struct Bridge {
    goose: ConnectionTo<Agent>,
    sessions: SessionManager,
    lifecycle: tokio::sync::Mutex<()>,
}
impl Bridge {
    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        self.goose
            .send_request(message(method, params))
            .block_task()
            .await
    }
    async fn request(&self, method: &str, mut p: Value) -> Result<Value> {
        if method == "initialize" {
            let mut result = self.rpc(method, p).await?;
            result["agentCapabilities"]["sessionCapabilities"]["systemPrompt"] = json!({});
            result["agentCapabilities"]["sessionCapabilities"]["fork"] = json!({});
            return Ok(result);
        }
        if !matches!(method, "session/new" | "session/load" | "session/fork") {
            return self.rpc(method, p).await;
        }
        let text = system_prompt(&p)?; // Validate before creating or changing native state.
        p.as_object_mut()
            .ok_or_else(|| invalid("Invalid session parameters"))?
            .remove("systemPrompt");
        // The older Goose schema requires this field on load; ACP 3 omits
        // empty server lists on fork requests.
        p.as_object_mut()
            .unwrap()
            .entry("mcpServers")
            .or_insert(json!([]));
        let _lifecycle = self.lifecycle.lock().await;
        if method == "session/fork" {
            return self.fork(p, text).await;
        }
        if method == "session/load" {
            self.apply_prompt(field(&p, "sessionId")?, text.clone())
                .await?;
        }
        let result = self.rpc(method, p).await?;
        if method == "session/new" {
            self.apply_prompt(field(&result, "sessionId")?, text)
                .await?;
        }
        Ok(result)
    }
    async fn apply_prompt(&self, id: &str, text: Option<String>) -> Result<()> {
        self.sessions
            .set_system_prompt_extra(id, PROMPT_KEY, text)
            .await
            .map_err(failure)
    }
    async fn fork(&self, mut p: Value, text: Option<String>) -> Result<Value> {
        let cwd = std::path::Path::new(field(&p, "cwd")?);
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(invalid("Fork cwd must be an existing absolute directory"));
        }
        let source = self
            .sessions
            .get_session(field(&p, "sessionId")?, true)
            .await
            .map_err(failure)?;
        let boundary = &p["_meta"]["jetbrains"]["air"]["fork"];
        let conversation = source
            .conversation
            .clone()
            .unwrap_or_else(Conversation::empty);
        let conversation = fork_conversation(conversation, boundary)?;
        let child = self
            .sessions
            .copy_session(&source.id, format!("{} (copy)", source.name))
            .await
            .map_err(failure)?;
        // Prepare the durable child before activating it, so provider restoration
        // can never see messages newer than the requested boundary.
        let result = async {
            self.sessions
                .replace_conversation(&child.id, &conversation)
                .await
                .map_err(failure)?;
            self.sessions
                .update(&child.id)
                .parent_session_id(Some(source.id))
                .apply()
                .await
                .map_err(failure)?;
            self.apply_prompt(&child.id, text).await?;
            p["sessionId"] = json!(child.id);
            let mut result = self.rpc("session/load", p).await?;
            result["sessionId"] = json!(child.id);
            Ok(result)
        }
        .await;
        if result.is_err() {
            // Do not leave an unusable copy when activation fails.
            let _ = self.sessions.delete_session(&child.id).await;
        }
        result
    }
}

pub async fn run() -> Result<()> {
    let server = AcpServer::new(AcpServerFactoryConfig {
        builtins: AcpBuiltinSelection::from_requested(vec!["developer".into()]),
        config_dir: Paths::config_dir(),
        goose_platform: GoosePlatform::GooseCli,
        additional_source_roots: Vec::new(),
        session_cwd: None,
        enable_scheduler: false,
    });
    let outer: Arc<OnceLock<ConnectionTo<Client>>> = Arc::default();
    if std::env::var("AGENTWORKS_CODEX_MANAGED").is_ok_and(|v| v == "1") {
        let credentials = outer.clone();
        goose::providers::chatgpt_codex::set_managed_auth(Arc::new(move || {
            let credentials = credentials.clone();
            Box::pin(async move {
                let connection = credentials.get().ok_or_else(|| std::io::Error::other("Agentworks is not connected"))?;
                let value = connection.send_request(UntypedMessage {
                    method: agentworks_runtime::codex_auth::TOKEN_METHOD.into(), params: json!({}),
                }).block_task().await.map_err(|_| std::io::Error::other("Could not obtain a Codex token. Check sign-in and host access in Settings."))?;
                let token: agentworks_runtime::codex_auth::AccessToken = serde_json::from_value(value)?;
                Ok(goose::providers::chatgpt_codex::ManagedToken { access_token: token.access_token, account_id: token.account_id })
            })
        }), std::env::var("AGENTWORKS_CODEX_API_URL").ok()).map_err(failure)?;
    }
    let agent = server.create_agent().await.map_err(failure)?;
    let (client_io, goose_io) = tokio::io::duplex(1024 * 1024);
    let (goose_read, goose_write) = tokio::io::split(goose_io);
    let serving = tokio::spawn(goose::acp::server::serve(
        agent,
        goose_read.compat(),
        goose_write.compat_write(),
    ));
    let (read, write) = tokio::io::split(client_io);
    let requests = outer.clone();
    let notifications = outer.clone();
    let result = Client
        .builder()
        .on_receive_request(
            async move |request: UntypedMessage, responder, cx| {
                let client = requests
                    .get()
                    .cloned()
                    .ok_or_else(|| failure("ACP client is not connected"))?;
                cx.spawn(async move {
                    responder.respond_with_result(client.send_request(request).block_task().await)
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: UntypedMessage, _cx| {
                notifications
                    .get()
                    .ok_or_else(|| failure("ACP client is not connected"))?
                    .send_notification(notification)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(
            ByteStreams::new(write.compat_write(), read.compat()),
            move |goose: ConnectionTo<Agent>| async move {
                let bridge = Arc::new(Bridge {
                    goose,
                    sessions: SessionManager::new(Paths::data_dir()),
                    lifecycle: tokio::sync::Mutex::new(()),
                });
                let requests = bridge.clone();
                let notifications = bridge.clone();
                Agent
                    .builder()
                    .name("agentworks-goose-acp")
                    .on_receive_request(
                        async move |request: UntypedMessage, responder, cx| {
                            let bridge = requests.clone();
                            cx.spawn(async move {
                                responder.respond_with_result(
                                    bridge.request(&request.method, request.params).await,
                                )
                            })
                        },
                        agent_client_protocol::on_receive_request!(),
                    )
                    .on_receive_notification(
                        async move |notification: UntypedMessage, _cx| {
                            notifications.goose.send_notification(notification)
                        },
                        agent_client_protocol::on_receive_notification!(),
                    )
                    .connect_with(
                        Stdio::new(),
                        move |client: ConnectionTo<Client>| async move {
                            outer
                                .set(client.clone())
                                .map_err(|_| failure("ACP client connected twice"))?;
                            client.incoming_closed().await;
                            Ok(())
                        },
                    )
                    .await
            },
        )
        .await;
    serving.abort();
    let _ = serving.await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use goose::conversation::message::Message;

    #[test]
    fn system_prompts_validate_modes_and_content_and_clear_when_absent() {
        assert_eq!(system_prompt(&json!({})).unwrap(), None);
        assert_eq!(
            system_prompt(&json!({"systemPrompt":{"mode":"append","content":[]}})).unwrap(),
            None
        );
        assert_eq!(system_prompt(&json!({"systemPrompt":{"mode":"append","content":[{"type":"text","text":"role"},{"type":"text","text":"context"}]}})).unwrap().as_deref(), Some("role\n\ncontext"));
        for prompt in [
            json!({"mode":"override","content":[]}),
            json!({"mode":"future","content":[]}),
            json!({"mode":"append","content":[{"type":"image","data":"x","mimeType":"image/png"}]}),
            json!({"content":[]}),
        ] {
            assert!(system_prompt(&json!({"systemPrompt":prompt})).is_err());
        }
    }

    #[test]
    fn native_fork_selects_exact_message_with_same_second_timestamps() {
        let mut messages = vec![
            Message::user().with_text("first").with_id("u1"),
            Message::assistant().with_text("first reply").with_id("a1"),
            Message::user().with_text("newer").with_id("u2"),
            Message::assistant().with_text("newer reply").with_id("a2"),
        ];
        for m in &mut messages {
            m.created = 100;
        }
        let parent = Conversation::new_unvalidated(messages);
        let child =
            fork_conversation(parent.clone(), &json!({"version":1,"messageId":"a1"})).unwrap();
        assert_eq!(child.len(), 2);
        assert_eq!(child.last().unwrap().id.as_deref(), Some("a1"));
        assert_eq!(parent.len(), 4);
        for boundary in [
            json!({"version":1,"messageId":"missing"}),
            json!({"version":2,"messageId":"a1"}),
            json!({"version":1,"messageId":"u1"}),
        ] {
            assert!(fork_conversation(parent.clone(), &boundary).is_err());
        }
        let mut ambiguous = parent.clone();
        ambiguous.messages_mut()[3].id = Some("a1".into());
        assert!(fork_conversation(ambiguous, &json!({"version":1,"messageId":"a1"})).is_err());
    }
}

/// Run sign-in in an isolated child, so Goose's global configuration is host-local.
pub async fn login(kind: &str) -> Result<()> {
    match kind {
        "claude-code" => {
            let command = std::env::var_os("CLAUDE_CODE_EXECUTABLE")
                .or_else(|| std::env::var_os("CLAUDE_CODE_COMMAND"))
                .unwrap_or_else(|| "claude".into());
            let mut child = tokio::process::Command::new(command);
            child.args(["auth", "login"]).env_remove("CLAUDECODE");
            for key in [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            ] {
                child.env_remove(key);
            }
            if !child.status().await.map_err(failure)?.success() {
                return Err(failure("Claude Code login did not complete"));
            }
        }
        _ => return Err(failure("Unknown subscription provider")),
    }
    Ok(())
}
