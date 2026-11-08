import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { isExecution, isLogStorage, isLogStream, isOperation, isWorkload, pageOf } from "../src/guards";
import type { Guard } from "../src/guards";

const adminFile = process.env.CRONHARBOR_E2E_TOKEN_FILE;
const viewerFile = process.env.CRONHARBOR_E2E_VIEWER_TOKEN_FILE;
const admin = adminFile ? readFileSync(adminFile, "utf8").trim() : "";
const viewer = viewerFile ? readFileSync(viewerFile, "utf8").trim() : "";
const python = process.env.CRONHARBOR_E2E_PYTHON;

async function get<T>(request: APIRequestContext, path: string, guard: Guard<T>): Promise<T> {
  const response = await request.get(`/api/v1${path}`, { headers: { Authorization: `Bearer ${admin}` } });
  expect(response.ok()).toBe(true);
  const value: unknown = await response.json();
  if (!guard(value)) throw new Error("Real daemon returned an invalid contract");
  return value;
}
async function post(request: APIRequestContext, path: string, data: unknown): Promise<void> {
  const response = await request.post(`/api/v1${path}`, { data, headers: {
    Authorization: `Bearer ${admin}`, "Idempotency-Key": crypto.randomUUID(),
  } });
  expect(response.status()).toBe(202);
  const value: unknown = await response.json();
  if (!isOperation(value)) throw new Error("Real daemon returned an invalid operation");
  await expect.poll(async () => (await get(request, `/operations/${value.id}`, isOperation)).state).toBe("SUCCEEDED");
}
async function connect(page: Page, token: string): Promise<void> {
  await page.goto("/");
  await page.getByLabel("Access token", { exact: true }).fill(token);
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByRole("heading", { name: "Overview.", exact: true })).toBeVisible();
}

test("real API: follow across rotation and review policy conflicts and denial", async ({ page, request }) => {
  test.skip(!admin || !viewer || !python, "Requires the disposable Full acceptance fixture");
  const name = `rolling-browser-${Date.now()}`;
  const definition = JSON.stringify({
    apiVersion: "cronharbor.dev/v1", kind: "Service", metadata: { name, namespace: "acceptance" },
    spec: { owner: "api", enabled: true,
      execution: { adapter: "process", command: python, args: ["-u", "-c",
        "import sys,time\ni=0\nwhile True:\n print(str(i)+' '+ 'x'*4000,flush=True);i+=1;time.sleep(.03)\n"], workingDirectory: "/tmp" },
      logs: { maxRunBytes: 131072, rotation: { segmentBytes: 65536, maxSegments: 2 } },
      service: { desiredState: "RUNNING", restart: "never" },
    },
  });
  await post(request, "/config/apply", { definition, expected_revision: null });
  const workloads = await get(request, "/workloads?namespace=acceptance&limit=100", pageOf(isWorkload));
  const workload = workloads.items.find((row) => row.name === name);
  if (!workload) throw new Error("Disposable rotating service was not created");
  try {
    await expect.poll(async () => (await get(request, `/executions?workload_id=${workload.id}`, pageOf(isExecution))).items.length).toBe(1);
    const executions = await get(request, `/executions?workload_id=${workload.id}`, pageOf(isExecution));
    const execution = executions.items[0];
    if (!execution) throw new Error("Disposable service has no execution");
    await connect(page, admin);
    await page.goto(`/#/executions/${execution.execution_id}`);
    await expect(page.getByLabel("Execution logs, untrusted output")).toContainText("xxx");
    await page.getByRole("button", { name: "Pause follow" }).click();
    const old = await get(request, `/executions/${execution.execution_id}/logs/info`, isLogStream);
    await expect.poll(async () => (await get(request, `/executions/${execution.execution_id}/logs/info`, isLogStream)).retained_from_offset,
      { timeout: 15000 }).toBeGreaterThan(old.written_end_offset);
    await page.getByRole("button", { name: "Resume follow" }).click();
    await expect(page.getByText("Log history expired during follow.", { exact: false })).toBeVisible();
    await post(request, `/services/${workload.id}/desired-state`, { desired_state: "STOPPED", expected_revision: workload.revision_id });
    await page.goto("/#/settings");
    await expect(page.getByRole("heading", { name: "Log storage", exact: true })).toBeVisible();
    const installed = await get(request, "/log-storage", isLogStorage);
    const changed = { max_disk_bytes: installed.policy.max_disk_bytes + 1, max_segments: installed.policy.max_segments,
      retention_seconds: installed.policy.retention_seconds };
    await post(request, "/log-storage", { policy: changed, expected_revision: installed.policy.revision });
    await page.getByRole("button", { name: "Refresh log storage", exact: true }).click();
    await expect(page.getByText("The installed policy changed.", { exact: false })).toBeVisible();
    await expect(page.getByRole("button", { name: "Review log policy", exact: true })).toBeDisabled();
    await page.getByRole("button", { name: "Use latest policy", exact: true }).click();
    await page.getByLabel("Captured log byte budget", { exact: true }).fill(String(changed.max_disk_bytes + 1));
    await page.getByRole("button", { name: "Review log policy", exact: true }).click();
    await expect(page.getByRole("dialog", { name: "Update installation log policy" })).toBeVisible();
    await page.getByRole("button", { name: "Apply log policy", exact: true }).click();
    await expect.poll(async () => (await get(request, "/log-storage", isLogStorage)).policy.revision).toBe(installed.policy.revision + 2);
    const denied = await request.get("/api/v1/log-storage", { headers: { Authorization: `Bearer ${viewer}` } });
    expect(denied.status()).toBe(403);
    await page.reload();
    await connect(page, viewer);
    await page.goto("/#/settings");
    await expect(page.getByRole("heading", { name: "Log storage", exact: true })).toHaveCount(0);
  } finally {
    const current = await get(request, `/workloads/${workload.id}`, isWorkload);
    if (current.desired_state !== "STOPPED") await post(request, `/services/${current.id}/desired-state`, { desired_state: "STOPPED", expected_revision: current.revision_id });
  }
});
