#!/usr/bin/env python3
"""Wire tests, with no Node or inference: python3 tests/claude-acp.test.py.

Build first: cargo build -p agentworks-claude-acp
This file also acts as the fake native Claude Code executable (stream-json mode).
"""
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest

MODELS = [
    dict(value="default", resolvedModel="claude-opus-test", displayName="Default (recommended)", description="Opus", supportsEffort=True, supportedEffortLevels=["low", "high"], supportsAutoMode=True),
    dict(value="haiku", resolvedModel="claude-haiku-test", displayName="Haiku", description="Fast"),
]


def project_dir(cwd):
    return Path(os.environ["CLAUDE_CONFIG_DIR"]) / "projects" / re.sub(r"[^a-zA-Z0-9]", "-", cwd)


def harness():
    args = sys.argv[1:]
    session = args[args.index("--resume" if "--resume" in args else "--session-id") + 1]
    if "--resume" in args and not (project_dir(os.getcwd()) / f"{session}.jsonl").is_file():
        print(f"No conversation found with session ID: {session}", file=sys.stderr)
        sys.exit(1)
    lines = queue.Queue()
    threading.Thread(target=lambda: [lines.put(json.loads(l)) for l in sys.stdin] + [lines.put(None)], daemon=True).start()
    mode = "default"
    responses = {}
    # Like Claude Code, report busy/idle only when asked; a marker file simulates an older CLI.
    states = os.environ.get("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS") == "1" and not Path("no-state-events").exists()

    def send(**value):
        print(json.dumps(value), flush=True)

    def ok(request_id, response=None):
        send(type="control_response", response=dict(subtype="success", request_id=request_id, response=response or {}))

    def ask(request_id, **request):
        """Send a control request to the host and wait for its answer, serving other messages meanwhile."""
        send(type="control_request", request_id=request_id, request=request)
        while request_id not in responses:
            if not serve(lines.get()):
                return None
        return responses.pop(request_id)

    def text(message_id, value, stream=True):
        if stream:
            send(type="stream_event", parent_tool_use_id=None, event=dict(type="message_start", message=dict(id=message_id, model="claude-opus-test", usage=dict(input_tokens=100, output_tokens=1))))
            for part in (value[:3], value[3:]):
                send(type="stream_event", parent_tool_use_id=None, event=dict(type="content_block_delta", index=0, delta=dict(type="text_delta", text=part)))
            send(type="stream_event", parent_tool_use_id=None, event=dict(type="message_delta", usage=dict(output_tokens=20)))
        send(type="assistant", parent_tool_use_id=None, message=dict(id=message_id, model="claude-opus-test", content=[dict(type="text", text=value)]))

    def tool(name, tool_id, tool_input):
        send(type="assistant", parent_tool_use_id=None, message=dict(id="msg-tool", model="claude-opus-test", content=[dict(type="tool_use", id=tool_id, name=name, input=tool_input)]))

    def result(subtype="success", is_error=False, value="", output=20, **extra):
        send(type="result", subtype=subtype, is_error=is_error, result=value, total_cost_usd=0.5, usage=dict(output_tokens=output), modelUsage={"claude-opus-test": dict(contextWindow=1000)}, uuid="result-1", **extra)

    def serve(v):
        nonlocal mode
        if v is None:
            return False
        if v["type"] == "control_response":
            responses[v["response"]["request_id"]] = v["response"].get("response")
            return True
        if v["type"] == "control_request":
            r = v["request"]
            if r["subtype"] == "initialize":
                ok(v["request_id"], dict(models=MODELS, commands=[dict(name="usage", description="Show usage", argumentHint=""), dict(name="clear", description="Clear", argumentHint=""), dict(name="review", description="Review", argumentHint="[pr]")], current_permission_mode=mode))
            elif r["subtype"] == "set_permission_mode":
                mode = r["mode"]
                ok(v["request_id"])
            elif r["subtype"] == "set_model" and r["model"] == "refuse":
                send(type="control_response", response=dict(subtype="error", request_id=v["request_id"], error="Model refused"))
            elif r["subtype"] == "stop_task":
                # Like Claude Code, the task's own notification can beat the response.
                send(type="system", subtype="task_notification", task_id=r["task_id"], tool_use_id="tool-b", status="stopped", summary="Build stopped", output_file="")
                ok(v["request_id"])
            elif r["subtype"] == "interrupt":
                ok(v["request_id"])
                raise KeyboardInterrupt
            else:
                ok(v["request_id"])
            return True
        if v["type"] == "user":
            content = v["message"]["content"]
            prompt = content[0]["text"]
            # Like Claude Code, persist the conversation once it has a prompt.
            project_dir(os.getcwd()).mkdir(parents=True, exist_ok=True)
            with open(project_dir(os.getcwd()) / f"{session}.jsonl", "a") as transcript:
                transcript.write(json.dumps(dict(type="user", uuid=f"u-{time.time_ns()}", parentUuid=None, sessionId=session, cwd=os.getcwd(), isSidechain=False, message=v["message"])) + "\n")
            if prompt == "exit":
                sys.exit(0)
            if states:
                send(type="system", subtype="session_state_changed", state="running")
            try:
                turn(prompt, content)
            except KeyboardInterrupt:
                send(type="control_cancel_request", request_id="perm")
                result("error_during_execution", True, output=0, errors=["interrupted"])
            if states:
                send(type="system", subtype="session_state_changed", state="idle")
            if prompt == "crash-when-idle":
                sys.exit(3)
        return True

    def turn(prompt, content):
        if prompt == "hello":
            text("msg-1", "hello world")
            result()
        elif prompt == "args":
            text("msg-args", json.dumps(dict(args=args, cwd=os.getcwd(), content=content, entrypoint=os.environ.get("CLAUDE_CODE_ENTRYPOINT"))))
            result()
        elif prompt == "bash":
            tool("Bash", "tool-1", dict(command="echo hi", description="Say hi"))
            answer = ask("perm", subtype="can_use_tool", tool_name="Bash", input=dict(command="echo hi"), tool_use_id="tool-1", permission_suggestions=[dict(type="addRules", rules=[dict(toolName="Bash", ruleContent="echo:*")], behavior="allow", destination="localSettings")])
            send(type="user", parent_tool_use_id=None, message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-1", content="hi")]), tool_use_result=dict(stdout="hi", stderr="", interrupted=False))
            text("msg-2", json.dumps(answer))
            result()
        elif prompt == "question":
            questions = [dict(question="Which color?", header="Color", options=[dict(label="Blue"), dict(label="Red")], multiSelect=False)]
            tool("AskUserQuestion", "tool-q", dict(questions=questions))
            answer = ask("perm", subtype="can_use_tool", tool_name="AskUserQuestion", input=dict(questions=questions), tool_use_id="tool-q")
            text("msg-3", json.dumps(answer))
            result()
        elif prompt == "plan":
            tool("ExitPlanMode", "tool-p", dict(plan="Do it"))
            answer = ask("perm", subtype="can_use_tool", tool_name="ExitPlanMode", input=dict(plan="Do it"), tool_use_id="tool-p")
            send(type="system", subtype="status", status=None, permissionMode="acceptEdits")
            text("msg-4", json.dumps(answer))
            result()
        elif prompt == "wait":
            tool("Bash", "tool-w", dict(command="sleep 100"))
            ask("perm", subtype="can_use_tool", tool_name="Bash", input=dict(command="sleep 100"), tool_use_id="tool-w")
        elif prompt == "limit":
            send(type="rate_limit_event", rate_limit_info=dict(status="rejected", resetsAt=1800000000, rateLimitType="five_hour"))
            send(type="assistant", parent_tool_use_id=None, error="rate_limit", message=dict(id="msg-limit", model="<synthetic>", content=[dict(type="text", text="You've hit your limit")]))
            result(is_error=True, value="You've hit your limit", output=0)
        elif prompt == "/usage":
            send(type="assistant", parent_tool_use_id=None, message=dict(id="msg-usage", model="<synthetic>", content=[dict(type="text", text="Current session: 23% used")]))
            result(value="Current session: 23% used", output=0)
        elif prompt == "tasks":
            tool("TaskCreate", "tool-t", dict(subject="Write tests", activeForm="Writing tests"))
            send(type="user", parent_tool_use_id=None, message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-t", content="Task #1 created successfully: Write tests")]))
            tool("TaskUpdate", "tool-u", dict(taskId="1", status="in_progress"))
            send(type="user", parent_tool_use_id=None, message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-u", content="Updated task #1 status")]))
            result()
        elif prompt == "agent":
            # A background subagent: the first result ends Claude's own turn, then the
            # subagent works, then Claude continues by itself once it is notified.
            tool("Agent", "tool-a", dict(description="Look around", prompt="List the files", subagent_type="general-purpose"))
            send(type="system", subtype="task_started", task_id="t1", tool_use_id="tool-a", is_backgrounded=True, description="Look around", prompt="List the files", subagent_type="general-purpose")
            send(type="user", parent_tool_use_id=None, message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-a", content=[dict(type="text", text="Async agent launched successfully")])]))
            text("msg-a1", "Waiting for the agent")
            result()
            send(type="assistant", parent_tool_use_id="tool-a", message=dict(id="sub-1", model="claude-haiku-test", content=[dict(type="text", text="Found 3 files")]))
            send(type="assistant", parent_tool_use_id="tool-a", message=dict(id="sub-2", model="claude-haiku-test", content=[dict(type="tool_use", id="tool-s", name="Bash", input=dict(command="ls"))]))
            answer = ask("perm", subtype="can_use_tool", tool_name="Bash", input=dict(command="ls"), tool_use_id="tool-s", agent_id="t1")
            send(type="user", parent_tool_use_id="tool-a", message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-s", content="a b c")]))
            send(type="assistant", parent_tool_use_id="tool-a", message=dict(id="sub-3", model="claude-haiku-test", content=[dict(type="text", text=json.dumps(answer["behavior"]))]))
            send(type="system", subtype="task_notification", task_id="t1", tool_use_id="tool-a", status="completed", summary="Found 3 files")
            text("msg-a2", "done")
            result()
        elif prompt == "background":
            # A backgrounded command: Claude names its task, then the result names it too.
            tool("Bash", "tool-b", dict(command="make", description="Build", run_in_background=True))
            send(type="system", subtype="task_started", task_id="b1", tool_use_id="tool-b", task_type="local_bash", is_backgrounded=True, description="Build")
            send(type="user", parent_tool_use_id=None, message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-b", content="Command running in background with ID: b1. Output is being written to: /tmp/x y/tasks/b1.output")]), tool_use_result=dict(stdout="", stderr="", interrupted=False, backgroundTaskId="b1"))
            send(type="system", subtype="task_progress", task_id="b1", description="Build", summary="Compiling", usage=dict(total_tokens=5, tool_uses=1, duration_ms=9))
            # Monitor tasks only feed the model.
            tool("Monitor", "tool-m", dict(command="tail -f log"))
            send(type="system", subtype="task_started", task_id="m1", tool_use_id="tool-m", task_type="local_bash", is_backgrounded=True, description="Watch")
            # A foreground command that goes to the background later.
            tool("Bash", "tool-f", dict(command="sleep 9"))
            send(type="system", subtype="task_started", task_id="f1", tool_use_id="tool-f", task_type="local_bash", is_backgrounded=False, description="Sleep")
            send(type="system", subtype="task_updated", task_id="f1", patch=dict(is_backgrounded=True))
            send(type="user", parent_tool_use_id=None, message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-f", content="Command was manually backgrounded with ID: f1")]), tool_use_result=dict(stdout="", stderr="", interrupted=False, backgroundTaskId="f1"))
            # A foreground command that just finishes is no background job.
            tool("Bash", "tool-g", dict(command="true"))
            send(type="system", subtype="task_started", task_id="g1", tool_use_id="tool-g", task_type="local_bash", is_backgrounded=False, description="True")
            send(type="system", subtype="task_notification", task_id="g1", tool_use_id="tool-g", status="completed", summary="", output_file="")
            send(type="user", parent_tool_use_id=None, message=dict(role="user", content=[dict(type="tool_result", tool_use_id="tool-g", content="")]), tool_use_result=dict(stdout="", stderr="", interrupted=False))
            text("msg-bg", "Started")
            result()
        elif prompt == "later":
            # The level drops f1 before its own terminal event, which then corrects it.
            send(type="system", subtype="background_tasks_changed", tasks=[dict(task_id="b1", task_type="local_bash")])
            send(type="system", subtype="task_notification", task_id="f1", tool_use_id="tool-f", status="completed", summary="Slept", output_file="/tmp/tasks/f1.output")
            send(type="system", subtype="task_started", task_id="w1", task_type="local_workflow", workflow_name="spec", description="Spec workflow")
            result()
        elif prompt == "unfinished":
            tool("Read", "tool-r", dict(file_path=os.getcwd() + "/a.txt"))
            result()
        else:
            text("msg-x", "unknown")
            result()

    while serve(lines.get()):
        pass


def entry(kind, uuid, parent, session, message, cwd="/tmp", **extra):
    return dict(type=kind, uuid=uuid, parentUuid=parent, sessionId=session, cwd=cwd, isSidechain=False, timestamp="2026-10-22T10:00:00.000Z", message=message, **extra)


def transcript(session="11111111-1111-4111-8111-111111111111", cwd="/tmp"):
    return [
        entry("user", "u1", None, session, dict(role="user", content="old prompt"), cwd),
        entry("assistant", "a1", "u1", session, dict(id="msg_old", role="assistant", model="claude-opus-test", content=[dict(type="text", text="old answer")]), cwd),
        entry("assistant", "a2", "a1", session, dict(id="msg_old", role="assistant", model="claude-opus-test", content=[dict(type="tool_use", id="toolu_1", name="Read", input=dict(file_path="/tmp/x"))]), cwd),
        entry("user", "u2", "a2", session, dict(role="user", content=[dict(type="tool_result", tool_use_id="toolu_1", content="file text")]), cwd),
        entry("user", "u3", "u2", session, dict(role="user", content="<command-name>/clear</command-name>"), cwd, isMeta=True),
        entry("user", "u4", "u2", session, dict(role="user", content="new prompt"), cwd),
        entry("assistant", "a3", "u4", session, dict(id="msg_new", role="assistant", model="claude-opus-test", content=[dict(type="text", text="new answer")]), cwd),
        dict(type="custom-title", customTitle="Saved chat", sessionId=session),
    ]


def subagent_history(config, cwd, session="33333333-3333-4333-8333-333333333333"):
    """A saved conversation whose background subagent started a nested one."""
    project = Path(config) / "projects" / re.sub(r"[^a-zA-Z0-9]", "-", cwd)
    agents = project / session / "subagents"
    agents.mkdir(parents=True, exist_ok=True)
    def write(path, rows):
        path.write_text("".join(json.dumps(r) + "\n" for r in rows))
    def side(kind, uuid, parent, message):
        return dict(entry(kind, uuid, parent, session, message, cwd), isSidechain=True)
    write(project / f"{session}.jsonl", [
        entry("user", "m1", None, session, dict(role="user", content="go"), cwd),
        entry("assistant", "m2", "m1", session, dict(id="msg_root", role="assistant", content=[dict(type="tool_use", id="toolu_A", name="Agent", input=dict(description="Look", prompt="Inspect the code"))]), cwd),
        entry("user", "m3", "m2", session, dict(role="user", content=[dict(type="tool_result", tool_use_id="toolu_A", content=[dict(type="text", text="Async agent launched successfully")])]), cwd),
        entry("assistant", "m4", "m3", session, dict(id="msg_done", role="assistant", content=[dict(type="text", text="all done")]), cwd),
    ])
    (agents / "agent-aaa.meta.json").write_text(json.dumps(dict(toolUseId="toolu_A", description="Look", agentType="general-purpose")))
    write(agents / "agent-aaa.jsonl", [
        side("user", "c1", None, dict(role="user", content="Inspect the code")),
        side("assistant", "c2", "c1", dict(id="msg_c2", role="assistant", content=[dict(type="text", text="Child says hi")])),
        side("assistant", "c3", "c2", dict(id="msg_c3", role="assistant", content=[dict(type="tool_use", id="toolu_B", name="Agent", input=dict(description="Nested", prompt="Go deeper"))])),
        side("user", "c4", "c3", dict(role="user", content=[dict(type="tool_result", tool_use_id="toolu_B", content="nested done")])),
        side("assistant", "c5", "c4", dict(id="msg_c5", role="assistant", content=[dict(type="tool_use", id="toolu_R", name="Read", input=dict(file_path=cwd + "/a.txt"))])),
        side("user", "c6", "c5", dict(role="user", content=[dict(type="tool_result", tool_use_id="toolu_R", content="file text")])),
    ])
    (agents / "agent-bbb.meta.json").write_text(json.dumps(dict(toolUseId="toolu_B", description="Nested", agentType="Explore")))
    write(agents / "agent-bbb.jsonl", [
        side("user", "n1", None, dict(role="user", content="Go deeper")),
        side("assistant", "n2", "n1", dict(id="msg_n2", role="assistant", content=[dict(type="text", text="Nested says hi")])),
    ])
    return session


class BridgeTests(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parents[1]
        binary = os.environ.get("CLAUDE_ACP_TEST_BIN", str(root / "target/debug/agentworks-claude-acp"))
        self.config = tempfile.TemporaryDirectory()
        self.addCleanup(self.config.cleanup)
        self.cwd = tempfile.mkdtemp(dir=self.config.name)
        env = dict(os.environ, CLAUDE_CODE_EXECUTABLE=str(Path(__file__).resolve()), CLAUDE_CONFIG_DIR=self.config.name, CLAUDECODE="1")
        env.pop("ANTHROPIC_MODEL", None)
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
        init = self.call("initialize", dict(protocolVersion=1, clientCapabilities=dict(elicitation=dict(form={}), _meta=dict(terminal_output=True, terminal_output_delta=True))))
        caps = init["agentCapabilities"]
        self.assertTrue(caps["loadSession"])
        self.assertTrue(all(k in caps["sessionCapabilities"] for k in ["resume", "fork", "list", "close", "delete"]))

    def close(self):
        try:
            self.process.stdin.close()
        except BrokenPipeError:  # the bridge already exited
            pass
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

    def response(self, request_id):
        return self.until(lambda v: v.get("id") == request_id and "method" not in v)

    def call(self, method, params):
        response = self.response(self.send(method, params))
        self.assertNotIn("error", response, response)
        return response["result"]

    def session(self, **extra):
        result = self.call("session/new", dict(cwd=self.cwd, mcpServers=[dict(name="agentworks", command="agentworks", args=["mcp"], env=[dict(name="TOKEN", value="test")])], **extra))
        self.id = result["sessionId"]
        return result

    def prompt(self, text, **extra):
        return self.call("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text=text)], **extra))

    def updates(self, kind=None):
        items = [v["params"]["update"] for v in self.seen if v.get("method") == "session/update"]
        return [u for u in items if kind is None or u["sessionUpdate"] == kind]

    def answer_text(self):
        return "".join(u["content"]["text"] for u in self.updates("agent_message_chunk"))

    def write_transcript(self, rows, session="11111111-1111-4111-8111-111111111111", cwd="/tmp"):
        directory = Path(self.config.name) / "projects" / re.sub(r"[^a-zA-Z0-9]", "-", cwd)
        directory.mkdir(parents=True, exist_ok=True)
        (directory / f"{session}.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
        return directory / f"{session}.jsonl"

    def test_streaming_usage_and_commands(self):
        options = self.session()["configOptions"]
        self.assertEqual([o["id"] for o in options], ["mode", "model", "effort"])
        self.assertEqual(options[1]["currentValue"], "default")
        commands = self.updates("available_commands_update")[0]["availableCommands"]
        self.assertEqual([c["name"] for c in commands], ["usage", "review"])
        self.assertIsNone(commands[0]["input"])  # required for Agentworks' /usage limit probe
        self.assertEqual(self.prompt("hello")["stopReason"], "end_turn")
        chunks = self.updates("agent_message_chunk")
        self.assertEqual([c["content"]["text"] for c in chunks], ["hel", "lo world"])  # no duplicate from the final message
        self.assertTrue(all(c["messageId"] == "msg-1" for c in chunks))
        usage = self.updates("usage_update")
        self.assertEqual([u["used"] for u in usage], [101, 120, 120])
        self.assertEqual(usage[-1]["size"], 1000)
        self.assertEqual(usage[-1]["cost"], dict(amount=0.5, currency="USD"))
        self.assertEqual(usage[-1]["_meta"]["_claude/model"], "claude-opus-test")

    def test_launch_arguments_and_prompt_content(self):
        self.session(additionalDirectories=["/tmp/extra"])
        self.call("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="args"), dict(type="image", mimeType="image/png", data="iVBO"), dict(type="resource", resource=dict(uri="agentworks://notes", text="# Notes"))]))
        seen = json.loads(self.answer_text())
        args = seen["args"]
        self.assertEqual(args[args.index("--session-id") + 1], self.id)
        self.assertEqual(args[args.index("--add-dir") + 1], "/tmp/extra")
        self.assertEqual(json.loads(args[args.index("--mcp-config") + 1])["mcpServers"]["agentworks"], dict(type="stdio", command="agentworks", args=["mcp"], env=dict(TOKEN="test")))
        self.assertIn("stdio", args[args.index("--permission-prompt-tool") + 1])
        self.assertNotIn("--disallowedTools", args)  # forms are supported, so AskUserQuestion stays
        self.assertEqual(seen["cwd"], self.cwd)
        self.assertEqual(seen["content"][1], dict(type="image", source=dict(type="base64", data="iVBO", media_type="image/png")))
        self.assertEqual(seen["content"][2], dict(type="text", text="agentworks://notes"))
        self.assertIn('<context ref="agentworks://notes">\n# Notes\n</context>', seen["content"][3]["text"])

    def test_system_prompt_reaches_the_cli(self):
        caps = self.call("initialize", dict(protocolVersion=1, clientCapabilities={}))["agentCapabilities"]["sessionCapabilities"]
        self.assertEqual(caps["systemPrompt"], dict(mode=dict(override={})))
        content = [dict(type="text", text="Shared context"), dict(type="text", text="Role: reviewer")]
        self.session(systemPrompt=dict(mode="append", content=content))
        self.prompt("args")
        args = json.loads(self.answer_text())["args"]
        self.assertEqual(args[args.index("--append-system-prompt") + 1], "Shared context\n\nRole: reviewer")
        self.assertNotIn("--system-prompt", args)
        self.session(systemPrompt=dict(mode="override", content=[dict(type="text", text="Only this")]))
        start = len(self.seen)
        self.prompt("args")
        text = "".join(v["params"]["update"]["content"]["text"] for v in self.seen[start:] if v.get("method") == "session/update" and v["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        args = json.loads(text)["args"]
        self.assertEqual(args[args.index("--system-prompt") + 1], "Only this")
        bad = self.response(self.send("session/new", dict(cwd=self.cwd, mcpServers=[], systemPrompt=dict(mode="replace", content=[]))))
        self.assertIn("error", bad)

    def test_permission_and_terminal_output(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="bash")]))
        request = self.until(lambda v: v.get("method") == "session/request_permission")
        options = request["params"]["options"]
        self.assertEqual([o["optionId"] for o in options], ["allow-once", "allow-with-updates", "reject"])
        self.assertEqual(options[1]["name"], "Yes, and allow Bash(echo:*) from now on")
        self.assertEqual(request["params"]["toolCall"]["kind"], "execute")
        self.write(dict(jsonrpc="2.0", id=request["id"], result=dict(outcome=dict(outcome="selected", optionId="allow-with-updates"))))
        self.assertEqual(self.response(request_id)["result"]["stopReason"], "end_turn")
        answer = json.loads(self.answer_text())
        self.assertEqual(answer["behavior"], "allow")
        self.assertEqual(answer["updatedPermissions"][0]["rules"][0]["ruleContent"], "echo:*")
        call = self.updates("tool_call")[0]
        self.assertEqual(call["title"], "echo hi")
        self.assertEqual(call["_meta"]["terminal_info"], dict(terminal_id="tool-1"))
        delta, done = self.updates("tool_call_update")
        self.assertEqual(delta["_meta"]["terminal_output_delta"], dict(terminal_id="tool-1", data="hi"))
        self.assertEqual(done["status"], "completed")
        self.assertEqual(done["_meta"]["terminal_exit"]["exit_code"], 0)

    def test_unoffered_permission_choice_is_denied(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="bash")]))
        request = self.until(lambda v: v.get("method") == "session/request_permission")
        self.write(dict(jsonrpc="2.0", id=request["id"], result=dict(outcome=dict(outcome="selected", optionId="exit-plan-auto"))))
        self.response(request_id)
        self.assertEqual(json.loads(self.answer_text())["behavior"], "deny")

    def test_ask_user_question_uses_form_elicitation(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="question")]))
        request = self.until(lambda v: v.get("method") == "elicitation/create")
        schema = request["params"]["requestedSchema"]["properties"]
        self.assertEqual(schema["question_0"]["oneOf"][0]["const"], "Blue")
        self.assertEqual(request["params"]["message"], "Which color?")
        self.write(dict(jsonrpc="2.0", id=request["id"], result=dict(action="accept", content=dict(question_0="Red", question_0_custom="darker"))))
        self.assertEqual(self.response(request_id)["result"]["stopReason"], "end_turn")
        answer = json.loads(self.answer_text())
        self.assertEqual(answer["updatedInput"]["answers"], {"Which color?": "Red"})
        self.assertEqual(answer["updatedInput"]["annotations"], {"Which color?": dict(notes="darker")})

    def test_exit_plan_mode_sets_mode(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="plan")]))
        request = self.until(lambda v: v.get("method") == "session/request_permission")
        self.assertEqual(request["params"]["options"][0]["optionId"], "exit-plan-auto")
        self.write(dict(jsonrpc="2.0", id=request["id"], result=dict(outcome=dict(outcome="selected", optionId="exit-plan-accept-edits"))))
        self.response(request_id)
        answer = json.loads(self.answer_text())
        self.assertEqual(answer["updatedPermissions"], [dict(type="setMode", mode="acceptEdits", destination="session")])
        self.assertEqual([u["currentModeId"] for u in self.updates("current_mode_update")], ["acceptEdits"])

    def test_cancel_withdraws_pending_permission_and_continues(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="wait")]))
        self.until(lambda v: v.get("method") == "session/request_permission")
        self.send("session/cancel", dict(sessionId=self.id), request=False)
        self.assertEqual(self.response(request_id)["result"]["stopReason"], "cancelled")
        self.assertTrue(any(v.get("method") == "$/cancel_request" for v in self.seen))
        failed = [u for u in self.updates("tool_call_update") if u["toolCallId"] == "tool-w"]
        self.assertEqual(failed[-1]["status"], "failed")
        # An idle cancel must not interrupt the next turn.
        self.send("session/cancel", dict(sessionId=self.id), request=False)
        self.assertEqual(self.prompt("hello")["stopReason"], "end_turn")

    def test_rate_limit_metadata_and_error_kind(self):
        self.session()
        response = self.response(self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="limit")])))
        self.assertEqual(response["error"]["data"]["errorKind"], "rate_limit")
        limit = self.updates("usage_update")[0]["_meta"]["_claude/rateLimit"]
        self.assertEqual(limit, dict(status="rejected", resetsAt=1800000000, rateLimitType="five_hour"))

    def test_local_command_output_is_shown(self):
        self.session()
        self.prompt("/usage")
        self.assertEqual(self.answer_text(), "Current session: 23% used")

    def test_tasks_become_plan_and_unfinished_tools_close(self):
        self.session()
        self.prompt("tasks")
        plans = self.updates("plan")
        self.assertEqual(plans[-1]["entries"], [dict(content="Writing tests", status="in_progress", priority="medium")])
        self.prompt("unfinished")
        read = self.updates("tool_call")[-1]
        self.assertEqual((read["title"], read["kind"]), ("Read a.txt", "read"))
        self.assertEqual(self.updates("tool_call_update")[-1]["status"], "failed")

    def test_config_options_reach_the_cli(self):
        self.session()
        options = self.call("session/set_config_option", dict(sessionId=self.id, configId="effort", value="high"))["configOptions"]
        self.assertEqual(options[2]["currentValue"], "high")
        options = self.call("session/set_config_option", dict(sessionId=self.id, configId="mode", value="auto"))["configOptions"]
        self.assertEqual(options[0]["currentValue"], "auto")
        # Haiku lacks effort and auto mode: the effort resets and the mode falls back.
        options = self.call("session/set_config_option", dict(sessionId=self.id, configId="model", value="haiku"))["configOptions"]
        self.assertEqual([o["id"] for o in options], ["mode", "model"])
        self.assertEqual(options[0]["currentValue"], "acceptEdits")
        self.call("session/set_mode", dict(sessionId=self.id, modeId="plan"))
        self.assertEqual(self.updates("current_mode_update")[-1]["currentModeId"], "plan")
        response = self.response(self.send("session/set_config_option", dict(sessionId=self.id, configId="model", value="nope")))
        self.assertIn("error", response)

    def test_resume_missing_transcript_is_not_found(self):
        response = self.response(self.send("session/resume", dict(sessionId="22222222-2222-4222-8222-222222222222", cwd=self.cwd, mcpServers=[])))
        self.assertEqual(response["error"]["code"], -32002)

    def test_load_replays_the_active_branch(self):
        self.write_transcript(transcript(cwd=self.cwd), cwd=self.cwd)
        self.call("session/load", dict(sessionId="11111111-1111-4111-8111-111111111111", cwd=self.cwd, mcpServers=[]))
        self.id = "11111111-1111-4111-8111-111111111111"
        kinds = [(u["sessionUpdate"], u.get("content", {}).get("text") if isinstance(u.get("content"), dict) else u.get("toolCallId")) for u in self.updates()]
        self.assertEqual(kinds[:6], [("user_message_chunk", "old prompt"), ("agent_message_chunk", "old answer"), ("tool_call", "toolu_1"), ("tool_call_update", "toolu_1"), ("user_message_chunk", "new prompt"), ("agent_message_chunk", "new answer")])
        self.assertEqual(self.updates("agent_message_chunk")[0]["messageId"], "msg_old")
        self.assertEqual(self.updates("available_commands_update")[0]["sessionUpdate"], "available_commands_update")
        args = None
        self.prompt("args")
        args = json.loads(self.answer_text().split("new answer", 1)[1])["args"]
        self.assertEqual(args[args.index("--resume") + 1], self.id)

    def test_list_close_delete(self):
        path = self.write_transcript(transcript(cwd=self.cwd), cwd=self.cwd)
        listed = self.call("session/list", dict(cwd=self.cwd))["sessions"]
        self.assertEqual([(s["sessionId"], s["title"], s["cwd"]) for s in listed], [("11111111-1111-4111-8111-111111111111", "Saved chat", self.cwd)])
        self.assertRegex(listed[0]["updatedAt"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        self.assertEqual(self.call("session/list", dict(cwd="/elsewhere"))["sessions"], [])
        self.call("session/resume", dict(sessionId="11111111-1111-4111-8111-111111111111", cwd=self.cwd, mcpServers=[]))
        self.call("session/close", dict(sessionId="11111111-1111-4111-8111-111111111111"))
        self.assertTrue(path.exists(), "Close must keep the transcript")
        self.call("session/delete", dict(sessionId="11111111-1111-4111-8111-111111111111"))
        self.assertFalse(path.exists())
        self.call("session/delete", dict(sessionId="11111111-1111-4111-8111-111111111111"))  # idempotent

    def test_fork_at_boundary_writes_a_resumable_transcript(self):
        self.write_transcript(transcript())
        fork = dict(sessionId="11111111-1111-4111-8111-111111111111", cwd=self.cwd, mcpServers=[], _meta=dict(jetbrains=dict(air=dict(fork=dict(version=1, messageId="msg_old:segment:2")))))
        child = self.call("session/fork", fork)["sessionId"]
        self.assertNotEqual(child, fork["sessionId"])
        rows = [json.loads(l) for l in (Path(self.config.name) / "projects" / re.sub(r"[^a-zA-Z0-9]", "-", self.cwd) / f"{child}.jsonl").read_text().splitlines()]
        self.assertEqual([r["forkedFrom"]["messageUuid"] for r in rows], ["u1", "a1", "a2"])  # whole boundary message, nothing newer
        self.assertTrue(all(r["sessionId"] == child for r in rows))
        self.assertEqual(rows[1]["parentUuid"], rows[0]["uuid"])
        self.assertNotIn(rows[0]["uuid"], ["u1"])
        fork["_meta"]["jetbrains"]["air"]["fork"]["messageId"] = "missing"
        self.assertIn("error", self.response(self.send("session/fork", fork)))

    def test_background_subagent_holds_the_turn_and_fills_its_card(self):
        self.session()
        request_id = self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="agent")]))
        # A subagent permission arrives after Claude's first result; the prompt is still open.
        request = self.until(lambda v: v.get("method") == "session/request_permission" or (v.get("id") == request_id and "method" not in v))
        self.assertEqual(request.get("method"), "session/request_permission")
        self.write(dict(jsonrpc="2.0", id=request["id"], result=dict(outcome=dict(outcome="selected", optionId="allow-once"))))
        self.assertEqual(self.response(request_id)["result"]["stopReason"], "end_turn")
        self.assertEqual(self.answer_text(), "Waiting for the agentdone")  # subagent text stays out of the transcript
        card = [u for u in self.updates("tool_call_update") if u["toolCallId"] == "tool-a"]
        self.assertEqual([u["status"] for u in card], ["in_progress", "in_progress", "in_progress", "completed"])
        self.assertEqual([c["content"]["text"] for c in card[-1]["content"]], ["List the files", 'Found 3 files\n\n"allow"'])
        nested = next(u for u in self.updates("tool_call") if u["toolCallId"] == "tool-s")
        self.assertEqual(nested["_meta"]["claudeCode"]["parentToolUseId"], "tool-a")
        result = [u for u in self.updates("tool_call_update") if u["toolCallId"] == "tool-s"]
        self.assertTrue(all(u["_meta"]["claudeCode"]["parentToolUseId"] == "tool-a" for u in result))
        self.assertEqual(self.prompt("hello")["stopReason"], "end_turn")

    def test_native_subagent_sessions_when_the_client_supports_them(self):
        self.call("initialize", dict(protocolVersion=1, clientCapabilities=dict(subagents={}, elicitation=dict(form={}))))
        self.session()
        request_id = self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="agent")]))
        request = self.until(lambda v: v.get("method") == "session/request_permission")
        self.assertEqual(request["params"]["sessionId"], "t1")  # the child's approval
        self.write(dict(jsonrpc="2.0", id=request["id"], result=dict(outcome=dict(outcome="selected", optionId="allow-once"))))
        self.assertEqual(self.response(request_id)["result"]["stopReason"], "end_turn")
        notes = [v["params"] for v in self.seen if v.get("method") == "session/update"]
        root = [n["update"] for n in notes if n["sessionId"] == self.id]
        child = [n["update"] for n in notes if n["sessionId"] == "t1"]
        lifecycle = [u for u in root if u["sessionUpdate"] == "subagent_update"]
        self.assertEqual(lifecycle[0], dict(sessionUpdate="subagent_update", sessionId="t1", title="Look around", description="List the files", capabilities={}, state=dict(state="running")))
        self.assertEqual(lifecycle[-1]["state"], dict(state="idle", stopReason="end_turn"))
        announced = notes.index(next(n for n in notes if n["update"] is lifecycle[0]))
        self.assertTrue(all(notes.index(n) > announced for n in notes if n["sessionId"] == "t1"))  # announced first
        self.assertEqual([u["content"]["text"] for u in child if u["sessionUpdate"] == "agent_message_chunk"], ["Found 3 files", '"allow"'])
        self.assertEqual([(u["sessionUpdate"], u["toolCallId"]) for u in child if u["sessionUpdate"].startswith("tool_call")], [("tool_call", "tool-s"), ("tool_call_update", "tool-s")])
        self.assertEqual("".join(u["content"]["text"] for u in root if u["sessionUpdate"] == "agent_message_chunk"), "Waiting for the agentdone")
        self.assertEqual([u["status"] for u in root if u.get("toolCallId") == "tool-a"][-1], "completed")

    def test_load_replays_subagents_into_child_sessions(self):
        self.call("initialize", dict(protocolVersion=1, clientCapabilities=dict(subagents={}, elicitation=dict(form={}))))
        self.id = subagent_history(self.config.name, self.cwd)
        self.call("session/load", dict(sessionId=self.id, cwd=self.cwd, mcpServers=[]))
        notes = [(v["params"]["sessionId"], v["params"]["update"]) for v in self.seen if v.get("method") == "session/update"]
        on = lambda sid, kind: [u for s, u in notes if s == sid and u["sessionUpdate"] == kind]
        text = lambda sid: [u["content"]["text"] for u in on(sid, "agent_message_chunk")]
        self.assertEqual([(u["sessionId"], u["title"], u["description"]) for u in on(self.id, "subagent_update") if "title" in u], [("aaa", "Look", "Inspect the code")])
        self.assertEqual([(u["sessionId"], u["title"]) for u in on("aaa", "subagent_update") if "title" in u], [("bbb", "Nested")])  # nested under its parent
        self.assertEqual(text("aaa"), ["Child says hi"])
        self.assertEqual(text("bbb"), ["Nested says hi"])
        self.assertEqual(text(self.id), ["all done"])
        self.assertEqual([u["toolCallId"] for u in on("aaa", "tool_call")], ["toolu_B", "toolu_R"])
        self.assertEqual([u["status"] for u in on("aaa", "tool_call_update") if u["toolCallId"] == "toolu_R"], ["completed"])
        self.assertEqual([u["content"]["text"] for s, u in notes if u["sessionUpdate"] == "user_message_chunk"], ["go"])  # prompts stay descriptions
        ended = [(s, u["sessionId"]) for s, u in notes if u["sessionUpdate"] == "subagent_update" and u.get("state", {}).get("state") == "idle"]
        self.assertEqual(sorted(ended), sorted([("aaa", "bbb"), (self.id, "aaa")]))
        first = {sid: next(i for i, (s, _) in enumerate(notes) if s == sid) for sid in ("aaa", "bbb")}
        announced = {u["sessionId"]: i for i, (s, u) in enumerate(notes) if u["sessionUpdate"] == "subagent_update" and "title" in u}
        self.assertTrue(all(announced[c] < first[c] for c in first))  # announced before their traffic
        self.assertEqual(self.prompt("hello")["stopReason"], "end_turn")

    def test_load_without_subagent_support_keeps_the_root_transcript(self):
        self.id = subagent_history(self.config.name, self.cwd)
        self.call("session/load", dict(sessionId=self.id, cwd=self.cwd, mcpServers=[]))
        notes = [v["params"] for v in self.seen if v.get("method") == "session/update"]
        self.assertTrue(all(n["sessionId"] == self.id for n in notes))
        self.assertFalse(any(n["update"]["sessionUpdate"] == "subagent_update" for n in notes))
        self.assertEqual(self.answer_text(), "all done")

    def test_background_jobs_and_individual_stop(self):
        air = dict(version=1, capabilities=["asyncTasks"])
        init = self.call("initialize", dict(protocolVersion=1, clientCapabilities=dict(_meta=dict(terminal_output=True, jetbrains=dict(air=air)))))
        self.assertEqual(init["_meta"]["jetbrains"]["air"], air)
        self.session()
        self.assertEqual(self.prompt("background")["stopReason"], "end_turn")
        jobs = lambda: [u for u in self.updates() if u["sessionUpdate"].startswith("async_task")]
        self.assertEqual(jobs(), [
            dict(sessionUpdate="async_task_spawned", asyncTaskId="b1", name="Build", taskType="shell", description="Build", showInTranscript=True, canStop=True, toolCallId="tool-b"),
            dict(sessionUpdate="async_task_progress", asyncTaskId="b1", outputFilePath="/tmp/x y/tasks/b1.output"),
            dict(sessionUpdate="async_task_progress", asyncTaskId="b1", description="Build", summary="Compiling", usage=dict(totalTokens=5, toolUses=1, durationMs=9)),
            dict(sessionUpdate="async_task_spawned", asyncTaskId="f1", name="Sleep", taskType="shell", description="Sleep", showInTranscript=True, canStop=True, toolCallId="tool-f"),
        ])  # Monitor and foreground tasks stay out
        launches = {u["toolCallId"]: u for u in self.updates("tool_call_update") if "status" in u}
        self.assertEqual(launches["tool-b"]["_meta"]["jetbrains"]["air"]["asyncTasks"], dict(backgrounded=True))
        self.assertNotIn("jetbrains", launches["tool-g"]["_meta"])
        delta = next(u for u in self.updates("tool_call_update") if u["toolCallId"] == "tool-b" and "terminal_output_delta" in u["_meta"])
        self.assertIn("running in background with ID: b1", delta["_meta"]["terminal_output_delta"]["data"])
        start = len(jobs())
        self.prompt("later")
        self.assertEqual(jobs()[start:], [
            dict(sessionUpdate="async_task_state_update", asyncTaskId="f1", state="stopped"),
            dict(sessionUpdate="async_task_state_update", asyncTaskId="f1", state="completed", summary="Slept", outputFilePath="/tmp/tasks/f1.output"),
            dict(sessionUpdate="async_task_spawned", asyncTaskId="w1", name="spec", taskType="workflow", description="spec", showInTranscript=True, canStop=True),
        ])
        stop = lambda task: self.call("_session/async_task/stop", dict(sessionId=self.id, asyncTaskId=task))
        start = len(jobs())
        self.assertEqual(stop("b1"), dict(stopped=True))
        self.assertEqual(stop("b1"), dict(stopped=False))  # already ended
        self.assertEqual(stop("m1"), dict(stopped=False))  # never shown
        self.assertEqual(stop("f1"), dict(stopped=False))
        self.assertEqual(jobs()[start:], [dict(sessionUpdate="async_task_state_update", asyncTaskId="b1", state="stopped", summary="Build stopped")])
        self.call("session/close", dict(sessionId=self.id))
        self.assertEqual(jobs()[-1], dict(sessionUpdate="async_task_state_update", asyncTaskId="w1", state="stopped"))

    def test_background_jobs_need_the_client_capability(self):
        self.assertNotIn("_meta", self.call("initialize", dict(protocolVersion=1, clientCapabilities={})))
        self.session()
        self.prompt("background")
        self.assertFalse(any(u["sessionUpdate"].startswith("async_task") for u in self.updates()))
        self.assertFalse(any("jetbrains" in u.get("_meta", {}) for u in self.updates("tool_call_update")))
        self.assertEqual(self.call("_session/async_task/stop", dict(sessionId=self.id, asyncTaskId="b1")), dict(stopped=False))

    def test_older_cli_without_state_events_ends_on_result(self):
        Path(self.cwd, "no-state-events").touch()
        self.session()
        self.assertEqual(self.prompt("hello")["stopReason"], "end_turn")
        self.assertEqual(self.prompt("/usage")["stopReason"], "end_turn")

    def exited(self):
        """The bridge ends after a CLI crash, reporting it on stderr."""
        self.assertEqual(self.process.wait(timeout=5), 1)
        return self.process.stderr.read()

    def test_cli_exit_fails_the_turn_and_ends_the_bridge(self):
        self.session()
        response = self.response(self.send("session/prompt", dict(sessionId=self.id, prompt=[dict(type="text", text="exit")])))
        self.assertEqual(response["error"]["data"]["errorKind"], "transport_lost")
        self.assertIn("not retried", response["error"]["message"])
        self.assertIn("Claude Code exited unexpectedly (exit status: 0) for session " + self.id, self.exited())

    def test_cli_exit_while_idle_ends_the_bridge(self):
        self.session()
        self.assertEqual(self.prompt("crash-when-idle")["stopReason"], "end_turn")
        self.assertIn("exit status: 3", self.exited())

    def test_deliberate_close_keeps_the_bridge(self):
        self.session()
        self.call("session/close", dict(sessionId=self.id))
        time.sleep(0.3)
        self.assertIsNone(self.process.poll())
        self.session()
        self.assertEqual(self.prompt("hello")["stopReason"], "end_turn")


if __name__ == "__main__":
    if "--input-format" in sys.argv:
        harness()
    else:
        unittest.main()
