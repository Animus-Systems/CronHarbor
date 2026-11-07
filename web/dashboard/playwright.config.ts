import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 45000,
  expect: { timeout: 12000 },
  use: {
    baseURL: process.env.CRONHARBOR_DASHBOARD_URL ?? "http://127.0.0.1:5173",
    viewport: { width: 1440, height: 1000 },
    contextOptions: { reducedMotion: "reduce" },
    trace: "off",
    screenshot: "only-on-failure",
  },
  reporter: [["list"], ["html", { open: "never" }]],
});
