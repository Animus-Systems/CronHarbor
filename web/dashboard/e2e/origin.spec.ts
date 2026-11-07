import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";

test("production headers permit the trusted remote API and preserve browser authentication", async ({ page }) => {
  const remote = process.env.CRONHARBOR_E2E_REMOTE_URL;
  const file = process.env.CRONHARBOR_E2E_TOKEN_FILE;
  test.skip(!remote || !file, "Requires two disposable real HTTP origins and a private credential fixture.");
  if (!remote || !file) return;
  const token = readFileSync(file, "utf8").trim();
  const response = await page.goto("/");
  if (!response) throw new Error("Dashboard document was unavailable");
  const remoteOrigin = new URL(remote).origin;
  expect(new URL(page.url()).origin).not.toBe(remoteOrigin);
  expect(response.headers()["content-security-policy"]).toContain(`connect-src 'self' ${remoteOrigin}`);
  const session = page.waitForResponse((reply) => reply.url() === `${remote}/session` && reply.status() === 200);
  await page.getByLabel("Access token", { exact: true }).fill(token);
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await session;
  await expect(page.getByText("Live connection", { exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Overview.", exact: true })).toBeVisible();
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
});
