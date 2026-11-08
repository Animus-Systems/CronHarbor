import { expect, test } from "@playwright/test";
import { logStream } from "../tests/log-fixtures";
import type {
  DiagnosticDto,
  ExecutionDto,
  PrincipalDto,
  StatusDto,
  WorkloadDto,
} from "../src/generated/contracts";

test("synthetic load: 1000 workloads and 10000 log lines keep bounded DOM windows", async ({
  page,
}) => {
  const workloads: WorkloadDto[] = Array.from({ length: 1000 }, (_, index) => ({
    id: `workload_${index}`,
    namespace: "synthetic",
    name: `fixture-${String(index).padStart(4, "0")}`,
    kind: "ScheduledJob",
    source_kind: "api",
    revision_id: "revision_1",
    enabled: true,
    paused: false,
    executor: "process",
    schedule_description: "0 * * * * UTC",
    next_due_at_us: 1800000000000000,
    desired_state: null,
    observed_state: null,
    restart_count: 0,
    definition: "",
    probe_history: [],
  }));
  const principal: PrincipalDto = {
    actor_id: "synthetic-operator",
    role: "admin",
    namespaces: ["synthetic"],
    capabilities: ["*"],
  };
  const status: StatusDto = {
    sqlite_version: "synthetic",
    workloads: 1000,
    active: 0,
    unknown: 0,
    paused: false,
    restore_required: false,
    adapter_health: [],
  };
  const execution: ExecutionDto = {
    execution_id: "execution_load",
    attempt_id: "attempt_load",
    workload_id: "workload_0",
    namespace: "synthetic",
    name: "fixture-0000",
    state: "SUCCEEDED",
    due_at_us: 1800000000000000,
    started_at_us: 1800000000000000,
    finished_at_us: 1800000001000000,
    error_code: null,
    exit_code: 0,
    log_truncated: false,
  };
  const diagnostic: DiagnosticDto = {
    execution_id: execution.execution_id,
    state: execution.state,
    observed_causes: [],
    hypotheses: [],
    evidence: [],
    safe_to_retry_automatically: false,
  };
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path.endsWith("/logs/info")) {
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(logStream(execution.attempt_id, 48890)) });
      return;
    }
    const body =
      path === "/session"
        ? principal
        : path === "/status"
          ? status
          : path === "/workloads"
            ? { items: workloads, next_cursor: null }
            : path === "/executions"
              ? { items: [execution], next_cursor: null }
              : path.endsWith("/logs")
                ? {
                    attempt_id: execution.attempt_id,
                    stream: logStream(execution.attempt_id, 48890),
                    text:
                      !url.searchParams.has("offset") || url.searchParams.get("offset") === "0"
                        ? Array.from(
                            { length: 10000 },
                            (_, i) => `${i}\n`,
                          ).join("")
                        : "",
                    next_offset: 48890,
                    response_truncated: false,
                    log_truncated: false,
                    untrusted: true,
                  }
                : path === "/diagnostics/execution_load"
                  ? diagnostic
                  : path === "/executions/execution_load"
                    ? execution
                    : undefined;
    await route.fulfill({
      status: body ? 200 : 404,
      contentType: "application/json",
      body: JSON.stringify(body ?? {}),
    });
  });
  await page.goto("/");
  await page
    .getByLabel("Access token", { exact: true })
    .fill("synthetic-fixture-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await page.getByRole("link", { name: "Jobs", exact: true }).click();
  await expect(page.getByText("1000 jobs", { exact: true })).toBeVisible();
  expect(
    await page.locator("tbody tr:not(.spacer)").count(),
  ).toBeLessThanOrEqual(20);
  await page.locator(".virtual-table").evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect(page.getByRole("link", { name: "fixture-0999" })).toBeVisible();
  expect(
    await page.locator("tbody tr:not(.spacer)").count(),
  ).toBeLessThanOrEqual(20);
  await page.goto("/#/executions/execution_load");
  await expect(
    page.getByLabel("Execution logs, untrusted output"),
  ).toBeVisible();
  await expect(page.locator(".log-viewport")).toContainText("9999");
  expect(await page.locator(".log-line").count()).toBeLessThanOrEqual(52);
  await page.setViewportSize({ width: 320, height: 740 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});

test("empty overview guides the first workload without fabricated operational data", async ({
  page,
}) => {
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/events")) return;
    const value = path.endsWith("/session")
      ? {
          actor_id: "fixture",
          role: "admin",
          namespaces: ["default"],
          capabilities: ["*"],
        }
      : path.endsWith("/status")
        ? {
            sqlite_version: "synthetic",
            workloads: 0,
            active: 0,
            unknown: 0,
            paused: false,
            restore_required: false,
            adapter_health: [],
          }
        : { items: [], next_cursor: null };
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(value),
    });
  });
  await page.goto("/");
  await page
    .getByLabel("Access token", { exact: true })
    .fill("synthetic-fixture-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(
    page.getByRole("heading", {
      name: "A quiet harbor. Let’s schedule something.",
    }),
  ).toBeVisible();
  await page
    .getByRole("link", { name: "Create first workload", exact: false })
    .click();
  await expect(page.getByLabel("Workload YAML definition")).toBeEditable();
});

