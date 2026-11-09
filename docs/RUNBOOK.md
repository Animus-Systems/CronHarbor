# Running CronHarbor

The application is a single-node Rust scheduler, service supervisor and local command service with optional REST/SSE, read-only MCP and a static Vue dashboard. Software features and measured release acceptance are separate: see the verification record in `IMPLEMENTATION.md`.

## Build and start the complete application

Source builds require a Unix host, the pinned Rust toolchain in `rust-toolchain.toml`, Node.js 22 and npm. Run these commands from the repository root; the example daemon configuration uses paths relative to the process working directory, not to the YAML file.

```sh
npm ci --prefix web/dashboard
npm run build --prefix web/dashboard
~/.cargo/bin/cargo build --locked --features full
./target/debug/cronharbor server --config examples/optional/daemon.yaml
```

Open `http://127.0.0.1:8090`. In another terminal, bootstrap an administrative credential through the owner-only local socket:

```sh
./target/debug/cronharbor tokens create --role admin --namespace '*' --capability '*' --output-file /tmp/cronharbor-admin.token
```

Read that private file locally and enter its value in the dashboard's password field. The browser holds it in memory; refreshing or disconnecting clears it. Keep the file private, remove it when no longer needed, and use a narrower token for routine work. The server stores only a hash, role, grants, namespaces, expiry and revocation state. `tokens list` and `tokens revoke TOKEN_ID` are audited controls.

