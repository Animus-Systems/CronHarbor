import { expect, test } from "@playwright/test";
import type { ExecutionDto, LogStorageDto, PrincipalDto, StatusDto } from "../src/generated/contracts";
import { logStream } from "../tests/log-fixtures";

const principal: PrincipalDto = { actor_id: "rotation-fixture", role: "admin", namespaces: ["*"], capabilities: ["*"] };
const status: StatusDto = { sqlite_version: "fixture", workloads: 0, active: 1, unknown: 0, paused: false, restore_required: false, adapter_health: [] };
const execution: ExecutionDto = {
  execution_id: "rotation-execution", attempt_id: "rotation-attempt", workload_id: "rotation-workload",
  namespace: "fixture", name: "rotating-worker", state: "RUNNING", due_at_us: null,
  started_at_us: 1800000000000000, finished_at_us: null, error_code: null, exit_code: null, log_truncated: false,
};

test("log follow starts at retained history and visibly recovers from rotation expiry", async ({ page }) => {
  let start = 64;
  const cursors: number[] = [];
  const metadata = () => logStream(execution.attempt_id, start + 8, {
    capture_mode: "ROLLING", capture_state: start === 64 ? "CAPTURING" : "FROZEN",
    retained_from_offset: start, retained_bytes: 8, history_evicted: true,
  });
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname.replace("/api/v1", "");
    if (path === "/events") return;
    let value: unknown = { items: [], next_cursor: null };
    if (path === "/session") value = principal;
    else if (path === "/status") value = status;
    else if (path === "/executions") value = { items: [execution], next_cursor: null };
    else if (path === `/executions/${execution.execution_id}`) value = execution;
    else if (path === `/diagnostics/${execution.execution_id}`) value = {
      execution_id: execution.execution_id, state: execution.state, observed_causes: [], hypotheses: [], evidence: [], safe_to_retry_automatically: false,
    };
    else if (path.endsWith("/logs/info")) value = metadata();
    else if (path.endsWith("/logs")) {
      const offset = Number(url.searchParams.get("offset") ?? start);
      cursors.push(offset);
      if (offset < start) {
        await route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ error: { code: "LOG_CURSOR_EXPIRED" } }) });
        return;
      }
      value = { stream: metadata(), attempt_id: execution.attempt_id, text: start === 64 ? "recent!\n" : "latest!\n", next_offset: start + 8, response_truncated: false, log_truncated: false, untrusted: true };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
      start = 128;
      return;
    }
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await page.goto(`/#/executions/${execution.execution_id}`);
  await page.getByLabel("Access token", { exact: true }).fill("rotation-fixture-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByLabel("Execution logs, untrusted output")).toContainText("recent!");
  await expect(page.getByText("Earlier log history expired.", { exact: false })).toBeVisible();
  await expect(page.getByLabel("Execution logs, untrusted output")).toContainText("latest!");
  await expect(page.getByText("Log history expired during follow.", { exact: false })).toBeVisible();
  await expect(page.getByText("Retained log evidence is frozen.", { exact: false })).toBeVisible();
  expect(cursors.slice(0, 3)).toEqual([64, 72, 128]);
});

test("global log policy uses the reviewed revision and one explicit mutation", async ({ page }) => {
  let current: LogStorageDto = {
    policy: { revision: 1, imported_conservative: false, max_disk_bytes: 268435456, max_segments: 1024, retention_seconds: 604800 },
    usage: { retained_bytes: 1024, retained_segments: 1, reserved_bytes: 0, reserved_segments: 0, protected_bytes: 0, protected_segments: 0, unexplained_bytes: 0, unexplained_segments: 0, capture_error_count: 0, under_pressure: false },
  };
  const submissions: unknown[] = [];
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    let value: unknown = { items: [], next_cursor: null };
    if (path === "/session") value = principal;
    else if (path === "/status") value = status;
    else if (path === "/log-storage") {
      if (route.request().method() === "POST") {
        const body: unknown = route.request().postDataJSON();
        submissions.push(body);
        expect(body).toEqual({ policy: { max_disk_bytes: 536870912, max_segments: 2048, retention_seconds: 1209600 }, expected_revision: 1 });
        expect(route.request().headers()["idempotency-key"]).toBeTruthy();
        current = { ...current, policy: { revision: 2, imported_conservative: false, max_disk_bytes: 536870912, max_segments: 2048, retention_seconds: 1209600 } };
        await route.fulfill({ status: 202, contentType: "application/json", body: JSON.stringify({ id: "policy-operation", state: "SUCCEEDED", target_id: "2", kind: "log_storage", created_at_us: 1, error_code: null }) });
        return;
      }
      value = current;
    }
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await page.goto("/#/settings");
  await page.getByLabel("Access token", { exact: true }).fill("policy-fixture-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByRole("heading", { name: "Log storage", exact: true })).toBeVisible();
  const duplicateIds = await page.locator("[id]").evaluateAll((nodes) => {
    const ids = nodes.map((node) => node.id);
    return ids.filter((id, index) => ids.indexOf(id) !== index);
  });
  expect(duplicateIds).toEqual([]);
  await page.getByLabel("Captured log byte budget").fill("536870912");
  await page.getByLabel("Physical segment budget").fill("2048");
  await page.getByLabel("Eligible segment retention (seconds)").fill("1209600");
  await page.getByRole("button", { name: "Review log policy" }).click();
  const dialog = page.getByRole("dialog", { name: "Update installation log policy" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Apply log policy" }).click();
  await expect(page.getByLabel("Physical segment budget")).toHaveValue("2048");
  await expect(page.getByText("Policy operation policy-operation: succeeded.", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Refresh log storage" }).click();
  expect(submissions).toHaveLength(1);
});