test("synthetic unknown outcome requires independent evidence before reconciliation", async ({
  page,
}) => {
  const execution: ExecutionDto = {
    execution_id: "unknown_execution",
    attempt_id: "unknown_attempt",
    workload_id: "unknown_workload",
    namespace: "fixture",
    name: "uncertain-worker",
    state: "UNKNOWN",
    due_at_us: null,
    started_at_us: 1800000000000000,
    finished_at_us: null,
    error_code: "OBSERVATION_LOST",
    exit_code: null,
    log_truncated: false,
  };
  const submitted: unknown[] = [];
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    const operation = {
      id: "reconcile_operation",
      state: "SUCCEEDED",
      kind: "reconcile",
      target_id: execution.execution_id,
      created_at_us: 1800000000000000,
      error_code: null,
    };
    let value: unknown = { items: [], next_cursor: null };
    if (path === "/session")
      value = {
        actor_id: "fixture",
        role: "admin",
        namespaces: ["fixture"],
        capabilities: ["*"],
      };
    else if (path === "/status")
      value = {
        sqlite_version: "synthetic",
        workloads: 0,
        active: 1,
        unknown: 1,
        paused: false,
        restore_required: false,
        adapter_health: [],
      };
    else if (path === "/executions")
      value = { items: [execution], next_cursor: null };
    else if (path === "/executions/unknown_execution") value = execution;
    else if (path === "/diagnostics/unknown_execution")
      value = {
        execution_id: execution.execution_id,
        state: "UNKNOWN",
        observed_causes: ["Executor observation was lost"],
        hypotheses: ["External effects may have occurred"],
        evidence: [],
        safe_to_retry_automatically: false,
      };
    else if (path.endsWith("/logs/info")) value = logStream(execution.attempt_id, 0, { capture_state: "FROZEN" });
    else if (path.endsWith("/logs"))
      value = {
        attempt_id: execution.attempt_id,
        stream: logStream(execution.attempt_id, 0, { capture_state: "FROZEN" }),
        text: "",
        next_offset: 0,
        response_truncated: false,
        log_truncated: false,
        untrusted: true,
      };
    else if (path.endsWith("/reconcile")) {
      const body: unknown = route.request().postDataJSON();
      submitted.push(body);
      expect(route.request().headers()["idempotency-key"]).toBeTruthy();
      value = operation;
    } else if (path.startsWith("/operations/")) value = operation;
    await route.fulfill({
      status: path.endsWith("/reconcile") ? 202 : 200,
      contentType: "application/json",
      body: JSON.stringify(value),
    });
  });
  await page.goto("/#/executions/unknown_execution");
  await page
    .getByLabel("Access token", { exact: true })
    .fill("synthetic-fixture-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(
    page.getByText("UNKNOWN is an uncertain outcome.", { exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Reconcile with evidence", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Record verified outcome", exact: true }),
  ).toBeDisabled();
  await page
    .getByLabel("Reason and evidence")
    .fill(
      "Verified worker identity is absent and external effect did not occur.",
    );
  await page
    .getByRole("button", { name: "Record verified outcome", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Operation detail" }),
  ).toBeVisible();
  expect(submitted).toEqual([
    {
      state: "FAILED",
      evidence:
        "Verified worker identity is absent and external effect did not occur.",
    },
  ]);
});

