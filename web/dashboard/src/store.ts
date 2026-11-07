import { computed, ref, shallowRef } from "vue";
import type {
  ExecutionDto,
  OperationDto,
  PrincipalDto,
  StatusDto,
  WorkloadDto,
  EventDto,
} from "./generated/contracts";
import { ApiClient, ApiError, formatError } from "./api";
import {
  isEvent,
  isExecution,
  isOperation,
  isPrincipal,
  isStatus,
  isWorkload,
  pageOf,
} from "./guards";
import { SseParser } from "./sse";
import { operationFinished, retryDelay } from "./operations";
import { permitted } from "./permissions";

const api = shallowRef<ApiClient | null>(null);
const principal = ref<PrincipalDto | null>(null);
const status = ref<StatusDto | null>(null);
const workloads = ref<WorkloadDto[]>([]);
const executions = ref<ExecutionDto[]>([]);
const workloadCursor = ref<string | null>(null);
const executionCursor = ref<string | null>(null);
const operations = ref<OperationDto[]>([]);
const events = ref<EventDto[]>([]);
const connected = ref(false);
const connecting = ref(false);
const error = ref("");
const connectionError = ref("");
const notice = ref("");
const lastRefresh = ref<number | null>(null);
const refreshing = ref(false);
const pending = ref(false);
interface Intent {
  path: string;
  body: unknown;
  key: string;
}
const uncertain = ref<Intent | null>(null);
let streamAbort: AbortController | null = null;
let refreshTimer: ReturnType<typeof setTimeout> | undefined;
let pollTimer: ReturnType<typeof setTimeout> | undefined;
let sessionGeneration = 0;
let recoveryAttempt = 0;
let refreshTask: Promise<void> | null = null;
const canAct = computed(
  () => connected.value && !pending.value && !uncertain.value,
);
function client(): ApiClient {
  if (!api.value) throw new ApiError("UNAUTHENTICATED");
  return api.value;
}
function can(capability: string, target?: string): boolean {
  return permitted(principal.value, capability, target);
}
function refresh(): Promise<void> {
  if (refreshTask) return refreshTask;
  const owned = refreshSnapshot().finally(() => {
    if (refreshTask === owned) refreshTask = null;
  });
  refreshTask = owned;
  return owned;
}
async function refreshSnapshot(): Promise<void> {
  const current = api.value;
  if (!current) return;
  refreshing.value = true;
  try {
    const identity = await current.get("/session", isPrincipal);
    if (api.value !== current) return;
    const [nextStatus, nextWorkloads, nextExecutions] = await Promise.all([
      permitted(identity, "workload.read")
        ? current.get("/status", isStatus)
        : Promise.resolve(null),
      permitted(identity, "workload.read")
        ? current.get("/workloads?limit=100", pageOf(isWorkload))
        : Promise.resolve({ items: [], next_cursor: null }),
      permitted(identity, "execution.read")
        ? current.get("/executions?limit=100", pageOf(isExecution))
        : Promise.resolve({ items: [], next_cursor: null }),
    ]);
    if (api.value !== current) return;
    status.value = nextStatus;
    workloads.value = nextWorkloads.items;
    executions.value = nextExecutions.items;
    workloadCursor.value = nextWorkloads.next_cursor;
    executionCursor.value = nextExecutions.next_cursor;
    principal.value = identity;
    connected.value = true;
    lastRefresh.value = Date.now();
    connectionError.value = "";
    recoveryAttempt = 0;
  } catch (caught) {
    if (api.value !== current) return;
    connected.value = false;
    connectionError.value = formatError(caught);
    if (caught instanceof ApiError && caught.status === 401) {
      disconnect();
      connectionError.value = formatError(caught);
    } else if (!(caught instanceof ApiError && caught.status === 403))
      scheduleRefresh();
    throw caught;
  } finally {
    if (api.value === current) refreshing.value = false;
  }
}
async function loadMore(kind: "workloads" | "executions"): Promise<void> {
  const current = api.value;
  if (!current) return;
  const cursor =
    kind === "workloads" ? workloadCursor.value : executionCursor.value;
  if (!cursor) return;
  try {
    if (kind === "workloads") {
      const result = await client().get(
        `/workloads?limit=100&cursor=${encodeURIComponent(cursor)}`,
        pageOf(isWorkload),
      );
      if (api.value !== current) return;
      workloads.value = [...workloads.value, ...result.items];
      workloadCursor.value = result.next_cursor;
    } else {
      const result = await client().get(
        `/executions?limit=100&cursor=${encodeURIComponent(cursor)}`,
        pageOf(isExecution),
      );
      if (api.value !== current) return;
      executions.value = [...executions.value, ...result.items];
      executionCursor.value = result.next_cursor;
    }
  } catch (caught) {
    if (api.value !== current) return;
    error.value = formatError(caught);
  }
}
function scheduleRefresh(): void {
  if (refreshTimer) return;
  refreshTimer = setTimeout(
    () => {
      refreshTimer = undefined;
      void refresh().catch(() => {});
    },
    connected.value ? 350 : retryDelay(recoveryAttempt++),
  );
}
async function streamLoop(
  generation: number,
  abort: AbortController,
): Promise<void> {
  let cursor = "";
  let attempt = 0;
  while (!abort.signal.aborted && generation === sessionGeneration) {
    try {
      const response = await client().stream(cursor, abort.signal);
      if (!response.body) throw new ApiError("INVALID_RESPONSE");
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      const parser = new SseParser();
      try {
        await refresh();
        attempt = 0;
        while (!abort.signal.aborted) {
          const chunk = await reader.read();
          if (abort.signal.aborted || generation !== sessionGeneration) return;
          if (chunk.done) break;
          for (const message of parser.push(
            decoder.decode(chunk.value, { stream: true }),
          )) {
            if (message.type === "auth_expired") {
              disconnect();
              error.value = formatError(new ApiError("UNAUTHENTICATED", 401));
              return;
            }
            if (message.type === "stream_error") throw new ApiError("NETWORK");
            if (message.type === "resync") {
              cursor = "";
              connected.value = false;
              scheduleRefresh();
              continue;
            }
            let payload: unknown;
            try {
              payload = JSON.parse(message.data);
            } catch {
              throw new ApiError("INVALID_RESPONSE");
            }
            if (
              !isEvent(payload) ||
              !Number.isSafeInteger(payload.seq) ||
              payload.seq < 0 ||
              message.id !== String(payload.seq) ||
              (cursor !== "" && payload.seq <= Number(cursor))
            )
              throw new ApiError("INVALID_RESPONSE");
            cursor = message.id;
            events.value = [payload, ...events.value].slice(0, 100);
            scheduleRefresh();
          }
        }
      } finally {
        await reader.cancel().catch(() => {});
        reader.releaseLock();
      }
      if (!abort.signal.aborted) throw new ApiError("NETWORK");
    } catch (caught) {
      if (abort.signal.aborted || generation !== sessionGeneration) return;
      connected.value = false;
      connectionError.value = formatError(caught);
      if (caught instanceof ApiError && caught.status === 401) {
        disconnect();
        connectionError.value = formatError(caught);
        return;
      }
      if (caught instanceof ApiError && caught.status === 403) return;
      await new Promise<void>((resolve) => {
        const done = () => {
          clearTimeout(timer);
          abort.signal.removeEventListener("abort", done);
          resolve();
        };
        const timer = setTimeout(done, retryDelay(attempt++));
        abort.signal.addEventListener("abort", done, { once: true });
      });
    }
  }
}
async function connect(base: string, token: string): Promise<void> {
  disconnect();
  const generation = sessionGeneration;
  connecting.value = true;
  error.value = "";
  connectionError.value = "";
  api.value = new ApiClient(base, token);
  try {
    await refresh();
    if (generation !== sessionGeneration) return;
    streamAbort = new AbortController();
    void streamLoop(sessionGeneration, streamAbort);
    pollOperations();
  } catch (caught) {
    if (generation === sessionGeneration) api.value = null;
    throw caught;
  } finally {
    if (generation === sessionGeneration) connecting.value = false;
  }
}
function disconnect(): void {
  sessionGeneration++;
  recoveryAttempt = 0;
  refreshTask = null;
  refreshing.value = false;
  streamAbort?.abort();
  streamAbort = null;
  if (refreshTimer) clearTimeout(refreshTimer);
  if (pollTimer) clearTimeout(pollTimer);
  refreshTimer = undefined;
  pollTimer = undefined;
  api.value = null;
  connecting.value = false;
  principal.value = null;
  connected.value = false;
  status.value = null;
  workloads.value = [];
  executions.value = [];
  operations.value = [];
  events.value = [];
  lastRefresh.value = null;
  uncertain.value = null;
  pending.value = false;
  notice.value = "";
  error.value = "";
  connectionError.value = "";
}
function rememberOperation(operation: OperationDto): void {
  operations.value = [
    operation,
    ...operations.value.filter((item) => item.id !== operation.id),
  ].slice(0, 100);
}
async function pollOperations(): Promise<void> {
  const generation = sessionGeneration;
  if (!api.value) return;
  if (connected.value) {
    for (const operation of operations.value.filter(
      (item) => !operationFinished(item.state),
    )) {
      try {
        const result = await client().get(
          `/operations/${encodeURIComponent(operation.id)}`,
          isOperation,
        );
        if (generation !== sessionGeneration) return;
        rememberOperation(result);
        if (operationFinished(result.state)) scheduleRefresh();
      } catch (caught) {
        if (generation !== sessionGeneration) return;
        if (caught instanceof ApiError && caught.status === 401) {
          disconnect();
          connectionError.value = formatError(caught);
          return;
        }
      }
    }
  }
  if (generation === sessionGeneration)
    pollTimer = setTimeout(() => {
      void pollOperations();
    }, 1500);
}
async function submitIntent(intent: Intent): Promise<OperationDto | null> {
  const generation = sessionGeneration;
  pending.value = true;
  error.value = "";
  notice.value = "";
  try {
    const operation = await client().post(
      intent.path,
      intent.body,
      isOperation,
      intent.key,
    );
    if (generation !== sessionGeneration) return null;
    rememberOperation(operation);
    uncertain.value = null;
    notice.value = `Operation ${operation.id} ${operation.state.toLowerCase()}. Acceptance does not mean completion.`;
    scheduleRefresh();
    return operation;
  } catch (caught) {
    if (generation !== sessionGeneration) return null;
    error.value = formatError(caught);
    if (caught instanceof ApiError && caught.status === 401) {
      const message = error.value;
      disconnect();
      error.value = message;
      return null;
    }
    if (
      caught instanceof ApiError &&
      (["NETWORK", "INVALID_RESPONSE"].includes(caught.code) ||
        caught.status >= 500)
    ) {
      uncertain.value = intent;
      connected.value = false;
      scheduleRefresh();
    }
    return null;
  } finally {
    if (generation === sessionGeneration) pending.value = false;
  }
}
async function mutate(
  path: string,
  body: unknown = {},
): Promise<OperationDto | null> {
  if (!canAct.value) {
    error.value = !connected.value
      ? "Action was not submitted: the connection must be verified first."
      : uncertain.value
        ? "Resolve the previous uncertain submission before starting a new action."
        : "Wait for the current submission before starting a new action.";
    return null;
  }
  let snapshot: unknown;
  try {
    const serialized = JSON.stringify(body);
    snapshot = serialized === undefined ? undefined : JSON.parse(serialized);
  } catch {
    error.value =
      "Action was not submitted: the request must be serializable JSON.";
    return null;
  }
  return submitIntent({ path, body: snapshot, key: crypto.randomUUID() });
}
async function retryUncertain(): Promise<void> {
  if (uncertain.value && connected.value && !pending.value)
    await submitIntent(uncertain.value);
}
function networkOffline(): void {
  if (!api.value) return;
  connected.value = false;
  connectionError.value = "The browser reports that the network is offline.";
  scheduleRefresh();
}
function networkOnline(): void {
  if (api.value) void refresh().catch(() => {});
}
export const dashboard = {
  networkOffline,
  networkOnline,
  api,
  principal,
  status,
  workloads,
  executions,
  operations,
  events,
  connected,
  connecting,
  error,
  connectionError,
  notice,
  lastRefresh,
  refreshing,
  pending,
  uncertain,
  canAct,
  workloadCursor,
  executionCursor,
  client,
  can,
  connect,
  disconnect,
  refresh,
  loadMore,
  mutate,
  retryUncertain,
  rememberOperation,
};
