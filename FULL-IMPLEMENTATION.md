# Complete single-node implementation plan

The user requested the complete scoped application on 2026-11-07. The implemented contracts and [retained acceptance catalog](docs/ACCEPTANCE.md) are the current reference. Existing changes remain on the current branch; no publication or production migration is implied.

## Shared boundaries

- `src/contracts.rs` owns application commands and public DTOs. Dashboard TypeScript types are generated from these Rust types.
- One daemon owns SQLite and dispatch. REST/MCP are clients of its Unix socket. Remote tokens are checked by the daemon for every command; transport-specific permissions never replace application policy.
- `Request::Execute { command: Box<AppCommand>, token: Option<String> }` is the new shared local wire command. Missing token means the existing owner-only local CLI. Remote HTTP rejects missing bearer credentials; MCP requires a scoped token file.
- Lists use `Page<T> { items, next_cursor }`. Times are UTC microseconds. Existing local status/executions commands remain compatible where practical.
- Core, API and full are Cargo profiles/features, sharing one engine. MCP stays read-only. Distributed agents, workflow DAGs and remote write-MCP remain the design's explicit future scope.

## Work packages

- [x] Runtime: scheduled/one-shot/service definitions; UTC/IANA cron, DST and intervals; bounded misfires/concurrency/retries; immutable revision CAS; durable manual operations; service readiness/restart budgets; shared capability authorization; token/audit/events; retention and reconciliation.
- [x] Transports: authenticated Axum REST/SSE, bounded/revocable streams, same-origin static hosting, read-only MCP through the official Rust SDK, generated client contract.
- [x] Dashboard: Vue/TypeScript overview/jobs/services/executions/logs/operations/settings, live reconnect, source ownership, async outcomes, accessible responsive UI, no stored bearer secrets.
- [x] Executors/operations: explicit Docker and allowlisted systemd adapters, safe backup/restore quarantine/resume, Cronicle import classification/shadow reports, profile packaging, runbooks and reproducible verification.
- [x] Integration: type/build/lint/test checks, Linux runtime/adapters/REST/MCP/UI scenarios, browser inspection, complete requirement-to-evidence matrix and final diff review.

Each work package writes regression tests before implementation. No feature is counted as implemented because an endpoint or screen exists without its backend behavior. Actual production inventory, migration approval, name/legal clearance and publishing credentials remain external evidence requirements.

## Completion evidence

Implemented and verified on 2026-11-07. See [the verification matrix](docs/VERIFICATION.md) for the current 168 macOS Rust tests, historical 158-test Linux/container checks, 56 frontend unit tests, 24 browser scenarios, both review-fix passes and the later security fixes. Real REST/MCP/restore checks were rerun after the security patch; Docker images and acceptance evidence remain from the preceding pass and require rebuilding. Run instructions and trust boundaries are in [IMPLEMENTATION.md](IMPLEMENTATION.md) and [the runbook](docs/RUNBOOK.md). External production and release gates are explicitly listed; no publication is implied.
