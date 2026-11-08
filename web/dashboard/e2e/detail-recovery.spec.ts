import { expect, test } from "@playwright/test";
import { logStream } from "../tests/log-fixtures";
import type { Page } from "@playwright/test";
import type {
  ExecutionDto,
  OperationDto,
  PrincipalDto,
  StatusDto,
  WorkloadDto,
  DiagnosticDto,
  LogDto,
} from "../src/generated/contracts";

const principal: PrincipalDto = {
  actor_id: "detail-fixture",
  role: "admin",
  namespaces: ["*"],
  capabilities: ["*"],
};
const workload: WorkloadDto = {
  id: "workload_detail",
  namespace: "fixture",
  name: "detail-worker",
  kind: "one_shot_job",
  source_kind: "API",
  revision_id: "revision_1",
  enabled: true,
  paused: false,
  executor: "process",
  schedule_description: null,
  next_due_at_us: null,
  desired_state: null,
  observed_state: null,
  restart_count: 0,
  definition: "fixture definition",
  probe_history: [],
};
const status: StatusDto = {
  sqlite_version: "fixture",
  workloads: 1,
  active: 0,
  unknown: 0,
  paused: false,
  restore_required: false,
  adapter_health: [],
};
const execution: ExecutionDto = {
  execution_id: "execution_before_mount",
  attempt_id: "attempt_before_mount",
  workload_id: workload.id,
  namespace: workload.namespace,
  name: workload.name,
  state: "SUCCEEDED",
  due_at_us: null,
  started_at_us: 1800000000000000,
  finished_at_us: 1800000001000000,
  error_code: null,
  exit_code: 0,
  log_truncated: false,
};
const operation: OperationDto = {
  id: "op_a",
  state: "SUCCEEDED",
  target_id: "target_a",
  kind: "run",
  created_at_us: 1800000000000000,
  error_code: null,
};

test("a new attempt invalidates queued-retry evidence and repeated diagnostics failures back off", async ({ page }) => {
  let running = false;
  let reads = 0;
  const failed: ExecutionDto = { ...execution, state: "FAILED", exit_code: 1, error_code: "EXIT_NONZERO" };
  const active: ExecutionDto = { ...failed, attempt_id: "new_attempt", state: "RUNNING", finished_at_us: null, exit_code: null, error_code: null };
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path === `/diagnostics/${execution.execution_id}`) {
      reads += 1;
      if (running) {
        await route.fulfill({ status: 503, contentType: "application/json", body: "{}" });
        return;
      }
    }
    const selected = running ? active : failed;
    const value: unknown = path === "/session" ? principal
      : path === "/status" ? status
      : path === "/workloads" ? { items: [workload], next_cursor: null }
      : path === `/workloads/${workload.id}` ? workload
      : path === "/executions" ? { items: [selected], next_cursor: null }
      : path === `/executions/${execution.execution_id}` ? selected
      : path === `/diagnostics/${execution.execution_id}` ? { execution_id: execution.execution_id, state: "FAILED", observed_causes: [], hypotheses: [], evidence: [], safe_to_retry_automatically: true } satisfies DiagnosticDto
      : path.endsWith("/logs") ? { stream: logStream(selected.attempt_id), attempt_id: selected.attempt_id, next_offset: 0, text: "", response_truncated: false, log_truncated: false, untrusted: true } satisfies LogDto
      : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page, `/#/executions/${execution.execution_id}`);
  await expect(page.getByText("Retry queued.", { exact: false })).toBeVisible();
  running = true;
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(page.getByText("The daemon or executor is unavailable.", { exact: false })).toBeVisible();
  await expect(page.getByText("Retry queued.", { exact: false })).toHaveCount(0);
  await page.waitForTimeout(12000);
  expect(reads).toBe(3);
  await expect(page.getByRole("button", { name: "Cancel execution", exact: true })).toBeEnabled();
});

