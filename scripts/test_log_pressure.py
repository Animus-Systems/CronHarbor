#!/usr/bin/env python3
"""Compare disposable daemons under injected log-only storage delays/failures.

macOS fixture: its temporary interposer affects only the selected daemon's log
files. No filesystem is mounted or filled and no production configuration changes.
"""
from __future__ import annotations

import concurrent.futures
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

from test_log_rotation import BINARY, manifest, query, stop_owner, wait_for

SHIM = r'''
#include <sys/types.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <limits.h>
static int selected(int fd) {
    char path[PATH_MAX];
    const char *root = getenv("CH_FIXTURE_LOG_ROOT");
    return root && fcntl(fd, F_GETPATH, path) == 0 &&
        strncmp(path, root, strlen(root)) == 0 && path[strlen(root)] == '/';
}
static void observed(char event) {
    const char *path = getenv("CH_FIXTURE_EVENTS");
    int fd = path ? open(path, O_WRONLY | O_APPEND | O_CREAT, 0600) : -1;
    if (fd >= 0) { write(fd, &event, 1); close(fd); }
}
static ssize_t fixture_write(int fd, const void *buf, size_t count) {
    if (selected(fd)) {
        observed('w');
        usleep(25000);
        if (getenv("CH_FIXTURE_FULL")) {
            observed('e'); errno = ENOSPC; return -1;
        }
    }
    return write(fd, buf, count);
}
static int fixture_fsync(int fd) {
    if (selected(fd)) { observed('s'); usleep(25000); }
    return fsync(fd);
}
#define INTERPOSE(replacement, replacee) \
    __attribute__((used)) static struct { const void *new; const void *old; } \
    pair_##replacee __attribute__((section("__DATA,__interpose"))) = \
        { (const void *)(unsigned long)&replacement, (const void *)(unsigned long)&replacee };
INTERPOSE(fixture_write, write)
INTERPOSE(fixture_fsync, fsync)
'''


