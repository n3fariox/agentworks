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
    settings = dict(model="test-model", reasoningEffort="medium", sandbox=dict(type="readOnly", networkAccess=False), approvalPolicy="on-request", approvalsReviewer="user", serviceTier=None)

    def notify(method, **params):
        send(dict(method=method, params=dict(threadId=active_thread, **params)))

    def complete(status="completed", error=None):
        notify("turn/completed", turn=dict(id=active, status=status, error=error))

    for line in sys.stdin:
        v = json.loads(line)
        method, p = v.get("method"), v.get("params", {})
        if method is None:  # approval or question response
            result = v.get("result", {})
            notify("item/agentMessage/delta", delta=json.dumps(result))
            notify("serverRequest/resolved", requestId="approval")
            complete()
            continue
        result = {}
        if method in ("turn/start","review/start") and late_compaction:
            notify("turn/completed",turn=dict(id=late_compaction,status="completed"))
            late_compaction=None
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
            if p["threadId"] == "legacy":
                result = dict(thread=dict(historyMode="legacy", turns=[dict(id="turn-old", items=[dict(id="old-user", type="userMessage", content=[dict(type="text", text="old prompt")]), dict(id="old-answer", type="agentMessage", text="old answer")])]))
            else:
                assert not p.get("includeTurns"), "Paginated history must not use full-history hydration"
                result = dict(thread=dict(historyMode="paginated", turns=[]))
        elif method == "thread/items/list":
            assert p["sortDirection"] == "asc"
            if p.get("cursor") is None:
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
            if text == "approval":
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
        init = self.call("initialize", dict(protocolVersion=1, clientCapabilities=dict(elicitation=dict(form={}), session=dict(configOptions=dict(boolean={})))))
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
