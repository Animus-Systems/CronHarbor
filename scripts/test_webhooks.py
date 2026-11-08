#!/usr/bin/env python3
"""Real daemon + private TLS receiver: durable deduplication, retries and restore.

The receiver is a disposable contract fixture, not the user's app or Grokbot.
"""
from __future__ import annotations

import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import socket
import sqlite3
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

from test_log_rotation import BINARY, manifest, object_value, query, run, stop_owner, wait_for


def envelope(body: bytes):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("Duplicate field")
            result[key] = value
        return result
    try:
        value = json.loads(body, object_pairs_hook=unique)
        if not isinstance(value, dict):
            return None
        if type(value.get("schema_version")) is not int or value["schema_version"] != 1 or type(value.get("incident_version")) is not int or value["incident_version"] not in (1, 2):
            return None
        if value.get("kind") not in ("job_failed", "job_timed_out", "execution_unknown", "service_unhealthy", "service_failed", "service_recovered") or value.get("severity") not in ("info", "error", "critical"):
            return None
        if any(type(value.get(key)) is not int or abs(value[key]) > 9007199254740991 for key in ("occurred_at_us", "incident_opened_at_us")):
            return None
        if any(not isinstance(value.get(key), str) or not 1 <= len(value[key].encode()) <= 256 for key in ("alert_id", "instance_id", "incident_id", "workload_id", "namespace", "workload_name", "revision_id")):
            return None
        if any(value.get(key) is not None and (not isinstance(value[key], str) or len(value[key].encode()) > 256) for key in ("execution_id", "attempt_id", "error_code")):
            return None
        if not isinstance(value.get("summary"), str) or len(value["summary"].encode()) > 512:
            return None
        return value
    except (ValueError, UnicodeError, TypeError):
        return None


def certificates(directory: Path):
    def openssl(*arguments):
        subprocess.run(["openssl", *arguments], cwd=directory, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=CronHarbor disposable test CA",
            "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign",
            "-addext", "subjectKeyIdentifier=hash",
            "-keyout", "ca.key", "-out", "ca.pem")
    openssl("req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
            "-keyout", "server.key", "-out", "server.csr")
    (directory / "extensions").write_text("subjectAltName=IP:127.0.0.1,DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n")
    openssl("x509", "-req", "-in", "server.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial",
            "-days", "1", "-extfile", "extensions", "-out", "server.pem")
    for path in directory.iterdir():
        path.chmod(0o600)


