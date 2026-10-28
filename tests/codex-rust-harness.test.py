#!/usr/bin/env python3
"""Application API smoke test for the selectable Rust harness; no inference or Node.

Build with cargo build -p agentworks, then run python3 tests/codex-rust-harness.test.py.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import unittest
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from uuid import uuid4


class HarnessTests(unittest.TestCase):
    def setUp(self):
        self.root=Path(__file__).resolve().parents[1]
        self.fixture=tempfile.TemporaryDirectory(prefix="agentworks-rust-harness-")
        self.addCleanup(self.fixture.cleanup)
        self.cwd=Path(self.fixture.name)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1",0))
            port=sock.getsockname()[1]
        self.base=f"http://127.0.0.1:{port}"
        self.env=dict(os.environ, AGENTWORKS_PORT=str(port),AGENTWORKS_BIND="127.0.0.1",AGENTWORKS_ROOTS=str(self.cwd),AGENTWORKS_CATALOG_PATH=str(self.cwd/"catalog.json"),AGENTWORKS_DB_PATH=str(self.cwd/"sessions.sqlite3"),AGENTWORKS_AUTH_MODE="existing",ACP_FIXTURE_ASYNC="1",AGENTWORKS_NODE="/nonexistent/node",CODEX_PATH=str(self.root/"tests/codex-acp.test.py"))
        self.env["ACP_FIXTURE_CWD"]=str(self.cwd)
        self.env["ACP_FIXTURE_NATIVE_LOG"]=str(self.cwd/"native.jsonl")
        self.log=tempfile.TemporaryFile(mode="w+")
        self.addCleanup(self.log.close)
        self.start()
        self.addCleanup(self.stop)
        self.until(lambda:self.request("/api/status"))

    def start(self):
        self.process=subprocess.Popen([str(self.root/"target/debug/agentworks")],env=self.env,stdin=subprocess.DEVNULL,stdout=self.log,stderr=self.log)

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            self.fail("Fixture server failed to shut down")

    def request(self,path,method="GET",body=None):
        request=Request(self.base+path,method=method,data=None if body is None else json.dumps(body).encode(),headers={"Content-Type":"application/json","Origin":self.base})
        try:
            with urlopen(request,timeout=10) as response:
                return json.load(response)
        except HTTPError as error:
            detail=error.read().decode()
            error.close()
            self.fail(f"{method} {path}: {error.code} {detail}")

    def until(self,check):
        deadline=time.monotonic()+15
        while time.monotonic()<deadline:
            if self.process.poll() is not None:
                self.log.seek(0)
                self.fail(self.log.read())
            try:
                value=check()
                if value:return value
            except OSError:
                pass
            time.sleep(.03)
        self.log.seek(0)
        self.fail("Timed out: "+self.log.read())

    def test_selected_harness_discovers_models_and_handles_commands(self):
        status=self.request("/api/status")
        providers={p["id"]:p for p in status["providers"]}
        self.assertIn("codex",providers)
        self.assertIn("Rust ACP",providers["codex-rust"]["name"])
        self.assertTrue(providers["codex-rust"]["installed"])
        models=self.request("/api/models","POST",dict(provider="codex-rust",cwd=str(self.cwd)))
        self.assertEqual(models["options"][0]["value"],"test-model")
        settings=self.request("/api/settings")
        settings["defaultModels"]["codex-rust"]="test-model"
        self.request("/api/settings","PUT",settings)
        automation=self.request("/api/automations","POST",dict(name="Rust recipe",provider="codex-rust",cwd=str(self.cwd),prompt="normal"))
        self.assertEqual(automation["provider"],"codex-rust")
        created=self.request("/api/sessions","POST",dict(provider="codex-rust",cwd=str(self.cwd),title="Rust harness test"))
        path="/api/sessions/"+created["id"]
        snapshot=self.until(lambda:(s if (s:=self.request(path))["session"]["status"]=="idle" else None))
        self.assertEqual(snapshot["session"]["provider"],"codex-rust")
        self.assertTrue(snapshot["attachments"]["image"])
        self.assertIn("compact",[c["name"] for c in snapshot["commands"]])
        for index,text in enumerate(["/status","/plan","/compact","/skills","/review"],1):
            self.request(path+"/prompt","POST",dict(text=text,requestId=str(uuid4())))
            snapshot=self.until(lambda:(s if (s:=self.request(path))["session"]["status"]=="idle" and len([e for e in s["events"] if e["kind"]=="turn_end"])>=index else None))
        messages=[e["data"]["content"]["text"] for e in snapshot["events"] if e["kind"]=="acp" and e["data"]["sessionUpdate"]=="agent_message_chunk"]
        self.assertTrue(any("Subscription quota: not reported" in text for text in messages))
        self.assertTrue(any("$check" in text for text in messages))
        # Restoring the application must retain which adapter owns the session.
        self.stop()
        self.start()
        restored=self.until(lambda:self.request(path))
        self.assertEqual(restored["session"]["provider"],"codex-rust")
        self.assertEqual(self.request("/api/settings")["defaultModels"]["codex-rust"],"test-model")
        self.assertEqual(restored["session"]["status"],"suspended")
        self.request(path+"/resume","POST",{})
        self.until(lambda:self.request(path)["session"]["status"]=="idle")

    def test_native_rename_names_the_session_in_both_places(self):
        created=self.request("/api/sessions","POST",dict(provider="codex-rust",cwd=str(self.cwd),title="Before rename"))
        path="/api/sessions/"+created["id"]
        self.until(lambda:self.request(path)["session"]["status"]=="idle")
        turns=lambda n:(s if (s:=self.request(path))["session"]["status"]=="idle" and len([e for e in s["events"] if e["kind"]=="turn_end"])>=n else None)
        # The fixture app-server accepts only this name for thread/name/set.
        self.request(path+"/prompt","POST",dict(text="/rename New conversation name",requestId=str(uuid4())))
        snapshot=self.until(lambda:turns(1))
        self.assertEqual(snapshot["session"]["title"],"New conversation name")
        native=[json.loads(line) for line in (self.cwd/"native.jsonl").read_text().splitlines()]
        self.assertTrue(any(v.get("method")=="thread/name/set" for v in native))
        # Codex refuses a bare /rename, so the session keeps its name.
        self.request(path+"/prompt","POST",dict(text="/rename",requestId=str(uuid4())))
        self.until(lambda:self.request(path)["session"]["status"] in ("idle","error"))
        self.assertEqual(self.request(path)["session"]["title"],"New conversation name")

    def native_lifecycle(self):
        return [v for line in (self.cwd/"native.jsonl").read_text().splitlines() if (v:=json.loads(line)).get("method") in ("thread/start","thread/resume","thread/fork")]

    def test_role_and_shared_context_survive_resume_restart_and_native_fork(self):
        providers={p["id"]:p for p in self.request("/api/status")["providers"]}
        self.assertTrue(providers["codex-rust"]["roles"])
        self.assertFalse(providers["codex"]["roles"])
        role=self.request("/api/roles","POST",dict(name="Test role",instructions="Keep the original role."))
        created=self.request("/api/sessions","POST",dict(provider="codex-rust",cwd=str(self.cwd),role=role["id"]))
        path="/api/sessions/"+created["id"]
        self.until(lambda:self.request(path)["session"]["status"]=="idle")
        def assert_delivery(call,session_id):
            text=call["params"]["developerInstructions"]
            self.assertIn("Configured developer guidance.",text)
            self.assertIn("agentworks://sessions/"+session_id,text)
            self.assertIn("Use the Agentworks MCP tools",text)
            self.assertIn("# Agentworks role: Test role",text)
            self.assertIn("Keep the original role.",text)
            self.assertNotIn("Edited role.",text)
            self.assertEqual(text.count("# Agentworks role:"),1)
            self.assertNotIn("baseInstructions",call["params"])
        self.assertEqual(self.native_lifecycle()[-1]["method"],"thread/start")
        assert_delivery(self.native_lifecycle()[-1],created["id"])
        self.request("/api/roles/"+role["id"],"PUT",dict(name="Changed",instructions="Edited role."))
        self.request(path+"/prompt","POST",dict(text="fork-boundary",requestId=str(uuid4())))
        self.until(lambda:(s:=self.request(path))["session"]["status"]=="idle" and any(e["kind"]=="turn_end" for e in s["events"]))
        self.request(path+"/suspend","POST",{})
        self.until(lambda:self.request(path)["session"]["status"]=="suspended")
        self.request(path+"/resume","POST",{})
        self.until(lambda:self.request(path)["session"]["status"]=="idle")
        self.assertEqual(self.native_lifecycle()[-1]["method"],"thread/resume")
        assert_delivery(self.native_lifecycle()[-1],created["id"])
        # This restarts only the isolated test server, never the running application.
        self.stop()
        self.start()
        self.until(lambda:self.request(path))
        self.request(path+"/resume","POST",{})
        self.until(lambda:self.request(path)["session"]["status"]=="idle")
        assert_delivery(self.native_lifecycle()[-1],created["id"])
        fork=self.request(path+"/fork","POST",dict(requestId=str(uuid4())))
        self.until(lambda:self.request("/api/sessions/"+fork["id"])["session"]["status"]=="idle")
        self.assertEqual(fork["role"],created["role"])
        call=self.native_lifecycle()[-1]
        self.assertEqual(call["method"],"thread/fork")
        assert_delivery(call,fork["id"])
        self.assertNotIn("agentworks://sessions/"+created["id"],call["params"]["developerInstructions"])

    def test_roleless_session_receives_shared_context(self):
        created=self.request("/api/sessions","POST",dict(provider="codex-rust",cwd=str(self.cwd)))
        self.until(lambda:self.request("/api/sessions/"+created["id"])["session"]["status"]=="idle")
        text=self.native_lifecycle()[-1]["params"]["developerInstructions"]
        self.assertIn("agentworks://sessions/"+created["id"],text)
        self.assertNotIn("# Agentworks role:",text)

    def test_import_load_receives_shared_context(self):
        created=self.request("/api/sessions/import","POST",dict(provider="codex-rust",providerSessionId="thread-1",cwd=str(self.cwd)))
        path="/api/sessions/"+created["id"]
        snapshot=self.until(lambda:(s if (s:=self.request(path))["session"]["status"]=="idle" else None))
        call=self.native_lifecycle()[-1]
        self.assertEqual(call["method"],"thread/resume")
        self.assertIn("agentworks://sessions/"+created["id"],call["params"]["developerInstructions"])
        self.assertTrue(any(e["kind"]=="acp" and e["data"].get("sessionUpdate")=="user_message_chunk" for e in snapshot["events"]))

    def test_native_background_jobs_reach_application_and_stop_individually(self):
        created=self.request("/api/sessions","POST",dict(provider="codex-rust",cwd=str(self.cwd),title="Native jobs test"))
        path="/api/sessions/"+created["id"]
        self.until(lambda:self.request(path)["session"]["status"]=="idle")
        self.request(path+"/prompt","POST",dict(text="async-child",requestId=str(uuid4())))
        snapshot=self.until(lambda:(s if (s:=self.request(path))["session"]["status"]=="idle" and len(s["asyncTasks"]["tasks"])==2 else None))
        self.assertTrue(snapshot["asyncTasks"]["supported"])
        tasks={t["asyncTaskId"]:t for t in snapshot["asyncTasks"]["tasks"]}
        self.assertEqual(tasks["bg-command"]["state"],"running")
        self.assertEqual(tasks["child-bg:bg-command"]["acpSessionId"],"child-bg")
        result=self.request(path+"/async-tasks/stop","POST",dict(asyncTaskId="child-bg:bg-command",acpSessionId="child-bg"))
        self.assertTrue(result["stopped"])
        snapshot=self.until(lambda:(s if (s:=self.request(path)) and any(t["asyncTaskId"]=="child-bg:bg-command" and t["state"]=="stopped" for t in s["asyncTasks"]["tasks"]) else None))
        self.assertEqual(next(t for t in snapshot["asyncTasks"]["tasks"] if t["asyncTaskId"]=="bg-command")["state"],"running")
        self.assertEqual(len([e for e in snapshot["events"] if e["kind"]=="turn_end"]),1)
        self.assertFalse(any(e["kind"]=="stop_requested" for e in snapshot["events"]))

    def test_native_subagents_reach_application_routing(self):
        created=self.request("/api/sessions","POST",dict(provider="codex-rust",cwd=str(self.cwd),title="Native child test"))
        path="/api/sessions/"+created["id"]
        self.until(lambda:self.request(path)["session"]["status"]=="idle")
        self.request(path+"/prompt","POST",dict(text="subagents",requestId=str(uuid4())))
        snapshot=self.until(lambda:(s if (s:=self.request(path))["session"]["status"]=="waiting" else None))
        approval=snapshot["approvals"][0]
        self.assertEqual(approval["request"]["sessionId"],"child-b")
        events=[e["data"] for e in snapshot["events"] if e["kind"]=="acp"]
        self.assertTrue(any(e.get("acpSessionId")=="child-a" and e.get("sessionId")=="child-b" and e.get("sessionUpdate")=="subagent_update" for e in events))
        self.assertTrue(any(e.get("acpSessionId")=="child-b" and e.get("parentSessionId")=="child-a" and e.get("content",{}).get("text")=="Nested answer" for e in events if isinstance(e.get("content"),dict)))
        self.request(path+"/approvals/"+approval["id"],"POST",dict(optionId="accept"))
        self.until(lambda:self.request(path)["session"]["status"]=="idle")


if __name__=="__main__":
    unittest.main()
