import type {
  WorkloadDto,
  Role,
  StatusDto,
  ExecutionDto,
  OperationDto,
  LogDto,
  LogCaptureMode,
  LogCaptureState,
  LogStreamDto,
  LogStorageDto,
  LogStoragePolicyDto,
  LogStorageUsageDto,
  EventDto,
  DiagnosticDto,
  PlanDto,
  TokenDto,
  PrincipalDto,
  PreviewDto,
  CreatedTokenDto,
  AuditDto,
  ValidationDto,
  AdapterHealthDto,
  ProbeEventDto,
  Page,
  AlertStatusDto,
  AlertDeliveryDto,
  AlertConfigurationState,
  AlertDeliveryState,
  AlertKind,
  AlertErrorCode,
} from "./generated/contracts";
export type Guard<T> = (value: unknown) => value is T;
export function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
const string = (value: unknown): value is string => typeof value === "string";
const number = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);
const count = (value: unknown): value is number =>
  number(value) && Number.isSafeInteger(value) && value >= 0;
const bool = (value: unknown): value is boolean => typeof value === "boolean";
const nullableString = (value: unknown) => value === null || string(value);
const nullableNumber = (value: unknown) => value === null || number(value);
const roles: Record<Role, true> = {
  viewer: true,
  operator: true,
  editor: true,
  admin: true,
  observe: true,
};
const isRole: Guard<Role> = (v): v is Role =>
  string(v) && Object.hasOwn(roles, v);
const strings = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every(string);
export const isAdapterHealth: Guard<AdapterHealthDto> = (
  v,
): v is AdapterHealthDto =>
  record(v) &&
  string(v.adapter) &&
  bool(v.enabled) &&
  string(v.state) &&
  nullableNumber(v.last_observed_at_us) &&
  string(v.detail);
export const isProbeEvent: Guard<ProbeEventDto> = (v): v is ProbeEventDto =>
  record(v) && string(v.kind) && bool(v.healthy) && number(v.observed_at_us);
export const isWorkload: Guard<WorkloadDto> = (v): v is WorkloadDto =>
  record(v) &&
  [
    "id",
    "namespace",
    "name",
    "kind",
    "source_kind",
    "revision_id",
    "executor",
    "definition",
  ].every((k) => string(v[k])) &&
  bool(v.enabled) &&
  bool(v.paused) &&
  nullableString(v.schedule_description) &&
  nullableNumber(v.next_due_at_us) &&
  nullableString(v.desired_state) &&
  nullableString(v.observed_state) &&
  number(v.restart_count) &&
  Array.isArray(v.probe_history) &&
  v.probe_history.every(isProbeEvent);
export const isStatus: Guard<StatusDto> = (v): v is StatusDto =>
  record(v) &&
  string(v.sqlite_version) &&
  number(v.workloads) &&
  number(v.active) &&
  number(v.unknown) &&
  bool(v.paused) &&
  bool(v.restore_required) &&
  Array.isArray(v.adapter_health) &&
  v.adapter_health.every(isAdapterHealth);
export const isExecution: Guard<ExecutionDto> = (v): v is ExecutionDto =>
  record(v) &&
  [
    "execution_id",
    "attempt_id",
    "workload_id",
    "namespace",
    "name",
    "state",
  ].every((k) => string(v[k])) &&
  ["due_at_us", "started_at_us", "finished_at_us", "exit_code"].every((k) =>
    nullableNumber(v[k]),
  ) &&
  nullableString(v.error_code) &&
  bool(v.log_truncated);
export const isOperation: Guard<OperationDto> = (v): v is OperationDto =>
  record(v) &&
  ["id", "state", "target_id", "kind"].every((k) => string(v[k])) &&
  number(v.created_at_us) &&
  nullableString(v.error_code);
