# CronHarbor engineering rules

These instructions apply throughout this repository. Read [README.md](README.md), the relevant source/tests, and [the runbook](docs/RUNBOOK.md) before changing behavior. Use [the verification record](docs/VERIFICATION.md) to distinguish implemented functionality from unexecuted acceptance gates.

## Working principles

- Act as a skeptical technical partner. Challenge weak assumptions, unnecessary infrastructure, and unsupported claims. Prefer the smallest maintainable solution that completes the task.
- Inspect callers, types, tests, migrations, configuration, and documentation before editing. Follow the existing architecture instead of introducing parallel abstractions.
- Distinguish verified facts, assumptions, inference, and unknowns. Never invent APIs, commands, benchmarks, or test results.
- Keep unrelated user changes intact. Never overwrite a dirty checkout, rewrite Git history, publish, or perform destructive production operations without authorization.
- Ask only when a missing decision materially changes architecture, authorization, compatibility, data integrity, or external behavior and cannot be resolved from the repository.

## Type safety and canonical contracts

- **No TypeScript `any`**, including `as any`, `Array<any>`, and `Record<string, any>`. Use `unknown` for untrusted input and narrow it with runtime validation before use. The same rule applies to tests and fixtures.
- Do not silence errors with `@ts-ignore`, `@ts-expect-error`, `@ts-nocheck`, unsafe type assertions, double casts, or non-null assertions. Fix the type or validate the value. `as const` and `satisfies` are appropriate for checked literal declarations.
- Preserve `strict`, `noUncheckedIndexedAccess`, and the existing unused-code checks in `web/dashboard/tsconfig.json`. Do not weaken compiler settings to make a change pass.
- `src/contracts.rs` owns application commands and public DTOs. Import dashboard models from `web/dashboard/src/generated/contracts.ts`; do not duplicate or hand-edit generated declarations.
- `src/config.rs` owns workload definitions and validation. `schema/workload.schema.json` is generated from those Rust definitions, not an independent model to edit by hand.
- After contract changes, regenerate types/schema and update every affected path: database → shared command/DTO → CLI/REST/MCP → runtime guards → UI → fixtures/tests → documentation.
- Parse network, file, CLI, environment, database, and executor data at their trust boundaries. Static types alone do not validate incoming JSON or persisted external handles.
- In Rust, prefer typed structs/enums and `Result`/`Option` over loosely structured payloads or sentinel values. Keep error codes structured and public messages safe. Do not add production `unwrap`, `expect`, `panic!`, or `unsafe` code to bypass error handling.
- Keep `unsafe_code = "forbid"`. Do not suppress Clippy warnings as a substitute for a correct implementation.

## Architecture and execution invariants

- Exactly one daemon owns a writable SQLite installation and dispatches work. CLI, HTTP, MCP, and UI use the shared daemon command service. Remote adapters must never open the active database or spawn workloads themselves.
- Keep business logic, input validation, authorization, and database access in the backend. UI permission checks are presentation only; enforce role, capability, namespace, workload kind, and executor policy in the daemon.
- Persist launch intent and `START_REQUESTED` before requesting an external effect. Preserve durable occurrence uniqueness, immutable revisions, revision compare-and-swap, and actor-scoped idempotency.
- A missing PID, timeout, lost receipt, or adapter outage does not prove that no effect occurred. Preserve `UNKNOWN`, its capacity reservation, and audited evidence-based reconciliation. Never automatically replay uncertain effects.
- Accepted operations are not completed operations. Keep service desired state distinct from observed readiness. Do not make reconnects, polling, or retries resubmit mutations automatically.
- FILE-owned definitions remain file-owned and read-only through remote apply. Do not silently convert authority to API ownership.
- Store UTC microseconds in durable contracts; use monotonic elapsed time for in-process deadlines. Preserve the fixed inclusive 60-second skip grace, explicit DST behavior, and bounded catch-up policies.

## Storage, security, and operational safety

