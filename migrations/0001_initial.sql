-- CronHarbor v2 design prototype. NOT a production migration.
-- Requires SQLite with WAL-reset fix (3.51.3+ or documented patched backport), verified on build.
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  checksum TEXT NOT NULL,
  applied_at_us INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS daemon_instances (
  instance_id TEXT PRIMARY KEY,
  boot_id TEXT NOT NULL,
  started_at_us INTEGER NOT NULL,
  last_observed_at_us INTEGER,
  state TEXT NOT NULL CHECK (state IN ('ACTIVE','STOPPED','CRASH_SUSPECTED'))
);

CREATE TABLE IF NOT EXISTS workloads (
  id TEXT PRIMARY KEY,
  namespace TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('scheduled_job','one_shot_job','managed_service')),
  source_kind TEXT NOT NULL CHECK (source_kind IN ('FILE','API')),
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  current_revision_id TEXT,
  deleted_at_us INTEGER,
  created_at_us INTEGER NOT NULL,
  UNIQUE(namespace, name),
  FOREIGN KEY (id,current_revision_id) REFERENCES workload_revisions(workload_id,id)
    ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE IF NOT EXISTS workload_revisions (
  id TEXT PRIMARY KEY,
  workload_id TEXT NOT NULL REFERENCES workloads(id) ON DELETE RESTRICT,
  revision_no INTEGER NOT NULL CHECK (revision_no > 0),
  schema_version TEXT NOT NULL,
  spec_json TEXT NOT NULL,
  spec_hash TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at_us INTEGER NOT NULL,
  UNIQUE(workload_id,revision_no),
  UNIQUE(workload_id,id)
);