test("synthetic service cancellation follows known workload kind and scoped admins cannot query global audit", async ({
  page,
}) => {
  const cases: {
    kind: string;
    role: PrincipalDto["role"];
    capabilities: string[];
    allowed: boolean;
  }[] = [
    {
      kind: "managed_service",
      role: "operator",
      capabilities: ["service.control_preapproved"],
      allowed: true,
    },
    {
      kind: "scheduled_job",
      role: "operator",
      capabilities: ["service.control_preapproved"],
      allowed: false,
    },
    {
      kind: "unsupported_kind",
      role: "admin",
      capabilities: ["workload.run_preapproved", "operations.manage"],
      allowed: false,
    },
  ];
  let selected = cases[0];
  let auditRequests = 0;
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace("/api/v1", "");
    if (path === "/events") return;
    if (path === "/audit") auditRequests++;
    const workload: WorkloadDto = {
      id: "control_workload",
      namespace: "fixture",
      name: "control-fixture",
      kind: selected?.kind ?? "unsupported_kind",
      source_kind: "API",
      revision_id: "revision",
      enabled: true,
      paused: false,
      executor: "process",
      schedule_description: null,
      next_due_at_us: null,
      desired_state: "RUNNING",
      observed_state: "READY",
      restart_count: 0,
      definition: "",
      probe_history: [],
    };
    const execution: ExecutionDto = {
      execution_id: "control_execution",
      attempt_id: "control_attempt",
      workload_id: workload.id,
      namespace: workload.namespace,
      name: workload.name,
      state: "RUNNING",
      due_at_us: null,
      started_at_us: 1800000000000000,
      finished_at_us: null,
      error_code: null,
      exit_code: null,
      log_truncated: false,
    };
    const value =
      path === "/session"
        ? {
            actor_id: "fixture",
            role: selected?.role,
            namespaces: ["fixture"],
            capabilities: selected?.capabilities,
          }
        : path === "/status"
          ? {
              sqlite_version: "synthetic",
              workloads: 1,
              active: 1,
              unknown: 0,
              paused: false,
              restore_required: false,
              adapter_health: [],
            }
          : path === "/workloads"
            ? { items: [workload], next_cursor: null }
            : path === "/executions"
              ? { items: [execution], next_cursor: null }
              : path === "/executions/control_execution"
                ? execution
                : path === "/diagnostics/control_execution"
                  ? {
                      execution_id: execution.execution_id,
                      state: "RUNNING",
                      observed_causes: [],
                      hypotheses: [],
                      evidence: [],
                      safe_to_retry_automatically: false,
                    }
                  : path === "/workloads/control_workload"
                    ? workload
                    : { items: [], next_cursor: null };
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(value),
    });
  });
  for (const entry of cases) {
    selected = entry;
    await page.goto("/#/executions/control_execution");
    await page.reload();
    await page
      .getByLabel("Access token", { exact: true })
      .fill("synthetic-fixture-token");
    await page.getByRole("button", { name: "Connect to instance" }).click();
    const cancel = page.getByRole("button", {
      name: "Cancel execution",
      exact: true,
    });
    if (entry.allowed) await expect(cancel).toBeEnabled();
    else await expect(cancel).toBeDisabled();
  }
  await page
    .getByRole("link", { name: "Settings & access", exact: true })
    .click();
  await expect(
    page.getByText(
      "Instance-wide audit requires operations.manage and wildcard namespace authority.",
      { exact: true },
    ),
  ).toBeVisible();
  expect(auditRequests).toBe(0);
});

