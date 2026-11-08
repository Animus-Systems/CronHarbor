#!/usr/bin/env python3
"""Exercise native rolling capture against a private disposable real daemon."""

from __future__ import annotations

import hashlib
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("CRONHARBOR_BINARY", ROOT / "target/debug/cronharbor")).resolve()


def run(state: Path, *arguments: str, success: bool = True) -> object:
    result = subprocess.run(
        [str(BINARY), "--state-dir", str(state), *arguments],
        capture_output=True, text=True, timeout=12,
    )
    assert len(result.stdout) <= 1_048_576 and len(result.stderr) <= 65_536, "CLI output exceeded fixture bounds"
    if not success:
        assert result.returncode != 0, "Expected a rejected operation"
        return result.stderr
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def query(state: Path, command: dict[str, object]) -> dict[str, object]:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(5)
        connection.connect(str(state / "control.sock"))
        connection.sendall((json.dumps({"command": "execute", "app_command": command, "token": None}) + "\n").encode())
        with connection.makefile("rb") as reader:
            line = reader.readline(2_097_153)
    assert len(line) <= 2_097_152 and line.endswith(b"\n"), "Invalid bounded daemon frame"
    value = json.loads(line)
    assert isinstance(value, dict)
    return value


def wait_for(predicate, timeout: float = 15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.025)
    raise AssertionError("Disposable fixture did not reach the expected state")


def object_value(value: object) -> dict:
    assert isinstance(value, dict), "Expected an object response"
    return value


def execution(state: Path, name: str, expected: str | None = None):
    response = query(state, {"action": "executions", "workload_id": None, "cursor": None, "limit": 100})
    assert response["error"] is None, response["error"]
    values = object_value(response["data"])["items"]
    assert isinstance(values, list)
    for value in values:
        assert isinstance(value, dict)
        if value.get("name") == name and (expected is None or value.get("state") == expected):
            return value
    return None


