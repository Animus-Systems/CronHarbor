# Implementation verification record

Dated entries distinguish current verification from historical runs. These results describe the local source and disposable fixtures. They do not certify a production installation or replace the remaining release gates.

## Canarias admission changes — 2026-11-08 (local branch)

Latest local checks: 296 Rust tests across 32 targets passed with no failures or
ignored tests; formatting, strict Clippy and eight repository checks passed. The
quota-boundary regression reproduced and fixed acceptance of an inexact JSON
integer or a silently clamped SQLite page limit. Dashboard 79 tests, strict types
and build passed for the unchanged generated contracts and UI. Full browser
acceptance passed 31 scenarios plus the separate-origin scenario. The main
command skipped the owner-restart and origin fixtures; origin then passed in its
dedicated run. The specialized owner-restart fixture subsequently passed in
79.6 seconds, verifying frozen UNKNOWN evidence after a real owner restart and
unavailability after a metadata-only restore. All 33 distinct browser scenarios
therefore ran successfully across the three commands.

The optimized x86_64 binary also passed a disposable Canarias integration proof:
69 disabled definitions, 6,900 matching UTC occurrences, nine simultaneous native
Node workers and nine synthetic effects, split-secret redaction on all nine,
Observe denial for control, pressure/replay and verified backup. The Observe
adapter reported 44 monitored jobs and 25 collectors in 2.57 seconds. Its SHA-256
is `adea11fe10a640c46d443934769a625f0b4895aa5941cce0a8ea682983c380a0`.
These fixtures used no business credentials or effects; live scheduler handover
and the 24-hour observation are still pending.

Earlier checks in this section retain their historical counts:

On x86_64 Linux, new admission regressions first reproduced the four-active global
cap, accepted unsafe policy files, allocation under database/WAL pressure and
unbounded main-page growth. After the changes, `cargo test --locked --all-features
--all-targets` passed 292 tests across 32 targets, with zero failures or skips.
The pinned Rust 1.99.1 checker includes Python/OpenSSL for existing fixtures.

Dashboard typecheck, all 78 unit tests and the production build passed using the
pinned Node 22 image. New trust-boundary tests first rejected the previous acceptance
of malformed admission status and generic handling of HTTP 507. Canonical Rust type
export adds optional admission status; the workload schema remains byte-identical.
All eight repository checks passed. No applied migration changed.

A follow-on status projection lets Observe with only `instance.read` inspect
log-policy limits while the detailed LogStorage command stays denied. Its Rust and
dashboard trust-boundary regressions were observed RED before implementation.
The resulting full Rust run passed 293 tests with zero skips/failures; dashboard
typecheck, all 79 tests and build passed. Canonical types include optional
`log_policy`; the limits-only projection adds no control authority or log data.

The native Canarias fixture then exposed a second four-worker assumption in log
capture. Independent nine-process output and capture-capacity lifecycle regressions
were RED before increasing its separate log-memory bound to 128 captures (128 MiB
of queued chunks, plus bounded reader/redaction overhead). No execution cap was
reintroduced. All 295 Rust tests passed with zero failures or skips. Strict capture
formatting and all-feature/all-target Clippy passed.

The x86_64 native debug daemon proof passed 69 API-owned disabled definitions,
6,900 next-due comparisons against the original Cronicle timings, nine simultaneous
native Node 24 workers with split-secret redaction, read-only policy observation,
storage-pressure admission, old operation replay and a hash-verified SQLite backup.
It used private disposable state and synthetic effects only; it does not establish
Canarias live migration, final optimized packaging or browser acceptance.

Formatting and strict all-feature/all-target Clippy passed. Full browser acceptance,
Canarias workload mapping, native packaging and live scheduler cutover remain
unexecuted for this branch; these local results do not claim a completed migration.

## Version 0.11.0 — 2026-11-08

The Rust package, dashboard package, both lockfiles and README now identify version `0.11.0`. This release includes the review repairs below and the preceding logs/webhooks feature commit. The version bump changes no dependency versions, workload API version or applied migration; the installation remains at schema 5, including the feature commit's migrations 4 and 5.

Fresh pre-commit checks on macOS passed: 284 Rust tests with all features/targets, Clippy with warnings denied, formatting, eight repository checks, dashboard typecheck/75 tests/build, Full build and `cronharbor --version` reporting `0.11.0`. Type/schema exports remained byte-identical. Full browser acceptance passed 31 scenarios plus the separate-origin scenario (32 total), including real REST/MCP/backup/restore journeys.

The Linux, Docker, HTTPS and dedicated 65-second rotation/freeze evidence in the review-repair section predates this metadata-only version bump. Those images were not rebuilt or published as `0.11.0`; live systemd/Grokbot and production-load gates remain unexecuted. Earlier records retain their then-current Git state and evidence.

## Review repairs — 2026-11-08

This pass repairs the findings against `ba42da2`, including equivalent scheduled-admission and external-terminal recovery paths identified during the fix review. It changes no applied migration. Migrations 1–5 were compared with that commit and are byte-identical. Runtime checks used private disposable state, receivers, credentials and the explicitly selected test Docker daemon.

| Area | Repaired behavior and maintained regression evidence |
|---|---|
| Executor authority | Cancel checks the immutable execution revision; service controls and revision-changing apply/batches check every affected active/UNKNOWN adapter. Denied changes remain atomic (`backend_review`) |
| Queue admission | Manual and scheduled candidates apply pinned-revision capacity eligibility before their limits. Scheduled expiry/forbid cleanup remains bounded and preserves inclusive 60-second grace and reason precedence (`backend_review`, `core`, `runtime_schedule`) |
| Recovery and operations | Verified reattachment permits an empty, error-free external snapshot to capture fresh redacted terminal output. Retained native/external evidence stays protected. Reconciliation and verified external termination resolve applicable Run/Cancel/STOP/START/RESTART operations without reviving superseded intent (`backend_review`, `runtime_external_recovery`) |
| Configuration | One budgeted YAML boundary limits raw/expanded scalars, depth, nodes, anchors and replay; strict semantic types and string keys reach the canonical Rust model. Alias amplification, coercion, duplicate/tag/key-normalization bypasses, canonical 65,536-byte dispatch and both secret-reference forms are covered (`config_parser`, `core`) |
| Executor correctness | Docker reuses canonical lowercase-capable environment-name validation; importer comments remain unsupported. External deadlines use monotonic time across pending start and in-owner UNKNOWN reattachment, with bounded deadline bookkeeping (`external`, `importer`, server unit tests) |
| Owner lifetime | Graceful close explicitly unlocks after SQLite closes, including when a child retains the file description. A deterministic inherited-descriptor test replaces the previously intermittent failure mechanism; forced termination still does not execute destructors (store unit test) |
| CLI | Local FILE apply preserves original command identity; explicit revisions retain CAS. Bounded cursor options expose inventories, services filter before LIMIT, and new `executions list` preserves page metadata while bare `executions` stays an array (`cli`, `runtime_application`, `api`, `mcp`) |
| Dashboard | Both lists reject stale responses/errors and duplicate owned page requests; workload detail coalesces invalidations. A reactive snapshot counter provides notifications while displayed refresh time remains actual wall time. HTTP 408 keeps the immutable intent for explicit retry (`store.test.ts`, `detail-recovery.spec.ts`) |
| Documentation | Corrected startup pause reapplication, alert destination identity/retirement/retry eligibility, Viewer read roles, Settings retention controls, browser fixture prerequisites and historical verification claims |