class Receiver:
    def __init__(self, directory: Path, credential: str):
        self.database = directory / "receiver.db"
        self.credential = credential
        self.lock = threading.Lock()
        self.requests: list[bytes] = []
        self.mode = "drop_first_ack"
        with sqlite3.connect(self.database) as connection:
            connection.executescript("CREATE TABLE alerts(instance_id TEXT,alert_id TEXT,body BLOB NOT NULL,PRIMARY KEY(instance_id,alert_id)); CREATE TABLE incidents(instance_id TEXT,incident_id TEXT,version INTEGER NOT NULL,PRIMARY KEY(instance_id,incident_id));")
        self.database.chmod(0o600)
        receiver = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_arguments):
                pass

            def do_POST(self):
                self.connection.settimeout(3)
                length = self.headers.get("Content-Length", "")
                if self.path != "/alerts" or self.headers.get("Authorization") != "Bearer " + receiver.credential:
                    self.send_error(401)
                    return
                if not length.isdecimal() or not 1 <= int(length) <= 8192:
                    self.send_error(413)
                    return
                body = self.rfile.read(int(length))
                if len(body) != int(length):
                    return
                payload = envelope(body)
                expected = {"schema_version", "alert_id", "instance_id", "incident_id", "incident_version", "kind", "severity",
                            "occurred_at_us", "incident_opened_at_us", "workload_id", "namespace", "workload_name", "revision_id",
                            "execution_id", "attempt_id", "error_code", "summary"}
                if not isinstance(payload, dict) or set(payload) != expected or payload["schema_version"] != 1 or payload["incident_version"] not in (1, 2):
                    self.send_error(422)
                    return
                if self.headers.get("Idempotency-Key") != payload["alert_id"]:
                    self.send_error(422)
                    return
                with receiver.lock:
                    receiver.requests.append(body)
                    mode = receiver.mode
                if mode in ("reject", "hold"):
                    self.send_response(401 if mode == "reject" else 503)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                with sqlite3.connect(receiver.database) as connection:
                    connection.execute("BEGIN IMMEDIATE")
                    previous = connection.execute("SELECT body FROM alerts WHERE instance_id=? AND alert_id=?", (payload["instance_id"], payload["alert_id"])).fetchone()
                    if previous and previous[0] != body:
                        connection.rollback()
                        self.send_error(409)
                        return
                    connection.execute("INSERT OR IGNORE INTO alerts VALUES(?,?,?)", (payload["instance_id"], payload["alert_id"], body))
                    connection.execute("INSERT INTO incidents VALUES(?,?,?) ON CONFLICT(instance_id,incident_id) DO UPDATE SET version=MAX(version,excluded.version)",
                                       (payload["instance_id"], payload["incident_id"], payload["incident_version"]))
                    connection.commit()
                if mode == "drop_first_ack":
                    with receiver.lock:
                        receiver.mode = "accept"
                    self.close_connection = True
                    self.connection.shutdown(socket.SHUT_RDWR)
                    return
                self.send_response(204)
                self.send_header("Content-Length", "0")
                self.end_headers()

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(directory / "server.pem", directory / "server.key")
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": .05}, daemon=True)
        self.thread.start()

    def set_mode(self, value):
        with self.lock:
            self.mode = value

    def received(self):
        with self.lock:
            return [json.loads(body) for body in self.requests]

    def accepted(self):
        with sqlite3.connect(self.database) as connection:
            return [json.loads(row[0]) for row in connection.execute("SELECT body FROM alerts")]

    def stop(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)
        assert not self.thread.is_alive()


