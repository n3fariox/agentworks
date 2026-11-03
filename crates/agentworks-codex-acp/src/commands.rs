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

// Keep the text for model context and attach native skill inputs so Codex loads
// the selected SKILL.md even when another skill has the same display name.
pub(super) fn skill_input(input: &mut Vec<Value>, skills: &[Value]) {
    if let Some(text) = input
        .first()
        .filter(|b| b["type"] == "text")
        .and_then(|b| b["text"].as_str().map(str::to_owned))
    {
        let trimmed = text.trim_start();
        if let Some(rest) = trimmed.strip_prefix('/') {
            let name = rest.split_whitespace().next().unwrap_or("");
            if !available().iter().any(|c| c["name"] == name)
                && skills.iter().any(|s| s["name"] == name)
            {
                input[0]["text"] = json!(format!("{}${rest}", &text[..text.len() - trimmed.len()]));
            }
        }
    }
    let mut selected = std::collections::HashSet::new();
    let mut extra = Vec::new();
    for block in input.iter().filter(|b| b["type"] == "text") {
        let text = block["text"].as_str().unwrap_or("");
        for (at, _) in text.match_indices('$') {
            if at > 0 && !text[..at].ends_with(char::is_whitespace) {
                continue;
            }
            let rest = &text[at + 1..];
            let end = rest
                .find(|ch: char| !ch.is_alphanumeric() && !matches!(ch, '-' | '_' | '.' | ':'))
                .unwrap_or(rest.len());
            let name = &rest[..end];
            if let Some(skill) = skills.iter().find(|s| s["name"] == name) {
                if selected.insert(name.to_owned()) {
                    extra.push(json!({"type":"skill","name":name,"path":skill["path"]}));
                }
            }
        }
    }
    input.extend(extra);
}

impl Bridge {
    pub(super) async fn refresh_skills(
        &self,
        id: &str,
        cx: &ConnectionTo<Client>,
    ) -> Result<Vec<Value>> {
        let roots = self.sessions.lock().unwrap()[id].roots.clone();
        let response = self.rpc("skills/list", json!({"cwds":roots})).await?;
        let mut seen = std::collections::HashSet::new();
        let skills: Vec<Value> = response["data"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|entry| entry["skills"].as_array().into_iter().flatten())
            .filter(|s| {
                s["enabled"] != false
                    && s["name"].as_str().is_some_and(|n| !n.is_empty())
                    && s["path"].is_string()
            })
            .filter(|s| seen.insert(s["name"].as_str().unwrap().to_owned()))
            .cloned()
            .collect();
        let mut commands = available();
        commands.extend(skills.iter().map(|s| json!({"name":format!("${}",s["name"].as_str().unwrap()),"description":s["description"],"input":{"hint":"optional instructions"}})));
        self.sessions
            .lock()
            .unwrap()
            .get_mut(id)
            .ok_or_else(|| invalid("Unknown session"))?
            .skills = skills.clone();
        update(
            cx,
            id,
            json!({"sessionUpdate":"available_commands_update","availableCommands":commands}),
        )?;
        Ok(skills)
    }

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
                let skills = self.refresh_skills(id, cx).await?;
                let lines = skills
                    .iter()
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
