import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
function tokenFrom(env: string): string {
  const file = process.env[`${env}_FILE`];
  return file ? readFileSync(file, "utf8").trim() : (process.env[env] ?? "");
}
const admin = tokenFrom("CRONHARBOR_E2E_TOKEN");
const viewer = tokenFrom("CRONHARBOR_E2E_VIEWER_TOKEN");
const fileWorkload = process.env.CRONHARBOR_E2E_FILE_WORKLOAD;
async function connect(page: Page, token: string): Promise<void> {
  await page.goto("/");
  await page.getByLabel("Access token", { exact: true }).fill(token);
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(
    page.getByRole("heading", { name: "Overview.", exact: true }),
  ).toBeVisible();
}
test("real API: editing a stopped systemd Service preserves HTTP readiness", async ({page}) => {
  test.skip(!admin, "A disposable API Admin token is required.");
  await connect(page,admin);
  const name=`http-roundtrip-${Date.now()}`;
  const httpReadiness={url:"http://127.0.0.1:9/health",status:200,check:{type:"json",equals:{ok:true}},intervalSeconds:15,timeoutSeconds:3,failureThreshold:3,successThreshold:2};
  await page.goto("/#/jobs/new?kind=Service");
  const editor=page.getByLabel("Workload YAML definition");
  await editor.fill(JSON.stringify({apiVersion:"cronharbor.dev/v1",kind:"Service",metadata:{name,namespace:"default"},spec:{owner:"api",enabled:false,execution:{adapter:"systemd",systemd:{unit:"browser-http-proof.service"}},service:{desiredState:"STOPPED",restart:"never",httpReadiness}}}));
  await page.getByRole("button",{name:"Validate & review plan"}).click();
  await expect(page.getByRole("heading",{name:"Review proposed revision"})).toBeVisible();
  await page.getByRole("button",{name:"Review impact & apply"}).click();
  await page.getByRole("button",{name:"Apply reviewed revision",exact:true}).click();
  await expect(page.getByRole("heading",{name:"Operation detail"})).toBeVisible();
  await expect(page.getByText("The requested operation completed.",{exact:false})).toBeVisible();
  await page.getByRole("link",{name:"Services",exact:true}).click();
  await page.getByRole("link",{name,exact:true}).click();
  const edit=page.getByRole("link",{name:"Edit definition",exact:true});
  await edit.click();
  await expect(page.getByLabel("Workload YAML definition")).toHaveValue(/"httpReadiness"/);
  const value:unknown=JSON.parse(await page.getByLabel("Workload YAML definition").inputValue());
  expect(value).toMatchObject({spec:{enabled:false,service:{desiredState:"STOPPED",httpReadiness}}});
});
test("authentication is keyboard accessible and tokens are memory only", async ({
  page,
}) => {
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "A clear view of what runs next." }),
  ).toBeVisible();
  await page.getByLabel("Access token", { exact: true }).focus();
  await expect(page.getByLabel("Access token", { exact: true })).toBeFocused();
  await expect(
    page.getByLabel("Access token", { exact: true }),
  ).toHaveAttribute("type", "password");
  expect(
    await page.evaluate(() => [localStorage.length, sessionStorage.length]),
  ).toEqual([0, 0]);
  await page.setViewportSize({ width: 320, height: 740 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: test.info().outputPath("connection-mobile.png"),
    fullPage: true,
  });
});
test("real API: create, validate, apply, run, inspect evidence and bounded logs", async ({
  page,
}) => {
  test.skip(
    !admin,
    "Set CRONHARBOR_E2E_TOKEN or CRONHARBOR_E2E_TOKEN_FILE to use a real local API.",
  );
  await connect(page, admin);
  const name = `browser-${Date.now()}`;
  await page
    .getByRole("link", { name: /Create workload/ })
    .first()
    .click();
  const editor = page.getByLabel("Workload YAML definition");
  await expect(editor).toBeVisible();
  const definition = (await editor.inputValue()).replace(
    "name: hello-world",
    `name: ${name}`,
  );
  await editor.fill(definition);
  await page.getByRole("button", { name: "Validate & review plan" }).click();
  await expect(
    page.getByRole("heading", { name: "Review proposed revision" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Review impact & apply" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page
    .getByRole("button", { name: "Apply reviewed revision", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Operation detail" }),
  ).toBeVisible();
  await expect(
    page.getByText("The requested operation completed.", { exact: false }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Jobs", exact: true }).click();
  await page.getByRole("link", { name, exact: true }).click();
  await page.getByRole("button", { name: "Run now", exact: true }).click();
  await page
    .getByRole("button", { name: "Confirm action", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Operation detail" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Executions", exact: true }).click();
  await page.getByRole("link", { name, exact: true }).first().click();
  await expect(
    page.getByRole("heading", { name: "Attempt timeline" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Deterministic diagnostics" }),
  ).toBeVisible();
  await expect(
    page.getByLabel("Execution logs, untrusted output"),
  ).toContainText("Hello from CronHarbor");
  await page.getByRole("button", { name: "Pause follow" }).click();
  await expect(
    page.getByRole("button", { name: "Resume follow" }),
  ).toHaveAttribute("aria-pressed", "false");
  expect(
    await page.evaluate(() => [localStorage.length, sessionStorage.length]),
  ).toEqual([0, 0]);
  await page.setViewportSize({ width: 320, height: 740 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.getByRole("button", { name: "Toggle navigation" }).click();
  await page.getByRole("link", { name: "Overview", exact: true }).click();
  await page.screenshot({
    path: test.info().outputPath("overview-mobile.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.screenshot({
    path: test.info().outputPath("overview-desktop.png"),
    fullPage: true,
  });
  await page.reload();
  await expect(page.getByLabel("Access token", { exact: true })).toBeVisible();
});
test("real API: source-owned editor is read only", async ({ page }) => {
  test.skip(
    !admin || !fileWorkload,
    "Set admin token and CRONHARBOR_E2E_FILE_WORKLOAD fixture ID.",
  );
  await connect(page, admin);
  await page.goto(`/#/jobs/${encodeURIComponent(fileWorkload ?? "")}/edit`);
  await expect(page.getByLabel("Workload YAML definition")).toHaveAttribute(
    "readonly",
  );
  await expect(
    page.getByText("File-owned definition: read only.", { exact: false }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Validate & review plan" }),
  ).toBeDisabled();
});
test("real API: viewer cannot run or edit an approved workload", async ({
  page,
}) => {
  test.skip(!viewer, "Set a scoped CRONHARBOR_E2E_VIEWER_TOKEN or token file.");
  await connect(page, viewer);
  await page.getByRole("link", { name: "Jobs", exact: true }).click();
  const first = page.locator("tbody tr").first().getByRole("link").first();
  await expect(first).toBeVisible();
  await first.click();
  await expect(
    page.getByRole("button", { name: "Run now", exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", {
      name: /Edit permission required|File-owned · read only/,
    }),
  ).toBeDisabled();
  await page.setViewportSize({ width: 320, height: 740 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});

test("real API: manage service desired state, restart and stop through durable operations", async ({
  page,
}) => {
  test.skip(!admin, "Set admin token to test real local service lifecycle.");
  await connect(page, admin);
  const name = `browser-service-${Date.now()}`;
  await page.goto("/#/jobs/new?kind=Service");
  const editor = page.getByLabel("Workload YAML definition");
  await expect(editor).toBeVisible();
  await editor.fill(
    (await editor.inputValue()).replace("name: hello-world", `name: ${name}`) +
      "    readiness:\n      command: /bin/echo\n      args: [ready]\n      intervalSeconds: 1\n      timeoutSeconds: 1\n    liveness:\n      command: /bin/echo\n      args: [alive]\n      intervalSeconds: 1\n      timeoutSeconds: 1\n",
  );
  await page.getByRole("button", { name: "Validate & review plan" }).click();
  await page.getByRole("button", { name: "Review impact & apply" }).click();
  await page
    .getByRole("button", { name: "Apply reviewed revision", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Operation detail" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Services", exact: true }).click();
  await page.getByRole("link", { name, exact: true }).click();
  await expect(page.getByText("Restart budget", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await page
    .getByRole("button", { name: "Confirm action", exact: true })
    .click();
  await expect(
    page.getByText("The requested operation completed.", { exact: false }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Services", exact: true }).click();
  await page.getByRole("link", { name, exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Readiness and liveness evidence" }),
  ).toBeVisible();
  await expect(
    page
      .getByRole("row")
      .filter({ hasText: "readiness" })
      .filter({ hasText: "passed" })
      .first(),
  ).toBeVisible();
  await page.screenshot({
    path: test.info().outputPath("service-evidence-desktop.png"),
    fullPage: true,
  });
  await page
    .getByRole("button", { name: "Restart service", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Confirm action", exact: true })
    .click();
  await expect(
    page.getByText("The requested operation completed.", { exact: false }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Services", exact: true }).click();
  await page.getByRole("link", { name, exact: true }).click();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await page
    .getByRole("button", { name: "Confirm action", exact: true })
    .click();
  await expect(
    page.getByText("The requested operation completed.", { exact: false }),
  ).toBeVisible();
});

test("real API: scoped token creation is masked and revocation updates metadata", async ({
  page,
}) => {
  test.skip(!admin, "Set admin token to test real local token administration.");
  await connect(page, admin);
  await page
    .getByRole("link", { name: "Settings & access", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "API tokens", exact: true }),
  ).toBeVisible();
  await page.screenshot({
    path: test.info().outputPath("settings-health-desktop.png"),
    fullPage: true,
  });
  await page.getByLabel("Lifetime · hours", { exact: true }).fill("1");
  await page
    .getByLabel("Namespaces · separated by commas", { exact: true })
    .fill("local");
  await page
    .getByRole("button", { name: "Review token grant", exact: true })
    .click();
  const responsePromise = page.waitForResponse(
    (response) =>
      response.url().endsWith("/api/v1/tokens") &&
      response.request().method() === "POST",
  );
  await page
    .getByRole("button", { name: "Create scoped token", exact: true })
    .click();
  const response = await responsePromise;
  expect(response.ok()).toBe(true);
  const value: unknown = await response.json();
  const { isCreatedToken } = await import("../src/guards");
  expect(isCreatedToken(value)).toBe(true);
  if (!isCreatedToken(value)) return;
  await expect(
    page.getByLabel("One-time token", { exact: true }),
  ).toHaveAttribute("type", "password");
  await page.getByRole("button", { name: "Clear secret", exact: true }).click();
  await expect(page.getByLabel("One-time token", { exact: true })).toHaveCount(
    0,
  );
  const row = page
    .locator(".panel")
    .filter({
      has: page.getByRole("heading", { name: "API tokens", exact: true }),
    })
    .getByRole("row")
    .filter({ hasText: value.metadata.id });
  await row.getByRole("button", { name: "Revoke", exact: true }).click();
  await page.getByRole("button", { name: "Revoke token", exact: true }).click();
  await expect(row).toContainText("Revoked");
  await expect(
    row.getByRole("button", { name: "Revoke", exact: true }),
  ).toBeDisabled();
});

test("real API: offline state disables actions and reconnect refreshes before enabling them", async ({
  page,
  context,
}) => {
  test.skip(!admin, "Set admin token to test real API reconnect recovery.");
  await connect(page, admin);
  await page.getByRole("link", { name: "Jobs", exact: true }).click();
  await page.locator("tbody tr").first().getByRole("link").first().click();
  const run = page.getByRole("button", { name: "Run now", exact: true });
  await expect(run).toBeEnabled();
  await run.click();
  await expect(
    page.getByRole("button", { name: "Keep current state", exact: true }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).not.toBeVisible();
  await expect(run).toBeFocused();
  await context.setOffline(true);
  await expect(
    page.getByText("Disconnected — actions are disabled.", { exact: true }),
  ).toBeVisible();
  await expect(run).toBeDisabled();
  await context.setOffline(false);
  await expect(page.getByText("Live connection", { exact: true })).toBeVisible({
    timeout: 35000,
  });
  await expect(run).toBeEnabled();
});