Root independently executed the following against the repaired source:

| Check | Result |
|---|---|
| macOS arm64 Rust | `cargo test --locked --all-features --all-targets`: **284 passed**, zero failed/ignored; 31 test binaries including the empty main harness |
| Linux aarch64 Rust | Actual Dockerfile `checks` target: the same **284 tests** and all-feature/all-target Clippy with warnings denied passed. An initial linker ENOSPC in the disposable VM was followed by a clean target-cache rebuild; no test or compiler setting was weakened |
| Formatting/static checks | `cargo fmt --all --check`, root all-feature/all-target Clippy with `-D warnings`, eight repository checks, migration comparison and diff whitespace checks passed |
| Contracts | Fresh Full `export-types` and `export-schema` succeeded; both generated files were byte-identical to the committed artifacts |
| Dashboard | Host typecheck, all **75 unit tests** and production build passed. The pinned Node 22 Full image independently ran the same 75 tests and production build |
| Browser and real daemon | `test_full.py --browser` passed 31 first-pass Chromium scenarios and the separate-origin scenario; `test_log_rotation.py --browser` passed the specialized owner-restart/freeze scenario: **33 distinct scenarios**. The suite combines synthetic UI failures with real API journeys. Both initially skipped specialized scenarios were executed through their dedicated fixtures |
| Feature isolation | Fresh Core, Core+webhooks, API and Full builds passed CLI/server interface checks: Core variants expose no HTTP/MCP, API exposes HTTP without MCP, Full exposes both |
| Real transports/executor | `test_webhooks.py` passed against final Full and Core+webhooks binaries using disposable private-CA HTTPS receivers. `test_docker_executor.py` passed against the explicitly selected disposable daemon and pinned Alpine image, including terminal logs/cleanup and outage UNKNOWN without replay |
| Dependency state | Public npm lockfile audit: 130 entries, zero known vulnerabilities. OSV batch query: 278 Rust registry packages, no findings. `serde_yaml_ng` and `unsafe-libyaml` are absent; the new parser enables only deserialization. This dependency result is separate from scoped source review |

Fresh local Linux aarch64 Core/API/Full images built successfully. Core smoke passed nonroot/read-only/no-TCP operation, second-owner denial, scheduled output, bounded logs and restart uniqueness. Full smoke passed packaged UI, bearer bootstrap/status and packaged Docker client without a socket mount. The API smoke passed loopback health, bearer enforcement, authenticated status, disabled unconfigured alerts and HTTP-without-MCP isolation. The Core smoke used a disposable Compose image override; existing development-image tags were preserved.

- Core: `sha256:3706801494c4f1ee0dde4f5050168e9dd42186cf0bc9e5af544e7fa6341a876d`.
- API: `sha256:84307977829f2cf19330367cd59ea8cbadfdfd8d218686e624d46216306ac50a`.
- Full: `sha256:e89ce74854e09f2aacf8b14591ab41a548d5e6321611469790d09473a52bc758`.

The same 20,639-byte alias fixture returned a late generic schema error with 29,835,264-byte maximum RSS before the parser change, then a parsing-budget error with 13,254,656-byte maximum RSS afterward, measured with `/usr/bin/time -l` on this macOS host. This is one bounded diagnostic fixture, not a production resource or throughput guarantee. Fresh source compliance/quality reviews covered each bundle; a separate security bypass review found the additional scheduled and terminal-recovery cases above, which were reproduced before repair.

Source-format tightening is deliberate: quote numeric/boolean/null-looking text and mapping keys, use explicit empty collections, and remove duplicate keys or unsupported custom YAML tags before upgrade. Persisted canonical JSON remains compatible. Earlier CLI update keys can contain an implicitly planned revision; the CLI cannot infer that old request. An original explicit revision permits single-file recovery; do not substitute a new key for an uncertain intent merely to avoid a conflict.

Live application API/Grokbot processing, host systemd integration, production migration/load, x86_64 and physical disk/OOM/power-loss drills remain unexecuted. Execution rows still accumulate through manual pagination in the existing nonvirtualized table; this repair does not establish a global execution-DOM limit or a large-history performance result. No commit, push, deployment or publication is part of this pass.

The 31 intended files were transferred byte-for-byte to the normal checkout on `main`, with no conflicting user edits. Full build, dashboard typecheck/75 tests/build, formatting, repository checks and diff checks passed there. Disposable containers/volumes and test daemons/Vite processes were gone; port 5186 was clear. The dedicated Colima `cronharbor` profile was restored to Stopped and the global Docker context remained `desktop-linux`.

## Log controls, rotation and outbound alerts — 2026-11-07–08

This earlier feature baseline added one installation log policy, opt-in native segment rotation and a durable HTTPS alert sender. It added migrations 4 and 5; migrations 1–3 were compared byte-for-byte with its base commit and were unchanged. Configuration and receiver responsibilities are in [RUNBOOK.md](RUNBOOK.md) and [the webhook contract](api/WEBHOOKS.md). No production state, endpoint or credential was used. Its counts and image IDs below remain historical; the review-repair record above supersedes them for the current source.