const captureModes: Record<LogCaptureMode, true> = {
  PREFIX: true,
  ROLLING: true,
  EXTERNAL_SNAPSHOT: true,
};
const captureStates: Record<LogCaptureState, true> = {
  PREPARED: true,
  CAPTURING: true,
  SEALED: true,
  FROZEN: true,
  STOPPED: true,
};
export const isLogStream: Guard<LogStreamDto> = (v): v is LogStreamDto =>
  record(v) &&
  string(v.attempt_id) &&
  string(v.capture_mode) && Object.hasOwn(captureModes, v.capture_mode) &&
  string(v.capture_state) && Object.hasOwn(captureStates, v.capture_state) &&
  count(v.retained_from_offset) && count(v.written_end_offset) &&
  v.written_end_offset >= v.retained_from_offset &&
  count(v.retained_bytes) && v.retained_bytes <= v.written_end_offset - v.retained_from_offset &&
  count(v.known_discarded_bytes) &&
  bool(v.history_evicted) && bool(v.log_truncated) &&
  nullableString(v.last_error_code);
export const isLogStoragePolicy: Guard<LogStoragePolicyDto> = (v): v is LogStoragePolicyDto =>
  record(v) && count(v.revision) && v.revision > 0 && bool(v.imported_conservative) &&
  count(v.max_disk_bytes) && v.max_disk_bytes > 0 && v.max_disk_bytes <= 10737418240 &&
  count(v.max_segments) && v.max_segments > 0 && v.max_segments <= 100000 &&
  count(v.retention_seconds) && v.retention_seconds > 0 && v.retention_seconds <= 9223372036854;
export const isLogStorageUsage: Guard<LogStorageUsageDto> = (v): v is LogStorageUsageDto =>
  record(v) && [
    "retained_bytes", "retained_segments", "reserved_bytes", "reserved_segments",
    "protected_bytes", "protected_segments", "unexplained_bytes", "unexplained_segments", "capture_error_count",
  ].every((key) => count(v[key])) && bool(v.under_pressure);
export const isLogStorage: Guard<LogStorageDto> = (v): v is LogStorageDto =>
  record(v) && isLogStoragePolicy(v.policy) && isLogStorageUsage(v.usage);
const alertConfigurations = { disabled: true, ready: true, credential_unavailable: true, worker_unavailable: true } satisfies Record<AlertConfigurationState, boolean>;
const alertStates = { PENDING: true, IN_FLIGHT: true, DELIVERED: true, FAILED: true, BLOCKED: true, EXPIRED: true, DESTINATION_CHANGED: true, RESTORE_HELD: true } satisfies Record<AlertDeliveryState, boolean>;
const alertKinds = { job_failed: true, job_timed_out: true, execution_unknown: true, service_unhealthy: true, service_failed: true, service_recovered: true } satisfies Record<AlertKind, boolean>;
const alertErrors = {
  CREDENTIAL_UNAVAILABLE: true, TLS_UNAVAILABLE: true, INVALID_DESTINATION: true, DNS_UNAVAILABLE: true,
  TRANSPORT_UNAVAILABLE: true, REQUEST_TIMEOUT: true, HTTP_REJECTED: true, RETRY_EXHAUSTED: true,
  DELIVERY_EXPIRED: true, DESTINATION_CHANGED: true, MANUAL_RECEIPT_UNKNOWN: true,
  RESTORE_REPLAY_SUPPRESSED: true, WORKER_UNAVAILABLE: true,
  INVALID_PAYLOAD: true,
} satisfies Record<AlertErrorCode, boolean>;
const alertError = (v: unknown): v is AlertErrorCode | null => v === null || string(v) && Object.hasOwn(alertErrors, v);
const timestamp = (v: unknown): v is number => number(v) && Number.isSafeInteger(v);
const nullableTimestamp = (v: unknown): v is number | null => v === null || timestamp(v);
export const isAlertStatus: Guard<AlertStatusDto> = (v): v is AlertStatusDto =>
  record(v) && string(v.configuration_state) && Object.hasOwn(alertConfigurations, v.configuration_state) &&
  ["pending", "in_flight", "blocked", "failed", "outstanding", "queue_overflow_count", "history_eviction_count", "restore_suppressed_count"].every((key) => count(v[key])) &&
  nullableTimestamp(v.oldest_pending_at_us) && nullableTimestamp(v.last_acknowledged_at_us) && alertError(v.last_error_code);