CREATE TABLE IF NOT EXISTS schedules (
  id TEXT PRIMARY KEY,
  workload_id TEXT NOT NULL REFERENCES workloads(id) ON DELETE RESTRICT,
  revision_id TEXT NOT NULL,
  schedule_kind TEXT NOT NULL CHECK (schedule_kind IN ('cron','interval')),
  expression TEXT,
  timezone TEXT,
  fold_policy TEXT NOT NULL CHECK (fold_policy IN ('first','second','both')),
  gap_policy TEXT NOT NULL CHECK (gap_policy IN ('skip','next_valid')),
  misfire_policy TEXT NOT NULL CHECK (misfire_policy IN ('skip','latest','catch_up')),
  lateness_grace_seconds INTEGER NOT NULL CHECK (lateness_grace_seconds = 60),
  next_due_at_us INTEGER,
  last_materialized_at_us INTEGER,
  active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
  UNIQUE(workload_id, revision_id),
  UNIQUE(id,workload_id,revision_id),
  FOREIGN KEY (workload_id,revision_id) REFERENCES workload_revisions(workload_id,id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS occurrences (
  id TEXT PRIMARY KEY,
  schedule_id TEXT NOT NULL,
  workload_id TEXT NOT NULL REFERENCES workloads(id) ON DELETE RESTRICT,
  revision_id TEXT NOT NULL,
  scheduled_at_us INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('DUE','QUEUED','DISPATCHED','SKIPPED','BLOCKED')),
  reason TEXT,
  created_at_us INTEGER NOT NULL,
  UNIQUE(schedule_id,revision_id,scheduled_at_us),
  UNIQUE(id,workload_id,revision_id),
  FOREIGN KEY (schedule_id,workload_id,revision_id) REFERENCES schedules(id,workload_id,revision_id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS executions (
  id TEXT PRIMARY KEY,
  workload_id TEXT NOT NULL REFERENCES workloads(id) ON DELETE RESTRICT,
  revision_id TEXT NOT NULL,
  occurrence_id TEXT UNIQUE,
  trigger_kind TEXT NOT NULL CHECK (trigger_kind IN ('SCHEDULE','MANUAL','API','INTERNAL')),
  actor_id TEXT,
  created_at_us INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('CREATED','RUNNING','SUCCEEDED','FAILED','TIMED_OUT','CANCELLED','UNKNOWN')),
  finished_at_us INTEGER,
  FOREIGN KEY (workload_id,revision_id) REFERENCES workload_revisions(workload_id,id) ON DELETE RESTRICT,
  FOREIGN KEY (occurrence_id,workload_id,revision_id) REFERENCES occurrences(id,workload_id,revision_id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS attempts (
  id TEXT PRIMARY KEY,
  execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE RESTRICT,
  attempt_index INTEGER NOT NULL CHECK (attempt_index > 0),
  launch_nonce TEXT NOT NULL UNIQUE,
  state TEXT NOT NULL CHECK (state IN ('CREATED','LAUNCH_INTENT_RECORDED','START_REQUESTED','RUNNING','SUCCEEDED','FAILED','TIMED_OUT','CANCELLED','UNKNOWN')),
  executor_kind TEXT NOT NULL,
  external_handle_json TEXT,
  error_code TEXT,
  exit_code INTEGER,
  created_at_us INTEGER NOT NULL,
  started_at_us INTEGER,
  finished_at_us INTEGER,
  UNIQUE(execution_id,attempt_index)
);

CREATE TABLE IF NOT EXISTS service_instances (
  id TEXT PRIMARY KEY,
  workload_id TEXT NOT NULL REFERENCES workloads(id) ON DELETE RESTRICT,
  revision_id TEXT NOT NULL,
  desired_state TEXT NOT NULL CHECK (desired_state IN ('RUNNING','STOPPED')),
  observed_state TEXT NOT NULL CHECK (observed_state IN ('ABSENT','STARTING','READY','UNHEALTHY','BACKOFF','STOPPING','FAILED','UNKNOWN')),
  external_handle_json TEXT,
  restart_count INTEGER NOT NULL DEFAULT 0 CHECK (restart_count >= 0),
  last_observed_at_us INTEGER,
  next_action_at_us INTEGER,
  UNIQUE(workload_id),
  FOREIGN KEY (workload_id,revision_id) REFERENCES workload_revisions(workload_id,id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS operations (
  id TEXT PRIMARY KEY,
  actor_id TEXT NOT NULL,
  request_key_hash TEXT,
  request_digest TEXT NOT NULL,
  operation_kind TEXT NOT NULL,
  target_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('ACCEPTED','RUNNING','SUCCEEDED','FAILED','UNKNOWN')),
  created_at_us INTEGER NOT NULL,
  updated_at_us INTEGER NOT NULL,
  expires_at_us INTEGER,
  UNIQUE(actor_id,request_key_hash)
);

CREATE TABLE IF NOT EXISTS events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  event_kind TEXT NOT NULL,
  workload_id TEXT,
  execution_id TEXT,
  payload_json TEXT NOT NULL,
  correlation_id TEXT NOT NULL,
  created_at_us INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS log_segments (
  id TEXT PRIMARY KEY,
  attempt_id TEXT REFERENCES attempts(id) ON DELETE RESTRICT,
  service_instance_id TEXT REFERENCES service_instances(id) ON DELETE RESTRICT,
  relative_path TEXT NOT NULL,
  bytes_written INTEGER NOT NULL DEFAULT 0 CHECK (bytes_written >= 0),
  truncated INTEGER NOT NULL DEFAULT 0 CHECK (truncated IN (0,1)),
  created_at_us INTEGER NOT NULL,
  closed_at_us INTEGER,
  CHECK ((attempt_id IS NOT NULL) != (service_instance_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS audit_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  actor_id TEXT NOT NULL,
  action TEXT NOT NULL,
  target_id TEXT,
  result TEXT NOT NULL,
  request_id TEXT NOT NULL,
  before_hash TEXT,
  after_hash TEXT,
  created_at_us INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS api_tokens (
  id TEXT PRIMARY KEY,
  actor_id TEXT NOT NULL,
  hash TEXT NOT NULL,
  scopes_json TEXT NOT NULL,
  created_at_us INTEGER NOT NULL,
  expires_at_us INTEGER,
  revoked_at_us INTEGER
);

CREATE INDEX IF NOT EXISTS idx_sched_next ON schedules(active,next_due_at_us);
CREATE INDEX IF NOT EXISTS idx_occ_state_due ON occurrences(state,scheduled_at_us);
CREATE INDEX IF NOT EXISTS idx_exec_workload_created ON executions(workload_id,created_at_us);
CREATE INDEX IF NOT EXISTS idx_attempt_execution ON attempts(execution_id,attempt_index);
CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at_us);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_events(created_at_us);
CREATE INDEX IF NOT EXISTS idx_operations_actor_state ON operations(actor_id,state);