| Check | Executed evidence |
|---|---|
| Rust on macOS arm64 | `cargo test --locked --all-features --all-targets`: 231 passed, zero failed/ignored. Fresh format and all-target Clippy with warnings denied passed |
| Rust on Linux aarch64 | Fresh Docker `checks` target: the same 231 tests and Clippy passed. Python/OpenSSL were added only to that test stage for disposable process/TLS fixtures |
| Dashboard | Typecheck, all 62 unit tests and production build passed. The pinned Node 22 image independently ran the 62 tests and build; the host used Node 24.22.1 |
| Browser/real transports | `test_full.py --browser` passed 29 first-pass scenarios plus the separate-origin scenario. The dedicated real owner-restart/freeze scenario passed through `test_log_rotation.py --browser`: 31 distinct Chromium scenarios in total. The suite mixes explicitly synthetic UI failures with real daemon/API journeys; it is not 31 independent live integrations |
| Long-running rotation | Real service producing approximately 400 KB/s into a 128 KiB retained window, followed for 65 seconds with an additional manual fetch. Output kept advancing, there were no HTTP 429 responses, and combined expensive requests stayed within the fixture's limit of 35. Owner SIGKILL/restart produced UNKNOWN/FROZEN; retained hashes stayed fixed and an unsafe policy decrease was denied |
| Cursor/redaction/restore | A numbered 390,889-byte producer retained 128,745 bytes from raw offset 262,144, within two segments. Secrets were absent; explicit expired offsets returned 409. Omitted offsets use one atomic retained-range read through REST/CLI/MCP. Metadata-only restore stayed paused and reported missing output as unavailable, with protected evidence and no fabricated EOF |
| HTTPS alerts | `test_webhooks.py` passed against Full and Core+webhooks binaries: actual private-CA TLS/bearer authentication, final failure after retries, identical ID/body after receiver commit/lost acknowledgement/owner restart, durable receiver deduplication, service stop/recovery correlation, one idempotent manual retry, restore-held suppression and fresh post-resume UNKNOWN metadata |
| Source/queue/network boundaries | Rust regressions cover source/enqueue rollback, retry/expiry/caps, terminal-history bounds, orphan manual operations, aged restore-held rows, destination retirement, strict persisted envelopes, private-address approvals, TLS failures, proxy/redirect behavior, response header bounds and bounded worker cancellation. Network validation does not depend on an Internet-accessible test endpoint |
| Shared administration | Actual REST/CLI authorization, revision CAS/idempotency and accepted-versus-completed operations passed through the full acceptance script. Scoped Admin is denied global policy/alert administration; MCP retains its ten read-only tools |
| Profiles | Independent Core, Core+webhooks, API and Full host builds passed feature isolation. Core rejects alert configuration before state/listener creation; Core+webhooks sends outbound without exposing TCP/MCP switches. Core/API/Full packaged-image help checks also passed |
| Generated artifacts/repository | Fresh Rust DTO/schema exports were byte-identical to the intended generated files. All eight repository checks, Python script compilation and `git diff --check` passed |
| Dependency/source hygiene | npm audit: zero known vulnerabilities in 130 dependencies. Official OSV batch lookup of all 270 locked Rust registry packages returned the existing `RUSTSEC-2024-0449` unmaintained `unsafe-libyaml` advisory through `serde_yaml_ng`, with no patched version. This is a maintenance issue, not evidence of an exploitable defect or a completed independent security audit. Gitleaks found no secrets in the intended tracked/new source inventory, excluding private state and build/dependency output |

Independent specification and quality reviews found and helped reproduce failures before approval. Corrections include escaped descendants retaining log pipes, queue overflow during owner I/O, retention of unavailable evidence, late cancellation after verified exit, deletion before an outer result transaction committed, stale retained-range reads during rapid rotation, and stale diagnostic retry evidence after an attempt change. The final focused regression verifies diagnostic failures back off to 15 seconds; normal queued retries get a five-second check. Tests verify outcomes and durable records, rather than treating an accepted request as completion.

### Packaged runtime evidence

Fresh local Core/API/Full images built successfully on Linux aarch64. Their image IDs are local evidence, not published release references:

- Core: `sha256:bf5ffc497ae425924b415b95303bc5eedffb2edba4e02dcab035f183862f6dd4`.
- API: `sha256:4538727a5b46f5c54c032f46bfd32cb5537630eec526a5b1207e3aa0683cd755`.
- Full: `sha256:225e03451b59b21dfd35159310416f04bc60720abd28878048aafd0b527a2a9d`.

Core smoke verified nonroot/read-only operation, no published ports or Docker socket, second-owner denial, real scheduled output, bounded logs and restart history without duplicate occurrences. Full smoke verified the packaged dashboard, bearer requirement, local token bootstrap, authenticated status and packaged Docker client without a socket mount. A separate API smoke verified loopback health, bearer enforcement, authenticated status and disabled alerts when unconfigured. Disposable smoke containers/volumes were removed. The Core smoke used a temporary Compose image override, preserving existing development-image tags.

`test_docker_executor.py` also passed against the explicitly selected disposable daemon and pinned Alpine image: opt-in policy, real owned container/output, terminal capture and cleanup, followed by daemon-outage UNKNOWN without automatic retry. No production Docker socket was used.

### Slow/full storage observation

`test_log_pressure.py` compared base commit `18ec9d6` and the current Full build on the same macOS arm64 host using private state. Every case produced 26,214,400 bytes, made 40 concurrent CLI/REST read samples and scheduled an unrelated due job. A temporary log-path-only interposer added 25 ms per write/sync; the failure cases returned ENOSPC without filling a real device.

| Case | Maximum / mean read ms | Due-to-start ms | Injected writes / ENOSPC |
|---|---:|---:|---:|
| Base, slow | 75.10 / 38.13 | 28.54 | 257 / 0 |
| Rolling, slow | 540.59 / 76.64 | 33.93 | 22 / 0 |
| Base, full | 74.29 / 38.00 | 27.57 | 2 / 2 |
| Rolling, full | 110.36 / 49.70 | 38.37 | 2 / 2 |

The slow rolling capture stopped with `CAPTURE_BACKPRESSURE`, after saving 1,376,256 bytes and counting 24,838,144 discarded bytes; it retained a 131,072-byte suffix. The full rolling capture stopped with `LOG_STORAGE_FAILURE`, saved zero and counted all 26,214,400 bytes discarded. In both cases saved plus known discarded equaled produced bytes, the verified process still succeeded, and capture did not silently resume. Capture queue contention is separately covered at its 64-message/1 MiB slot limit. These observations show the owner remains responsive in this fixture, with worse read latency than the base under slow storage. They are not an internal loop-duration measurement, throughput promise or performance improvement. The simpler single-owner coordinator is retained; a stricter device-latency requirement would require a separately designed acknowledged writer protocol.

### Boundaries still requiring external evidence

The real application ingestion API and Grokbot are not implemented or deployed by this repository. The disposable receiver proves the publisher contract, not bot processing. CronHarbor cannot emit alerts while its own daemon/host is down; external monitoring remains necessary for that condition.

Capture budgets cover tracked logical log-file bytes, not whole-filesystem allocation, SQLite/WAL, backups or executor-owned logs. Protected UNKNOWN/unavailable evidence may consume the budget and stop new capture. Native rotation is opt-in; external executors retain bounded terminal snapshots. Synchronous owner filesystem operations can still delay commands and dispatch on a sufficiently slow device; bounded reader queues stop capture instead of growing memory or silently resuming after loss. No production latency or resource improvement is claimed.

