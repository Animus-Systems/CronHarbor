import { expect, test } from "@playwright/test";
import { readFileSync, writeFileSync } from "node:fs";

test("real owner restart: retained evidence visibly freezes on UNKNOWN", async ({ page }) => {
  test.setTimeout(120000);
  const tokenPath = process.env.CRONHARBOR_ROTATION_TOKEN_FILE;
  const execution = process.env.CRONHARBOR_ROTATION_EXECUTION;
  const ready = process.env.CRONHARBOR_ROTATION_READY_FILE;
  const stable = process.env.CRONHARBOR_ROTATION_STABLE_FILE;
  test.skip(!tokenPath || !execution || !ready || !stable, "Requires the disposable rotation/restart fixture");
  if (!tokenPath || !execution || !ready || !stable) return;
  let logReads = 0;
  let diagnosticReads = 0;
  const rejected: string[] = [];
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (path.endsWith("/logs")) logReads += 1;
    if (path.includes("/diagnostics/")) diagnosticReads += 1;
  });
  page.on("response", (response) => { if (response.status() === 429) rejected.push(new URL(response.url()).pathname); });
  await page.goto(`/#/executions/${execution}`);
  await page.getByLabel("Access token", { exact: true }).fill(readFileSync(tokenPath, "utf8").trim());
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByLabel("Execution logs, untrusted output")).toContainText("live");
  writeFileSync(ready, "observing\n", { mode: 0o600 });
  await page.waitForTimeout(1000);
  await page.getByRole("button", { name: "Fetch next chunk", exact: true }).click();
  await page.waitForTimeout(64000);
  await expect(page.getByText("Live connection", { exact: true })).toBeVisible();
  await expect(page.getByLabel("Execution logs, untrusted output")).toContainText("live");
  expect(rejected).toEqual([]);
  expect(logReads).toBeGreaterThan(8);
  expect(logReads + diagnosticReads).toBeLessThanOrEqual(35);
  writeFileSync(stable, "rate-window-verified\n", { mode: 0o600 });
  await expect(page.getByText("Retained log evidence is frozen.", { exact: false })).toBeVisible({ timeout: 25000 });
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
});
