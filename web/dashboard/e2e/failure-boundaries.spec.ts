import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import type { PlanDto, PrincipalDto, StatusDto } from "../src/generated/contracts";

const principal: PrincipalDto = {
  actor_id: "failure-fixture",
  role: "admin",
  namespaces: ["*"],
  capabilities: ["*"],
};
const status: StatusDto = {
  sqlite_version: "fixture",
  workloads: 0,
  active: 0,
  unknown: 0,
  paused: false,
  restore_required: false,
  adapter_health: [],
};
const plan: PlanDto = {
  current_revision: null,
  proposed_hash: "reviewed-hash",
  changed: true,
  source_kind: "API",
  warnings: [],
};
async function connect(page: Page, path = "/"): Promise<void> {
  await page.goto(path);
  await page.getByLabel("Access token", { exact: true }).fill("synthetic-failure-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByText("Live connection", { exact: true })).toBeVisible();
}

for (const phase of ["validation", "plan"] as const) {
  test(`editing during delayed ${phase} invalidates the reviewed definition`, async ({ page }) => {
    let release: (() => void) | undefined;
    let blocked = false;
    let plans = 0;
    let applies = 0;
    await page.route("**/api/v1/**", async (route) => {
      const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
      if (path === "/events") return;
      if (path === "/config/plan") plans++;
      if (path === "/config/apply") applies++;
      if (path === (phase === "validation" ? "/workloads/validate" : "/config/plan")) {
        blocked = true;
        await new Promise<void>((resolve) => { release = resolve; });
      }
      const value = path === "/session"
        ? principal
        : path === "/status"
          ? status
          : path === "/workloads/validate"
            ? { valid: true, revision_hash: "reviewed-hash" }
            : path === "/config/plan"
              ? plan
              : { items: [], next_cursor: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
    });
    await connect(page, "/#/jobs/new");
    const editor = page.getByLabel("Workload YAML definition");
    await expect(editor).toBeEditable();
    await page.getByRole("button", { name: "Validate & review plan" }).click();
    await expect.poll(() => blocked).toBe(true);
    await editor.fill((await editor.inputValue()).replace("hello-world", "edited-after-review"));
    release?.();
    await expect(page.getByRole("button", { name: "Validate & review plan" })).toBeEnabled();
    await expect(page.getByRole("button", { name: "Review impact & apply" })).toHaveCount(0);
    await expect(page.getByText("Schema validated. A current plan is required before apply.")).toHaveCount(0);
    expect(plans).toBe(phase === "validation" ? 0 : 1);
    expect(applies).toBe(0);
  });
}

test("a late token creation response cannot use the next session credential", async ({ page }) => {
  let release: (() => void) | undefined;
  let blocked = false;
  const metadataReads: (string | undefined)[] = [];
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    let value: unknown = { items: [], next_cursor: null };
    if (path === "/session") value = principal;
    else if (path === "/status") value = status;
    else if (path === "/tokens" && route.request().method() === "POST") {
      blocked = true;
      await new Promise<void>((resolve) => { release = resolve; });
      value = {
        token: "late-created-private-token",
        metadata: { id: "created", actor_id: "created", role: "viewer", namespaces: ["default"], capabilities: [], expires_at_us: null, revoked: false },
      };
    } else if (path === "/tokens") metadataReads.push(route.request().headers().authorization);
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
  });
  await connect(page, "/#/settings");
  await page.getByRole("button", { name: "Review token grant" }).click();
  const creationResponse = page.waitForResponse((response) =>
    response.url().endsWith("/api/v1/tokens") && response.request().method() === "POST");
  await page.getByRole("button", { name: "Create scoped token", exact: true }).click();
  await expect.poll(() => blocked).toBe(true);
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await page.getByLabel("Access token", { exact: true }).fill("next-session-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(page.getByText("Live connection", { exact: true })).toBeVisible();
  release?.();
  await (await creationResponse).finished();
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
  await expect(page.getByLabel("One-time token", { exact: true })).toHaveCount(0);
  expect(metadataReads).toEqual(["Bearer synthetic-failure-token"]);
});