Live main-API/Grokbot integration, production migration/load, live systemd, x86_64, physical disk exhaustion/corruption, power loss, OOM, independent security assessment, SBOM/signing and multi-architecture publication remain unexecuted. The isolated ENOSPC/slow-write fixture above does not replace those gates. Nothing was committed, pushed, deployed or published as part of these local checks.

The reviewed implementation was transferred byte-for-byte into the normal checkout on `feature/log-controls-alerts`, preserving the pre-existing `.gitignore` edit and ignored planning documents. Repository checks, format, Full build, dashboard typecheck/tests/build were rerun there. Disposable Docker containers/volumes and CronHarbor/Vite processes were gone after verification; port 5185 had no listener. The dedicated Colima `cronharbor` profile is Stopped and the global Docker context remains `desktop-linux`.

## Historical verification before log controls and alerts

The sections below retain earlier evidence. Their 168 Rust / 56 unit / 24 browser counts, image IDs and then-unexecuted checks describe those earlier revisions; the dated section above is the current change's record.

### Executed checks

| Check | Actual evidence |
|---|---|
| Rust, macOS arm64 | 168 tests passed with all features/all targets after the security fixes; format, Clippy with warnings denied, Full build and generated-artifact comparisons passed |
| Rust, Linux aarch64 | Fresh pre-commit Docker `checks` build passed all 168 tests in 26 suites and Clippy with warnings denied, including the security fixes |
| Original design artifacts (historical) | 13 Python assertions passed against the original embedded SQL and design contracts. The maintained repository check now reads real migrations and current documentation directly; no design folder is required |
| Dashboard | Typecheck, 56 unit tests, production build, 24 distinct Chromium browser scenarios and npm audit passed; audit reported zero known vulnerabilities. The browser checks include real production CSP/CORS across two HTTP origins. Initial desktop/mobile overview, service evidence and settings screenshots were visually inspected |
| Real command service/transports | `scripts/test_full.py --browser` passed against a full binary: REST authorization/CAS/idempotency, actual jobs and service start/restart/stop, official SDK stdio MCP and restore quarantine/resume. Both REST and CLI operation lookup allow the target namespace and deny an unrelated namespace |
| Restore copy failure | A 65,536-byte file-size limit interrupted a real replacement copy; existing database hash and workload count remained intact, with no restore marker installed |
| Restore replay | A real process produced an external effect after a snapshot; restoring the snapshot under skip/latest/catch-up stayed paused and advanced beyond the explicit cutoff without repeating the effect |
| Docker workload adapter | `scripts/test_docker_executor.py` passed against an explicit local test daemon: pinned image, real owned container/output, terminal cleanup, daemon outage → UNKNOWN without retry |
| systemd bridge | Allowlist, literal start/show/stop and persisted start-time journal filter verified using bounded mock executables; native command/env/limit settings are rejected for unit-only definitions |
| Log boundaries | Malformed UTF-8/emoji raw cursors, secret rotation recovery, 7,000-byte secret under a 4,096-byte log cap, helper-capture truncation and 301-line capture regressions passed |
| Generated contract/schema | Rust DTO/schema export ran successfully. Draft 2020-12 schema and six workload examples independently validated; runtime validation accepted all six |

The complete `test_full.py --browser` run also passed using the rebuilt full binary. Full application verification runs locally before commit/push. Hosted CI only runs the fast repository migration/document-link checks; its status does not represent the full suite.

## Documentation fixes and pre-commit verification

The 2026-11-07 documentation review's eight findings are corrected: backup exclusions and recovery prerequisites, process-relative configuration paths and installed service examples, plan/revision-CAS apply, role/capability grants, Chromium installation, audit retention, POST media types/framework error exceptions, and Linux-only peer UID verification. No runtime source, dependency, contract or migration changed in this documentation pass.

Disposable real-daemon checks executed the documented `curl`/`jq` plan/apply blocks for both creation and update, an absolute-path daemon configuration from an unrelated working directory, and the scoped service-operator credential/control example. They confirmed 415 for unsupported POST content type, 400 plaintext for malformed UTF-8 paths, SQLite-only backup exclusions, and paused recovery without an HTTP listener. Independent final review caught and corrected the extra `execution.read` permission needed for Observe tokens using HTTP logs.

Fresh publication checks passed: 168 Rust tests on each of macOS arm64 and Linux aarch64, format, Clippy, Full build, eight repository checks, 56 dashboard unit tests, typecheck, production build, byte-identical generated DTO/schema comparisons, npm audit (zero known vulnerabilities), and all 24 distinct Chromium scenarios through real-daemon acceptance. Gitleaks scanned the intended source files with no secrets found; the final inventory includes source, lockfiles, migrations, examples, documentation and assets, excluding private state, build output and dependencies.

Core/API/Full images were rebuilt from the current source and profile isolation passed. Core/Full container smoke and the real opt-in Docker adapter acceptance passed, including owned log capture/cleanup and daemon-outage UNKNOWN behavior. The dedicated test Colima profile is stopped after cleanup; no images were published. Live systemd, production migration/load, x86_64, power-loss/OOM and release signing/licensing gates remain unexecuted.

## Additional failure-boundary tests

The 2026-11-07 hardening pass added 35 Rust tests, 36 frontend unit cases and three browser scenarios beyond the original 93/20/12 suite. The added tests run through the normal local Cargo, Vitest and Playwright commands; no new test dependency or public DTO was introduced. Generated TypeScript/schema exports remained byte-for-byte unchanged.

| Boundary | New evidence and fixes |
|---|---|
| SQLite transactions | Injected attempt/audit/event/operation-acknowledgement failures and writer contention. Failed acknowledgements previously left an open savepoint with visible uncommitted effects; every effect/completion error now rolls back before the connection handles another command |
| Crash recovery | Real SIGKILL at launch-intent, start-requested, persisted-receipt/finished-effect, and terminal-result boundaries. The expected CANCELLED/UNKNOWN/SUCCEEDED evidence survives, with no same-occurrence replay |
| Restore inputs | Active-owner rejection and FIFO/symlink/hard-link/nonprivate sidecar rejection before installation. A FIFO manifest previously blocked restore; the sidecar now opens nonblocking without following symlinks and is validated through its descriptor |
| Scheduling | Interval alignment and forward/reverse ordering across anchors, integer overflow/invalid intervals, half-hour DST folds, and a skipped date-line calendar day |
| Native capture and cleanup | Split/overlapping secret boundaries, I/O-error truncation, TERM refusal/escalation, private file checks and bounded cleanup. A capture read error previously flushed a buffered secret prefix; incomplete trailing bytes are now withheld |
| External executor evidence | All five ownership labels, truncated/multiple-object inspect replies, hard-linked policy rejection and complete-name-list fallback. Failed inspection previously allowed a false absence/termination claim; only a complete successful listing that excludes the exact name proves absence |
| HTTP/local framing | Ambiguous singleton headers, malformed/duplicate/deep JSON, exact body limits, query bounds, transient SSE closure and oversized daemon replies. Header ambiguity is rejected, bad daemon output maps to service failure, and response byte limits consistently account for the newline delimiter |
| MCP | FIFO token rejection, invalid arguments without dispatch, session continuity, per-query revocation, safe errors and the full serialized result cap |
| Browser sessions and intent | Delayed connect/pagination results across logout, exact immutable mutation retry bodies, one manual retry at a time, stale editor plans, and token callbacks crossing sessions |
| Browser event recovery | Chunked/malformed SSE, cancellation of rejected bodies, and durable cursor validation. Event IDs must equal a safe integer payload sequence and advance it; malformed or backward frames cannot poison the reconnect cursor |