def start_owner(state: Path, definition: Path | None = None, port: int | None = None):
    arguments = [str(BINARY), "--state-dir", str(state), "server"]
    if definition is not None:
        arguments += ["--file", str(definition)]
    if port is not None:
        arguments += ["--listen", f"127.0.0.1:{port}", "--ui-dir", str(ROOT / "web/dashboard/dist")]
    owner = subprocess.Popen(arguments, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        def ready():
            assert owner.poll() is None, "Disposable daemon exited before readiness"
            try:
                return query(state, {"action": "status"}).get("data")
            except (FileNotFoundError, ConnectionRefusedError):
                return None
        wait_for(ready)
        return owner
    except BaseException:
        owner.kill()
        owner.wait(timeout=5)
        raise


def stop_owner(owner):
    if owner.poll() is None:
        owner.terminate()
        try:
            owner.wait(timeout=8)
        except subprocess.TimeoutExpired:
            owner.kill()
            owner.wait(timeout=5)


def manifest(name: str, program: str, secret: Path | None = None):
    value = {
        "apiVersion": "cronharbor.dev/v1", "kind": "OneShotJob",
        "metadata": {"name": name, "namespace": "fixture"},
        "spec": {
            "owner": "file", "enabled": True,
            "execution": {"adapter": "process", "command": sys.executable,
                          "args": ["-u", "-c", program], "workingDirectory": "/tmp"},
            "timeoutSeconds": 20,
            "logs": {"maxRunBytes": 131072, "rotation": {"segmentBytes": 65536, "maxSegments": 2}},
        },
    }
    if secret is not None:
        value["spec"]["execution"]["secretRefs"] = {"FIXTURE_SECRET": {"provider": "file", "path": str(secret)}}
    return value


def file_hashes(state: Path):
    return {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in (state / "logs").glob("*.log")}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--browser", action="store_true")
    options = parser.parse_args()
    assert BINARY.is_file(), "Build Full and select CRONHARBOR_BINARY before this acceptance run"
    with tempfile.TemporaryDirectory(prefix="ch-log-", dir="/tmp") as temporary:
        directory = Path(temporary)
        state = directory / "state"
        state.mkdir(mode=0o700)
        secret = directory / "secret"
        secret_value = "fixture-secret-for-rotation-boundaries-" * 3
        secret.write_text(secret_value)
        secret.chmod(0o600)
        program = "import os,sys,time\nfor n in range(5000):\n sys.stdout.write(f'{n:06d} ' + 'x'*70 + '\\n')\n if n%97==0: sys.stdout.write('token='+os.environ['FIXTURE_SECRET']+'\\n')\n time.sleep(0.001)\nsys.stdout.write('done\\n')\n"
        expected = b"".join(
            f"{n:06d} {'x'*70}\n".encode() + (b"token=[REDACTED]\n" if n % 97 == 0 else b"")
            for n in range(5000)
        ) + b"done\n"
        definition = directory / "rotation.json"
        definition.write_text(json.dumps(manifest("rotating-job", program, secret)))
        port = None
        if options.browser:
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
        owner = start_owner(state, definition, port)
        worker_pid = None
        browser = None
        try:
            run(state, "log-storage", "set", "--max-disk-bytes", "1048576", "--max-segments", "16",
                "--retention-seconds", "604800", "--expected-revision", "1", "--idempotency-key", "fixture-policy")
            began = time.monotonic()
            run(state, "jobs", "run", "fixture/rotating-job", "--idempotency-key", "fixture-run")
            finished = wait_for(lambda: execution(state, "rotating-job", "SUCCEEDED"))
            info = object_value(run(state, "logs", finished["attempt_id"], "--info"))
            assert info["capture_mode"] == "ROLLING" and info["retained_from_offset"] > 0
            assert info["written_end_offset"] == len(expected), "Small flushed writes were lost during capture"
            assert info["known_discarded_bytes"] == 0
            assert info["retained_bytes"] <= 131072
            cursor = info["retained_from_offset"]
            actual = bytearray()
            pages = 0
            while cursor < info["written_end_offset"]:
                pages += 1
                assert pages <= 32, "Log read did not make bounded progress"
                response = query(state, {"action": "logs", "attempt_id": finished["attempt_id"], "offset": cursor, "max_bytes": 65536})
                assert response["error"] is None, response["error"]
                page = response["data"]
                assert page["next_offset"] > cursor
                actual.extend(page["text"].encode())
                cursor = page["next_offset"]
            assert bytes(actual) == expected[info["retained_from_offset"]:], "Retained suffix differs from produced redacted bytes"
            assert secret_value.encode() not in actual and b"[REDACTED]" in actual
            expired = query(state, {"action": "logs", "attempt_id": finished["attempt_id"], "offset": 0, "max_bytes": 64})
            assert object_value(expired["error"])["code"] == "LOG_CURSOR_EXPIRED"
            assert len(file_hashes(state)) <= 2

            pidfile = directory / "service.pid"
            service_program = f"import os,sys,time\nopen({str(pidfile)!r},'w').write(str(os.getpid()))\nwhile True:\n sys.stdout.write('live ' + 'y'*4090 + '\\n')\n time.sleep(0.01)\n"
            service = manifest("rotating-service", service_program)
            service["kind"] = "Service"
            service["spec"]["service"] = {"desiredState": "RUNNING", "restart": "never"}
            service_file = directory / "service.json"
            service_file.write_text(json.dumps(service))
            run(state, "config", "apply", "-f", str(service_file))
            wait_for(pidfile.exists)
            worker_pid = int(pidfile.read_text())
            running = wait_for(lambda: execution(state, "rotating-service", "RUNNING"))
            wait_for(lambda: object_value(run(state, "logs", running["attempt_id"], "--info"))["retained_from_offset"] > 0)
            if options.browser:
                token = directory / "browser.token"
                run(state, "tokens", "create", "--role", "admin", "--namespace", "*", "--capability", "*", "--output-file", str(token))
                ready = directory / "browser.ready"
                stable = directory / "browser.stable"
                environment = dict(os.environ, CRONHARBOR_DASHBOARD_URL=f"http://127.0.0.1:{port}",
                                   CRONHARBOR_ROTATION_TOKEN_FILE=str(token), CRONHARBOR_ROTATION_READY_FILE=str(ready),
                                   CRONHARBOR_ROTATION_STABLE_FILE=str(stable),
                                   CRONHARBOR_ROTATION_EXECUTION=running["execution_id"])
                browser = subprocess.Popen(["npm", "run", "e2e", "--", "e2e/log-freeze-real.spec.ts", "--reporter=list"],
                                           cwd=ROOT / "web/dashboard", env=environment)
                def observing():
                    assert browser.poll() is None, "Freeze browser exited before observing capture"
                    return ready.exists()
                wait_for(observing, timeout=30)
                print(json.dumps({"checkpoint": "observing_rapid_rotation_for_full_rate_window"}), flush=True)
                def paced():
                    assert browser.poll() is None, "Freeze browser lost capture during the rate window"
                    return stable.exists()
                wait_for(paced, timeout=90)
            owner.kill()
            owner.wait(timeout=5)
            before = file_hashes(state)
            owner = start_owner(state, port=port)
            uncertain = wait_for(lambda: execution(state, "rotating-service", "UNKNOWN"))
            frozen = object_value(run(state, "logs", uncertain["attempt_id"], "--info"))
            assert frozen["capture_state"] == "FROZEN"
            time.sleep(0.25)
            assert file_hashes(state) == before, "Restart changed retained UNKNOWN evidence"
            if browser is not None:
                assert browser.wait(timeout=35) == 0, "Real frozen evidence browser scenario failed"
            rejected = run(state, "log-storage", "set", "--max-disk-bytes", "524288", "--max-segments", "8",
                           "--retention-seconds", "1", "--expected-revision", "2", "--idempotency-key", "unsafe-lower", success=False)
            assert "STATE_CONFLICT" in rejected
            snapshot = object_value(run(state, "backup", "rotation"))
            stop_owner(owner)
            restored = directory / "restored"
            run(restored, "restore", snapshot["path"], "--evidence", "disposable rotation metadata-only restore")
            owner = start_owner(restored)
            status = object_value(run(restored, "status"))
            assert status["paused"] and status["restore_required"]
            unavailable = object_value(run(restored, "logs", uncertain["attempt_id"], "--info"))
            assert unavailable["capture_state"] == "FROZEN" and unavailable["last_error_code"] == "LOG_UNAVAILABLE"
            withheld = query(restored, {"action": "logs", "attempt_id": uncertain["attempt_id"],
                                         "offset": unavailable["retained_from_offset"], "max_bytes": 64})
            assert object_value(withheld["error"])["code"] == "LOG_UNAVAILABLE"
            assert object_value(run(restored, "log-storage", "status"))["usage"]["capture_error_count"] >= 1
            print(json.dumps({"accepted": True, "produced_redacted_bytes": len(expected),
                              "retained_bytes": len(actual), "retained_start": info["retained_from_offset"],
                              "elapsed_seconds": round(time.monotonic()-began, 3), "unknown_frozen": True,
                              "browser": options.browser,
                              "metadata_only_restore_unavailable": True}))
        finally:
            if browser is not None and browser.poll() is None:
                browser.terminate()
                try:
                    browser.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    browser.kill()
                    browser.wait(timeout=5)
            stop_owner(owner)
            if worker_pid is not None:
                try:
                    if os.getpgid(worker_pid) == worker_pid:
                        os.killpg(worker_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    main()