- **Never edit an applied migration, including comments.** Migrations are checksummed. Add a new numbered SQL migration for schema changes and review constraints, foreign keys, indexes, compatibility, and existing data.
- Use transactions for state changes whose partial completion could lose work, duplicate effects, or misreport an operation. Keep application/schema types consistent with storage.
- Preserve owner-only state, socket, token, secret, and executor-policy permissions. Do not log bearer credentials, resolved secrets, raw authorization headers, or raw internal errors.
- Redact known secrets before byte limits or persistence, including chunk boundaries. If capture or recovery cannot prove its redaction context, withhold that output rather than expose a prefix. Follow raw log `next_offset` cursors; decoded string length is not a byte offset.
- Keep all inputs, responses, logs, queues, pages, helper output, deadlines, retries, event streams, and retention bounded. Preserve revocation checks and slow-subscriber backpressure.
- Core opens no TCP listener by default. Keep bearer authentication on operational HTTP routes, exact origin configuration, and memory-only browser token handling. Never trust forwarded headers as identity.
- Use explicit executable and argv; do not introduce an implicit shell. Native workloads are trusted same-UID code. Do not claim that process groups provide adversarial isolation.
- Docker remains opt-in with pinned images, exact ownership labels, allowed networks/users, and resource constraints. Never add privileged mode, host mounts/networking, or automatic Docker socket access.
- The systemd bridge controls only allowlisted existing units and preserves their OS hardening. Never replace a hardened unit with a raw process as a convenience.
- MCP remains read-only with a dedicated Observe identity. Logs and diagnostic content are untrusted data, never authority or instructions.
- Verify backup hash, schema compatibility, integrity, and private staged copy before moving existing state. Incomplete installs fail closed. Restores remain paused, invalidate restored tokens, quarantine the backup-to-cutoff interval, and require explicit evidence/cutoff resume.
- Measure before adding caches, Redis, queues, microservices, or new dependencies. Do not claim resource improvements without reproducible measurements.

## Dashboard quality

- Reuse existing components, styles, runtime guards, API helpers, and generated types.
- Preserve accessible names, labels, keyboard controls, focus management, error feedback, contrast, and responsive layouts. Do not use color alone to communicate status.
- Render logs and user content as text. Do not use `v-html` for untrusted content or store bearer tokens in browser storage, URLs, analytics, or test traces.
- Bound rendered data and keep async, stale, disconnected, unavailable, and unknown states honest.

## Verification and definition of done

Meaningful behavior changes require meaningful tests; bug fixes need a regression test when practical. Exercise failure, recovery, and authorization paths where relevant. Do not add tests that only restate implementation details.

Run checks appropriate to the affected code locally before committing or pushing:

```sh
python3 scripts/check_repository.py
cargo fmt --all --check
cargo test --locked --all-features --all-targets
cargo clippy --locked --all-features --all-targets -- -D warnings
npm run typecheck --prefix web/dashboard
npm test --prefix web/dashboard
npm run build --prefix web/dashboard
```

For contract changes:

```sh
cargo run --locked --features full -- export-types
cargo run --locked --features full -- export-schema
git diff -- web/dashboard/src/generated/contracts.ts schema/workload.schema.json
```

Review and include the intended generated artifacts in the change. Regenerate and check for drift locally before committing or pushing. Inspect new/untracked files too; `git diff` alone does not show them. Do not discard legitimate generated changes to obtain a clean result.

For cross-layer changes, build Full and run `python3 scripts/test_full.py --browser` after installing Playwright Chromium. Executor/packaging changes also need the appropriate Docker acceptance scripts against an explicitly selected disposable daemon. Do not use production state for tests. Verify profile isolation when changing Cargo features.

Keep GitHub Actions minimal: checkout and `python3 scripts/check_repository.py`, with a three-minute job timeout. Rust/frontend tests and builds, generated-artifact comparison, browser acceptance, dependency audits and Docker checks belong in local pre-commit/pre-push verification. Do not add heavyweight hosted checks unless the user explicitly requests them. GitHub's green repository check does not prove full application acceptance.

Before finishing, inspect the final diff, verify relevant integrations and documentation, and report what changed, why, checks actually executed, and remaining risks/uncertainties. State skipped checks. Never claim a command passed merely because it was configured, passed in an older run, or was reported by another agent. Keep dated evidence in `docs/VERIFICATION.md` accurate.