Fault injection exercises specific SQLite failure points and real process death. It does not simulate every fsync failure, arbitrary filesystem corruption, cgroup OOM, physical power loss, or all scheduler/executor interleavings. Test counts are evidence of executed cases, not a proof of defect absence.

## Review fixes

All twelve reported findings were corrected on 2026-11-07. The fresh checks above include 17 additional Rust tests and five additional browser scenarios. Public DTO/schema exports remained byte-for-byte unchanged; no migration or dependency was added.

| Finding | Fix and regression evidence |
|---|---|
| 1: Retry cancellation | Cancelling during backoff atomically removes the retry and finalizes the execution/original operation without rewriting the failed attempt. Reopen/idempotency and injected finalization-failure rollback are covered |
| 2: UNKNOWN retention | Unresolved logs survive segment, disk and age pressure while still consuming budgets; normal retention resumes after evidence-based reconciliation. Recovered external captures are indexed even when the outcome remains UNKNOWN |
| 3: systemd terminal result | Validated Result/ExecMainCode/ExecMainStatus determine the outcome. Timeout with status zero fails; configured nonzero and clean-signal success remains successful; incomplete evidence remains UNKNOWN |
| 4: Log ownership | Capture requires fresh verified ownership. Inspection loss withholds logs, marks capture incomplete and retains the container for investigation |
| 5: Redaction bounds | One pass over original bytes selects the longest secret match without rescanning markers. Expansion and exact-cap/truncation cases stay bounded |
| 6: Stop grace | Docker receives validated 0/120/300-second values. Shutdown drains pending stops, including cancellation arriving during observation; supplied Compose/systemd supervisor grace is 360 seconds |
| 7: STOP then START | A real slow-TERM process is replaced and the START operation completes with never/on-failure automatic restart and zero restart budget |
| 8: Failed-service revision | A new revision relaunches a failed service with exhausted restart budget, preserving UNKNOWN evidence and explicit STOPPED intent |
| 9: Operation authorization | Other-actor lookup requires the target namespace plus operations.manage. Workload/execution/apply targets, own-actor access and wildcard/batch scope are covered; real REST and CLI checks also pass |
| 10–12: Dashboard detail | Looking up operation B replaces routed operation A; refresh includes new execution history; successful recovery clears transient detail errors. Late route/session responses cannot populate the next view |

A repeated Linux run exposed uncertainty in the new slow-stop fixture: it forced simultaneous shell/child termination, and the daemon correctly returned UNKNOWN instead of replacing an unverified process tree. The fixture now exits cleanly after recording TERM, within its grace period. Its eight-second assertion deadline is unchanged; five consecutive concurrent Linux service-suite runs passed. Separate tests still exercise kill escalation and uncertain cleanup.

## Second review fixes

The next ten reported findings were fixed one after another on 2026-11-07, with failing regressions before each behavior change. This pass adds 13 Rust tests and four browser scenarios. No migration, dependency or public DTO change was needed; regenerated contracts/schema compare byte-for-byte with the existing artifacts.

| Finding | Fix and executed regression evidence |
|---|---|
| 1: Native owner restart | A real native service resumes after clean daemon shutdown under on-failure and never policies, with zero restart budget. Explicit STOP persists and DAEMON_SHUTDOWN is recorded separately |
| 2: Reconciliation race | Reconciled attempts retire their watches before acknowledging success; stale running/finished observations cannot overwrite state or terminate the owner. Gated tests immediately release ordinary, stop-helper and log-helper inspection after that acknowledgement, without a scheduler-loop barrier, and verify no follow-up command |
| 3: Restore controls | A real backup/replace restore with an expired external timeout issues no automatic executor calls during quarantine. Reattachment resumes only after explicit resume; uncertain capacity stays reserved |
| 4: Interrupted service intent | Provably unlaunched service intents return to an eligible state after owner recovery. START_REQUESTED uncertainty remains UNKNOWN and cannot launch again |
| 5: Rate capacity | 1,024 invented bearer values on unknown routes cannot reserve credential buckets. Invalid operational bearers remain unauthorized and a fresh real viewer still receives status; normal request/log limits remain covered |
| 6: Retry selection | An older retry whose workload is full cannot starve another workload's admissible retry. Reconciliation releases the blocked retry without deleting its intent |
| 7: Operation completion | Exhausted START/RESTART operations fail with the observed cause; interrupted manual launches and discarded restored retries finish their original operations transactionally. Failed attempt evidence remains intact |
| 8: Retry cancellation UI | Browser cases exercise cancellation while the latest attempt is FAILED, including an initial diagnostics 503 recovered by polling or Refresh. The queued-retry warning and transient errors update without replaying mutations |
| 9: Recent failures | Old executions with new failed/timed-out completions appear within the window and completion ordering is correct. Unresolved UNKNOWN is age-independent, namespace filtering holds, and ordinary history pagination is unchanged |
| 10: Remote API hosting | CSP admits only the configured API origin; unsafe configuration fails closed. A real second HTTP host, daemon-backed API, CORS and browser authentication pass together |

The main browser pass executes 23 same-origin scenarios and intentionally skips the separate-origin scenario. The acceptance script then executes that scenario against two real HTTP origins, for 24 distinct executed cases. Its output directory and list-only reporter preserve the main pass's screenshots and HTML report.

Independent final review reproduced and challenged the fixes. It exposed the nested-inspection retirement and diagnostics-recovery cases above; both were reproduced failing and verified passing before final checks. Rebuilt macOS and Linux suites passed all 158 tests, and real REST/MCP/Docker/restore plus Core/Full smoke checks passed. Live host systemd, arbitrary process-tree containment, power-loss/OOM and production load remain separate acceptance gates.

