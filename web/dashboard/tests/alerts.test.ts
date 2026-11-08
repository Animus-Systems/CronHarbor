import { describe, expect, it } from "vitest";
import { isAlertDelivery, isAlertStatus } from "../src/guards";

const status = {
  configuration_state: "ready", pending: 1, in_flight: 0, blocked: 0, failed: 2, outstanding: 1,
  oldest_pending_at_us: 100, last_acknowledged_at_us: null,
  queue_overflow_count: 0, history_eviction_count: 1, restore_suppressed_count: 0, last_error_code: null,
};
const delivery = {
  alert_id: "alert", incident_id: "incident", incident_version: 1, kind: "job_failed",
  workload_id: "workload", workload_name: "name", namespace: "local", state: "PENDING", attempt_count: 0,
  occurred_at_us: 100, created_at_us: 110, next_attempt_at_us: 120, finished_at_us: null,
  delivered_at_us: null, http_status: null, last_error_code: null,
};

describe("alert delivery trust boundary", () => {
  it("accepts canonical status and delivery values", () => {
    expect(isAlertStatus(status)).toBe(true);
    expect(isAlertDelivery(delivery)).toBe(true);
    expect(isAlertDelivery({ ...delivery, state: "BLOCKED", last_error_code: "CREDENTIAL_UNAVAILABLE" })).toBe(true);
  });
  it("rejects unknown states, raw errors and unsafe counts", () => {
    expect(isAlertStatus({ ...status, configuration_state: "probably fine" })).toBe(false);
    expect(isAlertStatus({ ...status, queue_overflow_count: Number.MAX_SAFE_INTEGER + 1 })).toBe(false);
    expect(isAlertStatus({ ...status, last_error_code: "secret from endpoint" })).toBe(false);
    expect(isAlertDelivery({ ...delivery, state: "ACKNOWLEDGED_BY_GROKBOT" })).toBe(false);
    expect(isAlertDelivery({ ...delivery, attempt_count: -1 })).toBe(false);
    expect(isAlertDelivery({ ...delivery, incident_version: 3 })).toBe(false);
    expect(isAlertDelivery({ ...delivery, http_status: 999 })).toBe(false);
  });
});
