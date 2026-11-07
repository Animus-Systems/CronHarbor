#!/usr/bin/env python3
"""Check the packaged UI/API and Docker client using disposable container state."""
import argparse
import json
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]


def run(arguments):
    return subprocess.run(arguments, cwd=ROOT, check=True, capture_output=True, text=True, timeout=30)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default="cronharbor:full-dev")
    image = parser.parse_args().image
    name = "cronharbor-full-smoke-" + uuid.uuid4().hex[:12]
    volume = name + "-data"
    run(["docker", "volume", "create", volume])
    try:
        run(["docker", "run", "--detach", "--name", name, "--init", "--read-only", "--cap-drop=ALL",
             "--security-opt=no-new-privileges:true", "--tmpfs", "/tmp:rw,nosuid,nodev,size=32m,mode=1777",
             "--mount", f"type=volume,src={volume},dst=/var/lib/cronharbor", "--publish", "127.0.0.1::8090", image])
        inspection = json.loads(run(["docker", "inspect", name]).stdout)[0]
        assert inspection["Config"]["User"] == "10001:10001"
        assert inspection["HostConfig"]["ReadonlyRootfs"]
        assert not any("docker.sock" in mount["Destination"] for mount in inspection["Mounts"])
        port = inspection["NetworkSettings"]["Ports"]["8090/tcp"][0]["HostPort"]
        base = f"http://127.0.0.1:{port}"

        def request(path, token=None):
            headers = {} if token is None else {"Authorization": "Bearer " + token}
            call = urllib.request.Request(base + path, headers=headers)
            try:
                with urllib.request.urlopen(call, timeout=5) as response:
                    return response.status, response.read()
            except urllib.error.HTTPError as error:
                return error.code, error.read()

        deadline = time.monotonic() + 20
        while True:
            try:
                if request("/api/v1/health/live")[0] == 200:
                    break
            except (OSError, TimeoutError):
                pass
            assert time.monotonic() < deadline, "Packaged API did not become live"
            time.sleep(0.1)
        status, page = request("/")
        assert status == 200 and b'<div id="app"></div>' in page
        assert request("/api/v1/status")[0] == 401
        created = json.loads(run(["docker", "exec", name, "cronharbor", "--state-dir", "/var/lib/cronharbor",
                                  "tokens", "create", "--role", "admin", "--namespace", "*", "--capability", "*"]).stdout)
        status, body = request("/api/v1/status", created["token"])
        assert status == 200 and json.loads(body)["sqlite_version"]
        client = run(["docker", "exec", name, "/usr/bin/docker", "--version"]).stdout.strip()
        print(json.dumps({"result": "passed", "image_id": inspection["Image"], "docker_client": client,
                          "verified": ["nonroot_readonly", "loopback_http", "packaged_dashboard", "bearer_required",
                                       "local_token_bootstrap", "authenticated_status", "docker_client_no_socket"]}, indent=2))
    finally:
        subprocess.run(["docker", "rm", "--force", name], capture_output=True, timeout=30, check=False)
        run(["docker", "volume", "rm", volume])


if __name__ == "__main__":
    main()