One macOS rerun encountered OWNER_UNAVAILABLE during the existing lost-receipt test's immediate owner handover; five repeated process-suite runs then passed. That fixture now waits at most two seconds, retrying only OWNER_UNAVAILABLE and failing other errors immediately. Production locking and exclusive-owner assertions are unchanged. The [Rust file-lock contract](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock) accounts for inherited handles; parallel spawn inheritance is consistent with the transient, not a proven trace of that individual failure.

## Security review fixes

Both confirmed medium-severity findings were corrected locally on 2026-11-07, without the account-gated security service. Ten regression tests were added: seven shared authorization/audit/migration cases, two API cases, and one older-snapshot restore case. The complete macOS suite now passes 168 tests in 26 suites, with zero failures or ignored tests.

| Finding | Fix and executed evidence |
|---|---|
| Cross-namespace retention | Single/batch preflight requires Admin + `operations.manage` + wildcard namespaces for changes to the three global retention fields. Scoped creation cannot tighten current effective limits, including default creation when the installation permits a larger disk budget. Tests deny scoped Admin, wildcard Editor and insufficient grants; safe timeout/per-run changes, nonrestrictive creation, global administration and local FILE apply remain allowed. Existing active/UNKNOWN retention tests also pass |
| Rejection-driven storage growth | A constant-space API verification budget limits unknown credentials before all daemon work, including log lookup and SSE. Daemon rejection writes use a monotonic one-second sampling interval and retain only the latest 1,000 rejection records, leaving other audit actions intact. Repeated/rotating/short invalid credentials, valid-token continuity, revocation, window recovery, hash-table capacity and fail-closed audit errors are covered |
| Schema compatibility | New checksummed migration 3 adds the rejection-only index and bounds existing rejection history. Migrations 1/2 are unchanged. Version-2 upgrade keeps trusted audit history and a private pre-migration backup; an altered migration checksum is rejected. A version-2 snapshot restores, upgrades to version 3 and remains paused/quarantined. Existing backup, restore-failure and replay tests pass |

The original vulnerability assertions failed before the fixes: scoped edits/creation were accepted, 300 invalid daemon authentications produced 300 records, and repeated invalid HTTP requests never throttled. The corrected regressions pass. Independent local candidate review also identified and reproduced an authenticated-window expiry regression; verified hashes now remain in the bounded table across counter resets, while every command still checks daemon authorization and revocation. The failing expiry test then passed, and the independent API review passed all five API unit tests.

Fresh checks: all-feature/all-target Rust tests, format, Clippy with warnings denied, Full build, eight repository checks, TypeScript typecheck, 56 dashboard unit tests, production UI build and byte-identical generated DTO/schema comparisons. `scripts/test_full.py --browser` passed against a uniquely copied Full binary: actual REST/CLI permissions and operation scope, CAS/idempotency, service controls, bounded logs, read-only MCP, restore copy failure/quarantine/resume and all 24 distinct Chromium scenarios, including the separate-origin case.

No dependency or public DTO changed. No production state, deployment or hosted security-model workflow was used. Linux/container builds, Docker acceptance, live systemd, sustained hostile load, physical power loss and version-1 snapshot fixtures were not rerun for this patch. New or evicted credentials can receive 429 while the shared verification budget is exhausted; this is documented admission behavior, not cached authorization. Rejection samples are deliberately not a complete count of authentication attempts. The earlier dependency-maintenance advisory was outside these two vulnerability fixes.

## Packaged images

The image IDs below are fresh local pre-commit builds including the security fixes. Linux aarch64 profile checks verified that Core has no TCP/MCP switches, API has TCP without MCP, and Full has both. These local images have not been published.

- Core smoke passed: UID 10001, read-only root, no published ports/socket mount, second owner denied, real scheduled output, durable history and no duplicate occurrence after restart. Local image ID: `sha256:98b8cb58a9e1a7451da369d7d3f5adeae45a120d10b25dbc4f74fbdb5a9ca227`.
- API release build and profile isolation passed. Local image ID: `sha256:1b4b51676dbe26559e587ae49ed9a8ed547a56ef392c9f8b66796d4e737fd651`.
- Full smoke passed: packaged dashboard, loopback HTTP, unauthenticated API rejection, local token bootstrap, authenticated status and packaged Docker CLI without daemon socket. Local image ID: `sha256:21e7b93e95053b65403bc93713c4cb02e08e1229dff408d11a862a787182558b`.
- `scripts/smoke_docker.py` and `scripts/smoke_full_docker.py` removed their disposable containers/volumes. No multi-architecture publication or x86_64 run is implied by these local results.

## Requirement-to-evidence mapping

The original scenario identifiers are in [the retained acceptance catalog](ACCEPTANCE.md). A row identifies evidence and limits; it does not silently promote a narrower fixture into the entire proposed acceptance matrix.

