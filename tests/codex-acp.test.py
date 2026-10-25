#!/usr/bin/env python3
"""Wire tests, with no Node or inference: python3 tests/codex-acp.test.py.

Build first: cargo build -p agentworks-codex-acp
This file also acts as the fake native Codex executable (invoked with app-server).
"""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import tempfile
import time
import unittest


def harness():
    def send(value):
        print(json.dumps(value), flush=True)

    turn = 0
    active = None
    active_thread = "thread-1"
    deleted = set()
    late_compaction = None
    child_history = {}
    child_pending = False
    backgrounds = {}
    background_mode = "normal"
    pending_background_list = None
    def background_event(thread, item, done=False, code=0):
        send(dict(method="item/completed" if done else "item/started", params=dict(threadId=thread,item=dict(type="commandExecution",id=item["itemId"],command=item["command"],processId=item["processId"],status="completed" if done else "inProgress",exitCode=code if done else None))))
    settings = dict(model="test-model", reasoningEffort="medium", sandbox=dict(type="readOnly", networkAccess=False), approvalPolicy="on-request", approvalsReviewer="user", serviceTier=None)

    def notify(method, **params):
        send(dict(method=method, params=dict(threadId=active_thread, **params)))

    def complete(status="completed", error=None):
        notify("turn/completed", turn=dict(id=active, status=status, error=error))

    for line in sys.stdin:
        v = json.loads(line)
        method, p = v.get("method"), v.get("params", {})
        if os.environ.get("ACP_FIXTURE_NATIVE_LOG"):
            with open(os.environ["ACP_FIXTURE_NATIVE_LOG"],"a") as log:
                log.write(json.dumps(v)+"\n")
        if method is None:  # approval or question response
            result = v.get("result", {})
            if child_pending:
                activity = dict(id="child-finished",type="subAgentActivity",kind="completed",agentThreadId="child-b",agentPath="/root/research/reviewer")
                send(dict(method="item/started",params=dict(threadId="child-a",item=activity)))
                send(dict(method="turn/completed", params=dict(threadId="child-b", turn=dict(id="child-turn", status="completed"))))
                send(dict(method="item/completed",params=dict(threadId="child-a",item=activity)))
                child_pending = False
            notify("item/agentMessage/delta", delta=json.dumps(result))
            notify("serverRequest/resolved", requestId="approval")
            complete()
            continue
        result = {}
        if method == "thread/backgroundTerminals/list":
            assert os.environ.get("ACP_FIXTURE_ASYNC"), "Background API used without client negotiation"
            if os.environ.get("ACP_FIXTURE_OLD_BACKGROUND"):
                send(dict(id=v["id"],error=dict(code=-32601,message="Unknown background method")))
                continue
            entries=list(backgrounds.get(p["threadId"],{}).values())
            cursor=p.get("cursor")
            if background_mode == "stale" and entries:
                background_event(p["threadId"],entries[0],True)
                backgrounds[p["threadId"]].pop(entries[0]["itemId"],None)
            if background_mode == "hold" and entries and pending_background_list is None:
                pending_background_list=(v["id"],entries)
                continue
            if background_mode == "malformed" and entries:
                result=dict(data=[dict(itemId="invalid")],nextCursor=None)
            elif background_mode == "repeat" and entries:
                result=dict(data=entries,nextCursor="repeat")
            elif background_mode == "paged" and len(entries)>1:
                result=dict(data=entries[:1] if cursor is None else entries[1:],nextCursor="second" if cursor is None else None)
            else:
                result=dict(data=entries,nextCursor=None)
            send(dict(id=v["id"],result=result))
            continue
        elif method == "thread/backgroundTerminals/terminate":
            assert os.environ.get("ACP_FIXTURE_ASYNC"), "Terminate API used without client negotiation"
            entries=backgrounds.get(p["threadId"],{})
            item=next((item for item in entries.values() if item["processId"]==p["processId"]),None)
            if item is None:
                result=dict(terminated=False)
            elif background_mode == "stop-error":
                send(dict(id=v["id"],error=dict(code=-32603,message="Fixture terminate failed")))
                continue
            elif background_mode == "stop-false":
                result=dict(terminated=False)
            else:
                entries.pop(item["itemId"],None)
                if background_mode in ("stop-complete","stop-failed"):
                    background_event(p["threadId"],item,True,code=1 if background_mode=="stop-failed" else 0)
                result=dict(terminated=True)
            send(dict(id=v["id"],result=result))
            continue
        if method in ("turn/start","review/start") and late_compaction:
            notify("turn/completed",turn=dict(id=late_compaction,status="completed"))
            late_compaction=None
        if method == "thread/resume" and pending_background_list is not None:
            old_id,old_entries=pending_background_list
            backgrounds.clear()
            background_mode="normal"
            send(dict(id=old_id,result=dict(data=old_entries,nextCursor=None)))
        if method in ("thread/start", "thread/resume", "thread/fork"):
            assert p["cwd"].startswith("/")
            assert p["cwd"] in p["runtimeWorkspaceRoots"]
            if method == "thread/resume" and p["threadId"] in deleted:
                send(dict(id=v["id"], error=dict(code=-32602, message="Thread was deleted")))
                continue
            if p.get("config", {}).get("mcp_servers"):
                env=p["config"]["mcp_servers"]["agentworks"]["env"]
                if "TOKEN" in env:
                    assert env == {"TOKEN": "test"}
                else:
                    assert "AGENTWORKS_MCP_ENDPOINT" in env and "AGENTWORKS_MCP_TOKEN" in env
            thread_id = "thread-2" if method == "thread/fork" else p.get("threadId", "thread-1")
            if method == "thread/fork" and "lastTurnId" in p:
                assert p["lastTurnId"] == "turn-old", "Wrong fork boundary"
            result = dict(settings, thread=dict(id=thread_id))
        elif method == "model/list":
            result = dict(data=[dict(model="test-model", displayName="Test Model", additionalSpeedTiers=["fast"], supportedReasoningEfforts=[dict(reasoningEffort="medium"), dict(reasoningEffort="high")], defaultReasoningEffort="medium")])
        elif method == "thread/list":
            result = dict(data=[dict(id="thread-1", cwd="/tmp", preview="Previous chat")], nextCursor=None)
        elif method == "thread/read":
            if p["threadId"] == "child-b" and os.environ.get("ACP_FIXTURE_MISSING_CHILD"):
                send(dict(id=v["id"],error=dict(code=-32602,message="Child history was deleted")))
                continue
            if p["threadId"] == "legacy":
                result = dict(thread=dict(historyMode="legacy", turns=[dict(id="turn-old", items=[dict(id="old-user", type="userMessage", content=[dict(type="text", text="old prompt")]), dict(id="old-answer", type="agentMessage", text="old answer")])]))
            else:
                assert not p.get("includeTurns"), "Paginated history must not use full-history hydration"
                result = dict(thread=dict(historyMode="paginated", turns=[]))
        elif method == "thread/items/list":
            assert p["sortDirection"] == "asc"
            if p["threadId"] in child_history:
                result = dict(data=[dict(turnId="child-turn", item=item) for item in child_history[p["threadId"]]], nextCursor=None)
            elif p.get("cursor") is None:
                result = dict(data=[dict(turnId="turn-old", item=dict(id="old-user", type="userMessage", content=[dict(type="text", text="old prompt")]))], nextCursor="page-2")
            else:
                assert p["cursor"] == "page-2"
                result = dict(data=[dict(turnId="turn-old", item=dict(id="old-answer", type="agentMessage", text="old answer")), dict(turnId="turn-new", item=dict(id="new-answer", type="agentMessage", text="new answer"))], nextCursor=None)
        elif method == "thread/settings/update":
            if p.get("effort") == "high":
                send(dict(id=v["id"], error=dict(code=-32602, message="Managed policy refuses high effort")))
                continue
            for key, value in p.items():
                if key != "threadId":
                    settings[dict(effort="reasoningEffort", sandboxPolicy="sandbox").get(key, key)] = value
            if "sandboxPolicy" in p and p["sandboxPolicy"]["type"] == "workspaceWrite":
                assert p["sandboxPolicy"]["writableRoots"], "Missing workspace roots"
        elif method == "thread/unsubscribe":
            assert p["threadId"] not in deleted, "Close must not delete the conversation"
        elif method == "thread/delete":
            if p["threadId"] == "refuse-delete":
                send(dict(id=v["id"], error=dict(code=-32603, message="Delete failed")))
                continue
            deleted.add(p["threadId"])
        elif method in ("thread/compact/start", "review/start"):
            active_thread=p["threadId"]
            turn+=1
            active=f"command-{turn}"
            if method == "review/start":
                assert p["delivery"] == "inline"
                send(dict(id=v["id"], result=dict(turn=dict(id=active), reviewThreadId=active_thread)))
                notify("item/started",turnId=active,item=dict(id="entry-"+active,type="enteredReviewMode",review="fixture changes"))
            notify("turn/started", turn=dict(id="internal-"+active if method=="review/start" else active))
            if method=="thread/compact/start":
                notify("item/started",turnId=active,item=dict(id=active,type="contextCompaction"))
            if not os.environ.get("ACP_FIXTURE_HOLD_COMPACT") or method == "review/start":
                notify("item/completed", item=dict(id=active, type="exitedReviewMode" if method=="review/start" else "contextCompaction",review=json.dumps(p.get("target"))))
                if method=="review/start":complete()
                else:late_compaction=active
            if method == "thread/compact/start":
                send(dict(id=v["id"], result={}))
            continue
        elif method == "skills/list":
            assert p["cwds"]
            result=dict(data=[dict(skills=[dict(name="check",description="Check the project")])])
        elif method == "mcpServerStatus/list":
            assert p["threadId"]
            assert p["detail"] == "toolsAndAuthOnly"
            if p.get("cursor") is None:
                result=dict(data=[dict(name="one",authStatus="oAuth")],nextCursor="mcp-2")
            else:
                assert p["cursor"]=="mcp-2"
                result=dict(data=[dict(name="two",authStatus="notLoggedIn")],nextCursor=None)
        elif method == "thread/name/set":
            assert p["name"]=="New conversation name"
        elif method == "turn/start":
            active_thread = p["threadId"]
            turn += 1
            active = f"turn-{turn}"
            text = p["input"][0]["text"]
            assert not text.startswith('/'), "A slash command was incorrectly sent to the model"
            if text == "attachments":
                assert p["input"][1] == dict(type="image", url="data:image/png;base64,iVBORw0KGgo=")
                assert '<context ref="agentworks://notes">\n# Notes\n</context>' in p["input"][2]["text"]
            result = dict(turn=dict(id=active))
            if text == "exit":
                return
            if text == "settings":
                notify("item/agentMessage/delta", delta=json.dumps(settings))
            if text == "quota":
                send(dict(method="account/rateLimits/updated", params=dict(rateLimits=dict(limitId="codex", primary=dict(usedPercent=15, windowDurationMins=300), secondary=dict(usedPercent=60, windowDurationMins=10080)))))
                notify("thread/tokenUsage/updated", tokenUsage=dict(last=dict(totalTokens=123), modelContextWindow=10000))
                time.sleep(.005)
                send(dict(method="account/rateLimits/updated", params=dict(rateLimits=dict(primary=dict(resetsAt=1800000000), secondary=None))))
                notify("thread/tokenUsage/updated", tokenUsage=dict(last=dict(totalTokens=124), modelContextWindow=10000))
                send(dict(method="thread/tokenUsage/updated", params=dict(threadId="unknown-child", tokenUsage=dict(last=dict(totalTokens=999), modelContextWindow=10000))))
            if text == "early":
                notify("item/agentMessage/delta", delta="early answer")
                complete()  # deliberately before the turn/start response
            send(dict(id=v["id"], result=result))
            if text.startswith("async-"):
                background_mode=text.removeprefix("async-")
                if background_mode in ("complete","failed"):
                    for thread,items in list(backgrounds.items()):
                        for item in list(items.values()):
                            background_event(thread,item,True,1 if background_mode=="failed" else 0)
                        items.clear()
                elif background_mode == "disappear":
                    backgrounds.clear()
                    notify("item/started",item=dict(id="probe",type="webSearch",status="inProgress"))
                elif background_mode not in ("malformed","repeat"):
                    item=dict(itemId="bg-command",processId="pid-root",command="build root")
                    backgrounds[active_thread]={item["itemId"]:item}
                    background_event(active_thread,item)
                    if background_mode == "paged":
                        extra=dict(itemId="bg-extra",processId=2002,command="build extra")
                        backgrounds[active_thread][extra["itemId"]]=extra
                        background_event(active_thread,extra)
                    if background_mode == "child":
                        collab=dict(id="spawn-bg",type="collabAgentToolCall",tool="spawnAgent",status="completed",receiverThreadIds=["child-bg"],prompt="Run the child build",agentsStates={})
                        notify("item/completed",item=collab)
                        child_history={active_thread:[collab],"child-bg":[]}
                        child_item=dict(itemId="bg-command",processId="pid-child",command="build child")
                        backgrounds["child-bg"]={child_item["itemId"]:child_item}
                        background_event("child-bg",child_item)
                        send(dict(method="turn/completed",params=dict(threadId="child-bg",turn=dict(id="child-bg-turn",status="completed"))))
                    # This unrelated session must never create a discoverable job.
                    background_event("foreign",dict(itemId="foreign",processId="evil",command="foreign job"))
                if background_mode != "busy":complete()
            elif text.startswith("subagents"):
                child_pending = True
                legacy = dict(id="spawn-a", type="collabAgentToolCall", tool="spawnAgent", status="completed", senderThreadId=active_thread, receiverThreadIds=["child-a"], prompt="Inspect the project", agentsStates={"child-a":dict(status="running")})
                modern = dict(id="spawn-b", type="subAgentActivity", kind="started", agentThreadId="child-b", agentPath="/root/research/reviewer")
                child_history = {
                    active_thread:[legacy],
                    "child-a":[modern,dict(id="same-message",type="agentMessage",text="Child answer")],
                    "child-b":[dict(id="same-message",type="agentMessage",text="Nested answer")],
                }
                notify("item/completed", item=legacy)
                send(dict(method="thread/started", params=dict(thread=dict(id="child-a",parentThreadId=active_thread,agentNickname="Researcher",agentRole="explorer"))))
                send(dict(method="item/started", params=dict(threadId="child-a", item=modern)))
                # An unrelated relationship notification cannot reparent an existing child.
                send(dict(method="thread/started",params=dict(thread=dict(id="child-a",parentThreadId="child-b"))))
                for sid, text in [(active_thread,"Root answer"),("child-a","Child answer"),("child-b","Nested answer"),("unknown-child","Must not leak")]:
                    send(dict(method="item/agentMessage/delta", params=dict(threadId=sid,itemId="same-message",delta=text)))
                    send(dict(method="item/started", params=dict(threadId=sid,item=dict(id="tool-1",type="commandExecution",command="echo fixture",status="inProgress"))))
                send(dict(method="turn/completed", params=dict(threadId="child-a",turn=dict(id=active,status="completed"))))
                send(dict(method="thread/status/changed",params=dict(threadId="child-b",status=dict(type="active",activeFlags=["waitingOnApproval"]))))
                send(dict(id="approval",method="item/commandExecution/requestApproval",params=dict(threadId="child-b",turnId="child-turn",itemId="tool-1",command="echo child",availableDecisions=["accept","decline"])))
            elif text == "approval":
                send(dict(id="approval", method="item/commandExecution/requestApproval", params=dict(threadId="thread-1", turnId=active, itemId="tool-1", command="echo hello", availableDecisions=["accept", "decline", "cancel"])))
            elif text == "question":
                send(dict(id="approval", method="item/tool/requestUserInput", params=dict(threadId="thread-1", turnId=active, itemId="tool-1", questions=[dict(id="color", header="Color", question="Choose a color", options=[dict(label="Blue"), dict(label="Red")])])))
            elif text == "wait":
                notify("item/agentMessage/delta", delta="waiting")
                send(dict(id="approval", method="item/commandExecution/requestApproval", params=dict(threadId="thread-1", turnId=active, itemId="tool-1", command="echo hello")))
            elif text == "fail":
                complete("failed", dict(message="Quota exhausted", codexErrorInfo="usageLimitExceeded"))
            elif text != "early":
                if text=="automatic_compaction":
                    notify("item/completed", item=dict(id="automatic",type="contextCompaction"))
                item = dict(id="tool-1", type="commandExecution", command="echo hello", status="inProgress")
                notify("item/started", item=item)
                notify("item/commandExecution/outputDelta", itemId="tool-1", delta="hello\n")
                notify("item/completed", item=dict(item, status="completed", aggregatedOutput="hello\n"))
                notify("item/agentMessage/delta", delta="answer")
                complete()
            continue
        elif method == "turn/interrupt":
            assert p["turnId"] == active
            if child_pending:
                send(dict(method="turn/completed",params=dict(threadId="child-b",turn=dict(id="child-turn",status="interrupted"))))
                child_pending = False
            notify("serverRequest/resolved", requestId="approval")
            complete("interrupted")
        if "id" in v:
            send(dict(id=v["id"], result=result))


