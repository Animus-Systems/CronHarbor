import type {
  WorkloadDto,
  Role,
  StatusDto,
  ExecutionDto,
  OperationDto,
  LogDto,
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
} from "./generated/contracts";
export type Guard<T> = (value: unknown) => value is T;
export function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
const string = (value: unknown): value is string => typeof value === "string";
const number = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);
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
export const isLog: Guard<LogDto> = (v): v is LogDto =>
  record(v) &&
  string(v.attempt_id) &&
  string(v.text) &&
  number(v.next_offset) &&
  Number.isSafeInteger(v.next_offset) &&
  v.next_offset >= 0 &&
  bool(v.response_truncated) &&
  bool(v.log_truncated) &&
  bool(v.untrusted);
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