| Requirement/scenario | Implemented behavior and executed evidence | Remaining limit |
|---|---|---|
| CH-REL-01 / T-001 | Exclusive owner lock; `core` owner regression and real Compose second-owner denial | Multi-host/shared-network storage is outside scope |
| CH-REL-02 / T-002 | Actual SIGKILL after spawn before receipt; restart records UNKNOWN and does not redispatch (`crash`, `process`) | Failure injection after every durable write/external call has not been exhausted |
| T-003 | Durable unique due occurrence and UNKNOWN capacity regression (`core`) | High-contention WAL stress remains a release drill |
| CH-SCH-01 / T-004, T-005 | Europe/Madrid first/second/both fold and skip/next-valid gap fixtures (`runtime_schedule`) | Live Cronicle inventory parity requires frozen source fixtures |
| CH-SCH-02 / T-006 | Bounded skip/latest/catch-up generation; lateness and backlog regressions (`core`, `runtime_schedule`) | Full large-inventory downtime benchmark not measured |
| CH-SCH-03 / T-007 | Declared overlap/concurrency and global admission count active plus UNKNOWN attempts | Production workload capacity sizing not measured |
| CH-API-01 / T-008, T-009 | Durable idempotency, conflicting digest and revision CAS regressions plus real REST runs | No distributed command owner |
| T-010 | Real timeout, TERM/KILL, forked probe cleanup, bounded helper EOF, concurrent disk-log caps | No adversarial fork-bomb/cgroup calibration; native same-UID execution is trusted |
| CH-DKR-02 / T-011 | Default-disabled adapter, explicit policy/ownership validation, real Docker outage and fake recovery/rotated-secret regressions | Full privileged-daemon incident matrix and real container-engine crash recovery not exhausted |
| CH-SEC-02 / T-012 | Daemon-side namespace/capability checks, safe-editor probe-edit denial, job-only service Pause/Cancel denial, global controls reject namespace-scoped admin | No independent penetration test claimed |
| CH-MCP-01 / T-013, T-014 | Fixed read-only tools, Observe-only identity, hostile-log data annotations, protocol 2025/2026 tests and real stdio exchange | No write-MCP tools by design |
| CH-UX-01 / T-015 | Shared operation state, reconnect snapshots and no automatic mutation retry; browser interruption tests | Sustained 100-client SSE load not benchmarked |
| CH-OPS-01 / T-016, T-021 | Hash/integrity/FK/schema checks, staging before replacement, incomplete-install guard, token invalidation, actual post-snapshot effect drill for all misfire policies | Power-loss/fsync fault simulation and real disk-loss recovery not exhausted |
| CH-SEC-01, CH-DKR-01 / T-017 | Core nonroot/read-only/no ports/no Docker socket; owner-only health and real scheduled output | Only locally reported architecture tested |
| CH-CONF-01 / T-018 | FILE/API source authority and atomic batch/CAS enforced in daemon, REST and dashboard | Existing foreign authority requires explicit operator migration |
| T-019 | Passive importer blocks or flags hardened conversions; bridge keeps exact pre-existing unit | Real hardened host unit, AppArmor and polkit integration not exercised |
| T-020 | Bundled SQLite 3.54.1; startup version floor/known patched-runtime exceptions | Future upstream advisories need normal dependency maintenance |
| T-022 | Inclusive 60-second grace, canonical-size and dispatch recheck regressions | Wall-clock/host failure matrix not exhaustive |
| CH-MIG-01 | Passive bounded import, disabled proposals, unsupported/security-gap classification, next-100 UTC projections | No access to live production inventory; exclusive cutover and source parity need operator evidence |

## Reproduce

Use the commands in [IMPLEMENTATION.md](../IMPLEMENTATION.md) and [RUNBOOK.md](RUNBOOK.md). The acceptance scripts create private disposable state and remove their test containers/volumes. Explicitly select a test Docker context; no production daemon is assumed. `test_full.py --browser` requires the dashboard dependencies, build and Playwright Chromium.

Docker targets build Core/API/Full separately. The `checks` target runs all Rust features; the full target also builds and tests the Vue source. Generated DTO and schema exports must match committed artifacts before publication.

After verification, all disposable containers/volumes and application test processes were gone. Ports 5173, 5176, 5177, 5178, 5179 and 8090 had no listeners. The dedicated Colima cronharbor profile was restored to Stopped; the global Docker context remained desktop-linux.

## Documentation and planning-folder retirement

The README, contributor instructions, and maintained checks were updated on 2026-11-07. `scripts/check_repository.py` replaces the former design-artifact script and applies the actual checked-in migrations, including runtime migration 2. All eight relationship/grace/document-link checks passed with the original planning folder temporarily absent. `cargo check --locked --all-features` also passed with that folder absent; active CI and documentation no longer reference it.

The retained [acceptance catalog](ACCEPTANCE.md) preserves scenario IDs and release/performance gates. The root README includes an original SVG banner, a real test-workload dashboard screenshot, and two Mermaid diagrams. Both diagrams rendered successfully in local Chromium using Mermaid 12.3.0, and the banner was XML-validated and visually inspected. GitHub's hosted rendering was not exercised.

The later twelve-finding fix pass reran the application runtime suites, browser checks, real transports/executors and container builds. The results at the top of this record describe that pass.

## External acceptance gates

Unexecuted gates include the real Banana Ventura migration/cutover, host systemd security drill, the complete disk-full/OOM/power-loss fault matrix, performance profiles P0–P5, x86_64 execution, independent security review, Rust advisory/license policy automation, SBOM, signing/provenance and name/legal clearance. No production migration, public release or published image is claimed by these local checks.

## Scoped systemd controller — 2026-11-09 (prepared source)

The v0.11.1 candidate passed scoped-manager and historical-baseline authorization,
actual invocation identity, physical-unit ownership, 900/901-second validation,
reference lifetime, preflight rejection and recovery checks. The all-target Rust
run exposed a shutdown regression in uncertain legacy controls; that target was
interrupted, fixed, and passed with the library and all affected external targets.
Other all-target results were retained. Strict Clippy, format, generated exports,
workload schema and eight repository checks passed. Dashboard types, 79 unit tests
and build passed. Full browser/API/MCP/backup acceptance passed 31 scenarios plus
its separate-origin scenario; the optional owner-restart browser fixture was
skipped in this command. The first browser attempt lacked the installed Chromium
mount; the passing run reused the VM's existing read-only browser installation.

A real user-systemd fixture passed instantaneous and 16-second oneshots, adoption
without changing PID/invocation, owner restart reattachment and verified Service
stop, with zero UNKNOWN. It used disposable units and private state only. Existing
Python dbus references retain completed oneshot evidence; permanent Services need
no extra RefUnit authority. Compose validation passed. Optimized release, packaged
container acceptance, integrated review and live four-unit handover remain pending.

The real user-systemd drill also passed quiescent rollback to the existing v0.11.0
binary and back, preserving synthetic counters and execution history. A stuck
metadata-reference helper regression reproduced excessive serialized shutdown
cleanup; cleanup is now bounded to 100 ms per reference, leaving headroom for
the 900-second service drain. An active unit with no invocation receipt is rejected
before any StartUnit request rather than manufacturing an uncertain launch.

The final independent review found three Important defects. Each regression failed
first and now passes: transitional systemd states remain STARTING, scheduled
StartUnit receipts detect competing starts, and scheduled logs filter the persisted
invocation. The fresh all-feature/all-target Rust suite passed 319 tests across 35
targets, followed by strict Clippy and unchanged canonical exports. Eight repository
checks and Python compilation passed. The real user-unit drill passed a genuine
OS on-failure restart while retaining the same Harbor execution, normal startup,
fast and 16-second oneshots, same-invocation adoption, daemon recovery, stop and
quiescent old/new rollback. A separate real D-Bus race returned uncertainty and
left the foreign invocation running. These fixtures touched no business workers.
The live root PDF authorization and four-family deployment remain operator gates;
no paid qualification, production or other-host acceptance is claimed.

## Dashboard rate-limit recovery — 2026-11-09

Continuous scheduler events reproduced 122 four-request snapshots (488 reads) in
one minute, exceeding the existing 240-per-credential API window. Recovery also
ignored Retry-After. The eight new regression cases failed first. Background
snapshots now coalesce to five seconds; the API client shares a bounded monotonic
429 cooldown across reads, event-stream reconnects and writes, with no automatic
mutation replay. Both snapshot and stream recovery respect that cooldown. A default
same-origin config file removes the optional config.json 404.

