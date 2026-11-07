# CronHarbor application

The scoped single-node application is implemented: Rust scheduler and service supervisor, CLI, authenticated REST/SSE, read-only MCP, Vue dashboard, opt-in Docker/systemd adapters, backup/restore and passive Cronicle import. Release acceptance and live migration approval remain separate from implementation. See [verification evidence](docs/VERIFICATION.md) and the [operator runbook](docs/RUNBOOK.md).

## Start the complete application

Use a Unix host, the pinned Rust toolchain and Node.js 22. Run from the repository root: relative manifest/UI paths in the example configuration resolve from the process working directory. Installed service paths are covered in the [runbook](docs/RUNBOOK.md#installed-systemd-service).

```sh
npm ci --prefix web/dashboard
npm run build --prefix web/dashboard
~/.cargo/bin/cargo build --locked --features full
./target/debug/cronharbor server --config examples/optional/daemon.yaml
```

Open http://127.0.0.1:8090. In a second terminal, create a credential through the private local socket:

```sh
./target/debug/cronharbor tokens create --role admin --namespace '*' --capability '*' --output-file /tmp/cronharbor-admin.token
```

Enter the file's value in the dashboard. Credentials stay in browser memory; use narrower grants for routine work. The daemon owns a private `.cronharbor` state directory. Supply the same `--state-dir PATH` to daemon and CLI when choosing another installation.

For Docker, use `docker compose -f compose.full.yaml up --build -d`, then bootstrap a token with `docker compose -f compose.full.yaml exec cronharbor cronharbor --state-dir /var/lib/cronharbor tokens create --role admin --namespace '*' --capability '*'`. The published port is host loopback only. The default `compose.yaml` builds the headless Core profile without TCP or a Docker socket.

## Implemented behavior

- Scheduled, one-shot and managed-service definitions; five-field numeric cron, IANA zones and explicit DST policies; intervals, bounded misfires, concurrency and safe retries.
- Durable launch intent, unique occurrences, crash recovery, conservative UNKNOWN outcomes, audited reconciliation, immutable revisions, compare-and-swap and idempotent asynchronous operations.
- Service desired/observed state, executable readiness/liveness probes, startup grace, thresholds, restart budgets/backoff and explicit operator restart.
- Shared daemon authorization for CLI/REST/MCP, namespace and capability grants, hashed expiring/revocable tokens, bounded event history and audit queries, safe errors and owner-only Unix transport. Ordinary audit history is preserved without an automatic storage cap; only authentication rejection samples have bounded retention. See the [retention details](docs/RUNBOOK.md#secrets-and-log-budgets).
- Authenticated REST/SSE and static dashboard hosting. Vue provides overview, jobs, services, editor, executions, logs, operations and settings with role-aware controls and reconnect recovery. MCP provides ten fixed observation tools through the official Rust SDK.
- Shell-free native execution, bounded/redacted logs and retention. Docker requires an explicit private policy, pinned images and verified ownership labels; systemd controls only allowlisted existing units.
- Verified SQLite backups, offline restore quarantine, restored-token invalidation and explicit evidence/cutoff resume. Passive Cronicle reports classify unsupported/security-sensitive conversions and emit disabled proposals.
- Generated TypeScript DTOs and workload JSON Schema, Core/API/Full image targets, hardened Compose/native service examples and CI configuration.

## Code map

| Area | Source |
|---|---|
| Definition and public contracts | `src/config.rs`, `src/contracts.rs`, `schema/workload.schema.json` |
| Shared command service and authorization | `src/application.rs`, `src/policy.rs` |
| Scheduling, persistence and ownership | `src/scheduler.rs`, `src/store.rs`, `src/server.rs`, `migrations/` |
| Native, probes and external execution | `src/process.rs`, `src/external.rs` |
| Backup/restore and import | `src/ops.rs`, `src/importer.rs` |
| CLI, REST/SSE and MCP | `src/cli.rs`, `src/api.rs`, `src/mcp.rs`, `docs/api/README.md` |
| Dashboard | `web/dashboard/` |
| Reproducible checks | `tests/`, `scripts/`, `.github/workflows/ci.yml` |

## Verification

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

The full integration script starts a disposable daemon and tests real CLI/REST/MCP and backup/restore. The optional browser flag runs Playwright against that daemon; Linux hosts may need `npx playwright install --with-deps chromium` from `web/dashboard` for system libraries. Build Full immediately before acceptance or select a preserved Full artifact with `CRONHARBOR_BINARY`; different Cargo feature builds share the default binary path. Docker adapter and Compose checks have separate scripts and require a test Docker daemon. Actual results, platforms and untested acceptance gates are recorded in [docs/VERIFICATION.md](docs/VERIFICATION.md).

## Operational boundaries

Native workloads execute as the daemon user and are trusted code. Process groups do not provide an adversarial sandbox. UNKNOWN attempts retain capacity until evidence-based reconciliation; a restore never silently replays the backup-to-cutoff interval. HTTP requires a TLS reverse proxy for remote deployment. Direct Docker daemon access is privileged authority and remains disabled by default.

Distributed agents, workflow DAGs and write-capable MCP are outside the agreed single-node scope. Production migration, live hardening review, hardware benchmarks, multi-architecture release certification, signing and name/legal clearance require external evidence. They are not represented as passed by local automated checks.

The root [README](README.md) is the product entry point; this document records implementation scope and the source map. Local verification does not imply a production deployment or public release.
