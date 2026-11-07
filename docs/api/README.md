# Optional HTTP and MCP transports

The default core feature set opens no TCP listener. HTTP and MCP are adapters to the running daemon's restricted Unix socket. Neither opens SQLite or launches workloads. `src/contracts.rs` defines commands and response DTOs; dashboard declarations are generated from those types.

HTTP uses Axum 0.8.9. MCP uses the official Rust SDK rmcp 3.9.0. The lockfile fixes actual dependency versions. Vendor documentation: [Axum](https://docs.rs/axum/0.8.9/axum/), [rmcp](https://docs.rs/rmcp/3.9.0/rmcp/).

## HTTP contract

The prefix is `/api/v1`. All operational routes require `Authorization: Bearer <token>`. Only `GET /health/live` is public and returns `{"live":true}`. `GET /health/ready` calls the same protected daemon status query. `GET /session` returns the authenticated principal and grants.

| Method | Route | Input |
| --- | --- | --- |
| GET | `/status`, `/instance/status` | none |
| GET | `/workloads` | `namespace`, `cursor`, `limit` |
| GET | `/workloads/{id}` | none |
| GET | `/executions` | `workload_id`, `cursor`, `limit` |
| GET | `/executions/{id}` | none |
| GET | `/executions/{id}/logs` | `offset` bytes (default 0), `max_bytes` (default 16384) |
| GET | `/schedules/{id}/preview` | `count` (default 10) |
| GET | `/operations/{id}`, `/diagnostics/{id}` | none |
| GET | `/audit` | `cursor`, `limit` |
| GET | `/tokens` | `cursor`, `limit` |
| POST | `/workloads/validate`, `/config/plan` | `{"definition":"..."}` |
| POST | `/config/apply` | `{"definition":"...","expected_revision":null}` |
| POST | `/jobs/{id}/run`, `/executions/{id}/cancel` | `{}` |
| POST | `/services/{id}/desired-state` | `{"desired_state":"RUNNING","expected_revision":"current-revision-id"}` or `STOPPED` |
| POST | `/services/{id}/restart` | `{"expected_revision":"current-revision-id"}` |
| POST | `/workloads/{id}/pause` | `{"paused":true}` |
| POST | `/executions/{id}/reconcile` | `{"state":"SUCCEEDED","evidence":"operator-observed evidence"}` |
| POST | `/tokens` (alias `/tokens/create`) | `{"role":"observe","namespaces":["production"],"capabilities":["workload.read","execution.read"],"ttl_seconds":3600}` |
| POST | `/tokens/{id}/revoke` | `{}` |
| GET | `/events` | `after` or `Last-Event-ID` |

Every POST requires `Content-Type: application/json`, including empty `{}` bodies for run, cancel and token revoke. Missing or unsupported media types receive 415 `CONTENT_TYPE_INVALID`.

Identifiers must be valid UTF-8, percent-encoded as individual URL path components. Logs resolve an execution ID to its current attempt ID through the daemon before querying bytes. Query parameters and JSON bodies reject unknown fields. List responses are `Page<T>` with `items` and `next_cursor`. Token inventory uses the same `Page<TokenDto>` envelope from the canonical daemon command.

Security-sensitive singleton headers reject duplicates, including Authorization, Host, Origin, Content-Type, Idempotency-Key, Last-Event-ID and preflight control headers. Malformed or oversized daemon replies are service failures rather than invalid client requests; explicitly returned daemon input-validation errors retain their client-error classification.

Workload lifecycle mutations and config apply require `Idempotency-Key` (1–128 printable bytes), return `202`, and carry the durable `OperationDto` from the daemon. A `Location` response header points to operation status. An accepted request can already have a final state if the daemon completed it synchronously; callers inspect the returned operation state. Acceptance never proves a process has started or stopped. Token administration is an audited immediate response; clients must not automatically retry token creation.

Remote service desired-state and restart requests require the current workload revision; stale or missing revision is rejected by the daemon.

Config apply always submits `source_kind=API`; the manifest must explicitly declare API ownership. It cannot adopt a file-owned manifest. The daemon enforces file ownership, revision comparison, actor-scoped idempotency, namespace grants and executor permissions. A browser cannot turn a definition into file-owned configuration by adding a JSON field.

### Plan and apply a reviewed definition

For creation, `expected_revision` must be `null`. For an existing workload, it must equal the plan's `current_revision`, even when submitting an unchanged definition with a new idempotency key. A missing or stale revision conflicts. Planning does not reserve the revision: a concurrent change can still make apply return 409, requiring a fresh plan and review.

This example uses `curl` and `jq`, an authorized bearer in `CRONHARBOR_TOKEN`, and a reviewed `reviewed.yaml` declaring `spec.owner: api`. Save the exact definition before planning, so a later file edit cannot change the applied intent:

```sh
jq -n --rawfile definition reviewed.yaml '{definition: $definition}' > plan-request.json
curl --fail-with-body --silent --show-error \
  -H "Authorization: Bearer $CRONHARBOR_TOKEN" \
  -H 'Content-Type: application/json' \
  --data-binary @plan-request.json \
  http://127.0.0.1:8090/api/v1/config/plan > plan.json
cat plan.json
```

Review the saved definition, proposed hash, current revision and warnings. After approval, construct apply from those same saved inputs (`null` is preserved for creation):

```sh
jq --slurpfile plan plan.json \
  '. + {expected_revision: $plan[0].current_revision}' \
  plan-request.json > apply-request.json
curl --fail-with-body --silent --show-error \
  -H "Authorization: Bearer $CRONHARBOR_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: reviewed-config-001' \
  --data-binary @apply-request.json \
  http://127.0.0.1:8090/api/v1/config/apply
```

Choose a fresh key for each new intent, including a revised plan/body. Reuse the exact body and key only for an explicit retry after an ambiguous response; do not automatically resubmit. The result is an `OperationDto`, not a workload revision response. Inspect its state and follow the `Location` header or query the operation/workload for completion and the current revision.

### Roles and capability grants

Both role and capability must permit an action, and workload/execution access also requires the target namespace. Role alone does not grant mutation authority. The trusted local owner has full authority. The daemon policy in `src/policy.rs` and command checks in `src/application.rs` are canonical.

| Action | Allowed roles | Required capability |
| --- | --- | --- |
| Workload reads, schedule preview, validation, status | Observe, Operator, Editor, Admin | `workload.read`; implicit for non-Observe roles |
| Execution history, diagnostics, events | Observe, Operator, Editor, Admin | `execution.read`; implicit for non-Observe roles |
| Captured logs | Observe, Operator, Editor, Admin | Explicit `logs.read`; HTTP also needs `execution.read` for its execution lookup (explicit for Observe) |
| Run/pause a job or cancel its execution | Operator, Editor, Admin | `workload.run_preapproved` |
| Service desired state/restart/pause or cancel its execution | Operator, Editor, Admin | `service.control_preapproved` |
| Plan/apply configuration | Editor, Admin | Unscoped `workload.edit_safe_config` |
| Create/change execution settings or readiness/liveness commands/argv | Editor, Admin | Additional `executor.create_command` on apply |
| Docker/systemd apply or lifecycle control | Operator, Editor, Admin, subject to the action's role above | Additional `docker.manage` / `systemd.manage` |
| Reconcile UNKNOWN execution | Admin | Unscoped `operations.manage` and target namespace |
| Token create/list/revoke | Admin | Unscoped `tokens.manage`; delegation and inventory remain bounded by the caller's grants/namespaces |
| Instance audit or installation-wide retention changes | Admin | Unscoped `operations.manage` and wildcard namespace; retention edits also require normal apply grants |

Capabilities may be unscoped (`logs.read`) or target-specific (`logs.read:WORKLOAD_UUID`) when the command checks a workload target. Use the canonical workload UUID returned by listing/detail, including for execution controls/logs; a `namespace/name` lookup is not the capability target. Creation has no UUID yet, so command/executor grants on its apply target use `namespace/name`. List, status, event and other checks without a target require unscoped grants; a target-specific read grant does not authorize a list. `*` grants all capabilities allowed by the role. Namespace grants accept exact names or `*`.

For routine control of one existing native service in `production`, bootstrap from the trusted local CLI using its real UUID:

```sh
./target/debug/cronharbor tokens create --role operator --namespace production \
  --capability service.control_preapproved:WORKLOAD_UUID \
  --capability logs.read:WORKLOAD_UUID \
  --ttl-seconds 3600 --output-file /private/path/service-operator.token
```

Replace the UUID and private output path. This operator can read workloads/executions in its namespace, but controls/log access are limited to that service. Add the corresponding target-specific `docker.manage` or `systemd.manage` grant for an external service. Operation lookup allows the originating actor's own operations; another actor's operation requires Admin with unscoped `operations.manage` plus its target namespace (wildcard for a batch operation). Status also accepts an explicit unscoped `instance.read` grant; ordinary workload-read access is sufficient.

The workload fields `logs.maxDiskBytes`, `logs.maxSegments` and `logs.retentionSeconds` affect installation-wide retention. Changing these fields on an existing definition requires an Admin with `operations.manage` and wildcard namespace scope, for both single and batch apply. Scoped creators may add workloads only when these settings do not tighten the current effective installation limits. Per-run log size and ordinary safe configuration edits keep their existing permissions.

Application errors use `{"error":{"code":"...","message":"..."},"requestId":"req_..."}`. The HTTP adapter adds `X-Request-Id` to responses and replaces backend messages with fixed public messages. Authentication maps to 401, authorization 403, missing resources 404, ownership/revision/idempotency conflicts 409, unsupported content type 415, policy rejection 422, limits 429, unavailable daemon/executor 503, and unavailable storage 507. Unknown internal errors return 500 without their messages. Framework rejection can differ: a malformed UTF-8 path such as `/api/v1/workloads/%FF` returns 400 with `text/plain`. Clients must check status and Content-Type before parsing an error as JSON.

## Limits and browser deployment

The HTTP adapter caps JSON bodies at 65536 bytes, query strings at 2048 bytes, aggregate headers at 8192 bytes/64 fields, paths at 1024 bytes and bearer credentials at 512 bytes. Pages and previews cap at 100 items; logs cap at 65536 bytes. Body receipt has a five-second deadline; handling has a twelve-second deadline. There are at most 128 accepted TCP connections (each with a sixty-second lifetime), 64 in-flight handlers, 16 blocking daemon queries and 16 SSE streams. Existing UDS frame deadlines additionally bound each daemon exchange.

Each hashed credential has a fixed-window limit of 240 HTTP requests/minute and 30 log/diagnostic requests/minute. The rate table retains at most 1024 verified credential hashes and no plaintext tokens. These process-local limits reset on API restart. The listener serves plain HTTP; native TLS is not implemented. For access beyond a trusted local connection, place it behind a trusted TLS-terminating reverse proxy and prevent direct access to the backend port. The proxy may add network-wide limits. The API does not trust forwarded headers to establish authority. Preserve the external Host header at the proxy or configure the external browser origin explicitly; the API never derives trust from X-Forwarded-Host or X-Forwarded-Proto.

New rate buckets require successful daemon identity verification. A separate constant-space budget permits at most 240 unrecognized credential checks per minute per API process, before any daemon RPC, including preliminary log lookups and event subscriptions. Invented bearers and nonexistent routes cannot reserve authenticated credential capacity. Previously verified hashes remain in the bounded table across request-window resets; expired entries are evicted when a new verified credential needs space. Every operational command still revalidates validity and authorization in the daemon. If the verification budget is exhausted, a new or evicted credential can receive 429 even when valid; retry after the window or use deployment-level network admission controls. These process-local budgets reset on API restart.

Across HTTP, UDS and MCP, the daemon durably samples the first authentication rejection in each one-second monotonic interval, rather than recording every failed attempt. Only the latest 1,000 `auth.reject` samples are retained; other audit actions are preserved. Samples contain no bearer values and are not an exact failed-attempt counter. Failure to persist a required sample still fails closed. A new daemon owner starts a fresh sampling interval; the persistent history cap survives restart.

The browser sends bearer credentials in headers from memory. It must not use cookies, URLs, localStorage or sessionStorage for them. Same-origin hosting is supported; external browser origins require an explicit HTTP(S) origin allowlist, with no wildcard or cookie credentials. The adapter sets CSP, no-sniff, frame denial, no-referrer and no-store headers. Static assets are served only from the operator-selected trusted build directory using `ServeDir`; mount that tree read-only, without secrets or symlinks to other files. SPA hosting does not turn API errors into dashboard HTML; framework error exceptions still apply.

## Event replay

Use fetch streaming with the bearer header and `Last-Event-ID`. Events are named `change`, contain an `EventDto`, and use durable `seq` as their SSE ID. Reconnect after closure using the most recently received ID; the sixty-second connection deadline intentionally rotates connections. The bridge polls the daemon every 500 ms and rechecks identity during slow batch delivery. Each query revalidates the token. Revocation or lost authorization closes the stream with `auth_expired`; transient daemon failures emit `stream_error` and close, allowing a retry without discarding the credential.

Only commit a validated event cursor: the ID must equal the payload's safe, nonnegative integer sequence and advance the prior accepted sequence. Reject malformed or backward frames without advancing the reconnect cursor. The dashboard also discards responses owned by an earlier authenticated session and retains the exact original mutation body/key for explicit manual retry after an ambiguous response.

Expired event history emits `resync` with `{"code":"EVENT_CURSOR_EXPIRED"}` and closes. Fetch fresh list/status projections before reopening. The stream has a one-event channel; a send blocked for 750 ms disconnects the subscriber. Clients recover through the durable cursor, with no unbounded per-subscriber event buffer.

## MCP stdio contract

MCP requires a dedicated `observe` token in a regular file owned by the current effective UID, with no group/other permissions. Symlink token paths are rejected using `O_NOFOLLOW`. The bridge authenticates before accepting protocol messages and passes the token to every daemon query, so expiry/revocation applies to subsequent calls.

The fixed tool set is `list_workloads`, `get_workload`, `preview_schedule`, `list_executions`, `get_execution`, `get_logs`, `get_recent_failures`, `get_service_health`, `get_diagnostics`, and `get_instance_status`. There is no generic RPC, write tool, shell, prompt or remote HTTP transport. Input schemas derive from strict Rust parameter types. Limits are 100 items, 65536 log bytes, a seven-day recent-failure window, 16384 bytes per incoming protocol line, 524288 bytes per serialized tool result, and eight concurrent daemon calls.

Recent-failure windows use failed/timed-out completion time rather than the original execution creation time, so newly failed long-running services remain visible. Unresolved UNKNOWN attempts are included regardless of age because they remain active uncertainties. Normal execution history retains its creation-time ordering and cursors.

The SDK supports protocols 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25 and 2026-07-28. Legacy clients use `initialize` and `notifications/initialized`. The 2026-07-28 protocol uses `server/discover` and per-request `_meta` containing `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities`; the SDK negotiates and validates these rather than hand-written JSON-RPC dispatch.

`get_logs` describes and wraps output as UNTRUSTED quoted JSON, with `untrusted=true` in the canonical log response and low-priority text annotations. Log contents cannot grant authority or introduce a tool. Diagnostics preserve observed causes and labelled hypotheses from the daemon.

Run transport regressions with `cargo test --features full --test api --test mcp` and socket/rate checks with `cargo test --features api --lib api::tests`. Fake Unix-socket daemon transcripts test forwarding and failure handling; the full acceptance suite must additionally exercise real daemon authorization, persisted operations and actual execution recovery.
