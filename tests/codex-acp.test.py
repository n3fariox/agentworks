#!/usr/bin/env python3
"""Wire tests, with no Node or inference: python3 tests/codex-acp.test.py.

Build first: cargo build -p agentworks-runtime --bin agentworks-codex-acp
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

    def notify(method, **params):
        send(dict(method=method, params=dict(threadId="thread-1", **params)))

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
        if method in ("thread/start", "thread/resume"):
            assert p["cwd"].startswith("/")
            if p.get("config", {}).get("mcp_servers"):
                assert p["config"]["mcp_servers"]["agentworks"]["env"] == {"TOKEN": "test"}
            result = dict(thread=dict(id="thread-1"), model="test-model", reasoningEffort="medium")
        elif method == "model/list":
            result = dict(data=[dict(model="test-model", displayName="Test Model", supportedReasoningEfforts=[dict(reasoningEffort="medium")], defaultReasoningEffort="medium")])
        elif method == "thread/list":
            result = dict(data=[dict(id="thread-1", cwd="/tmp", preview="Previous chat")], nextCursor=None)
        elif method == "thread/read":
            result = dict(thread=dict(turns=[dict(items=[dict(type="userMessage", content=[dict(type="text", text="old prompt")]), dict(type="agentMessage", text="old answer")])]))
        elif method == "turn/start":
            turn += 1
            active = f"turn-{turn}"
            text = p["input"][0]["text"]
            if text == "attachments":
                assert p["input"][1] == dict(type="image", url="data:image/png;base64,iVBORw0KGgo=")
                assert '<context ref="agentworks://notes">\n# Notes\n</context>' in p["input"][2]["text"]
            result = dict(turn=dict(id=active))
            if text == "exit":
                return
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
        self.process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=dict(os.environ, CODEX_PATH=str(Path(__file__).resolve())))
        self.addCleanup(self.close)
        self.messages = queue.Queue()
        self.seen = []
        self.next_id = 0

        def read():
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
            self.messages.put(None)

        threading.Thread(target=read, daemon=True).start()
        init = self.call("initialize", dict(protocolVersion=1, clientCapabilities=dict(elicitation=dict(form={}))))
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
