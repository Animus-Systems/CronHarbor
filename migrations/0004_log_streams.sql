CREATE TABLE log_storage_policy (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  revision INTEGER NOT NULL CHECK (revision > 0),
  max_disk_bytes INTEGER NOT NULL CHECK (max_disk_bytes BETWEEN 1 AND 10737418240),
  max_segments INTEGER NOT NULL CHECK (max_segments BETWEEN 1 AND 100000),
  retention_seconds INTEGER NOT NULL CHECK (retention_seconds BETWEEN 1 AND 9223372036854)
);
INSERT INTO log_storage_policy
SELECT 1, 1,
  COALESCE(MIN(COALESCE(json_extract(r.spec_json,'$.spec.logs.maxDiskBytes'),268435456)),268435456),
  MIN(1024,COALESCE(MIN(COALESCE(json_extract(r.spec_json,'$.spec.logs.maxSegments'),1024)),1024)),
  MIN(604800,COALESCE(MIN(COALESCE(json_extract(r.spec_json,'$.spec.logs.retentionSeconds'),604800)),604800))
FROM workloads w JOIN workload_revisions r ON r.id=w.current_revision_id;

CREATE TABLE log_streams (
  id TEXT PRIMARY KEY,
  attempt_id TEXT UNIQUE REFERENCES attempts(id),
  service_instance_id TEXT UNIQUE REFERENCES service_instances(id),
  capture_mode TEXT NOT NULL CHECK (capture_mode IN ('PREFIX','ROLLING','EXTERNAL_SNAPSHOT')),
  capture_state TEXT NOT NULL CHECK (capture_state IN ('PREPARED','CAPTURING','SEALED','FROZEN','STOPPED')),
  retained_from_offset INTEGER NOT NULL DEFAULT 0 CHECK (retained_from_offset >= 0),
  written_end_offset INTEGER NOT NULL DEFAULT 0 CHECK (written_end_offset >= retained_from_offset),
  known_discarded_bytes INTEGER NOT NULL DEFAULT 0 CHECK (known_discarded_bytes >= 0),
  truncated INTEGER NOT NULL DEFAULT 0 CHECK (truncated IN (0,1)),
  history_evicted INTEGER NOT NULL DEFAULT 0 CHECK (history_evicted IN (0,1)),
  last_error_code TEXT,
  created_at_us INTEGER NOT NULL,
  closed_at_us INTEGER,
  CHECK ((attempt_id IS NOT NULL) != (service_instance_id IS NOT NULL))
);

ALTER TABLE log_segments RENAME TO log_segments_before_rotation;
CREATE TABLE log_segments (
  id TEXT PRIMARY KEY,
  stream_id TEXT NOT NULL REFERENCES log_streams(id),
  sequence_no INTEGER NOT NULL CHECK (sequence_no >= 0),
  relative_path TEXT NOT NULL UNIQUE,
  start_offset INTEGER NOT NULL CHECK (start_offset >= 0),
  end_offset INTEGER NOT NULL CHECK (end_offset >= start_offset),
  bytes_written INTEGER NOT NULL CHECK (bytes_written = end_offset-start_offset),
  state TEXT NOT NULL CHECK (state IN ('PREPARED','OPEN','SEALED','DELETE_PENDING')),
  created_at_us INTEGER NOT NULL,
  closed_at_us INTEGER,
  UNIQUE(stream_id,sequence_no)
);
CREATE INDEX log_segment_ranges ON log_segments(stream_id,start_offset);
CREATE INDEX log_segment_retention ON log_segments(state,created_at_us,id);
CREATE UNIQUE INDEX log_one_open_segment ON log_segments(stream_id)
  WHERE state IN ('PREPARED','OPEN');

INSERT INTO log_streams(id,attempt_id,service_instance_id,capture_mode,capture_state,
  written_end_offset,truncated,created_at_us,closed_at_us)
SELECT l.id,l.attempt_id,l.service_instance_id,
  CASE WHEN a.executor_kind IN ('docker','systemd') THEN 'EXTERNAL_SNAPSHOT' ELSE 'PREFIX' END,
  CASE WHEN a.state='UNKNOWN' THEN 'FROZEN'
       WHEN l.closed_at_us IS NULL THEN 'PREPARED' ELSE 'SEALED' END,
  l.bytes_written,l.truncated,l.created_at_us,l.closed_at_us
FROM log_segments_before_rotation l LEFT JOIN attempts a ON a.id=l.attempt_id;

INSERT INTO log_segments(id,stream_id,sequence_no,relative_path,start_offset,end_offset,
  bytes_written,state,created_at_us,closed_at_us)
SELECT id,id,0,relative_path,0,bytes_written,bytes_written,
  CASE WHEN closed_at_us IS NULL THEN 'OPEN' ELSE 'SEALED' END,
  created_at_us,closed_at_us FROM log_segments_before_rotation;
DROP TABLE log_segments_before_rotation;
INSERT INTO runtime_settings(key,value)
SELECT 'log_policy_import_discrepancy','true'
WHERE (SELECT COALESCE(MIN(COALESCE(json_extract(r.spec_json,'$.spec.logs.maxDiskBytes'),268435456)),268435456) FROM workloads w JOIN workload_revisions r ON r.id=w.current_revision_id WHERE w.deleted_at_us IS NULL)
 != (SELECT max_disk_bytes FROM log_storage_policy WHERE singleton=1);