def case(private: Path, binary: Path, library: Path, label: str, full: bool, rolling: bool):
    directory = private / label
    directory.mkdir(mode=0o700)
    state = directory / "state"
    events = directory / "events"
    produced = 400 * 65536
    program = "import sys,time\nfor i in range(400):\n sys.stdout.buffer.write(b'x'*65536);sys.stdout.flush();time.sleep(.01)\n"
    noisy = manifest("pressure", program)
    noisy["spec"]["logs"] = {"maxRunBytes": 131072}
    if rolling:
        noisy["spec"]["logs"]["rotation"] = {"segmentBytes": 65536, "maxSegments": 2}
    due_at = time.time_ns() // 1000 + 2_000_000
    due = manifest("due", "print('due')")
    due["kind"] = "ScheduledJob"
    due["spec"]["logs"] = {"maxRunBytes": 4096}
    due["spec"]["schedule"] = {"type": "interval", "intervalSeconds": 3600,
                                      "anchorAtUs": due_at}
    definitions = directory / "definitions"
    definitions.mkdir()
    for name, value in [("pressure", noisy), ("due", due)]:
        (definitions / (name + ".json")).write_text(json.dumps(value))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    environment = dict(os.environ, DYLD_INSERT_LIBRARIES=str(library),
                       CH_FIXTURE_LOG_ROOT=str((state / "logs").resolve()), CH_FIXTURE_EVENTS=str(events))
    if full:
        environment["CH_FIXTURE_FULL"] = "1"
    owner = subprocess.Popen([str(binary), "--state-dir", str(state), "server", "--file", str(definitions),
                              "--listen", f"127.0.0.1:{port}"], env=environment,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        def ready():
            assert owner.poll() is None, "Fixture daemon exited"
            try:
                return query(state, {"action": "status"})["data"]
            except (FileNotFoundError, ConnectionRefusedError):
                return None
        wait_for(ready)
        token_file = directory / "token"
        created = subprocess.run([str(binary), "--state-dir", str(state), "tokens", "create", "--role", "admin",
                                  "--namespace", "*", "--capability", "*", "--output-file", str(token_file)],
                                 capture_output=True, timeout=10)
        assert created.returncode == 0, created.stderr
        credential = token_file.read_text().strip()
        # Adapter invocations deliberately use each selected binary, including baseline.
        launched = subprocess.run([str(binary), "--state-dir", str(state), "jobs", "run", "fixture/pressure",
                                   "--idempotency-key", "pressure"], capture_output=True, timeout=10)
        assert launched.returncode == 0, launched.stderr
        def active():
            rows = query(state, {"action": "executions", "workload_id": None, "cursor": None, "limit": 100})["data"]["items"]
            return next((row for row in rows if row["name"] == "pressure"), None)
        target = wait_for(active)
        def sample(kind):
            began = time.monotonic()
            if kind == "cli":
                result = subprocess.run([str(binary), "--state-dir", str(state), "logs", target["attempt_id"], "--max-bytes", "4096"],
                                        capture_output=True, timeout=10)
                assert result.returncode == 0 or rolling and b"LOG_CURSOR_EXPIRED" in result.stderr, result.stderr.decode(errors="replace")
                assert len(result.stdout) < 1048576
            else:
                offset = 0
                if rolling:
                    offset = query(state, {"action": "log_info", "attempt_id": target["attempt_id"]})["data"]["retained_from_offset"]
                request = urllib.request.Request(f"http://127.0.0.1:{port}/api/v1/executions/{target['execution_id']}/logs?offset={offset}&max_bytes=4096",
                                                 headers={"Authorization": "Bearer " + credential})
                try:
                    with urllib.request.urlopen(request, timeout=10) as response:
                        assert len(response.read(1048577)) <= 1048576
                except urllib.error.HTTPError as error:
                    payload = error.read(65537)
                    assert rolling and error.code == 409 and len(payload) <= 65536
                    assert json.loads(payload)["error"]["code"] == "LOG_CURSOR_EXPIRED"
            return (time.monotonic() - began) * 1000
        latencies = []
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as readers:
            for _ in range(20):
                futures = [readers.submit(sample, kind) for kind in ("cli", "rest")]
                latencies.extend(f.result() for f in futures)
                time.sleep(.1)
        def executions():
            response = query(state, {"action": "executions", "workload_id": None, "cursor": None, "limit": 100})
            assert response["error"] is None
            return response["data"]["items"]
        rows = wait_for(lambda: (rows if any(r["name"] == "pressure" and r["state"] == "SUCCEEDED" for r in rows)
                                 and any(r["name"] == "due" for r in rows) else None) if (rows := executions()) else None)
        pressure = next(r for r in rows if r["name"] == "pressure")
        due_row = next(r for r in rows if r["name"] == "due")
        detail = query(state, {"action": "execution", "id": due_row["execution_id"]})
        assert detail["error"] is None
        faults = events.read_bytes()
        assert b"w" in faults, "Storage injection did not run"
        if full:
            assert b"e" in faults, f"ENOSPC injection did not run: {faults!r}"
        started_at = detail["data"]["started_at_us"]
        assert isinstance(started_at, int), "Unrelated due job did not start"
        result = {"case": label, "produced_bytes": produced, "read_samples": len(latencies),
                  "max_read_ms": round(max(latencies), 2), "mean_read_ms": round(sum(latencies)/len(latencies), 2),
                  "due_dispatch_lag_ms": round((started_at - due_at) / 1000, 2),
                  "injected_writes": faults.count(b"w"), "injected_enospc": faults.count(b"e")}
        if rolling:
            metadata = query(state, {"action": "log_info", "attempt_id": pressure["attempt_id"]})
            assert metadata["error"] is None
            result["capture"] = metadata["data"]
            assert metadata["data"]["written_end_offset"] + metadata["data"]["known_discarded_bytes"] == produced, "Capture accounting lost produced bytes"
        print(json.dumps(result), flush=True)
    finally:
        stop_owner(owner)


def main():
    assert sys.platform == "darwin", "This isolated storage interposer currently requires macOS"
    baseline = Path(os.environ["CRONHARBOR_BASELINE_BINARY"]).resolve()
    assert baseline.is_file() and BINARY.is_file()
    with tempfile.TemporaryDirectory(prefix="ch-pressure-", dir="/tmp") as temporary:
        private = Path(temporary)
        source = private / "storage.c"
        source.write_text(SHIM)
        library = private / "storage.dylib"
        subprocess.run(["clang", "-dynamiclib", "-Wall", "-Werror", "-o", str(library), str(source)], check=True)
        for full in [False, True]:
            mode = "full" if full else "slow"
            case(private, baseline, library, "baseline-" + mode, full, False)
            case(private, BINARY, library, "rolling-" + mode, full, True)


if __name__ == "__main__":
    main()
