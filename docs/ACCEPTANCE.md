# Acceptance scenarios and release gates

These scenario IDs and benchmark profiles are retained from the original planning material. They describe acceptance targets, not passing results. Current executed evidence and limitations are recorded in [VERIFICATION.md](VERIFICATION.md). The implementation, generated contracts, and migrations are maintained in the repository root.

## Test layers

| Layer | Tooling / principle | Gates |
|---|---|---|
| Pure scheduling/property | deterministic fake clock, timezone fixture corpus | no duplicate UTC occurrence records; finite generation |
| Storage | ephemeral SQLite, migrations, concurrent clients | FK, uniqueness, rollback, single owner |
| Process lifecycle | Linux host + container matrix | exit, orphan, subtree kill, unknown classification |
| Docker | disposable isolated Docker daemon | correct labels, opt-in privileged access, reattach, API loss |
| Security | policy matrix, malicious input, fuzz | no unauthorized operations or unbounded outputs |
| API/CLI/MCP | schema conformance, mock actor, handshake | same semantics/authorization, no transport bypass |
| Dashboard | Playwright/Vitest/a11y smoke | reconnect, role UI, asynchronous operations and accessibility |
| Migration | frozen Cronicle fixtures and synthetic live state | schedule parity, no double owner |
| Disaster recovery | `SIGKILL`, OOM, disk full, DB locked | durable event truth, no side-effect replay |
| Performance | independent benchmark rig | publish observed metrics with reproducible environment |

## Test IDs and acceptance scenarios

| ID | Scenario | Required outcome |
|---|---|---|
| T-001 | Start two daemons with same DB | exactly one becomes active owner, loser exits before dispatch |
| T-002 | Kill daemon after launch effect, before receipt | inspect/reconcile or `UNKNOWN`; no automatic double launch |
| T-003 | Materialize same due instant in concurrent wakeups | exactly one occurrence record |
| T-004 | DST fold `Europe/Madrid` with first, second, both | expected 1/1/2 distinct UTC instants |
| T-005 | DST gap, local 02:30 | skip or defined next-valid instant, never random offset |
| T-006 | 30-day downtime, policy `skip` | no 30-day backlog of side effects |
| T-007 | `forbid`, active previous run | skipped/blocked with explicit reason, not silently overlapped |
| T-008 | same manual Idempotency-Key twice | same durable operation ID, once admitted |
| T-009 | identical key, different payload | HTTP 409; no new execution |
| T-010 | fork bomb / ignoring SIGTERM | constrained OS resource use; unresolved descendants reported |
| T-011 | Docker API unavailable during start | `EXECUTOR_UNAVAILABLE` or `UNKNOWN`, no fabricated success |
| T-012 | unauthorized viewer calls service restart via REST | 403 even with direct API call |
| T-013 | read-only MCP agent calls a write tool | tool absent or server-side denied, audited |
| T-014 | malicious log tells agent to restart | data returned as untrusted, no privileged operation initiated |
| T-015 | API SSE disconnect during restart | UI re-queries operation, never auto-submits duplicate |
| T-016 | restore verified DB backup and start daemon | starts paused; schema/owner integrity and backup-to-cutoff reconciliation before explicit resume |
| T-017 | non-root Docker core without socket | health succeeds with UDS, no published ports |
| T-018 | file-managed workload edited by dashboard | prohibited; UI offers change via source file |
| T-019 | import hardened systemd worker | exact hardening preserved via bridge or classified SECURITY_GAP |
| T-020 | initial SQLite WAL runtime version check | unpatched library blocked or supported-binary exception documented |
| T-021 | restore snapshot that omits a successful post-backup execution | paused; no automatic backup-to-cutoff replay under any misfire policy; external effects reconciled/quarantined before explicit resume |
| T-022 | admit `skip` due at +59.999999s, +60s, +60.000001s | first two admissible, last is misfire; recheck after queue delay |

Test harness should inject failure after each durable state write and each external adapter call, not merely `kill -9` once at random.

T-021 failure drill: take backup at B, execute a side-effecting job successfully at B+1 and verify its downstream effect, stop the former owner at cutoff C, then restore the backup without that execution row. Even with intact schema and no active restored attempts, the daemon must remain paused. Test `skip`, `latest` and `catch_up`; none may replay [B,C]. Require recorded external-effect reconciliation or visible unresolved quarantine, cursor advancement beyond C and explicit operator resume. Also test unknown cutoff/owner: resumption stays blocked. The restore marker must survive DB replacement. A clean `integrity_check` alone fails this acceptance scenario.

## Repository checks

Run `python3 scripts/check_repository.py` from the repository root. It uses Python's standard library to apply the actual SQL migrations in memory, exercise positive and negative relationship fixtures, check the inclusive grace constraint, and verify maintained documentation links. These checks do not require the original design folder. They do not replace Rust lifecycle tests, real transports, browser journeys, or deployment acceptance drills.

## Performance profiles

- **P0:** no jobs/services; API/MCP off; 5 warm steady-state 10-minute samples on pinned Linux host; RSS, PSS, CPU by cgroup, DB size, startup; target hypotheses ≤30 MiB RSS and ≤0.5% one CPU equivalent. Report confidence, not invented guarantee.
- **P1:** 100 and 1,000 schedules; no active external processes; measure planning allocations, p95 due-to-dispatch and timer drift.
- **P2:** 10/100 short-lived external tasks with bounded output; measure throughput, process-tree RSS, process cleanup and OOM.
- **P3:** 100 services with probes; restart storm and health flapping; measure CPU and state convergence.
- **P4:** connected Vue + MCP queries + 100 SSE clients; cap memory and slow subscriber backpressure.
- **P5:** crash/restart; quantify uncertain runs, reconciliation duration and audit completeness.

**Benchmarks are comparisons only if** OS, CPU, memory cgroup, run length, workload definitions, compiler build profile, storage type and log retention are identical across competing tools. Avoid cherry-picked idle-only comparisons to Cronicle/RunWisp.

## CI gates

The current [CI workflow](../.github/workflows/ci.yml) runs repository checks, Rust format/tests/Clippy, generated-contract drift checks, Vue typecheck/tests/build/audit, Chromium journeys and container build/smoke checks. Further release gates include secret/license policy automation, fuzzing, SQLite WAL stress, performance measurements, multi-architecture images, SBOM, signing/provenance, clean install, rollback/restore and privilege review. A configured workflow is not evidence of an executed hosted run.

## Definition of done

A feature cannot be accepted without: requirement ID, approved design contract, normal and failure behavior, auth policy, automated verification, operator documentation and migration compatibility effect. For resource-related changes, record reproducible measurements and compare against an accepted benchmark baseline before claiming an improvement.
