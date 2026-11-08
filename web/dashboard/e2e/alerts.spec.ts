import { expect, test, type Page } from "@playwright/test";
import type { AlertDeliveryDto, AlertStatusDto, PrincipalDto } from "../src/generated/contracts";

const identity: PrincipalDto = { actor_id: "fixture", role: "admin", namespaces: ["*"], capabilities: ["operations.manage"] };
const summary: AlertStatusDto = { configuration_state: "ready", pending: 0, in_flight: 0, blocked: 0, failed: 1, outstanding: 0,
  oldest_pending_at_us: null, last_acknowledged_at_us: null, queue_overflow_count: 2, history_eviction_count: 3,
  restore_suppressed_count: 1, last_error_code: "HTTP_REJECTED" };
const failed: AlertDeliveryDto = { alert_id: "fixture-alert", incident_id: "fixture-incident", incident_version: 1, kind: "job_failed",
  workload_id: "workload", workload_name: "<script>untrusted</script>", namespace: "local", state: "FAILED", attempt_count: 1,
  occurred_at_us: 1000000, created_at_us: 1000000, next_attempt_at_us: null, finished_at_us: 2000000,
  delivered_at_us: null, http_status: 401, last_error_code: "HTTP_REJECTED" };

async function connect(page: Page): Promise<void> {
  await page.goto("/#/settings");
  await page.getByLabel("Access token", { exact: true }).fill("private-fixture-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
}

test("alert retry is one reviewed intent and acknowledgement stays distinct from bot processing", async ({ page }) => {
  let row = failed;
  let mutations = 0;
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    let value: unknown = { items: [], next_cursor: null };
    if (path === "/session") value = identity;
    else if (path === "/status") value = { sqlite_version: "fixture", workloads: 0, active: 0, unknown: 0, paused: false, restore_required: false, adapter_health: [] };
    else if (path === "/alerts/status") value = summary;
    else if (path === "/alerts/deliveries") value = { items: [row], next_cursor: null };
    else if (path === "/alerts/deliveries/fixture-alert/retry") {
      expect(route.request().method()).toBe("POST");
      expect(route.request().postDataJSON()).toEqual({});
      expect(route.request().headers()["idempotency-key"]).toBeTruthy();
      mutations += 1;
      row = { ...row, state: "DELIVERED", attempt_count: 2, delivered_at_us: 3000000, finished_at_us: 3000000, http_status: 204, last_error_code: null };
      value = { id: "retry-operation", kind: "alert_retry", target_id: row.alert_id, state: "ACCEPTED", created_at_us: 2500000, error_code: null };
    }
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page);
  await expect(page.getByRole("heading", { name: "Alert deliveries", exact: true })).toBeVisible();
  await expect(page.getByRole("cell").filter({ hasText: "local/<script>untrusted</script>" })).toBeVisible();
  expect(await page.locator("script").filter({ hasText: "untrusted" }).count()).toBe(0);
  await expect(page.getByText("Grokbot processing is tracked by that application.", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Review retry for alert fixture-alert" }).click();
  await expect(page.getByRole("dialog", { name: "Retry one alert delivery" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Keep current state", exact: true })).toBeFocused();
  await page.getByRole("button", { name: "Send one retry", exact: true }).click();
  await expect(page.getByText("Retry operation retry-operation: accepted.", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Refresh alert deliveries", exact: true }).click();
  await expect(page.getByRole("cell", { name: "DELIVERED · HTTP 204", exact: false })).toBeVisible();
  expect(mutations).toBe(1);
  await page.setViewportSize({ width: 320, height: 740 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test("a scoped administrator never queries installation alert history", async ({ page }) => {
  let queries = 0;
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path.startsWith("/alerts/")) queries += 1;
    const value: unknown = path === "/session" ? { ...identity, namespaces: ["local"] } : path === "/status" ?
      { sqlite_version: "fixture", workloads: 0, active: 0, unknown: 0, paused: false, restore_required: false, adapter_health: [] } : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page);
  await expect(page.getByRole("heading", { name: "Instance boundaries", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Alert deliveries", exact: true })).toHaveCount(0);
  expect(queries).toBe(0);
});