for (const recovery of ["normal", "poll", "refresh"] as const) {
test(`a queued retry remains cancellable while preserving the failed attempt (${recovery})`, async ({ page }) => {
  let pending = true;
  let cancellations = 0;
  let diagnosticReads = 0;
  const failed: ExecutionDto = { ...execution, state: "FAILED", exit_code: 1, error_code: "EXIT_NONZERO" };
  const cancelled: OperationDto = { ...operation, id: "cancel_retry", kind: "cancel", target_id: failed.execution_id };
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path === `/diagnostics/${failed.execution_id}` && ++diagnosticReads === 1 && recovery !== "normal") {
      await route.fulfill({ status: 503, contentType: "application/json", body: "{}" });
      return;
    }
    if (path === `/executions/${failed.execution_id}/cancel`) {
      expect(route.request().method()).toBe("POST");
      expect(route.request().headers()["idempotency-key"]).toBeTruthy();
      const body: unknown = route.request().postDataJSON();
      expect(body).toEqual({});
      cancellations++;
      pending = false;
      await route.fulfill({ status: 202, contentType: "application/json", body: JSON.stringify(cancelled) });
      return;
    }
    const diagnostic: DiagnosticDto = { execution_id: failed.execution_id, state: failed.state, observed_causes: ["EXIT_NONZERO"], hypotheses: [], evidence: [], safe_to_retry_automatically: pending };
    const value = path === "/session" ? principal
      : path === "/status" ? status
      : path === `/workloads/${workload.id}` ? workload
      : path === "/workloads" ? { items: [workload], next_cursor: null }
      : path === `/executions/${failed.execution_id}` ? failed
      : path === `/diagnostics/${failed.execution_id}` ? diagnostic
      : path === "/operations/cancel_retry" ? cancelled
      : path.endsWith("/logs/info") ? logStream(failed.attempt_id)
      : path.endsWith("/logs") ? { stream: logStream(failed.attempt_id), attempt_id: failed.attempt_id, next_offset: 0, text: "", response_truncated: false, log_truncated: false, untrusted: true } satisfies LogDto
      : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page, `/#/executions/${failed.execution_id}`);
  if (recovery !== "normal") {
    await expect(page.getByText("The daemon or executor is unavailable.", { exact: false })).toBeVisible();
    if (recovery === "refresh") await page.getByRole("button", { name: "Refresh", exact: true }).click();
  }
  await expect(page.getByRole("button", { name: "Cancel execution", exact: true })).toBeEnabled();
  await expect(page.getByText("The daemon or executor is unavailable.", { exact: false })).toHaveCount(0);
  await expect(page.getByText("Retry queued.", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Cancel execution", exact: true }).click();
  await page.getByRole("button", { name: "Request cancellation", exact: true }).click();
  await expect(page).toHaveURL(/#\/operations\/cancel_retry$/);
  await expect(page.getByText("The requested operation completed.", { exact: false })).toBeVisible();
  await page.evaluate((id) => { window.location.hash = `/executions/${id}`; }, failed.execution_id);
  await expect(page.getByRole("heading", { name: `${workload.name}.`, exact: true })).toBeVisible();
  await expect(page.getByText("Retry queued.", { exact: false })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Cancel execution", exact: true })).toHaveCount(0);
  expect(cancellations).toBe(1);
});
}
async function connect(page: Page, path: string): Promise<void> {
  await page.goto(path);
  await page.getByLabel("Access token", { exact: true }).fill("synthetic-detail-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByText("Live connection", { exact: true })).toBeVisible();
}

test("looking up a different operation replaces the routed success with its failed outcome", async ({ page }) => {
  const failed: OperationDto = {
    ...operation,
    id: "op_b",
    state: "FAILED",
    target_id: "target_b",
    error_code: "EXECUTOR_UNAVAILABLE",
  };
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    const value = path === "/session" ? principal
      : path === "/status" ? status
        : path === "/operations/op_a" ? operation
          : path === "/operations/op_b" ? failed
            : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page, "/#/operations/op_a");
  const detail = page.locator("section").filter({ has: page.getByRole("heading", { name: "Operation detail", exact: true }) });
  await expect(detail).toContainText("target_a");
  await expect(detail).toContainText("The requested operation completed.");
  await page.getByLabel("Inspect durable operation ID").fill("op_b");
  await page.getByRole("button", { name: "Inspect operation", exact: true }).click();
  await expect(detail).toContainText("op_b");
  await expect(detail).toContainText("target_b");
  await expect(detail).toContainText("FAILED");
  await expect(detail).toContainText("EXECUTOR_UNAVAILABLE");
  await expect(detail).toContainText("The operation ended without success.");
  await expect(detail).not.toContainText("The requested operation completed.");
  await expect(page).toHaveURL(/#\/operations\/op_b$/);
});

test("refreshing workload detail includes executions created after mounting", async ({ page }) => {
  let history = [execution];
  const scopedReads: string[] = [];
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path === "/executions" && url.searchParams.has("workload_id")) scopedReads.push(url.search);
    const value = path === "/session" ? principal
      : path === "/status" ? status
        : path === `/workloads/${workload.id}` ? workload
          : path === "/workloads" ? { items: [workload], next_cursor: null }
            : path === "/executions" ? { items: history, next_cursor: null }
              : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page, `/#/jobs/${workload.id}`);
  const recent = page.locator("section").filter({ has: page.getByRole("heading", { name: "Recent executions", exact: true }) });
  await expect(recent).toContainText(execution.execution_id);
  history = [{ ...execution, execution_id: "execution_after_mount", attempt_id: "attempt_after_mount", state: "FAILED", exit_code: 1 }, execution];
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(recent).toContainText("execution_after_mount");
  await expect(recent).toContainText("FAILED");
  expect(scopedReads.length).toBeGreaterThanOrEqual(2);
  expect(scopedReads.every((query) => query === `?workload_id=${workload.id}&limit=20`)).toBe(true);
});

test("workload detail recovers after a transient failure on refresh", async ({ page }) => {
  let unavailable = false;
  let detailReads = 0;
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path === `/workloads/${workload.id}`) {
      detailReads++;
      if (unavailable) {
        await route.fulfill({ status: 503, contentType: "application/json", body: "{}" });
        return;
      }
    }
    const value = path === "/session" ? principal
      : path === "/status" ? status
        : path === `/workloads/${workload.id}` ? workload
          : path === "/workloads" ? { items: [workload], next_cursor: null }
            : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page, `/#/jobs/${workload.id}`);
  await expect(page.getByRole("heading", { name: `${workload.name}.`, exact: true })).toBeVisible();
  unavailable = true;
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("The daemon or executor is unavailable.");
  unavailable = false;
  const readsBeforeRecovery = detailReads;
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect.poll(() => detailReads).toBeGreaterThan(readsBeforeRecovery);
  await expect(page.getByRole("heading", { name: `${workload.name}.`, exact: true })).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("a delayed operation lookup cannot replace a later route or populate its operations", async ({ page }) => {
  let release: (() => void) | undefined;
  let blocked = false;
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path === "/operations/op_b") {
      blocked = true;
      await new Promise<void>((resolve) => { release = resolve; });
    }
    const value = path === "/session" ? principal
      : path === "/status" ? status
        : path.startsWith("/operations/") ? { ...operation, id: path.slice("/operations/".length) }
          : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page, "/#/operations/op_a");
  await expect(page.getByRole("link", { name: "op_a", exact: true })).toBeVisible();
  await page.getByLabel("Inspect durable operation ID").fill("op_b");
  const delayedResponse = page.waitForResponse((response) => response.url().endsWith("/operations/op_b"));
  await page.getByRole("button", { name: "Inspect operation", exact: true }).click();
  await expect.poll(() => blocked).toBe(true);
  await page.evaluate(() => { window.location.hash = "/operations/op_c"; });
  await expect(page.getByRole("link", { name: "op_c", exact: true })).toBeVisible();
  release?.();
  await (await delayedResponse).finished();
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
  await expect(page).toHaveURL(/#\/operations\/op_c$/);
  await expect(page.getByRole("link", { name: "op_b", exact: true })).toHaveCount(0);
});

test("late workload detail cannot query execution history using the next session", async ({ page }) => {
  let release: (() => void) | undefined;
  let blocked = false;
  const scopedCredentials: (string | undefined)[] = [];
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path === `/workloads/${workload.id}`) {
      blocked = true;
      await new Promise<void>((resolve) => { release = resolve; });
    }
    if (path === "/executions" && url.searchParams.has("workload_id")) scopedCredentials.push(route.request().headers().authorization);
    const value = path === "/session" ? principal
      : path === "/status" ? status
        : path === `/workloads/${workload.id}` ? workload
          : { items: [], next_cursor: null };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  const delayedResponse = page.waitForResponse((response) => response.url().endsWith(`/workloads/${workload.id}`));
  await connect(page, `/#/jobs/${workload.id}`);
  await expect.poll(() => blocked).toBe(true);
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await page.getByLabel("Access token", { exact: true }).fill("next-detail-session-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByText("Live connection", { exact: true })).toBeVisible();
  release?.();
  await (await delayedResponse).finished();
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
  expect(scopedCredentials).toEqual([]);
});