export const isAlertDelivery: Guard<AlertDeliveryDto> = (v): v is AlertDeliveryDto =>
  record(v) && ["alert_id", "incident_id", "workload_id", "workload_name", "namespace"].every((key) => string(v[key])) &&
  (v.incident_version === 1 || v.incident_version === 2) && string(v.kind) && Object.hasOwn(alertKinds, v.kind) &&
  string(v.state) && Object.hasOwn(alertStates, v.state) && count(v.attempt_count) && v.attempt_count <= 4294967295 &&
  timestamp(v.occurred_at_us) && timestamp(v.created_at_us) && nullableTimestamp(v.next_attempt_at_us) &&
  nullableTimestamp(v.finished_at_us) && nullableTimestamp(v.delivered_at_us) &&
  (v.http_status === null || count(v.http_status) && v.http_status >= 100 && v.http_status <= 599) && alertError(v.last_error_code);
export const isLog: Guard<LogDto> = (v): v is LogDto =>
  record(v) &&
  string(v.attempt_id) &&
  string(v.text) &&
  number(v.next_offset) &&
  Number.isSafeInteger(v.next_offset) &&
  v.next_offset >= 0 &&
  bool(v.response_truncated) &&
  bool(v.log_truncated) &&
  bool(v.untrusted) &&
  isLogStream(v.stream) && v.stream.attempt_id === v.attempt_id &&
  v.next_offset >= v.stream.retained_from_offset && v.next_offset <= v.stream.written_end_offset;
export const isEvent: Guard<EventDto> = (v): v is EventDto =>
  record(v) &&
  number(v.seq) &&
  number(v.created_at_us) &&
  string(v.event_kind) &&
  string(v.payload_json) &&
  nullableString(v.workload_id) &&
  nullableString(v.execution_id);
export const isDiagnostic: Guard<DiagnosticDto> = (v): v is DiagnosticDto =>
  record(v) &&
  string(v.execution_id) &&
  string(v.state) &&
  strings(v.observed_causes) &&
  strings(v.hypotheses) &&
  Array.isArray(v.evidence) &&
  v.evidence.every(isEvent) &&
  bool(v.safe_to_retry_automatically);
export const isPlan: Guard<PlanDto> = (v): v is PlanDto =>
  record(v) &&
  nullableString(v.current_revision) &&
  string(v.proposed_hash) &&
  bool(v.changed) &&
  string(v.source_kind) &&
  strings(v.warnings);
export const isToken: Guard<TokenDto> = (v): v is TokenDto =>
  record(v) &&
  string(v.id) &&
  string(v.actor_id) &&
  isRole(v.role) &&
  strings(v.namespaces) &&
  strings(v.capabilities) &&
  nullableNumber(v.expires_at_us) &&
  bool(v.revoked);
export const isPrincipal: Guard<PrincipalDto> = (v): v is PrincipalDto =>
  record(v) &&
  string(v.actor_id) &&
  isRole(v.role) &&
  strings(v.namespaces) &&
  strings(v.capabilities);
export const isCreatedToken: Guard<CreatedTokenDto> = (
  v,
): v is CreatedTokenDto => record(v) && string(v.token) && isToken(v.metadata);
export const isPreview: Guard<PreviewDto> = (v): v is PreviewDto =>
  record(v) &&
  string(v.timezone) &&
  Array.isArray(v.items) &&
  v.items.every(
    (item) => record(item) && number(item.utc_us) && string(item.local),
  );
export const isAudit: Guard<AuditDto> = (v): v is AuditDto =>
  record(v) &&
  number(v.seq) &&
  number(v.created_at_us) &&
  ["actor_id", "action", "result", "request_id"].every((k) => string(v[k])) &&
  nullableString(v.target_id);
export function pageOf<T>(guard: Guard<T>): Guard<Page<T>> {
  return (v): v is Page<T> =>
    record(v) &&
    Array.isArray(v.items) &&
    v.items.length <= 1000 &&
    v.items.every(guard) &&
    nullableString(v.next_cursor);
}
export const isValid: Guard<unknown> = (_v): _v is unknown => true;

export const isValidation: Guard<ValidationDto> = (v): v is ValidationDto =>
  record(v) && bool(v.valid) && string(v.revision_hash);
