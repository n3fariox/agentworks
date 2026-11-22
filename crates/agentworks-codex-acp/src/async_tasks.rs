//! Optional AIR asyncTasks: Codex owns the terminals; this bridge only observes and stops them.
use super::*;
use std::collections::HashSet;

type Key = (String, String); // Native thread and command item, never client-supplied process IDs.
#[derive(Clone)]
struct Task {
    root: String,
    thread: String,
    session: String,
    item: String,
    process: Value,
    name: String,
    epoch: u64,
    published: bool,
    stopping: bool,
}
impl Task {
    fn id(&self) -> String {
        if self.root == self.thread {
            self.item.clone()
        } else {
            format!("{}:{}", self.thread, self.item)
        }
    }
    fn key(&self) -> Key {
        (self.thread.clone(), self.item.clone())
    }
}
#[derive(Clone)]
struct Context {
    root: String,
    thread: String,
    session: String,
    epoch: u64,
}
#[derive(Default)]
pub(super) struct Tasks {
    enabled: bool,
    list_supported: bool,
    next_epoch: u64,
    epochs: HashMap<String, u64>,
    tasks: HashMap<Key, Task>,
    ended: HashSet<Key>,
    // One paginated snapshot at a time per thread. Notifications during a read request another pass.
    refreshing: HashMap<(String, u64), bool>,
    gates: HashMap<(String, u64), Arc<AsyncMutex<()>>>,
    owners: HashMap<String, String>,
}
impl Tasks {
    fn current(&self, context: &Context) -> bool {
        self.epochs.get(&context.root) == Some(&context.epoch)
    }
    fn forget_root(&mut self, root: &str) {
        let threads: HashSet<_> = self
            .owners
            .iter()
            .filter(|(_, owner)| *owner == root)
            .map(|(thread, _)| thread.clone())
            .collect();
        self.tasks.retain(|_, t| t.root != root);
        self.ended.retain(|(thread, _)| !threads.contains(thread));
        self.owners.retain(|_, owner| owner != root);
        self.gates
            .retain(|(thread, _), _| !threads.contains(thread));
        self.refreshing
            .retain(|(thread, _), _| !threads.contains(thread));
    }
}
fn capability(p: &Value) -> bool {
    let air = &p["clientCapabilities"]["_meta"]["jetbrains"]["air"];
    air["version"].as_u64().is_some_and(|v| v >= 1)
        && air["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|v| v == "asyncTasks"))
}
impl Bridge {
    pub(super) fn negotiate_tasks(&self, p: &Value, response: &mut Value) {
        let enabled = capability(p);
        let mut jobs = self.async_tasks.lock().unwrap();
        jobs.enabled = enabled;
        jobs.list_supported = true;
        if enabled {
            response["_meta"]["jetbrains"] =
                json!({"air":{"version":1,"capabilities":["asyncTasks"]}});
        }
    }
    pub(super) fn open_tasks(&self, root: &str) {
        let mut jobs = self.async_tasks.lock().unwrap();
        jobs.forget_root(root);
        jobs.next_epoch += 1;
        let epoch = jobs.next_epoch;
        jobs.epochs.insert(root.into(), epoch);
    }
    pub(super) fn close_tasks(&self, root: &str) {
        let mut jobs = self.async_tasks.lock().unwrap();
        jobs.epochs.remove(root);
        jobs.forget_root(root);
    }
    fn task_context(&self, thread: &str) -> Option<Context> {
        if thread.is_empty() {
            return None;
        }
        let mut root = thread.to_owned();
        {
            let children = self.children.lock().unwrap();
            let mut visited = HashSet::new();
            while let Some(parent) = children.get(&root) {
                if !visited.insert(root.clone()) {
                    return None;
                }
                root = parent.clone();
            }
        }
        if !self.sessions.lock().unwrap().contains_key(&root) {
            return None;
        }
        let mut jobs = self.async_tasks.lock().unwrap();
        if !jobs.enabled {
            return None;
        }
        let epoch = *jobs.epochs.get(&root)?;
        jobs.owners.insert(thread.into(), root.clone());
        Some(Context {
            epoch,
            session: if self.subagents.load(Ordering::Relaxed) {
                thread.to_owned()
            } else {
                root.clone()
            },
            root,
            thread: thread.to_owned(),
        })
    }
    fn finish_task(
        &self,
        cx: &ConnectionTo<Client>,
        context: &Context,
        item: &str,
        state: &str,
    ) -> Result<()> {
        let mut jobs = self.async_tasks.lock().unwrap();
        if !jobs.current(context) {
            return Ok(());
        }
        let key = (context.thread.clone(), item.to_owned());
        jobs.ended.insert(key.clone());
        if let Some(task) = jobs.tasks.get(&key) {
            if task.published {
                update(
                    cx,
                    &task.session,
                    json!({"sessionUpdate":"async_task_state_update","asyncTaskId":task.id(),"state":state,"toolCallId":task.item}),
                )?;
            }
        }
        jobs.tasks.remove(&key);
        Ok(())
    }
    pub(super) fn observe_tasks(
        self: &Arc<Self>,
        cx: &ConnectionTo<Client>,
        method: &str,
        p: &Value,
    ) -> Result<()> {
        let Some(context) = p["threadId"].as_str().and_then(|id| self.task_context(id)) else {
            return Ok(());
        };
        let item = &p["item"];
        if item["type"] == "commandExecution" {
            if let Some(id) = item["id"].as_str().filter(|s| !s.is_empty()) {
                if method == "item/completed" {
                    let success = item["status"] == "completed"
                        && item["exitCode"].as_i64().is_none_or(|code| code == 0);
                    self.finish_task(
                        cx,
                        &context,
                        id,
                        if success { "completed" } else { "failed" },
                    )?;
                } else if method == "item/started" && process_id(&item["processId"]) {
                    let mut jobs = self.async_tasks.lock().unwrap();
                    let key = (context.thread.clone(), id.to_owned());
                    if jobs.current(&context) && !jobs.ended.contains(&key) {
                        jobs.tasks.entry(key).or_insert_with(|| Task {
                            root: context.root.clone(),
                            thread: context.thread.clone(),
                            session: context.session.clone(),
                            item: id.into(),
                            process: item["processId"].clone(),
                            name: item["command"].as_str().unwrap_or("Terminal").into(),
                            epoch: context.epoch,
                            published: false,
                            stopping: false,
                        });
                    }
                }
            }
        }
        let has_candidates = self
            .async_tasks
            .lock()
            .unwrap()
            .tasks
            .values()
            .any(|task| task.thread == context.thread);
        if method == "turn/completed" || (method == "item/started" && has_candidates) {
            self.refresh_tasks(cx, context)?;
        }
        Ok(())
    }
    fn refresh_tasks(self: &Arc<Self>, cx: &ConnectionTo<Client>, context: Context) -> Result<()> {
        {
            let mut jobs = self.async_tasks.lock().unwrap();
            if !jobs.list_supported || !jobs.current(&context) {
                return Ok(());
            }
            if let Some(again) = jobs
                .refreshing
                .get_mut(&(context.thread.clone(), context.epoch))
            {
                *again = true;
                return Ok(());
            }
            jobs.refreshing
                .insert((context.thread.clone(), context.epoch), false);
        }
        let bridge = self.clone();
        let client = cx.clone();
        cx.spawn(async move {
            loop {
                // Never await an RPC in the native read loop: it also delivers that RPC's response.
                bridge.reconcile_tasks(&client, &context).await;
                let next = {
                    let mut jobs = bridge.async_tasks.lock().unwrap();
                    if jobs.current(&context)
                        && jobs.list_supported
                        && jobs
                            .refreshing
                            .get(&(context.thread.clone(), context.epoch))
                            == Some(&true)
                    {
                        jobs.refreshing
                            .insert((context.thread.clone(), context.epoch), false);
                        true
                    } else {
                        jobs.refreshing
                            .remove(&(context.thread.clone(), context.epoch));
                        false
                    }
                };
                if !next {
                    break;
                }
            }
            Ok(())
        })
    }
    pub(super) async fn restore_tasks(&self, cx: &ConnectionTo<Client>, root: &str) {
        // History announces the descendant tree first, so job notifications have a known owner.
        let threads: Vec<_> = std::iter::once(root.to_owned())
            .chain(self.children.lock().unwrap().keys().cloned())
            .collect();
        for thread in threads {
            if let Some(context) = self.task_context(&thread).filter(|c| c.root == root) {
                self.reconcile_tasks(cx, &context).await;
            }
        }
    }
    async fn list_tasks(&self, thread: &str) -> Result<Vec<Value>> {
        let mut cursor = Value::Null;
        let mut seen = HashSet::new();
        let mut tasks = Vec::new();
        for _ in 0..100 {
            let page = self
                .rpc(
                    "thread/backgroundTerminals/list",
                    json!({"threadId":thread,"cursor":cursor}),
                )
                .await?;
            let entries = page["data"]
                .as_array()
                .ok_or_else(|| failure("Malformed Codex background-terminal list"))?;
            for entry in entries {
                if entry["itemId"].as_str().is_none_or(|s| s.is_empty())
                    || !process_id(&entry["processId"])
                    || !entry["command"].is_string()
                {
                    return Err(failure("Malformed Codex background terminal"));
                }
                tasks.push(entry.clone());
            }
            if tasks.len() > 10000 {
                return Err(failure("Codex background-terminal list exceeds the limit"));
            }
            cursor = page
                .get("nextCursor")
                .ok_or_else(|| failure("Missing Codex background-terminal cursor"))?
                .clone();
            if cursor.is_null() {
                return Ok(tasks);
            }
            if !cursor.is_string() || !seen.insert(cursor.clone()) {
                return Err(failure(
                    "Codex repeated or returned an invalid background-terminal cursor",
                ));
            }
        }
        Err(failure(
            "Codex background-terminal pagination exceeds the limit",
        ))
    }
    async fn reconcile_tasks(&self, cx: &ConnectionTo<Client>, context: &Context) {
        let gate = {
            let mut jobs = self.async_tasks.lock().unwrap();
            if !jobs.list_supported || !jobs.current(context) {
                return;
            }
            jobs.gates
                .entry((context.thread.clone(), context.epoch))
                .or_default()
                .clone()
        };
        let _guard = gate.lock().await;
        {
            let jobs = self.async_tasks.lock().unwrap();
            if !jobs.list_supported || !jobs.current(context) {
                return;
            }
        }
        let result = self
            .list_tasks(&context.thread)
            .await
            .and_then(|snapshot| self.apply_tasks(cx, context, snapshot));
        if let Err(error) = result {
            // Older native Codex builds can lack these experimental methods. Keep ordinary ACP usable.
            if error
                .data
                .as_ref()
                .is_some_and(|data| data["code"] == -32601)
            {
                self.async_tasks.lock().unwrap().list_supported = false;
            }
            eprintln!("Could not list Codex background terminals: {error}");
        }
    }
    fn apply_tasks(
        &self,
        cx: &ConnectionTo<Client>,
        context: &Context,
        snapshot: Vec<Value>,
    ) -> Result<()> {
        let mut jobs = self.async_tasks.lock().unwrap();
        if !jobs.current(context) {
            return Ok(());
        }
        let listed: HashSet<_> = snapshot
            .iter()
            .map(|t| {
                (
                    context.thread.clone(),
                    t["itemId"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        jobs.ended
            .retain(|key| key.0 != context.thread || listed.contains(key));
        for terminal in snapshot {
            let key = (
                context.thread.clone(),
                terminal["itemId"].as_str().unwrap().to_owned(),
            );
            if jobs.ended.contains(&key) {
                continue;
            }
            let task = jobs.tasks.entry(key.clone()).or_insert_with(|| Task {
                root: context.root.clone(),
                thread: context.thread.clone(),
                session: context.session.clone(),
                item: key.1.clone(),
                process: terminal["processId"].clone(),
                name: terminal["command"].as_str().unwrap().into(),
                epoch: context.epoch,
                published: false,
                stopping: false,
            });
            task.process = terminal["processId"].clone();
            if !task.published {
                update(
                    cx,
                    &task.session,
                    json!({"sessionUpdate":"tool_call_update","toolCallId":task.item,"_meta":{"jetbrains":{"air":{"version":1,"asyncTasks":{"backgrounded":true}}}}}),
                )?;
                update(
                    cx,
                    &task.session,
                    json!({"sessionUpdate":"async_task_spawned","asyncTaskId":task.id(),"name":task.name,"taskType":"shell","showInTranscript":false,"canStop":true,"toolCallId":task.item}),
                )?;
                task.published = true;
            }
        }
        // Only a complete native snapshot may resolve a previously published, missing terminal.
        let missing: Vec<_> = jobs
            .tasks
            .values()
            .filter(|t| t.thread == context.thread && t.published && !listed.contains(&t.key()))
            .cloned()
            .collect();
        for task in missing {
            update(
                cx,
                &task.session,
                json!({"sessionUpdate":"async_task_state_update","asyncTaskId":task.id(),"state":"stopped","toolCallId":task.item}),
            )?;
            jobs.ended.insert(task.key());
            jobs.tasks.remove(&task.key());
        }
        Ok(())
    }
    pub(super) async fn stop_task(&self, cx: &ConnectionTo<Client>, p: &Value) -> Result<Value> {
        let root = field(p, "sessionId")?;
        let id = field(p, "asyncTaskId")?;
        let task = {
            let mut jobs = self.async_tasks.lock().unwrap();
            if !jobs.enabled {
                return Ok(json!({"stopped":false}));
            }
            let task = jobs
                .tasks
                .values_mut()
                .find(|t| t.root == root && t.id() == id && t.published && !t.stopping);
            let Some(task) = task else {
                return Ok(json!({"stopped":false}));
            };
            task.stopping = true;
            task.clone()
        };
        let result = self
            .rpc(
                "thread/backgroundTerminals/terminate",
                json!({"threadId":task.thread,"processId":task.process}),
            )
            .await
            .and_then(|v| {
                v["terminated"]
                    .as_bool()
                    .ok_or_else(|| failure("Malformed Codex background-terminal stop response"))
            });
        let mut jobs = self.async_tasks.lock().unwrap();
        // A native completion or a replacement session can win while terminate is in flight.
        if jobs.epochs.get(root) == Some(&task.epoch) {
            if let Some(live) = jobs
                .tasks
                .get_mut(&task.key())
                .filter(|t| t.epoch == task.epoch)
            {
                live.stopping = false;
                if matches!(result, Ok(true)) {
                    update(
                        cx,
                        &live.session,
                        json!({"sessionUpdate":"async_task_state_update","asyncTaskId":live.id(),"state":"stopped","toolCallId":live.item}),
                    )?;
                    jobs.ended.insert(task.key());
                    jobs.tasks.remove(&task.key());
                }
            }
        }
        Ok(json!({"stopped":result?}))
    }
}
fn process_id(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty()) || value.is_u64()
}
