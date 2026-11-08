# Outbound alert receiver contract

CronHarbor sends incident metadata to one configured HTTPS API. That application owns forwarding to Grokbot and tracking its processing. No receiver has been deployed by this repository. The disposable receiver in [test_webhooks.py](../../scripts/test_webhooks.py) exercises the integration contract.

The destination uses a separate ingestion-only bearer credential. Never give it a CronHarbor administrator token. LAN, loopback and public HTTPS endpoints use the same sender; private addresses require exact approvals and an internal CA can be supplied. There is no incoming webhook listener, insecure TLS option or log upload. Configuration is file-owned; see [the example](../../examples/optional/alerts.yaml) and [runbook](../RUNBOOK.md).

Requests use `POST`, `Content-Type: application/json`, `Authorization: Bearer <credential>`, `Idempotency-Key: <alert_id>` and a fixed sender user agent. Every retry uses identical JSON bytes and the same ID. The complete body is at most 8,192 UTF-8 bytes. Rust-owned `AlertEnvelope` in [contracts.rs](../../src/contracts.rs) defines the wire schema; dashboard types are generated from it.

```json
{
  "schema_version": 1,
  "alert_id": "immutable-alert-uuid",
  "instance_id": "installation-uuid",
  "incident_id": "incident-uuid",
  "incident_version": 1,
  "kind": "job_failed",
  "severity": "error",
  "occurred_at_us": 1794096000000000,
  "incident_opened_at_us": 1794096000000000,
  "workload_id": "workload-uuid",
  "namespace": "local",
  "workload_name": "nightly-import",
  "revision_id": "revision-uuid",
  "execution_id": "execution-uuid",
  "attempt_id": "attempt-uuid",
  "error_code": "EXIT_NONZERO",
  "summary": "Workload failed after its configured retries."
}
```

Identifiers and summary above are illustrative. Timestamps are UTC microseconds. Kinds are `job_failed`, `job_timed_out`, `execution_unknown`, `service_unhealthy`, `service_failed` and `service_recovered`. Job failure/timeout describes a final result after automatic retries; `UNKNOWN` is a separate unresolved uncertainty incident. Service health uses configured probe thresholds. Recovery uses the opening incident ID, its original opening time, and version 2. A later distinct incident gets a new ID.

The receiver must:

1. Authenticate the dedicated credential, cap input bytes, validate the exact versioned schema and apply per-credential rate limits.
2. Atomically insert a unique `(instance_id, alert_id)` with the original body. An identical duplicate returns 2xx. Conflicting content returns 409 without replacing evidence.
3. Commit durable acceptance before returning 2xx. Storage failure returns 5xx. A lost acknowledgement can repeat an already accepted alert.
4. Keep the greatest observed version for each incident. A delayed opening version 1 remains history and cannot reopen an incident recovered by version 2.
5. Forward to Grokbot through its own bounded, visible retry process. Receiver acceptance does not prove bot processing. Treat workload names, summaries and all model-facing content as data, never command authority.
6. Retain deduplication records for at least 30 days and unresolved incident state until resolved, then at least 30 days. Operator-requested retries of older retained alerts remain a reviewed duplicate risk.

The sender acknowledges any 2xx. Transport failures, timeouts, 408/425/429 and 5xx are retryable; redirects and other HTTP statuses fail. Automatic delivery is capped at 12 attempts within 24 hours from enqueue. Backoff starts at 30 seconds, doubles to one hour and adds deterministic 0–20% jitter. Integer `Retry-After` seconds can lengthen a delay up to one hour. Receiver bodies are discarded.

The queue holds at most 1,000 outstanding rows. Overflow is counted as suppression and never described as delivered. Terminal history is capped at 1,000 rows and seven days; SQLite file/WAL/backup sizes are separate. Manual retry sends one additional request with the original ID/body and does not restart the workload or reset automatic retries. Status/list/retry require installation-wide Admin authority.

Restore quarantine sends nothing and consumes no fresh source markers. Explicit restore resume suppresses the historical held queue and scans current actionable conditions. An individual retained expired alert can be retried after operator review. Intentional service stop preserves an existing incident as unresolved; only actual `READY` observation sends recovery. Administrative closure/decommission events are outside version 1.
