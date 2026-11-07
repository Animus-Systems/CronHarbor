#!/usr/bin/env python3
"""Exercise the opt-in Docker adapter against an explicitly supplied local test daemon."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("CRONHARBOR_BINARY", ROOT / "target/debug/cronharbor"))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--image", required=True, help="An already pulled name@sha256:digest")
    options = parser.parse_args()
    assert options.host.startswith("unix:///") and "@sha256:" in options.image
    docker = shutil.which("docker")
    assert docker
    with tempfile.TemporaryDirectory(prefix="cronharbor-executor-", dir="/tmp") as temporary:
        state = Path(temporary)
        policy = {"dockerEnabled": True, "dockerBinary": docker, "dockerHost": options.host, "images": [options.image]}
        policy_path = state / "executors.json"
        policy_path.write_text(json.dumps(policy))
        policy_path.chmod(0o600)
        definition = {"apiVersion": "cronharbor.dev/v1", "kind": "OneShotJob",
                      "metadata": {"name": "external", "namespace": "acceptance"},
                      "spec": {"owner": "file", "enabled": True, "execution": {"adapter": "docker", "command": "/bin/echo", "args": ["owned-container"], "workingDirectory": "/tmp", "docker": {"image": options.image, "user": "10001:10001", "network": "none", "memoryBytes": 67108864, "pidsLimit": 32}}, "timeoutSeconds": 10, "logs": {"maxRunBytes": 4096}}}
        manifest = state / "job.json"
        manifest.write_text(json.dumps(definition))
        daemon = subprocess.Popen([str(BINARY), "--state-dir", str(state), "server", "--file", str(manifest)], cwd=ROOT,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

        def cli(*arguments):
            result = subprocess.run([str(BINARY), "--state-dir", str(state), *arguments], cwd=ROOT,
                                    capture_output=True, text=True, timeout=20)
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def wait(query, timeout=20):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                result = query()
                if result:
                    return result
                time.sleep(0.1)
            raise AssertionError("Docker executor did not reach expected state")

        try:
            wait(lambda: state.joinpath("control.sock").exists())
            job = cli("jobs", "list")["items"][0]
            operation = cli("jobs", "run", job["id"])
            wait(lambda: cli("operations", "inspect", operation["id"])["state"] == "SUCCEEDED")
            history = cli("executions")
            assert len(history) == 1 and history[0]["state"] == "SUCCEEDED", history
            assert cli("logs", history[0]["attempt_id"])["text"] == "owned-container\n"
            def remaining():
                result = subprocess.run([docker, "--host", options.host, "ps", "-aq", "--filter", "label=dev.cronharbor.workload=" + job["id"]], capture_output=True, text=True, check=True, timeout=10)
                return not result.stdout.strip()
            wait(remaining)
            policy["dockerHost"] = "unix:///tmp/cronharbor-no-daemon.sock"
            policy_path.write_text(json.dumps(policy))
            cli("jobs", "run", job["id"])
            wait(lambda: any(row["state"] == "UNKNOWN" for row in cli("executions")))
            count = len(cli("executions"))
            time.sleep(0.5)
            assert len(cli("executions")) == count
            print(json.dumps({"result": "passed", "verified": ["opt_in_policy", "pinned_image", "real_owned_container", "terminal_log_capture", "owned_terminal_cleanup", "daemon_outage_unknown_no_retry"]}, indent=2))
        finally:
            if daemon.poll() is None:
                daemon.terminate()
                try:
                    daemon.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait(timeout=5)
            subprocess.run([docker, "--host", options.host, "container", "prune", "--force", "--filter", "label=dev.cronharbor.workload=" + locals().get("job", {}).get("id", "never-created")], capture_output=True, timeout=15, check=False)


if __name__ == "__main__":
    main()
