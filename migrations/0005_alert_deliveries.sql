-- Source markers outlive bounded outbox history and are committed with observed transitions.
ALTER TABLE executions ADD COLUMN alert_failure_outcome TEXT NOT NULL DEFAULT 'NONE'
  CHECK (alert_failure_outcome IN ('NONE','QUEUED','SUPPRESSED'));
ALTER TABLE attempts ADD COLUMN alert_unknown_outcome TEXT NOT NULL DEFAULT 'NONE'
  CHECK (alert_unknown_outcome IN ('NONE','QUEUED','SUPPRESSED'));
ALTER TABLE service_instances ADD COLUMN alert_incident_id TEXT;
ALTER TABLE service_instances ADD COLUMN alert_incident_opened_at_us INTEGER;
ALTER TABLE service_instances ADD COLUMN alert_open_outcome TEXT NOT NULL DEFAULT 'NONE'
  CHECK ((alert_open_outcome='NONE' AND alert_incident_id IS NULL AND alert_incident_opened_at_us IS NULL)
      OR (alert_open_outcome IN ('QUEUED','SUPPRESSED') AND alert_incident_id IS NOT NULL AND alert_incident_opened_at_us IS NOT NULL));

CREATE UNIQUE INDEX idx_execution_alert_source ON executions(workload_id,id);
CREATE UNIQUE INDEX idx_attempt_alert_source ON attempts(execution_id,id);
CREATE TABLE alert_deliveries (
  id TEXT PRIMARY KEY,
  source_key TEXT NOT NULL UNIQUE,
  incident_id TEXT NOT NULL,
  incident_version INTEGER NOT NULL CHECK (incident_version IN (1,2)),
  kind TEXT NOT NULL CHECK (kind IN ('job_failed','job_timed_out','execution_unknown','service_unhealthy','service_failed','service_recovered')),
  destination_hash TEXT NOT NULL CHECK (length(destination_hash)=64),
  workload_id TEXT NOT NULL REFERENCES workloads(id) ON DELETE RESTRICT,
  execution_id TEXT REFERENCES executions(id) ON DELETE RESTRICT,
  attempt_id TEXT REFERENCES attempts(id) ON DELETE RESTRICT,
  payload_json TEXT NOT NULL CHECK (length(CAST(payload_json AS BLOB))<=8192 AND json_valid(payload_json)),
  state TEXT NOT NULL CHECK (state IN ('PENDING','IN_FLIGHT','DELIVERED','FAILED','BLOCKED','EXPIRED','DESTINATION_CHANGED','RESTORE_HELD')),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 2147483647),
  nonce TEXT,
  created_at_us INTEGER NOT NULL,
  updated_at_us INTEGER NOT NULL,
  expires_at_us INTEGER NOT NULL,
  next_attempt_at_us INTEGER,
  finished_at_us INTEGER,
  delivered_at_us INTEGER,
  http_status INTEGER CHECK (http_status BETWEEN 100 AND 599),
  last_error_code TEXT CHECK (last_error_code IN ('CREDENTIAL_UNAVAILABLE','TLS_UNAVAILABLE','INVALID_DESTINATION','DNS_UNAVAILABLE','TRANSPORT_UNAVAILABLE','REQUEST_TIMEOUT','HTTP_REJECTED','RETRY_EXHAUSTED','DELIVERY_EXPIRED','DESTINATION_CHANGED','MANUAL_RECEIPT_UNKNOWN','RESTORE_REPLAY_SUPPRESSED','WORKER_UNAVAILABLE','INVALID_PAYLOAD')),
  manual_once INTEGER NOT NULL DEFAULT 0 CHECK (manual_once IN (0,1)),
  manual_operation_id TEXT REFERENCES operations(id) ON DELETE RESTRICT,
  UNIQUE (incident_id,incident_version),
  FOREIGN KEY (workload_id,execution_id) REFERENCES executions(workload_id,id) ON DELETE RESTRICT,
  FOREIGN KEY (execution_id,attempt_id) REFERENCES attempts(execution_id,id) ON DELETE RESTRICT,
  CHECK (attempt_id IS NULL OR execution_id IS NOT NULL),
  CHECK ((state='IN_FLIGHT')=(nonce IS NOT NULL)),
  CHECK ((state IN ('DELIVERED','FAILED','EXPIRED'))=(finished_at_us IS NOT NULL)),
  CHECK ((state='DELIVERED')=(delivered_at_us IS NOT NULL)),
  CHECK ((manual_once=1)=(manual_operation_id IS NOT NULL)),
  CHECK (manual_once=0 OR state IN ('PENDING','IN_FLIGHT'))
);
CREATE INDEX idx_alert_dispatch ON alert_deliveries(state,next_attempt_at_us,created_at_us,id);
CREATE INDEX idx_alert_history ON alert_deliveries(created_at_us,id);
CREATE INDEX idx_alert_finished ON alert_deliveries(finished_at_us,id) WHERE state IN ('DELIVERED','FAILED','EXPIRED');
INSERT INTO runtime_settings(key,value) VALUES
  ('alert.queue_overflow_count','0'),('alert.history_eviction_count','0'),('alert.restore_suppressed_count','0');
