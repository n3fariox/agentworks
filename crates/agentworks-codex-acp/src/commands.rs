//! Slash syntax is client-side; every operation executes through Codex app-server.
use super::*;

pub(super) fn available() -> Vec<Value> {
    [
        ("compact","Compact conversation history",None),
        ("review","Review changes or follow review instructions",Some("optional instructions")),
        ("review-branch","Review changes against a branch",Some("branch")),
        ("review-commit","Review a commit",Some("commit SHA")),
        ("plan","Toggle planning mode",None),
        ("model","Show models or select a model",Some("optional model ID")),
        ("permissions","Show permission presets or select one",Some("optional preset")),
        ("fast","Toggle fast mode (increased usage), or choose on/off",Some("optional on|off")),
        ("status","Show session settings and last reported usage",None),
        ("skills","List configured skills",None),
        ("mcp","Show MCP server status",None),
        ("rename","Rename the native Codex conversation",Some("new name")),
    ].into_iter().map(|(name,description,hint)|json!({"name":name,"description":description,"input":hint.map(|hint|json!({"hint":hint}))})).collect()
}

pub(super) fn parse(prompt: &Value) -> Option<(&str, &str)> {
    let first = prompt.as_array()?.first()?;
    if first["type"] != "text" {
        return None;
    }
    let text = first["text"].as_str()?.trim().strip_prefix('/')?;
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    let (name, args) = text.split_at(end);
    // Absolute paths and ordinary prose remain ordinary prompts.
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-'))
        .then_some((name, args.trim()))
}