test("synthetic log follow uses raw cursors for replacement characters and split Unicode chunks", async ({
  page,
}) => {
  const cursors: number[] = [];
  let retried = false;
  const execution: ExecutionDto = {
    execution_id: "unicode_execution",
    attempt_id: "unicode_attempt",
    workload_id: "unicode_workload",
    namespace: "fixture",
    name: "unicode-fixture",
    state: "FAILED",
    due_at_us: null,
    started_at_us: 1800000000000000,
    finished_at_us: 1800000001000000,
    error_code: null,
    exit_code: 1,
    log_truncated: false,
  };
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname.replace("/api/v1", "");
    if (path === "/events") return;
    const latest: ExecutionDto = retried
      ? {
          ...execution,
          attempt_id: "unicode_retry",
          state: "SUCCEEDED",
          exit_code: 0,
        }
      : execution;
    if (path.endsWith("/logs/info")) {
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(logStream(latest.attempt_id, retried ? 12 : 10)) });
      return;
    }
    let value: unknown = { items: [], next_cursor: null };
    if (path === "/session")
      value = {
        actor_id: "fixture",
        role: "admin",
        namespaces: ["fixture"],
        capabilities: ["*"],
      };
    else if (path === "/status")
      value = {
        sqlite_version: "synthetic",
        workloads: 0,
        active: 0,
        unknown: 0,
        paused: false,
        restore_required: false,
        adapter_health: [],
      };
    else if (path === "/executions")
      value = { items: [latest], next_cursor: null };
    else if (path === "/executions/unicode_execution") value = latest;
    else if (path === "/diagnostics/unicode_execution")
      value = {
        execution_id: execution.execution_id,
        state: latest.state,
        observed_causes: [],
        hypotheses: [],
        evidence: [],
        safe_to_retry_automatically: !retried,
      };
    else if (path.endsWith("/logs")) {
      const offset = Number(url.searchParams.get("offset") ?? 0);
      cursors.push(offset);
      value = retried
        ? {
            attempt_id: latest.attempt_id,
            stream: logStream(latest.attempt_id, 12),
            text: offset === 0 ? "new attempt\n" : "",
            next_offset: 12,
            response_truncated: false,
            log_truncated: false,
            untrusted: true,
          }
        : {
            attempt_id: execution.attempt_id,
            stream: logStream(execution.attempt_id, 10),
            text:
              offset === 0
                ? "\uFFFD"
                : offset === 1
                  ? "😀"
                  : offset === 5
                    ? "done\n"
                    : "",
            next_offset: offset === 0 ? 1 : offset === 1 ? 5 : 10,
            response_truncated: offset < 5,
            log_truncated: false,
            untrusted: true,
          };
    }
    await route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(value),
    });
  });
  await page.goto("/#/executions/unicode_execution");
  await page
    .getByLabel("Access token", { exact: true })
    .fill("synthetic-fixture-token");
  await page.getByRole("button", { name: "Connect to instance" }).click();
  await expect(
    page.getByLabel("Execution logs, untrusted output"),
  ).toContainText("\uFFFD😀done");
  expect(cursors.slice(0, 3)).toEqual([0, 1, 5]);
  await expect(
    page.getByText("10 raw byte cursor · following", { exact: true }),
  ).toBeVisible();
  retried = true;
  await expect(
    page.getByLabel("Execution logs, untrusted output"),
  ).toContainText("new attempt");
  await expect(
    page.getByLabel("Execution logs, untrusted output"),
  ).not.toContainText("\uFFFD");
  expect(cursors.at(-1)).toBe(0);
  await expect(
    page.getByText("12 raw byte cursor · following", { exact: true }),
  ).toBeVisible();
});
