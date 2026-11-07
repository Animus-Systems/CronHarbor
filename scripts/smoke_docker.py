#!/usr/bin/env python3
"""Exercise the built local image with a private, disposable Compose project."""

import json
from pathlib import Path
import subprocess
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
PROJECT = "cronharbor-smoke-" + uuid.uuid4().hex[:10]
COMPOSE = ["docker", "compose", "--project-name", PROJECT]
BINARY = ["cronharbor", "--state-dir", "/var/lib/cronharbor"]


def run(arguments, check=True):
    return subprocess.run(arguments, cwd=ROOT, check=check, capture_output=True, text=True, timeout=30)


def query(*arguments):
    result = run(COMPOSE + ["exec", "-T", "cronharbor"] + BINARY + list(arguments), check=False)
    return json.loads(result.stdout) if result.returncode == 0 else None


def wait_for(query_fn, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = query_fn()
        if value:
            return value
        time.sleep(0.25)
    raise AssertionError("Timed out waiting for the expected daemon state")


def main():
    try:
        run(COMPOSE + ["up", "--detach", "--no-build"])
        status = wait_for(lambda: query("status"), 10)
        container = run(COMPOSE + ["ps", "--quiet", "cronharbor"]).stdout.strip()
        inspection = json.loads(run(["docker", "inspect", container]).stdout)[0]
        assert inspection["Config"]["User"] == "10001:10001"
        assert inspection["HostConfig"]["ReadonlyRootfs"] is True
        assert not inspection["HostConfig"].get("PortBindings")
        assert not inspection["Config"].get("ExposedPorts")
        assert not any("docker.sock" in mount["Destination"] for mount in inspection["Mounts"])
        second = run(COMPOSE + ["exec", "-T", "cronharbor"] + BINARY + ["server"], check=False)
        assert second.returncode != 0 and "OWNER_UNAVAILABLE" in second.stderr

        def succeeded():
            return next((row for row in (query("executions") or []) if row["state"] == "SUCCEEDED"), None)

        execution = wait_for(succeeded, 75)
        logs = query("logs", execution["attempt_id"])
        assert logs and logs["text"] == "Hello from CronHarbor\n"
        run(COMPOSE + ["restart", "cronharbor"])
        wait_for(lambda: query("status"), 10)
        history = query("executions")
        assert any(row["execution_id"] == execution["execution_id"] for row in history)
        assert len({(row["name"], row["due_at_us"]) for row in history}) == len(history)
        print(json.dumps({
            "result": "passed",
            "platform": run(["docker", "info", "--format", "{{.OSType}}/{{.Architecture}}"]).stdout.strip(),
            "image_id": inspection["Image"],
            "sqlite_version": status["sqlite_version"],
            "execution_id": execution["execution_id"],
            "verified": ["nonroot", "read_only_root", "no_ports_or_docker_socket", "second_owner_denied", "real_scheduled_process", "bounded_log_query", "restart_history_and_no_duplicate_due"]
        }, indent=2))
    finally:
        cleanup = run(COMPOSE + ["down", "--volumes", "--remove-orphans"], check=False)
        if cleanup.returncode:
            raise RuntimeError("Disposable smoke project cleanup failed: " + PROJECT)


if __name__ == "__main__":
    main()