impl Bridge {
    pub(super) async fn command(
        self: &Arc<Self>,
        id: &str,
        name: &str,
        args: &str,
        cx: &ConnectionTo<Client>,
    ) -> Result<Value> {
        {
            let sessions = self.sessions.lock().unwrap();
            let session = sessions.get(id).ok_or_else(|| invalid("Unknown session"))?;
            if session.active.is_some() || session.configuring {
                return Err(invalid("A turn or settings change is already running"));
            }
        }
        if !available().iter().any(|c| c["name"] == name) {
            return Err(invalid(
                "Unknown slash command; choose an advertised command",
            ));
        }
        if matches!(name, "compact" | "plan" | "status" | "skills" | "mcp") && !args.is_empty() {
            return Err(invalid("This command takes no arguments"));
        }
        match name {
            "compact" => {
                return self
                    .run_turn(id, "thread/compact/start", json!({"threadId":id}))
                    .await;
            }
            "review" | "review-branch" | "review-commit" => {
                let target = match name {
                    "review" if args.is_empty() => json!({"type":"uncommittedChanges"}),
                    "review" => json!({"type":"custom","instructions":args}),
                    _ if args.is_empty() => return Err(invalid("A branch or commit is required")),
                    "review-branch" => json!({"type":"baseBranch","branch":args}),
                    _ => json!({"type":"commit","sha":args,"title":null}),
                };
                return self
                    .run_turn(
                        id,
                        "review/start",
                        json!({"threadId":id,"target":target,"delivery":"inline"}),
                    )
                    .await;
            }
            "model" | "permissions" | "plan" | "fast" => {
                let key = match name {
                    "model" => "model",
                    "permissions" => "mode",
                    "plan" => "collaboration_mode",
                    _ => "fast-mode",
                };
                if args.is_empty() && matches!(name, "model" | "permissions") {
                    let options = self.config_options(id).await?;
                    let option = options
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|o| o["id"] == key)
                        .unwrap();
                    let choices = option["options"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|o| {
                            format!(
                                "- `{}` — {}",
                                o["value"].as_str().unwrap_or(""),
                                o["name"].as_str().unwrap_or("")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    text(
                        cx,
                        id,
                        &format!(
                            "Current {name}: `{}`\n\n{choices}",
                            option["currentValue"].as_str().unwrap_or("")
                        ),
                    )?;
                } else {
                    let value = match name {
                        "plan" => json!(
                            if self.sessions.lock().unwrap()[id].settings["collaborationMode"]["mode"]
                                == "plan"
                            {
                                "default"
                            } else {
                                "plan"
                            }
                        ),
                        "fast" => {
                            let enabled = match args {
                                "" => {
                                    self.sessions.lock().unwrap()[id].settings["serviceTier"]
                                        != "fast"
                                }
                                "on" => true,
                                "off" => false,
                                _ => return Err(invalid("Use /fast, /fast on, or /fast off")),
                            };
                            if self.boolean_config.load(Ordering::Relaxed) {
                                json!(enabled)
                            } else {
                                json!(if enabled { "on" } else { "off" })
                            }
                        }
                        _ => json!(args),
                    };
                    // Reuse ACP config validation, native acknowledgement, and update publishing.
                    Box::pin(self.request(
                        "session/set_config_option",
                        json!({"sessionId":id,"configId":key,"value":value}),
                        cx,
                    ))
                    .await?;
                    text(
                        cx,
                        id,
                        &format!(
                            "{name}: {}",
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string())
                        ),
                    )?;
                }
            }
            "status" => {
                let sessions = self.sessions.lock().unwrap();
                let s = &sessions[id];
                let mut lines = vec![
                    format!(
                        "Model: {}",
                        s.settings["model"].as_str().unwrap_or("not reported")
                    ),
                    format!(
                        "Reasoning: {}",
                        s.settings["effort"].as_str().unwrap_or("model default")
                    ),
                    format!("Permissions: {}", policy_mode(&s.settings)),
                    format!(
                        "Planning: {}",
                        s.settings["collaborationMode"]["mode"] == "plan"
                    ),
                    format!("Fast mode: {}", s.settings["serviceTier"] == "fast"),
                ];
                if s.usage.is_object() {
                    lines.push(format!(
                        "Last reported context: {} / {} tokens",
                        s.usage["used"], s.usage["size"]
                    ));
                }
                let limits = self.limits.lock().unwrap();
                if limits.is_empty() {
                    lines.push("Subscription quota: not reported".into());
                }
                for (limit_id, limit) in limits.iter() {
                    for window in ["primary", "secondary"] {
                        if let Some(used) = limit[window]["usedPercent"]["value"]
                            .as_f64()
                            .filter(|n| (0.0..=100.0).contains(n))
                        {
                            lines.push(format!(
                                "Last reported {limit_id} {window} quota: {:.1}% remaining",
                                100.0 - used
                            ));
                        }
                    }
                }
                text(cx, id, &lines.join("\n"))?;
            }
            "skills" => {
                let roots = self.sessions.lock().unwrap()[id].roots.clone();
                let response = self.rpc("skills/list", json!({"cwds":roots})).await?;
                let lines = response["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|entry| entry["skills"].as_array().into_iter().flatten())
                    .map(|skill| {
                        format!(
                            "- `${}` — {}",
                            skill["name"].as_str().unwrap_or(""),
                            skill["description"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>();
                text(
                    cx,
                    id,
                    &if lines.is_empty() {
                        "No skills configured.".into()
                    } else {
                        lines.join("\n")
                    },
                )?;
            }
            "mcp" => {
                let mut cursor = Value::Null;
                let mut seen = std::collections::HashSet::new();
                let mut lines = Vec::new();
                loop {
                    let response = self
                        .rpc(
                            "mcpServerStatus/list",
                            json!({"threadId":id,"detail":"toolsAndAuthOnly","cursor":cursor}),
                        )
                        .await?;
                    for server in response["data"].as_array().into_iter().flatten() {
                        lines.push(format!(
                            "- {} — authentication: {}",
                            server["name"].as_str().unwrap_or("unknown"),
                            server["authStatus"].as_str().unwrap_or("not reported")
                        ));
                    }
                    cursor = response["nextCursor"].clone();
                    if cursor.is_null() {
                        break;
                    }
                    if !seen.insert(cursor.to_string()) {
                        return Err(failure("Codex repeated an MCP status cursor"));
                    }
                }
                text(
                    cx,
                    id,
                    &if lines.is_empty() {
                        "No MCP servers reported.".into()
                    } else {
                        lines.join("\n")
                    },
                )?;
            }
            "rename" => {
                if args.is_empty() {
                    return Err(invalid("A new conversation name is required"));
                }
                self.rpc("thread/name/set", json!({"threadId":id,"name":args}))
                    .await?;
                text(cx, id, &format!("Conversation renamed to {args}."))?;
            }
            _ => unreachable!(),
        }
        Ok(json!({"stopReason":"end_turn"}))
    }
}

fn text(cx: &ConnectionTo<Client>, id: &str, text: &str) -> Result<()> {
    update(
        cx,
        id,
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}),
    )
}