def start(state: Path, config: Path):
    owner = subprocess.Popen([str(BINARY), "--state-dir", str(state), "server", "--config", str(config)],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    def ready():
        assert owner.poll() is None, "Webhook fixture daemon exited before readiness"
        try:
            return query(state, {"action": "status"})["data"]
        except (FileNotFoundError, ConnectionRefusedError):
            return None
    try:
        wait_for(ready)
        return owner
    except BaseException:
        stop_owner(owner)
        raise


def deliveries(state: Path):
    return object_value(run(state, "alerts", "list", "--limit", "100"))["items"]


def main():
    assert BINARY.is_file(), "Build Full and select CRONHARBOR_BINARY"
    with tempfile.TemporaryDirectory(prefix="ch-webhooks-", dir="/tmp") as temporary:
        private = Path(temporary)
        certificates(private)
        credential = "fixture-webhook-" + os.urandom(16).hex()
        token = private / "token"
        token.write_text(credential + "\n")
        token.chmod(0o600)
        receiver = Receiver(private, credential)
        config = private / "daemon.json"
        config.write_text(json.dumps({"alerts": {"endpoint": f"https://127.0.0.1:{receiver.server.server_port}/alerts",
            "tokenRef": {"provider": "file", "path": str(token)}, "allowedPrivateAddresses": ["127.0.0.1"], "caFile": str(private / "ca.pem")}}))
        config.chmod(0o600)
        state = private / "state"
        owner = start(state, config)
        try:
            def submit(body: bytes, identifier: str):
                request = urllib.request.Request(f"https://127.0.0.1:{receiver.server.server_port}/alerts", body,
                    {"Authorization": "Bearer " + credential, "Idempotency-Key": identifier, "Content-Type": "application/json"}, method="POST")
                try:
                    with urllib.request.urlopen(request, context=ssl.create_default_context(cafile=str(private / "ca.pem")), timeout=5) as response:
                        return response.status
                except urllib.error.HTTPError as error:
                    error.close()
                    return error.code
            def apply(value):
                definition = private / (value["metadata"]["name"] + ".json")
                definition.write_text(json.dumps(value))
                run(state, "config", "apply", "-f", str(definition))
            def completed(operation):
                result = wait_for(lambda: (row if row["state"] not in ("ACCEPTED", "RUNNING") else None)
                                  if (row := object_value(run(state, "operations", "inspect", operation["id"]))) else None)
                assert result["state"] == "SUCCEEDED", result
            job = manifest("failed", "raise SystemExit(3)")
            job["spec"]["logs"] = {"maxRunBytes": 4096}
            job["spec"]["retry"] = {"maxAttempts": 2, "policy": "idempotent", "backoffSeconds": 2}
            apply(job)
            operation = object_value(run(state, "jobs", "run", "fixture/failed", "--idempotency-key", "failed-job"))
            wait_for(lambda: any(row["state"] == "FAILED" for row in run(state, "executions")))
            assert object_value(run(state, "operations", "inspect", operation["id"]))["state"] in ("ACCEPTED", "RUNNING")
            assert receiver.received() == [], "An intermediate retry failure emitted an alert"
            first = wait_for(lambda: next((row for row in receiver.received() if row["kind"] == "job_failed"), None))
            pending = wait_for(lambda: next((row for row in deliveries(state) if row["alert_id"] == first["alert_id"] and row["state"] == "PENDING"), None))
            assert pending["attempt_count"] == 1
            assert len(receiver.accepted()) == 1
            stop_owner(owner)
            owner = start(state, config)
            wait_for(lambda: len([row for row in receiver.received() if row["alert_id"] == first["alert_id"]]) == 2, timeout=45)
            wait_for(lambda: next((row for row in deliveries(state) if row["alert_id"] == first["alert_id"] and row["state"] == "DELIVERED"), None))
            copies = [body for body in receiver.requests if json.loads(body)["alert_id"] == first["alert_id"]]
            assert copies[0] == copies[1] and len(receiver.accepted()) == 1
            assert submit(json.dumps({**first, "summary": "Conflicting body"}).encode(), first["alert_id"]) == 409
            assert submit(json.dumps({**first, "schema_version": True}).encode(), first["alert_id"]) == 422
            assert submit(b'{"schema_version":1,' + copies[0][1:], first["alert_id"]) == 422
            assert len(receiver.accepted()) == 1
            print(json.dumps({"checkpoint": "lost_ack_deduplicated_across_restart", "payload_sha256": hashlib.sha256(copies[0]).hexdigest()}), flush=True)

            healthy = private / "healthy"
            service = manifest("health", "import time;time.sleep(90)")
            service["kind"] = "Service"
            service["spec"]["logs"] = {"maxRunBytes": 4096}
            service["spec"]["service"] = {"desiredState": "RUNNING", "restart": "never", "startupGraceSeconds": 0,
                "readiness": {"command": sys.executable, "args": ["-c", "import pathlib,sys;sys.exit(0 if pathlib.Path(sys.argv[1]).is_file() else 1)", str(healthy)], "intervalSeconds": 1, "timeoutSeconds": 1}}
            apply(service)
            unhealthy = wait_for(lambda: next((row for row in receiver.accepted() if row["kind"] == "service_unhealthy"), None))
            completed(object_value(run(state, "services", "stop", "fixture/health")))
            time.sleep(.3)
            assert not any(row["kind"] == "service_recovered" for row in receiver.accepted()), "Intentional stop falsely recovered an incident"
            healthy.write_text("ready")
            completed(object_value(run(state, "services", "start", "fixture/health")))
            recovered = wait_for(lambda: next((row for row in receiver.accepted() if row["kind"] == "service_recovered"), None))
            assert recovered["incident_id"] == unhealthy["incident_id"] and recovered["incident_version"] == 2
            assert recovered["incident_opened_at_us"] == unhealthy["incident_opened_at_us"]
            opening = next(body for body in receiver.requests if json.loads(body)["alert_id"] == unhealthy["alert_id"])
            assert submit(opening, unhealthy["alert_id"]) == 204
            with sqlite3.connect(receiver.database) as connection:
                version = connection.execute("SELECT version FROM incidents WHERE instance_id=? AND incident_id=?", (unhealthy["instance_id"], unhealthy["incident_id"])).fetchone()[0]
            assert version == 2, "A late opening message reopened a recovered incident"

            receiver.set_mode("reject")
            rejected = manifest("rejected", "raise SystemExit(4)")
            rejected["spec"]["logs"] = {"maxRunBytes": 4096}
            apply(rejected)
            run(state, "jobs", "run", "fixture/rejected", "--idempotency-key", "rejected-job")
            failed = wait_for(lambda: next((row for row in deliveries(state) if row["workload_name"] == "rejected" and row["state"] == "FAILED"), None))
            receiver.set_mode("accept")
            intent = object_value(run(state, "alerts", "retry", failed["alert_id"], "--idempotency-key", "reviewed-retry"))
            repeated = object_value(run(state, "alerts", "retry", failed["alert_id"], "--idempotency-key", "reviewed-retry"))
            assert intent["id"] == repeated["id"]
            retried = wait_for(lambda: next((row for row in deliveries(state) if row["alert_id"] == failed["alert_id"] and row["state"] == "DELIVERED"), None))
            assert retried["attempt_count"] == 2

            receiver.set_mode("hold")
            held = manifest("held", "raise SystemExit(5)")
            held["spec"]["logs"] = {"maxRunBytes": 4096}
            apply(held)
            run(state, "jobs", "run", "fixture/held", "--idempotency-key", "held-job")
            old = wait_for(lambda: next((row for row in deliveries(state) if row["workload_name"] == "held" and row["state"] == "PENDING" and row["attempt_count"] == 1), None))
            snapshot = object_value(run(state, "backup", "outbox"))
            stop_owner(owner)
            receiver.set_mode("accept")
            before = len(receiver.received())
            state = private / "restored"
            run(state, "restore", snapshot["path"], "--evidence", "disposable outbox restore drill")
            owner = start(state, config)
            quarantined = next(row for row in deliveries(state) if row["alert_id"] == old["alert_id"])
            assert quarantined["state"] == "RESTORE_HELD"
            time.sleep(.3)
            assert len(receiver.received()) == before, "Restore quarantine sent a webhook"
            run(state, "resume", "--cutoff-us", str(time.time_ns() // 1000), "--evidence", "former owner stopped and all fixture effects reconciled")
            expired = next(row for row in deliveries(state) if row["alert_id"] == old["alert_id"])
            assert expired["state"] == "EXPIRED" and expired["last_error_code"] == "RESTORE_REPLAY_SUPPRESSED"
            wait_for(lambda: any(row["kind"] == "execution_unknown" for row in receiver.accepted()))
            assert not any(row["alert_id"] == old["alert_id"] for row in receiver.accepted()), "Restored historical queue was replayed"
            assert object_value(run(state, "alerts", "status"))["restore_suppressed_count"] >= 1
            assert all("logs" not in row and credential not in json.dumps(row) for row in receiver.received())
            print(json.dumps({"result": "passed", "verified": ["real_TLS_CA_and_bearer", "final_failure_only", "stable_id_body_lost_ack_restart",
                "receiver_atomic_deduplication", "service_stop_recovery_correlation", "one_manual_retry_idempotent", "restore_held_no_replay", "post_resume_current_UNKNOWN", "metadata_only"]}), flush=True)
        finally:
            stop_owner(owner)
            receiver.stop()


if __name__ == "__main__":
    main()