Fresh dashboard tests passed 87/87, strict types, production Vite build and eight
repository checks passed. Disposable Full/browser acceptance passed 31 scenarios
(two optional fixtures skipped) plus the separate-origin scenario. A 125-second
read-only browser soak against the actual 73-workload DEV scheduler made 67 API
requests / 16 snapshots, with zero 429s, zero mutations and no browser token storage.
The server's existing 60-second connection lifetime caused two one-second offline
samples that recovered automatically. The scoped test credential was revoked.

Focused independent review found no Critical/Important issue. Deferred Minor:
when a background refresh is already pending, an action's follow-up snapshot can
wait up to five seconds; operation acceptance/status tracking remains immediate
and backend revision checks remain authoritative. This bounded delay is accepted
for this repair rather than adding another timer-preemption path.

Rust, Cargo, migrations, API rate limits and public DTOs are unchanged. The verified
optimized daemon bytes are reused from 4649ed1, SHA-256
`dcfa3fa5e2736ebc4d1c5b1fd902c520e57e34de3da1dcdbe804c1ea042743a6`;
this is not a fresh Rust build/full Rust suite claim. The immutable deployment
pairs those unchanged bytes with the newly built UI, preserves worker invocations
and 73 definitions, and refreshes monitor provenance for the selected release.

## Application HTTP readiness — 2026-11-09

Product0.12.0 adds owner-allowlisted HTTP readiness to systemd Services.
Existing manifests and public DTOs remain compatible; no migration changed.
Literal loopback-only transport rejects proxies, redirects, credentials, expansion
and overlong responses. Invocation checks and store fences reject obsolete samples.
HTTP failure reports unhealthy without introducing process restarts.

Source checks passed334distinct all-feature/all-target Rust tests (plus one nested
proxy child), strict Clippy, format, canonical exports and eight repository checks.
Current browser evidence covers32distinct primary scenarios plus separate origin;
two optional fixtures skipped. Production dashboard inputs and all four asset
hashes exactly match the verified14e8e8b release, retaining its87unit/type/build
checks; these are reused checks/assets, not a fresh frontend rebuild claim.

Optimized Core/API/Full builds and profile isolation passed. Core/API explicitly
reject HTTP manifests; Full accepts them. The18MiB Full binary SHA-256 is
`b362919443c3352a6d421845e8a0577ac2d36016363f753409ebbe51c4654587`.
Optimized REST/MCP/backup/quarantine/cutoff acceptance passed. Real disposable
user-systemd acceptance passed fast and16-second oneshots, same-process adoption,
daemon/OS restart recovery, verified stop, zeroUNKNOWN, old-format binary rollback
and journaled bad-candidate/artifact rollback. No business worker was restarted.

Minimal package images reuse those verified binaries/UI with the maintained
runtime Docker stages. Full/Core container smoke passed nonroot/read-only roots,
owner fencing, real scheduled output, history, authenticated API/UI and packaged
Docker CLI without a socket mount. Opt-in Docker adapter acceptance used owned
UUID fixtures on the selected localDEV engine; its outage case changes only the
fixture policy to a dead socket, preservingUNKNOWN without replay. It does not
stop the shared Docker engine. Disposable test containers/volumes were removed.

A real disposable Canarias inventory passed79definitions,71disabled schedules,
eight stopped Services, ten auxiliaries,44jobs/25collectors, zero omissions,
6900schedule occurrences, nine concurrent synthetic effects and storage/idempotency/
backup cases. The final optimized inventory completed in3.001seconds.

Fresh independent review found no Critical issue and two Important Python helper
uncertainty-persistence gaps. Their regressions failed first and now pass in the
56-case control/lifecycle/monitor suite and optimized real-systemd drill. Deferred
Minor: pre-promotion controller refusal can leave graph phase labelled refreshing
while the scheduler correctly reports failure; no serving effect occurs.

The six actual DEV application handovers completed on2026-11-09; see the live
receipt below. This evidence does not claim production, another-host, VM reboot, signed-in business journeys,
paid provider qualification, power-loss/OOM fault coverage or downgrade support for
new HTTP definitions in older product binaries.

### Live connection deadline regression — 2026-11-09

The actual79-workload DEV browser check found a healthy operator SSE session
disconnecting at60.1seconds, despite53HTTP200 responses and no API errors/writes.
The existing absolute socket deadline expired during authenticated response traffic.
Product0.12.1 renews it only on positive server response writes; request-header
progress cannot renew it. The real duplex heartbeat regression failed first with
TimedOut and now passes. Partial-header and absent-header deadline regressions pass.
All336distinct all-feature/all-target Rust tests plus one nested proxy child passed,
with zero skips/failures, followed by strict Clippy, format and eight repository
checks. No dependency, contract, migration, dashboard input or security quota changed.
Optimized artifact and live post-patch browser acceptance are recorded separately.

### Actual DEV application handover

The six application units were adopted after verified HTTP readiness, preserving
their existing invocations/container before old boot enablement was disabled.
The controlled Admin deployment and same-artifact API planned stop/start passed.
All original73 definitions/revisions/flags and evaluation/PDF invocations remained
unchanged. A normal API report at13:15:41UTC confirmed79bindings,44jobs,25collectors,
ten auxiliaries/eightREADY,zeroomissions/UNKNOWN. Reporting identity, generation1
credentials and monotonic sequence were preserved through an audited scope amendment.

The optimized0.12.1 Full binary built in2m09s, SHA-256
`127e9f76d0f73b37a3f1008c67b46a7b2dde8d47ce2ef9a02cdda31d8debcd39`,
from `aa55e12f1cb7e763208ad95f8c4d970e64e55e63`. It replaced0.12.0 at13:29:28UTC
after natural native-job drain and a hash/schema5/integrity/foreign-key verified
private staged backup. All79definitions and eight managed invocations were preserved;
all eight recoveredREADY and scheduled admission resumed with zeroUNKNOWN.
The actual post-patch operator browser stayed connected for120239ms across
Overview/Services/Operations:24/24 samples,65HTTP200, zero API/page errors or writes,
zero stream closures and empty local/session storage. A normal API receipt
1613 at 2026-11-09T13:30:46.218Z reports0.12.1,44jobs,25collectors,tenauxiliaries,
eightREADY and zeroomissions/UNKNOWN with the original credential identity.
Final unit/probe/independent-supervisor checks passed; disk148GiB used/130GiBfree,
9.9GiB available RAM, zero swap. Only verified unused build/package outputs were
removed. These are actual DEV receipts; signed-in business/VM reboot/other-host/
production acceptance remains unclaimed.
