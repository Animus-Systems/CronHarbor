#!/usr/bin/env python3
"""Real-daemon acceptance checks; all state and credentials are disposable/private."""
import argparse
import json
from pathlib import Path
import os
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import select
import hashlib
import resource
import signal
import shutil

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("CRONHARBOR_BINARY", ROOT / "target/debug/cronharbor"))


def mcp_check(state, token_file):
    bridge = subprocess.Popen([str(BINARY), "--state-dir", str(state), "mcp", "--token-file", str(token_file)],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    pending = bytearray()
    def exchange(message):
        bridge.stdin.write(json.dumps(message).encode() + b"\n")
        bridge.stdin.flush()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            while b"\n" in pending:
                line, _, remaining = pending.partition(b"\n")
                pending[:] = remaining
                response = json.loads(line)
                assert response.get("jsonrpc") == "2.0"
                if response.get("id") == message.get("id"):
                    return response
            if select.select([bridge.stdout], [], [], 0.1)[0]:
                data = os.read(bridge.stdout.fileno(), 4096)
                if not data:
                    raise AssertionError("MCP bridge exited before replying")
                pending.extend(data)
        raise AssertionError("MCP request did not complete")
    try:
        hello = exchange({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "real-daemon-acceptance", "version": "1"}}})
        assert hello["result"]["protocolVersion"] == "2025-11-25"
        bridge.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        bridge.stdin.flush()
        tools = exchange({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})["result"]["tools"]
        assert len(tools) == 10 and all(tool["annotations"]["readOnlyHint"] for tool in tools)
        result = exchange({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_instance_status", "arguments": {}}})
        assert not result["result"].get("isError", False)
        rejected = exchange({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "run_shell", "arguments": {"command": "false"}}})
        assert "error" in rejected
        bridge.stdin.close()
        bridge.wait(timeout=5)
        pending.extend(bridge.stdout.read())
        assert all(json.loads(line).get("jsonrpc") == "2.0" for line in pending.splitlines() if line)
    finally:
        if bridge.poll() is None:
            bridge.kill()
            bridge.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--browser", action="store_true")
    parser.add_argument("--browser-grep", help="Run affected browser scenarios while reusing unchanged checks")
    options = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="cronharbor-acceptance-", dir="/tmp") as temporary:
        private = Path(temporary)
        state = private / "state"
        log = (private / "daemon.log").open("w+")
        daemon = None
        dashboard = None
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            dashboard_origin = f"http://127.0.0.1:{sock.getsockname()[1]}"

        def cli(*arguments, directory=state, check=True):
            result = subprocess.run([str(BINARY), "--state-dir", str(directory), *arguments], cwd=ROOT,
                                    capture_output=True, text=True, timeout=20)
            if check and result.returncode:
                raise AssertionError("CLI failed: " + result.stderr)
            return json.loads(result.stdout) if result.returncode == 0 else result

        def wait(query, timeout=15):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                value = query()
                if value:
                    return value
                time.sleep(0.05)
            raise AssertionError("Expected daemon state was not observed")

        def start(directory):
            nonlocal daemon
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            arguments = [str(BINARY), "--state-dir", str(directory), "server", "--listen", f"127.0.0.1:{port}",
                         "--ui-dir", str(ROOT / "web/dashboard/dist")]
            if options.browser:
                arguments += ["--allowed-origin", dashboard_origin]
            if directory == state:
                arguments += ["--file", str(ROOT / "examples/hello.yaml")]
            daemon = subprocess.Popen(arguments, cwd=ROOT, stdout=log, stderr=log)
            wait(lambda: cli("status", directory=directory, check=False) if (directory / "control.sock").exists() else None)
            return f"http://127.0.0.1:{port}"

        def stop():
            nonlocal daemon
            if daemon and daemon.poll() is None:
                daemon.terminate()
                try:
                    daemon.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait(timeout=5)
                    raise AssertionError("Daemon did not stop gracefully")
            daemon = None

        try:
            base = start(state)
            for role, grants in [("admin", ["*"]), ("viewer", []), ("observe", ["workload.read", "execution.read", "logs.read"])]:
                arguments = ["tokens", "create", "--role", role, "--namespace", "*", "--output-file", str(private / f"{role}.token")]
                for grant in grants:
                    arguments += ["--capability", grant]
                cli(*arguments)
            admin = (private / "admin.token").read_text().strip()
            viewer = (private / "viewer.token").read_text().strip()

            def request(path, method="GET", body=None, credential=admin, key=None):
                headers = {"Authorization": "Bearer " + credential}
                if key:
                    headers["Idempotency-Key"] = key
                encoded = None if body is None else json.dumps(body).encode()
                if encoded is not None:
                    headers["Content-Type"] = "application/json"
                call = urllib.request.Request(base + "/api/v1" + path, encoded, headers, method=method)
                try:
                    with urllib.request.urlopen(call, timeout=10) as response:
                        return response.status, json.load(response)
                except urllib.error.HTTPError as error:
                    return error.code, json.load(error)

            assert request("/workloads", credential="bad")[0] == 401
            status, page = request("/workloads")
            assert status == 200 and len(page["items"]) == 1
            file_workload = page["items"][0]

            def definition(name, kind="OneShotJob", command="/bin/echo", args=None):
                return {"apiVersion": "cronharbor.dev/v1", "kind": kind,
                        "metadata": {"name": name, "namespace": "acceptance"},
                        "spec": {"owner": "api", "enabled": True,
                                 "execution": {"adapter": "process", "command": command, "args": args or ["acceptance-output"], "workingDirectory": "/tmp", "sandbox": "default"},
                                 "timeoutSeconds": 5, "logs": {"maxRunBytes": 4096}}}

            def await_operation(value):
                identifier = value["id"]
                result = wait(lambda: (row if row.get("state") not in ("ACCEPTED", "RUNNING") else None)
                              if (row := request("/operations/" + identifier)[1]) else None)
                assert result["state"] == "SUCCEEDED", result
                return result

            def apply(manifest):
                payload = {"definition": json.dumps(manifest)}
                assert request("/workloads/validate", "POST", payload)[0] == 200
                status, plan = request("/config/plan", "POST", payload)
                assert status == 200
                status, accepted = request("/config/apply", "POST", {**payload, "expected_revision": plan["current_revision"]}, key=str(uuid.uuid4()))
                assert status == 202, accepted
                await_operation(accepted)
                return next(row for row in request("/workloads?namespace=acceptance")[1]["items"] if row["name"] == manifest["metadata"]["name"])

            job = apply(definition("manual"))
            key = str(uuid.uuid4())
            status, accepted = request("/jobs/" + job["id"] + "/run", "POST", {}, key=key)
            assert status == 202
            repeated = request("/jobs/" + job["id"] + "/run", "POST", {}, key=key)[1]
            assert repeated["id"] == accepted["id"]
            await_operation(accepted)
            for namespace, permitted in [("unrelated", False), ("acceptance", True)]:
                token_path = private / f"operations-{namespace}.token"
                cli("tokens", "create", "--role", "admin", "--namespace", namespace,
                    "--capability", "operations.manage", "--output-file", str(token_path))
                credential = token_path.read_text().strip()
                assert request("/operations/" + accepted["id"], credential=credential)[0] == (200 if permitted else 403)
                inspected = cli("--token-file", str(token_path), "operations", "inspect", accepted["id"], check=False)
                if permitted:
                    assert inspected["id"] == accepted["id"]
                else:
                    assert inspected.returncode != 0 and "NOT_AUTHORIZED" in inspected.stderr
            rows = request("/executions?workload_id=" + job["id"])[1]["items"]
            assert len(rows) == 1 and rows[0]["state"] == "SUCCEEDED"
            execution = rows[0]
            logs = request("/executions/" + execution["execution_id"] + "/logs")[1]
            assert logs["untrusted"] and logs["text"] == "acceptance-output\n"
            assert request("/jobs/" + job["id"] + "/run", "POST", {}, credential=viewer, key=str(uuid.uuid4()))[0] == 403
            pause_key = str(uuid.uuid4())
            assert request("/workloads/" + job["id"] + "/pause", "POST", {"paused": True}, key=pause_key)[0] == 202
            assert request("/workloads/" + job["id"] + "/pause", "POST", {"paused": False}, key=pause_key)[0] == 409

            service_manifest = definition("worker", "Service", "/bin/sleep", ["60"])
            service_manifest["spec"]["service"] = {"desiredState": "STOPPED"}
            service = apply(service_manifest)
            status, accepted = request("/services/" + service["id"] + "/desired-state", "POST",
                                       {"desired_state": "RUNNING", "expected_revision": service["revision_id"]}, key=str(uuid.uuid4()))
            assert status == 202
            await_operation(accepted)
            old_attempts = {row["attempt_id"] for row in request("/executions?workload_id=" + service["id"])[1]["items"]}
            status, accepted = request("/services/" + service["id"] + "/restart", "POST", {"expected_revision": service["revision_id"]}, key=str(uuid.uuid4()))
            assert status == 202
            await_operation(accepted)
            assert any(row["attempt_id"] not in old_attempts for row in request("/executions?workload_id=" + service["id"])[1]["items"])
            status, accepted = request("/services/" + service["id"] + "/desired-state", "POST", {"desired_state": "STOPPED", "expected_revision": service["revision_id"]}, key=str(uuid.uuid4()))
            assert status == 202
            await_operation(accepted)

            mcp_check(state, private / "observe.token")

            if options.browser:
                browser_token = private / "browser-admin.token"
                cli("tokens", "create", "--role", "admin", "--namespace", "*", "--capability", "*", "--output-file", str(browser_token))
                environment = dict(os.environ, CRONHARBOR_DASHBOARD_URL=base,
                                   CRONHARBOR_E2E_PYTHON=os.sys.executable,
                                   CRONHARBOR_E2E_TOKEN_FILE=str(browser_token),
                                   CRONHARBOR_E2E_VIEWER_TOKEN_FILE=str(private / "viewer.token"),
                                   CRONHARBOR_E2E_FILE_WORKLOAD=file_workload["id"], CRONHARBOR_E2E_REMOTE_URL="")
                environment["PLAYWRIGHT_HTML_OUTPUT_DIR"] = str(private / "browser-report")
                browser_command=["npm", "run", "e2e", "--", "--output", str(private / "browser-results")]
                if options.browser_grep:browser_command.extend(["--grep",options.browser_grep])
                subprocess.run(browser_command, cwd=ROOT / "web/dashboard", env=environment, check=True, timeout=300)
                ui = private / "dashboard"
                shutil.copytree(ROOT / "web/dashboard/dist", ui)
                (ui / "config.json").write_text(json.dumps({"apiBaseUrl": base + "/api/v1"}))
                dashboard = subprocess.Popen([str(BINARY), "--state-dir", str(state), "api", "--listen", dashboard_origin.removeprefix("http://"), "--ui-dir", str(ui)], cwd=ROOT, stdout=log, stderr=log)
                def dashboard_ready():
                    try:
                        with urllib.request.urlopen(dashboard_origin + "/api/v1/health/live", timeout=1) as response:
                            return response.status == 200
                    except (OSError, urllib.error.URLError):
                        return False
                wait(dashboard_ready)
                origin_token = private / "origin.token"
                cli("tokens", "create", "--role", "admin", "--namespace", "*", "--capability", "*", "--output-file", str(origin_token))
                environment.update(CRONHARBOR_DASHBOARD_URL=dashboard_origin, CRONHARBOR_E2E_REMOTE_URL=base + "/api/v1",
                                   CRONHARBOR_E2E_TOKEN_FILE=str(origin_token))
                subprocess.run(["npm", "run", "e2e", "--", "e2e/origin.spec.ts", "--reporter=list", "--output", str(private / "origin-results")], cwd=ROOT / "web/dashboard", env=environment, check=True, timeout=90)
                dashboard.terminate()
                dashboard.wait(timeout=15)
                dashboard = None

            snapshot = cli("backup", "acceptance")
            original_workloads = cli("status")["workloads"]
            stop()
            original_hash = hashlib.sha256((state / "state.db").read_bytes()).digest()
            def limit_restore_copy():
                signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
                resource.setrlimit(resource.RLIMIT_FSIZE, (65_536, 65_536))
            assert Path(snapshot["path"]).stat().st_size > 65_536
            failed = subprocess.run([str(BINARY), "--state-dir", str(state), "restore", snapshot["path"],
                                     "--replace", "--evidence", "copy-stage disk limit regression"],
                                    capture_output=True, text=True, timeout=15, preexec_fn=limit_restore_copy)
            assert failed.returncode != 0
            assert hashlib.sha256((state / "state.db").read_bytes()).digest() == original_hash
            assert not (state / "restore-required.json").exists()
            assert not (state / "restore-installing").exists()
            base = start(state)
            assert cli("status")["workloads"] == original_workloads
            stop()
            restored = private / "restored"
            cli("restore", snapshot["path"], "--evidence", "automated restore drill", directory=restored)
            base = start(restored)
            state_info = cli("status", directory=restored)
            assert state_info["paused"] and state_info["restore_required"]
            assert request("/session")[0] == 401, "Restored credentials must never become valid again"
            rejected = cli("resume", "--evidence", "unsafe implicit resume denied", directory=restored, check=False)
            assert isinstance(rejected, subprocess.CompletedProcess) and rejected.returncode != 0
            cutoff = time.time_ns() // 1000
            cli("resume", "--cutoff-us", str(cutoff), "--evidence", "all effects reconciled in disposable test", directory=restored)
            assert not cli("status", directory=restored)["paused"]
            print(json.dumps({"result": "passed", "verified": ["real_bearer_permissions", "operation_namespace_scope_REST_and_CLI", "API_CAS_apply", "manual_run_idempotency", "bounded_logs", "service_start_restart_stop", "real_MCP_read_only_protocol", "restore_copy_failure_preserves_state", "backup_verified_restore_quarantine", "explicit_cutoff_resume"], "browser": options.browser}, indent=2))
        finally:
            if dashboard and dashboard.poll() is None:
                dashboard.terminate()
                try:
                    dashboard.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    dashboard.kill()
                    dashboard.wait(timeout=5)
            stop()
            log.close()


if __name__ == "__main__":
    main()