class BridgeTests(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parents[1]
        binary = os.environ.get("CODEX_ACP_TEST_BIN", str(root / "target/debug/agentworks-codex-acp"))
        env=dict(os.environ, CODEX_PATH=str(Path(__file__).resolve()))
        if self._testMethodName=="test_cancel_native_compaction":
            env["ACP_FIXTURE_HOLD_COMPACT"]="1"
        if self._testMethodName=="test_subagents_missing_child_history_keeps_parent_loadable":
            env["ACP_FIXTURE_MISSING_CHILD"]="1"
        if self._testMethodName.startswith("test_async_"):
            self.native_fixture=tempfile.TemporaryDirectory(prefix="codex-background-")
            self.addCleanup(self.native_fixture.cleanup)
            self.native_log=Path(self.native_fixture.name)/"native.jsonl"
            env["ACP_FIXTURE_NATIVE_LOG"]=str(self.native_log)
            if self._testMethodName != "test_async_without_negotiation":env["ACP_FIXTURE_ASYNC"]="1"
            if self._testMethodName == "test_async_old_native_keeps_ordinary_turns_usable":env["ACP_FIXTURE_OLD_BACKGROUND"]="1"
        self.process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        self.addCleanup(self.close)
        self.messages = queue.Queue()
        self.seen = []
        self.next_id = 0

        def read():
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
            self.messages.put(None)

        threading.Thread(target=read, daemon=True).start()
        if self._testMethodName.startswith("test_async_") and self._testMethodName != "test_async_without_negotiation":
            capabilities_async = True
        else:
            capabilities_async = False
        capabilities = dict(elicitation=dict(form={}), session=dict(configOptions=dict(boolean={})))
        if self._testMethodName not in ("test_subagents_without_client_capability","test_async_child_without_subagent_capability_routes_jobs_to_root"):
            capabilities["subagents"] = {}
        if capabilities_async:
            capabilities["_meta"]=dict(jetbrains=dict(air=dict(version=1,capabilities=["asyncTasks"])))
        init = self.call("initialize", dict(protocolVersion=1, clientCapabilities=capabilities))
        self.assertEqual("asyncTasks" in init.get("_meta",{}).get("jetbrains",{}).get("air",{}).get("capabilities",[]),capabilities_async)
        self.assertTrue(init["agentCapabilities"]["loadSession"])

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            self.fail("Bridge did not exit after ACP stdin closed")
        finally:
            self.process.stdout.close()
            self.process.stderr.close()

    def send(self, method, params, request=True):
        value = dict(jsonrpc="2.0", method=method, params=params)
        if request:
            self.next_id += 1
            value["id"] = self.next_id
        self.write(value)
        return self.next_id

    def write(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def until(self, predicate):
        deadline = time.monotonic() + 5
        while True:
            item = self.messages.get(timeout=max(0, deadline - time.monotonic()))
            if item is None:
                self.fail("Bridge exited: " + self.process.stderr.read())
            self.seen.append(item)
            if predicate(item):
                return item

    def call(self, method, params):
        request_id = self.send(method, params)
        response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
        self.assertNotIn("error", response, response)
        return response["result"]

    def session(self, method="session/new"):
        p = dict(cwd="/tmp", mcpServers=[dict(name="agentworks", command="agentworks", args=["mcp"], env=[dict(name="TOKEN", value="test")])])
        if method == "session/load":
            p["sessionId"] = "thread-1"
        return self.call(method, p)

    def prompt(self, text):
        return self.call("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text=text)]))

    def test_streaming_and_early_completion_and_second_turn(self):
        self.assertEqual(self.session()["configOptions"][0]["currentValue"], "test-model")
        self.assertEqual(self.prompt("early")["stopReason"], "end_turn")
        self.assertEqual(self.prompt("normal")["stopReason"], "end_turn")
        kinds = [v["params"]["update"]["sessionUpdate"] for v in self.seen if v.get("method") == "session/update"]
        self.assertIn("tool_call", kinds)
        self.assertIn("tool_call_update", kinds)

    def test_approval_and_question_roundtrip(self):
        self.session()
        for text, method, answer in [("approval", "session/request_permission", dict(outcome=dict(outcome="selected", optionId="accept"))), ("question", "elicitation/create", dict(action="accept", content=dict(color="Blue")))]:
            request_id = self.send("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text=text)]))
            request = self.until(lambda v: v.get("method") == method)
            self.write(dict(jsonrpc="2.0", id=request["id"], result=answer))
            response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
            self.assertEqual(response["result"]["stopReason"], "end_turn")
        chunks = str(self.seen)
        self.assertIn("accept", chunks)
        self.assertIn("Blue", chunks)

    def subagent_prompt(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text="subagents")]))
        approval = self.until(lambda v:v.get("method")=="session/request_permission")
        self.assertFalse(any(v.get("id")==request_id and "method" not in v for v in self.seen), "Child completion ended parent prompt")
        return request_id, approval

    def finish_subagents(self, request_id, approval):
        self.write(dict(jsonrpc="2.0",id=approval["id"],result=dict(outcome=dict(outcome="selected",optionId="accept"))))
        response=self.until(lambda v:v.get("id")==request_id and "method" not in v)
        self.assertEqual(response["result"]["stopReason"],"end_turn")

    def test_subagents_route_nested_traffic_and_approvals(self):
        request_id, approval = self.subagent_prompt()
        self.assertEqual(approval["params"]["sessionId"], "child-b")
        announcements = set()
        for v in self.seen:
            if v.get("method") != "session/update": continue
            p=v["params"];u=p["update"]
            if u["sessionUpdate"]=="subagent_update":
                if p["sessionId"]!="thread-1": self.assertIn(p["sessionId"],announcements)
                announcements.add(u["sessionId"])
                if u["sessionId"]=="child-a": self.assertEqual(p["sessionId"],"thread-1")
                self.assertNotIn("prompt",u.get("capabilities",{}))
            elif p["sessionId"]!="thread-1": self.assertIn(p["sessionId"],announcements)
            self.assertNotEqual(p["sessionId"],"unknown-child")
        self.assertEqual(announcements,{"child-a","child-b"})
        for sid in ["thread-1","child-a","child-b"]:
            self.assertTrue(any(v.get("params",{}).get("sessionId")==sid and v.get("params",{}).get("update",{}).get("toolCallId")=="tool-1" for v in self.seen))
        self.assertTrue(any(v.get("params",{}).get("update",{}).get("state")=={"state":"requires_action"} for v in self.seen))
        rejected=self.send("session/prompt",dict(sessionId="child-b",prompt=[dict(type="text",text="not allowed")]))
        self.assertIn("error",self.until(lambda v:v.get("id")==rejected))
        self.finish_subagents(request_id,approval)
        self.assertEqual(self.prompt("normal")["stopReason"],"end_turn")

    def test_subagents_without_client_capability(self):
        request_id, approval = self.subagent_prompt()
        self.assertEqual(approval["params"]["sessionId"],"thread-1")
        self.assertFalse(any(v.get("params",{}).get("sessionId") in ["child-a","child-b"] for v in self.seen))
        self.assertFalse(any(v.get("params",{}).get("update",{}).get("sessionUpdate")=="subagent_update" for v in self.seen))
        self.finish_subagents(request_id,approval)

    def test_subagents_history_replays_after_close(self):
        request_id, approval = self.subagent_prompt()
        self.finish_subagents(request_id,approval)
        self.call("session/close",dict(sessionId="thread-1"))
        self.seen.clear()
        self.session("session/load")
        for sid,text in [("child-a","Child answer"),("child-b","Nested answer")]:
            self.assertTrue(any(v.get("params",{}).get("sessionId")==sid and v.get("params",{}).get("update",{}).get("content",{}).get("text")==text for v in self.seen if isinstance(v.get("params",{}).get("update",{}).get("content"),dict)))

    def test_subagents_parent_cancel_with_child_approval(self):
        request_id, _ = self.subagent_prompt()
        self.send("session/cancel",dict(sessionId="thread-1"),request=False)
        response=self.until(lambda v:v.get("id")==request_id and "method" not in v)
        self.assertEqual(response["result"]["stopReason"],"cancelled")
        self.assertTrue(any(v.get("params",{}).get("update",{}).get("state")=={"state":"idle","stopReason":"cancelled"} for v in self.seen))
        self.assertEqual(self.prompt("normal")["stopReason"],"end_turn")

    def test_subagents_missing_child_history_keeps_parent_loadable(self):
        request_id, approval = self.subagent_prompt()
        self.finish_subagents(request_id,approval)
        self.call("session/close",dict(sessionId="thread-1"))
        self.seen.clear()
        self.session("session/load")
        self.assertTrue(any(v.get("params",{}).get("update",{}).get("sessionId")=="child-b" and v["params"]["update"].get("state")=={"state":"unknown"} for v in self.seen))
        self.assertEqual(self.prompt("normal")["stopReason"],"end_turn")

    def test_cancel_with_pending_approval_then_continue(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text="wait")]))
        self.until(lambda v: v.get("method") == "session/request_permission")
        self.send("session/cancel", dict(sessionId="thread-1"), request=False)
        response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
        self.assertEqual(response["result"]["stopReason"], "cancelled")
        self.assertEqual(self.prompt("normal")["stopReason"], "end_turn")
        self.assertTrue(any(v.get("method") == "$/cancel_request" for v in self.seen))

    def test_history_and_model_config(self):
        self.session("session/load")
        self.assertIn("old answer", str(self.seen))
        self.assertEqual(self.call("session/list", {})["sessions"][0]["sessionId"], "thread-1")
        configured = self.call("session/set_config_option", dict(sessionId="thread-1", configId="reasoning_effort", value="medium"))
        self.assertEqual(configured["configOptions"][1]["currentValue"], "medium")

    def test_images_and_context_reach_native_input(self):
        self.session()
        response = self.call("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text="attachments"), dict(type="image", mimeType="image/png", data="iVBORw0KGgo="), dict(type="resource", resource=dict(uri="agentworks://notes", text="# Notes"))]))
        self.assertEqual(response["stopReason"], "end_turn")

    def test_approval_cannot_select_an_unoffered_decision(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text="approval")]))
        request = self.until(lambda v: v.get("method") == "session/request_permission")
        self.write(dict(jsonrpc="2.0", id=request["id"], result=dict(outcome=dict(outcome="selected", optionId="acceptForSession"))))
        self.until(lambda v: v.get("id") == request_id and "method" not in v)
        chunks = [v["params"]["update"]["content"]["text"] for v in self.seen if v.get("method") == "session/update" and v["params"]["update"]["sessionUpdate"] == "agent_message_chunk"]
        self.assertEqual(json.loads("".join(chunks)), dict(decision="cancel"))

    def test_native_settings_acknowledgement_and_rejection(self):
        options = self.session()["configOptions"]
        self.assertEqual(next(o for o in options if o["id"] == "fast-mode")["type"], "boolean")
        for key, value in [("mode", "workspace-write"), ("collaboration_mode", "plan"), ("fast-mode", True)]:
            result = self.call("session/set_config_option", dict(sessionId="thread-1", configId=key, value=value))
            self.assertEqual(next(o for o in result["configOptions"] if o["id"] == key)["currentValue"], value)
        self.call("session/set_mode", dict(sessionId="thread-1", modeId="read-only"))
        request_id = self.send("session/set_config_option", dict(sessionId="thread-1", configId="reasoning_effort", value="high"))
        response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
        self.assertIn("error", response)
        result = self.call("session/set_config_option", dict(sessionId="thread-1", configId="fast-mode", value=False))
        self.assertEqual(next(o for o in result["configOptions"] if o["id"] == "reasoning_effort")["currentValue"], "medium")
        self.prompt("settings")
        chunks = [v["params"]["update"]["content"]["text"] for v in self.seen if v.get("method") == "session/update" and v["params"]["update"]["sessionUpdate"] == "agent_message_chunk"]
        native = json.loads(chunks[-2])
        self.assertEqual(native["sandbox"]["type"], "readOnly")
        self.assertEqual(native["collaborationMode"]["mode"], "plan")
        self.assertIsNone(native["serviceTier"])

    def test_close_preserves_history_and_delete_prevents_resume(self):
        self.session()
        self.call("session/close", dict(sessionId="thread-1"))
        self.session("session/load")
        self.call("session/delete", dict(sessionId="thread-1"))
        self.call("session/delete", dict(sessionId="thread-1"))  # explicit retry is idempotent
        request_id = self.send("session/load", dict(sessionId="thread-1", cwd="/tmp", mcpServers=[]))
        response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
        self.assertIn("error", response)

    def test_failed_delete_retains_live_session(self):
        self.call("session/load", dict(sessionId="refuse-delete", cwd="/tmp", mcpServers=[]))
        request_id = self.send("session/delete", dict(sessionId="refuse-delete"))
        response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
        self.assertIn("error", response)
        result = self.call("session/prompt", dict(sessionId="refuse-delete", prompt=[dict(type="text", text="normal")]))
        self.assertEqual(result["stopReason"], "end_turn")

    def test_paginated_fork_uses_requested_boundary(self):
        self.session()
        request = dict(sessionId="thread-1", cwd="/tmp", mcpServers=[], _meta=dict(jetbrains=dict(air=dict(fork=dict(version=1, messageId="old-answer:segment:2")))))
        self.assertEqual(self.call("session/fork", request)["sessionId"], "thread-2")
        request["_meta"]["jetbrains"]["air"]["fork"]["messageId"] = "missing"
        request_id = self.send("session/fork", request)
        response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
        self.assertIn("error", response)
        self.assertEqual(self.prompt("normal")["stopReason"], "end_turn")

    def test_legacy_history_still_replays(self):
        self.call("session/load", dict(sessionId="legacy", cwd="/tmp", mcpServers=[]))
        messages = [v["params"]["update"] for v in self.seen if v.get("method") == "session/update" and v["params"]["update"]["sessionUpdate"] == "agent_message_chunk"]
        self.assertEqual(messages[-1]["messageId"], "old-answer")

    def test_sparse_quota_pushes_preserve_observation_times(self):
        self.session()
        self.prompt("quota")
        updates = [v["params"]["update"] for v in self.seen if v.get("method") == "session/update" and v["params"]["update"]["sessionUpdate"] == "usage_update"]
        self.assertEqual([u["used"] for u in updates], [123, 124])  # no child usage leak
        first, second = [u["_meta"]["_codex/subscriptionLimits"]["limits"][0] for u in updates]
        self.assertEqual(first["primary"]["usedPercent"], second["primary"]["usedPercent"])
        self.assertGreater(second["primary"]["resetsAt"]["observedAtMs"], first["primary"]["usedPercent"]["observedAtMs"])
        self.assertIsNone(second["secondary"])
        self.assertEqual(first["secondary"]["usedPercent"]["value"], 60)

    def test_additional_roots_are_used_for_workspace_mode(self):
        self.call("session/new", dict(cwd="/tmp", additionalDirectories=["/tmp/extra"], mcpServers=[]))
        self.call("session/set_mode", dict(sessionId="thread-1", modeId="workspace-write"))
        self.prompt("settings")
        chunks = [v["params"]["update"]["content"]["text"] for v in self.seen if v.get("method") == "session/update" and v["params"]["update"]["sessionUpdate"] == "agent_message_chunk"]
        self.assertEqual(json.loads(chunks[-2])["sandbox"]["writableRoots"], ["/tmp", "/tmp/extra"])

    def test_advertised_commands_run_without_becoming_model_prompts(self):
        self.session()
        announced=[v["params"]["update"] for v in self.seen if v.get("method")=="session/update" and v["params"]["update"]["sessionUpdate"]=="available_commands_update"]
        names={c["name"] for c in announced[-1]["availableCommands"]}
        for command in ["/status","/model","/model test-model","/permissions","/permissions read-only","/plan","/fast on","/fast off","/skills","/mcp","/rename New conversation name","/compact","/review","/review check naming","/review-branch main","/review-commit abc123"]:
            self.assertIn(command.split()[0][1:],names)
            self.assertEqual(self.prompt(command)["stopReason"],"end_turn")
        chunks=[v["params"]["update"]["content"]["text"] for v in self.seen if v.get("method")=="session/update" and v["params"]["update"]["sessionUpdate"]=="agent_message_chunk"]
        self.assertTrue(any("Subscription quota: not reported" in c for c in chunks))
        self.assertTrue(any("$check" in c for c in chunks))
        self.assertTrue(any("one" in c and "two" in c for c in chunks))
        self.assertIn(dict(type="baseBranch",branch="main"),[json.loads(c) for c in chunks if c.startswith('{')])
        self.assertEqual(self.prompt("normal")["stopReason"],"end_turn")

    def test_invalid_commands_fail_without_starting_a_model_turn(self):
        self.session()
        for command in ["/unknown","/compact unexpected","/rename","/review-branch","/review-commit","/fast faster"]:
            request_id=self.send("session/prompt",dict(sessionId="thread-1",prompt=[dict(type="text",text=command)]))
            response=self.until(lambda v:v.get("id")==request_id and "method" not in v)
            self.assertIn("error",response)
        self.assertEqual(self.prompt("normal")["stopReason"],"end_turn")

    def test_cancel_native_compaction(self):
        self.session()
        request_id=self.send("session/prompt",dict(sessionId="thread-1",prompt=[dict(type="text",text="/compact")]))
        self.until(lambda v:v.get("method")=="session/update" and v["params"]["update"]["sessionUpdate"]=="tool_call")
        self.send("session/cancel",dict(sessionId="thread-1"),request=False)
        response=self.until(lambda v:v.get("id")==request_id and "method" not in v)
        self.assertEqual(response["result"]["stopReason"],"cancelled")
        self.assertEqual(self.prompt("normal")["stopReason"],"end_turn")

    def native_requests(self):
        return [json.loads(line) for line in self.native_log.read_text().splitlines()]

    def job_event(self, task, state=None):
        def match(v):
            u=v.get("params",{}).get("update",{})
            return u.get("asyncTaskId")==task and u.get("sessionUpdate")==("async_task_spawned" if state is None else "async_task_state_update") and (state is None or u.get("state")==state)
        return next((v for v in reversed(self.seen) if match(v)),None) or self.until(match)

    def test_async_background_discovery_and_individual_stop(self):
        self.session()
        self.assertEqual(self.prompt("async-start")["stopReason"],"end_turn")
        event=self.job_event("bg-command")
        self.assertEqual(event["params"]["sessionId"],"thread-1")
        self.assertEqual(event["params"]["update"]["toolCallId"],"bg-command")
        self.assertTrue(event["params"]["update"]["canStop"])
        self.assertTrue(any(v.get("params",{}).get("update",{}).get("_meta",{}).get("jetbrains",{}).get("air",{}).get("asyncTasks",{}).get("backgrounded") for v in self.seen))
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="other-root",asyncTaskId="bg-command")),dict(stopped=False))
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="unknown")),dict(stopped=False))
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=True))
        self.job_event("bg-command","stopped")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=False))
        native=self.native_requests()
        stops=[v["params"] for v in native if v.get("method")=="thread/backgroundTerminals/terminate"]
        self.assertEqual(stops,[dict(threadId="thread-1",processId="pid-root")])
        self.assertFalse(any(v.get("method")=="turn/interrupt" for v in native))

    def test_async_stop_during_turn_does_not_cancel_turn(self):
        self.session()
        prompt=self.send("session/prompt",dict(sessionId="thread-1",prompt=[dict(type="text",text="async-busy")]))
        self.job_event("bg-command")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=True))
        self.job_event("bg-command","stopped")
        self.assertFalse(any(v.get("id")==prompt and "method" not in v for v in self.seen))
        self.send("session/cancel",dict(sessionId="thread-1"),request=False)
        self.assertEqual(self.until(lambda v:v.get("id")==prompt and "method" not in v)["result"]["stopReason"],"cancelled")

    def test_async_child_jobs_are_scoped_and_resume_rediscovers_live_jobs(self):
        self.session();self.prompt("async-child")
        self.job_event("bg-command")
        child=self.job_event("child-bg:bg-command")
        self.assertEqual(child["params"]["sessionId"],"child-bg")
        self.session("session/load")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="child-bg:bg-command")),dict(stopped=True))
        self.job_event("child-bg:bg-command","stopped")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=True))
        requests=self.native_requests()
        stops=[v["params"] for v in requests if v.get("method")=="thread/backgroundTerminals/terminate"]
        self.assertEqual(stops,[dict(threadId="child-bg",processId="pid-child"),dict(threadId="thread-1",processId="pid-root")])
        self.assertFalse(any(v.get("params",{}).get("threadId")=="foreign" and v.get("method")=="thread/backgroundTerminals/list" for v in requests))

    def test_async_child_without_subagent_capability_routes_jobs_to_root(self):
        self.session();self.prompt("async-child")
        child=self.job_event("child-bg:bg-command")
        self.assertEqual(child["params"]["sessionId"],"thread-1")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="child-bg:bg-command")),dict(stopped=True))
        self.job_event("child-bg:bg-command","stopped")

    def test_async_pagination_and_missing_terminals(self):
        self.session();self.prompt("async-paged")
        self.job_event("bg-command");self.job_event("bg-extra")
        self.prompt("async-disappear")
        self.job_event("bg-command","stopped");self.job_event("bg-extra","stopped")
        self.assertTrue(any(v.get("method")=="thread/backgroundTerminals/list" and v["params"].get("cursor")=="second" for v in self.native_requests()))

    def test_async_completion_beats_stale_list_response(self):
        self.session();self.prompt("async-stale")
        self.until(lambda v:v.get("params",{}).get("update",{}).get("toolCallId")=="bg-command" and v["params"]["update"].get("status")=="completed")
        self.session("session/load")
        self.assertFalse(any(v.get("params",{}).get("update",{}).get("sessionUpdate")=="async_task_spawned" for v in self.seen))

    def test_async_reload_fences_inflight_old_snapshot(self):
        self.session();self.prompt("async-hold")
        # The native thread/resume releases the held snapshot from before this load.
        self.session("session/load")
        self.assertFalse(any(v.get("params",{}).get("update",{}).get("sessionUpdate")=="async_task_spawned" for v in self.seen))

    def test_async_completion_beats_stop_response(self):
        self.session();self.prompt("async-stop-complete");self.job_event("bg-command")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=True))
        self.job_event("bg-command","completed")
        finals=[v["params"]["update"]["state"] for v in self.seen if v.get("params",{}).get("update",{}).get("sessionUpdate")=="async_task_state_update"]
        self.assertEqual(finals,["completed"])

    def test_async_nonzero_exit_beats_stop_response(self):
        self.session();self.prompt("async-stop-failed");self.job_event("bg-command")
        self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command"))
        self.job_event("bg-command","failed")
        self.assertFalse(any(v.get("params",{}).get("update",{}).get("state")=="stopped" for v in self.seen))

    def test_async_stop_false_keeps_job_live(self):
        self.session();self.prompt("async-stop-false");self.job_event("bg-command")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=False))
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=False))
        self.assertFalse(any(v.get("params",{}).get("update",{}).get("sessionUpdate")=="async_task_state_update" for v in self.seen))

    def test_async_stop_error_keeps_job_live(self):
        self.session();self.prompt("async-stop-error");self.job_event("bg-command")
        for _ in range(2):
            request=self.send("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command"))
            self.assertIn("error",self.until(lambda v:v.get("id")==request and "method" not in v))
        self.assertFalse(any(v.get("params",{}).get("update",{}).get("sessionUpdate")=="async_task_state_update" for v in self.seen))

    def test_async_bad_or_repeated_pages_do_not_finish_existing_jobs(self):
        self.session();self.prompt("async-start");self.job_event("bg-command")
        self.prompt("async-malformed");self.prompt("async-repeat")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=True))
        self.job_event("bg-command","stopped")

    def test_async_without_negotiation(self):
        self.session();self.prompt("async-start")
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=False))
        self.assertFalse(any(v.get("method","").startswith("thread/backgroundTerminals/") for v in self.native_requests()))
        self.assertFalse(any(v.get("params",{}).get("update",{}).get("sessionUpdate","").startswith("async_task_") for v in self.seen))

    def test_async_old_native_keeps_ordinary_turns_usable(self):
        self.session();self.prompt("normal");self.prompt("async-start")
        self.assertEqual(len([v for v in self.native_requests() if v.get("method")=="thread/backgroundTerminals/list"]),1)
        self.assertEqual(self.call("_session/async_task/stop",dict(sessionId="thread-1",asyncTaskId="bg-command")),dict(stopped=False))

    def test_automatic_compaction_does_not_end_a_normal_prompt(self):
        self.session()
        self.assertEqual(self.prompt("automatic_compaction")["stopReason"],"end_turn")
        self.assertTrue(any(v.get("method")=="session/update" and v["params"]["update"]["sessionUpdate"]=="agent_message_chunk" and v["params"]["update"]["content"]["text"]=="answer" for v in self.seen))

    def test_native_errors_and_disconnect_do_not_hang(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text="fail")]))
        response = self.until(lambda v: v.get("id") == request_id and "method" not in v)
        self.assertEqual(response["error"]["data"]["codexErrorInfo"], "usageLimitExceeded")
        self.send("session/prompt", dict(sessionId="thread-1", prompt=[dict(type="text", text="exit")]))
        self.process.wait(timeout=5)


if __name__ == "__main__":
    if "app-server" in sys.argv:
        harness()
    else:
        unittest.main()