Use the [role/capability reference and scoped operator example](api/README.md#roles-and-capability-grants) to issue routine credentials instead of distributing the bootstrap Admin token.

`--state-dir PATH` selects the installation and must be supplied to clients too. Existing directories must be owned by the daemon user with mode 0700. The local socket is `PATH/control.sock`, mode 0600. Linux additionally verifies the peer UID; other Unix platforms rely on the owner-only directory and socket permissions. Exactly one daemon owns that directory/database.

The Core build uses no TCP listener or frontend runtime:

```sh
~/.cargo/bin/cargo build --locked
./target/debug/cronharbor server --file examples/hello.yaml
```

The `api` feature includes REST/SSE without MCP; `full` includes API and MCP. Vue builds to static assets and has no production Node.js server. Native full builds serve an explicit `--ui-dir`, or the built repository dashboard when present. API and MCP always call the daemon over its local socket.

### Installed systemd service

`packaging/cronharbor.service` expects an existing `cronharbor` user/group, the binary at `/usr/local/bin/cronharbor`, and configuration at `/etc/cronharbor/daemon.yaml`. Its working directory is `/var/lib/cronharbor`; copying the checkout example unchanged will fail to find its manifests. Install reviewed definitions and use absolute paths, for example:

```yaml
workloads:
  - /etc/cronharbor/workloads/hello.yaml
paused: true
listen: 127.0.0.1:8090
uiDirectory: /usr/share/cronharbor/ui
allowedOrigins: []
```

Build/install an API or Full binary for `listen` and `uiDirectory`; for Core, omit those fields and `allowedOrigins`. Install the built dashboard at the selected UI path. Keep configuration and manifests controlled by the operator and readable by the service user; keep secrets and executor policy private to that user. State must be owned by `cronharbor` with mode 0700. The supplied unit creates its state directory but does not install the user, binary, definitions or UI assets. Start paused and inspect plans and status. `paused: true` reapplies the pause on every startup. After bootstrap review, set it to `false` or remove it before explicitly resuming if dispatch should remain enabled across restarts. Setting it to `false` does not clear an already persisted pause. Use the same service UID and `--state-dir` for trusted local administration.

## Docker profiles

```sh
docker compose up --build -d
docker compose exec cronharbor cronharbor --state-dir /var/lib/cronharbor status
docker compose down
```

The default Compose profile is Core: UID 10001, a read-only root filesystem, private persistent state, no published ports and no Docker socket. For the complete dashboard:

```sh
docker compose -f compose.full.yaml up --build -d
docker compose -f compose.full.yaml exec cronharbor cronharbor --state-dir /var/lib/cronharbor tokens create --role admin --namespace '*' --capability '*'
```

The full profile binds host loopback port 8090. For remote use, put a TLS reverse proxy in front, preserve the intended Host or configure exact allowed browser origins, and block direct backend access. The HTTP adapter does not terminate TLS or trust forwarded client identity headers. Do not put secrets or symlinks to secrets in the static UI directory.

Dockerfile targets `core`, `api`, `full` and `checks` build the respective binaries/application or run the complete Rust test and Clippy suite. The full image includes static Vue assets and a Docker CLI for explicitly enabled external execution; it still has no default Docker daemon access.

## Definitions, plans and ownership

```sh
./target/debug/cronharbor config validate -f examples
./target/debug/cronharbor config plan -f examples
./target/debug/cronharbor config apply -f examples
./target/debug/cronharbor jobs list --namespace local
./target/debug/cronharbor jobs preview WORKLOAD_ID --count 10
./target/debug/cronharbor jobs run WORKLOAD_ID --idempotency-key operator-intent-001
```

Directory apply validates the entire batch first and commits all revisions atomically. Local FILE apply without `--expected-revision` resolves the current revision inside the daemon; supply that option when applying one specific reviewed revision with compare-and-swap. Remote API updates still require an expected revision. Repeating the same file contents, explicit revision option and idempotency key recovers the original operation without reapplying it. Older CLI updates embedded implicitly planned revisions in their requests; the new CLI cannot infer those old requests. An old single-file update needs its original expected revision explicitly to replay. Requests are bounded, so an exceptionally large batch should be split into separately reviewed batches rather than bypassing the limit.

File-owned definitions remain read-only through the dashboard/API; use local file apply to change them. API-created definitions must say `owner: api`. The transport cannot silently convert an existing FILE workload into an editable API workload. Lifecycle pause/run/service operations are separate authorized actions.

YAML/JSON configuration is limited to 64 KiB of source and 64 KiB of expanded scalar content, with separate depth, event and anchor-storage bounds. Ordinary aliases that fit those limits remain supported. Parsed values must match the configuration schema: boolean and number fields require those types; quote text that looks like a number, boolean or `null`/`~`, including mapping keys. Use `[]` or `{}` for empty collections, and give each string mapping key only once. Implicit scalar-to-string and blank-to-collection coercions, duplicate mapping keys and application-specific YAML tags accepted by the previous parser are rejected. Standard YAML tags remain subject to schema validation. Persisted canonical JSON uses explicit types and unique keys and remains readable. Validate reviewed workload files with `config validate -f PATH` before upgrading; correct the same source-format issues in daemon configuration.

Scheduled jobs support five numeric cron fields, IANA time zones, explicit DST fold/gap selection, anchored intervals, bounded skip/latest/catch-up, allow/forbid concurrency and explicit safe-retry policy. One-shot jobs run only on manual request. No implicit shell is added; commands and argv are separate. Retries default to one attempt. UNKNOWN effects never automatically retry, even under an idempotent retry policy.

Cancelling during retry backoff removes the pending retry and cancels the execution. The failed attempt remains in history with its original error; the original run operation reports cancellation.

## Services and evidence

```sh
./target/debug/cronharbor services list
./target/debug/cronharbor services start WORKLOAD_ID
./target/debug/cronharbor services restart WORKLOAD_ID
./target/debug/cronharbor services stop WORKLOAD_ID
./target/debug/cronharbor executions
./target/debug/cronharbor executions list --workload-id WORKLOAD_ID --limit 25
./target/debug/cronharbor executions inspect EXECUTION_ID
./target/debug/cronharbor executions diagnostics EXECUTION_ID
./target/debug/cronharbor logs ATTEMPT_ID --follow --max-bytes 65536
./target/debug/cronharbor operations inspect OPERATION_ID
```

`jobs list`, `services list` and `tokens list` accept `--limit` and `--cursor` and return `items` plus `next_cursor`. Service filtering happens before the page limit. Use a returned cursor with the same filters to request the next page. Bare `executions` preserves its bounded array output for existing scripts; `executions list` returns the page envelope and accepts `--limit`, `--cursor` and optional `--workload-id`. The default page size is 100; no command silently scans an unbounded inventory.

Services distinguish desired RUNNING/STOPPED from observed startup/readiness/liveness/failure states. Explicit executable probes have deadlines and thresholds, with recent observations queryable. Automatic restart has a bounded window and backoff; operator restart is a separate authorized intent. Accepted operations are not completion evidence. Conflicting controls supersede earlier operations explicitly.

A clean daemon shutdown records native cancellation as `DAEMON_SHUTDOWN`. Services still desired RUNNING become eligible on the next owner without spending their automatic restart budget, including `restart: never`. Explicit STOP remains stopped. Recovery also releases provably unlaunched service intents; uncertain starts remain UNKNOWN. Exhausted START/RESTART operations finish with the observed error instead of remaining accepted indefinitely.

START received while an earlier STOP is terminating a process waits for that termination and requests a replacement, including with automatic restart disabled. Applying a new revision to a failed service permits a fresh launch; it preserves existing STOPPED intent and UNKNOWN evidence.

`service.stopGraceSeconds` accepts 0–900 seconds. Docker receives the full grace period; the systemd helper waits for that period plus bounded overhead while the existing unit's own stop policy remains authoritative. The supplied systemd and Compose packaging allows 960 seconds for shutdown to finish an in-flight stop and bounded executor observation. Custom supervisors need the same allowance. Healthy external workloads remain available for reattachment after daemon restart.

Operation lookup permits the originating actor to inspect its own operations. Inspecting another actor's operation requires `operations.manage` and the target namespace. Batch operations have no single target namespace and require wildcard namespace scope for another actor to inspect them.

On daemon death, native attempts without reliable identity/outcome evidence become UNKNOWN and retain capacity. Do not guess that a missing PID proves no effect occurred. Investigate the downstream effect, then explicitly reconcile with evidence:

```sh
./target/debug/cronharbor executions reconcile EXECUTION_ID --state CANCELLED --evidence 'Verified downstream record and terminated worker at the recorded time'
```

Use a state supported by the evidence. Reconciliation is audited, and it does not automatically repeat the operation.

Reconciliation retires pending executor observations before sending its success response, and checks retirement again after ownership inspection before follow-up controls or capture. It cannot undo a command already issued.

Unresolved UNKNOWN logs remain protected from retention and still consume the configured disk and segment budgets. This can truncate later captures until an operator reconciles the uncertainty or increases capacity. Reconciled evidence becomes eligible for normal retention.

## External executors and trust

Copy the policy shape from `examples/optional/executors.json` to `STATE_DIR/executors.json`, owned by the daemon user with mode 0600. Enable only the adapter and exact targets required.

Docker execution requires an explicit local Unix daemon connection and digest-pinned image allowlist. Admission fixes non-root container UID, network allowlist, read-only filesystem, dropped capabilities, no-new-privileges, memory/PID/CPU limits and bounded Docker logs. Workload manifests cannot request host mounts, privileged mode or host networking. Immutable labels bind instance/workload/revision/execution/launch identity; reconnect never adopts an object with mismatched labels. Owned terminal containers are removed only after durable outcome and successful log capture. A Docker daemon outage yields uncertainty, not success or blind replay. Access to a privileged Docker daemon remains high authority, even when CronHarbor itself is non-root.

Mixed user and system managers use private `executors.json` entries such as
`"scopedUnits": [{"unit":"worker.service","scope":"user"}, {"unit":"pdf.service","scope":"system"}]`
with `systemdEnabled: true`. Legacy `units` and `systemdUser` remain supported;
duplicate names across either list are refused. New `systemd_v2` handles pin the
manager and actual invocation receipt. Legacy handles cannot adopt a scoped
mapping. Existing unscoped handles additionally require an owner-verified
`legacySystemdScope` (`user` or `system`) matching the legacy global manager.
An absent baseline or changed global manager blocks those old handles; new
starts still use the pinned v2 format. Drain or reconcile old work before
changing either historical setting. Scope changes retain uncertainty instead of redirecting control.

Scheduled systemd jobs require `/usr/bin/python3` with the installed `dbus` and GLib modules
(owner-private `systemdReferencePython` can select another absolute interpreter).
A bounded helper holds a D-Bus unit reference before start, so systemd cannot
collect a fast oneshot's invocation/result before durable settlement. The reference
stays held during UNKNOWN and is released on terminal settlement or reconciliation.
This needs RefUnit authorization for that scheduled unit; persistent Services do
not use RefUnit. Missing prerequisites reject before start. After a daemon crash,
missing invocation metadata remains UNKNOWN and requires evidence-based resolution.
A Service stop uses the successful blocking StopUnit receipt; unavailable post-stop
logs are marked truncated instead of changing verified termination into success logs.

Scheduled starts additionally require permission to monitor systemd control metadata
on that manager bus. A bounded barrier window ties the returned StartUnit job and
JobNew signal to the new InvocationID; competing mutations make the receipt UNKNOWN.
Before requesting work, the bridge rejects active/enabled timer/path triggers,
reverse activation dependencies and scheduled units with OS automatic restart.
Disable old triggers before handover. This does not add any OS permissions.
Starts enqueue without waiting for a oneshot to finish. An old success or another invocation cannot
complete a scheduled run. Lost receipts stay UNKNOWN. A managed Service can adopt
an already healthy allowlisted unit without restarting it and owns that logical
unit across its existing OS restart policy; disable Cron Harbor automatic restart
when that policy owns retries. Scheduled jobs cannot coalesce onto busy units, and
unit reservations are exclusive across workload definitions, including UNKNOWN.
Old binaries cannot interpret new scoped handles: drain/reconcile and verify a
compatible rollback state before downgrading. Do not test a downgrade against live
state or restore an old business database over current work.

The systemd bridge controls only exact allowlisted existing `.service` units. Existing units retain their execution environment, credentials, AppArmor, filesystem and cgroup protections. Unit manifests reject native command/env/limit settings instead of silently ignoring them. Scheduled v2 journal capture also filters the exact persisted `_SYSTEMD_INVOCATION_ID`; logical Services retain unit context. Journal capture uses the persisted start-intent time as its lower boundary; legacy handles without that evidence return unavailable logs. This bridge needs existing OS authorization; CronHarbor does not install broad polkit/D-Bus permissions or recreate units as raw processes. The time filter follows the official [journalctl](https://www.freedesktop.org/software/systemd/man/256/journalctl.html) and [systemd timestamp](https://github.com/systemd/systemd/blob/main/man/systemd.time.xml) contracts; a live hardened-unit drill is still required for each deployment.

Terminal unit outcomes use systemd's `Result` with validated `ExecMainCode` and `ExecMainStatus`. A timeout remains a failure even with status zero; configured successful nonzero exits or clean signals remain successful. Incomplete evidence stays UNKNOWN. See the official [service result properties](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html) and [successful exit configuration](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html).

Native workloads run with the daemon UID and namespace. They are trusted code, not adversarial tenants. Configured Linux resource limits use the platform's `prlimit`; root NPROC enforcement is rejected because it would give a false guarantee. Process groups and bounded probes provide cleanup, while unresolved escaped descendants remain uncertain. For critical isolation, use reviewed systemd units or the constrained Docker adapter.

## Secrets and log budgets

Use `execution.secretRefs` with `provider: env` plus `name`, or `provider: file` plus an absolute private file `path`. The database stores references, not resolved values. Resolved values are injected only into approved execution and redacted from captured output, including across chunk boundaries. Known sensitive environment names cannot carry plaintext values in definitions. Do not assume arbitrary unknown secrets printed by a program can always be recognized.

Native output is one logical byte stream per attempt. Without `logs.rotation`, `maxRunBytes` preserves the bounded beginning of output as before. Explicit rotation retains the newest complete-segment window, with `maxRunBytes` as its retained-byte ceiling:

```yaml
logs:
  maxRunBytes: 10485760
  rotation:
    segmentBytes: 1048576
    maxSegments: 10
```

This example retains at most 10 MiB in at most ten 1 MiB segments. Rotation is native-only and opt-in; apply a reviewed definition revision for future attempts. It does not rewrite the capture policy of an already running attempt. Older sealed segments can expire while a known RUNNING rolling workload continues. UNKNOWN freezes whatever evidence remains, including its capacity charge; already expired history cannot be recovered by freezing.

API/CLI clients follow the server's raw `next_offset` byte cursor, including malformed UTF-8. `logs ATTEMPT_ID --info` and `GET /api/v1/executions/EXECUTION_ID/logs/info` report retained start/end, capture state and known loss. An explicit offset below retained start returns `LOG_CURSOR_EXPIRED` (HTTP 409). Omit the offset to select the current retained start and read it within one daemon command; CLI/MCP defaults and dashboard initial/recovery reads use that path. Followers display the gap before resuming. The dashboard follows every five seconds and bounds unchanged diagnostic refreshes, preserving the existing request budget; changed execution state and explicit diagnostic refresh fetch current evidence. A missing expected retained file is unavailable evidence, not EOF.

Native pipe readers redact before the bounded queue. Each installation supports 128 simultaneous native captures, each with at most 1 MiB of queued chunks, plus bounded per-stream reader/redaction overhead. This log-memory bound does not limit executions. If intake, storage or capture slots are exhausted, output saving stops with an explicit reason while the workload can continue; it does not silently resume within that attempt. The viewer distinguishes frozen evidence, expired history and stopped/truncated capture. Docker/systemd retain bounded terminal snapshots with the existing additional 1 MiB limit, not continuous rotating ingestion. Recovered Docker logs use the original launch secret values; unverifiable context withholds capture and retains the container.

One versioned installation policy now owns shared disk, physical-segment and eligible-segment age limits. Inspect and update it through Settings or the trusted CLI:

```sh
./target/debug/cronharbor log-storage status
./target/debug/cronharbor log-storage set --max-disk-bytes 268435456 --max-segments 1024 \
  --retention-seconds 604800 --expected-revision CURRENT_REVISION \
  --idempotency-key reviewed-log-policy-001
```

Use the actual revision from status and a fresh key per new reviewed intent. These controls require local owner or Admin with unscoped `operations.manage` and wildcard namespaces. Decreases are rejected while active/UNKNOWN attempts or log reservations exist; protected/unexplained evidence is never deleted to make a proposal fit. Larger approved count/age values are effective, rather than silently capped at old defaults. The byte budget covers captured log-file bytes, not filesystem block overhead, SQLite/WAL, backups, daemon output or Docker/journald's own storage.

Upgrade seeds this policy once from conservative effective legacy limits. Existing immutable definitions/files remain readable; unchanged legacy `logs.maxDiskBytes`, `logs.maxSegments` and `logs.retentionSeconds` can remain or be removed. New or changed explicit legacy values are rejected with `LOG_POLICY_MOVED`; use the installation policy instead. `maxRunBytes` and rotation remain per-workload safe configuration edits and cannot override the installation ceiling. Source files remain FILE-owned.

HTTP allows 240 unrecognized credential checks per minute per API process before daemon work, separately from authenticated request limits. A new or evicted credential can receive 429 during rejection traffic; retry after the minute window and configure network-level admission at a remote proxy. Verified hashes stay in the bounded table across request-window resets, with expired entries evicted only to admit another verified credential. This does not cache authorization: every operation still checks revocation. Authentication rejection evidence is sampled at most once per second per daemon owner and retains only its latest 1,000 records across restarts. Other audit history is preserved; rejection samples are not a complete attempt count.

## Outbound alert webhooks

Use API, Full, or a Core build with `--features webhooks`. An absent `alerts` block disables capture and sending. A configured destination in a binary without that feature fails explicitly. Add [the example alert block](../examples/optional/alerts.yaml) to daemon configuration and restart; there is no remote destination editor.

Provision an HTTPS receiver and a separate ingestion-only credential first. The token is 1–512 visible ASCII bytes, from an environment reference or owned private regular file (0600, no symlink/hard link). File credentials may end with one LF or CRLF. It is resolved per request so file rotation does not change destination identity. An optional CA file supplies internal trust and is limited to 1 MiB; hostname verification remains enabled. Container/native service users need explicit access to these deployment resources.

Public unicast destinations are allowed. Private RFC1918/ULA and loopback addresses require exact entries in `allowedPrivateAddresses` (at most 16); link-local, unspecified, multicast, broadcast and mapped-address bypasses are rejected. The sender validates at most 16 DNS answers and pins the approved set to each HTTPS request. Ambient proxy configuration and redirects cannot change its destination. Destination identity includes the normalized endpoint and sorted `allowedPrivateAddresses`. Changing either retires old outstanding deliveries as `DESTINATION_CHANGED`, even when the endpoint stays the same; they are not redirected. Rotating credentials or the trusted CA does not change this identity.

```sh
./target/debug/cronharbor alerts status
./target/debug/cronharbor alerts list --limit 25
./target/debug/cronharbor alerts retry ALERT_ID --idempotency-key reviewed-alert-retry-1
```

Status/list/retry require installation-wide Admin authority (`operations.manage` and wildcard namespaces) or the trusted local owner. The dashboard Settings panel uses the same backend rules. A retry is one additional send with the original ID and body, not a workload restart or a new automatic retry cycle. Only eligible `FAILED`, `BLOCKED` or `EXPIRED` rows whose destination still matches the current configuration can be retried. `DESTINATION_CHANGED` rows cannot be retried. Polling never resubmits it. Blocked credentials/TLS/destination require repair and explicit retry; HTTP redirects are failures. No resolved credential, raw receiver body or log excerpt enters delivery history.

The queue holds at most 1,000 outstanding alerts, each at most 8,192 bytes. Overflow is visibly suppressed, not delivered. Automatic attempts are bounded to 12 within 24 hours; terminal history is bounded to 1,000 rows and seven days. These limits exclude SQLite/WAL and backup file sizes. A slow or stuck resolver makes delivery unavailable without blocking scheduling or spawning replacements indefinitely.

Job alerts follow final failure/timeout after automatic retries. `UNKNOWN` alerts describe uncertainty, not proven failure. Service incidents use existing health thresholds; recovery requires actual `READY`. Pause and intentional stop preserve an open incident; stop sends no false recovery. Decommission closure is outside v1. The receiving API must durably accept and deduplicate before acknowledging, then track Grokbot processing itself. See [the complete receiver contract](api/WEBHOOKS.md).

Ordinary pause keeps existing notifications deliverable. Restore quarantine holds the restored queue and creates no fresh source markers. Explicit evidence/cutoff resume expires historical held alerts instead of replaying them, then observes current actionable states. Review an individual expired row before retrying; backup restoration cannot guarantee continuous or exactly-once notification.

## Metadata storage admission

Concurrency limits apply independently to each workload. UNKNOWN retains its
workload's capacity; unrelated workloads do not share a global execution cap.
The bounded dispatch batch per daemon tick limits loop work, not active executions.
Size each workload's limits for the host and downstream database/provider capacity.

To bound metadata growth without deleting history or idempotency keys, create
`STATE/admission-policy.json` as an owner-only regular file (0600, no links):

```json
{
  "maxDatabaseBytes": 1073741824,
  "warningBytes": 805306368,
  "admissionPauseBytes": 939524096,
  "maxWalBytes": 67108864
}
```

The policy loads at startup and must have strictly increasing positive warning,
pause and main-database ceilings. The main ceiling must be at least 1 MiB; the WAL
waterline must be at least 4096 bytes and no greater than the main ceiling. Unknown
fields, oversized files and unsafe permissions refuse startup. An absent policy
adds no metadata quota. Limits must fit exact JSON integers and SQLite's actual
page-count ceiling; oversized limits are rejected rather than silently clamped.
Applied schema migrations remain unchanged.

`status` and REST status expose optional `admission` policy/usage, warning and pause
fields. The dashboard shows pressure in Overview and Settings. Usage measures
SQLite logical pages, including pages still in the WAL, and the physical WAL file.
At the pause threshold, new dispatch, manual intents, definition changes and token
creation stop before allocating rows; retries of existing operation keys still
return their durable result. Repeated due ticks create no occurrences while held.
Cancellation, service STOP, pause, reconciliation and recording existing outcomes
remain available within the reserve. A storage pause does not change desired or
workload pause state, and is separate from restore quarantine.

SQLite's page ceiling bounds the main database, rounded down to whole pages. The
WAL setting is an admission/checkpoint waterline, not a hard file ceiling: new or
settling transactions can cross it. Above it the daemon attempts a non-waiting
checkpoint before admitting more work. A pinned reader keeps admission paused
until the journal can truncate. Disk exhaustion or exhausted settlement space can
still prevent durable writes; errors remain errors and never prove an external
effect failed. Inspect downstream evidence before reconciling UNKNOWN.

No metadata is purged automatically to regain admission. Review capacity, stop
unexpected writers/readers and adjust the private policy before a controlled
restart, preserving active effects and their evidence. An existing database larger
than the configured main ceiling refuses startup. Backups exclude this external
policy file; transfer and verify it separately with deployment configuration.
Captured logs, routine verified backups, incident archives and other disk usage
need their own retention policies; they are not charged to this main-database cap.

## Backup, restore and upgrades

```sh
./target/debug/cronharbor pause --evidence 'Planned upgrade; new dispatch paused'
./target/debug/cronharbor backup before-upgrade
```

Backups use SQLite's backup API, integrity/foreign-key checks, a SHA-256 manifest and private durable files. Preserve the previous binary/image digest. Schema migrations are monotonic and checksummed; an unsupported version/checksum fails closed. Never copy only a live WAL main file.

Schema version 3 bounded authentication rejection history; version 4 adds logical log streams, segments and the installation log policy; version 5 adds incident markers and the alert outbox. Upgrade preserves a private pre-migration database backup. Earlier supported snapshots upgrade under restore quarantine. Older binaries cannot open the new schema; rollback requires the matching older binary and a verified pre-upgrade snapshot, followed by normal restore reconciliation.

Transfer the snapshot and its `.db.manifest.json` sidecar together. Both must be regular files owned by the restoring user with no group/other permissions or additional hard links. Symlink or FIFO sidecars are rejected before installation; preserve mode 0600 when archiving or moving backups.

The snapshot contains SQLite state, including workload revisions and log metadata. It does **not** contain these resources; back them up and restore or provision them separately as required:

- `STATE_DIR/executors.json` (private mode 0600); without it, external adapters default to disabled.
- `STATE_DIR/logs/`; database log metadata alone cannot recover captured bytes.
- Daemon alert endpoint configuration, ingestion credential and optional internal CA file; backups include delivery rows and incident markers, but no resolved webhook credential.
- Referenced secret files and the daemon's secret environment, executables, pinned images and existing systemd units.
- Daemon configuration and static UI assets. Review endpoint/origin settings for the recovery installation.

Stop the previous owner before restoring. A new target directory is safest; replacing existing state requires explicit `--replace` and preserves prior database/WAL files in a private quarantine directory:

```sh
./target/debug/cronharbor --state-dir /var/lib/cronharbor-restored restore /path/to/snapshot.db --evidence 'Restore incident and snapshot identity verified'
./target/debug/cronharbor --state-dir /var/lib/cronharbor-restored server
```

That startup is a paused, CLI-only recovery session with no HTTP listener. To inspect through the dashboard using an API/Full build, instead start `server --listen 127.0.0.1:8090 --ui-dir /absolute/path/to/dashboard/dist`. Keep recovery startup free of `--file` and workload-bearing `--config`: those apply definitions at startup and can replace snapshot revisions. Re-provision reviewed executor policy and required runtime/secret resources before startup, then inspect them before resume. Preserve private ownership when restoring logs. Do not copy live state indiscriminately or restore old token files as usable credentials.

Restore fully stages, syncs and verifies the replacement's hash, integrity and supported migration checksums before touching existing state. It writes external installation/quarantine markers before swapping files. An interrupted installation blocks startup; preserve all files and retry the verified snapshot into a new private target directory. Startup after a completed restore stays paused, quarantines the backup-to-cutoff interval and invalidates restored API credentials before remote reads; bootstrap fresh credentials locally. Reconcile surviving external work and lost downstream effects. A snapshot cannot tell you what ran after it was taken. Resume requires explicit evidence and cutoff:

```sh
./target/debug/cronharbor --state-dir /var/lib/cronharbor-restored resume --cutoff-us VERIFIED_UTC_MICROSECONDS --evidence 'Exclusive owner and backup-to-cutoff effects reconciled'
```

The daemon advances cursors beyond that interval rather than catching it up. Unknown attempts still hold capacity until separately resolved. Restoring an old snapshot is not an ordinary restart or permission to replay work.

Automatic external reattachment is suspended throughout restore quarantine, so snapshot timeouts, cancellation flags and desired state cannot issue executor commands before resume. Inspect surviving work independently before reconciling it. Resume discards queued retries and finalizes their execution/operation as quarantined, preserving the failed attempt's evidence.

## Read-only MCP

```sh
./target/debug/cronharbor tokens create --role observe --namespace local --capability workload.read --capability execution.read --capability logs.read --output-file /private/path/observe.token
./target/debug/cronharbor mcp --token-file /private/path/observe.token
```

Configure that command as a stdio MCP server. The SDK supports the tested protocol versions; the bridge exposes ten fixed observation tools, with no arbitrary command, SQL, HTTP fetch or mutable tool. The Observe identity is enforced by the daemon. Logs are returned as bounded untrusted data, never instructions.

## Migration

```sh
./target/debug/cronharbor import cronicle exported-events.json --dry-run --namespace pilot --report json
```

Import is passive: reports classify schedules and produce disabled proposals only where literal command semantics can be preserved. Complex shell scripts, year constraints, unknown plugins and hardened unit conversions require review or are blocked. Reports do not echo rejected scripts or activate either scheduler. Compare the next 100 source UTC instants, current credentials/cwd/sandbox and live ownership before a one-at-a-time cutover. Actual Banana Ventura production inventory and operator approval are external requirements; source templates alone cannot establish them.

## Verification

Run verification locally before committing or pushing. The hosted workflow only checks repository migrations and document links, with a three-minute job timeout.

```sh
npm ci --prefix web/dashboard
(cd web/dashboard && npx playwright install chromium)
python3 scripts/check_repository.py
~/.cargo/bin/cargo fmt --all --check
~/.cargo/bin/cargo test --locked --all-features --all-targets
~/.cargo/bin/cargo clippy --locked --all-features --all-targets -- -D warnings
npm run typecheck --prefix web/dashboard
npm test --prefix web/dashboard
npm run build --prefix web/dashboard
~/.cargo/bin/cargo build --locked --features full
python3 scripts/test_full.py --browser
```

On Linux, Playwright's `npx playwright install --with-deps chromium` also installs required system libraries. Build the full debug binary immediately before `test_full.py`, or point `CRONHARBOR_BINARY` to a preserved full-feature artifact. Cargo builds with different feature sets share the default binary path. Docker adapter acceptance additionally uses `scripts/test_docker_executor.py` with an explicitly supplied test daemon and digest-pinned image. Run generated-contract comparisons, dependency audits and the appropriate container checks locally too; execution evidence is recorded separately from workflow configuration.

A busy event stream refreshes dashboard snapshots at most once per five seconds.
HTTP 429 pauses that tab's API client according to the bounded Retry-After window,
including event reconnects and attempted writes; mutations are never retried
without an explicit user action. Tabs using the same token share its server-side
request quota. After a UI repair, reload the tab and connect again to load the new
assets. An existing server rate window can take up to one minute to clear.
