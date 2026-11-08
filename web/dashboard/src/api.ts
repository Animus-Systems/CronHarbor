import type { Guard } from "./guards";

export class ApiError extends Error {
  constructor(
    readonly code: string,
    readonly status = 0,
    readonly requestId?: string,
  ) {
    super(code);
    this.name = "ApiError";
  }
}
const messages: Record<string, string> = {
  UNAUTHENTICATED:
    "Your token was rejected or expired. Connect again with a valid token.",
  FORBIDDEN:
    "Your token does not grant permission for this action or namespace.",
  NOT_FOUND: "This item is unavailable or outside your token’s scope.",
  CONFLICT:
    "The definition changed or this request conflicts with current state. Refresh and review the current revision.",
  INVALID:
    "The server rejected this definition or request. Check the supported schema and policy.",
  RATE_LIMITED: "The server is busy. Wait before trying again.",
  UNAVAILABLE:
    "The daemon or executor is unavailable. Check its health before trying again.",
  STORAGE_UNAVAILABLE:
    "State storage needs attention. Check metadata admission status and disk space before submitting new work.",
  SERVER_ERROR:
    "The server could not complete this request. Review server diagnostics using the request ID.",
  INVALID_RESPONSE:
    "The server returned an unsupported response. Check that the dashboard and API versions match.",
  NETWORK:
    "Connection lost. The outcome of a submitted action may be unknown. Reconnect and inspect its operation before retrying.",
};
export function formatError(error: unknown): string {
  if (!(error instanceof ApiError))
    return "The request could not be completed. Refresh and try again.";
  return (
    (messages[error.code] ?? messages.SERVER_ERROR ?? "Request failed.") +
    (error.requestId ? ` Request ID: ${error.requestId}` : "")
  );
}
export function apiBaseUrl(value: string, origin: string): string {
  const url = new URL(value, origin);
  if (
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    !["https:", "http:"].includes(url.protocol)
  )
    throw new Error("Invalid API address");
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if (url.protocol !== "https:" && !loopback)
    throw new Error("API requires HTTPS outside localhost");
  return url.href.replace(/\/$/, "");
}
export async function loadApiBase(): Promise<string> {
  let configured = "/api/v1";
  const response = await fetch("./config.json", {
    credentials: "omit",
    cache: "no-store",
    redirect: "error",
  });
  if (
    response.ok &&
    response.headers.get("content-type")?.includes("application/json")
  ) {
    const config: unknown = await response.json();
    if (
      typeof config !== "object" ||
      config === null ||
      !("apiBaseUrl" in config) ||
      typeof config.apiBaseUrl !== "string"
    )
      throw new Error("Invalid trusted API configuration");
    configured = config.apiBaseUrl;
  }
  return apiBaseUrl(configured, window.location.origin);
}
function codeForStatus(status: number): string {
  if (status === 401) return "UNAUTHENTICATED";
  if (status === 403) return "FORBIDDEN";
  if (status === 404) return "NOT_FOUND";
  if (status === 409 || status === 412) return "CONFLICT";
  if (status === 400 || status === 422) return "INVALID";
  if (status === 429) return "RATE_LIMITED";
  if (status === 503) return "UNAVAILABLE";
  if (status === 507) return "STORAGE_UNAVAILABLE";
  return "SERVER_ERROR";
}
export class ApiClient {
  constructor(
    readonly base: string,
    private readonly token: string,
  ) {}
  async request<T>(
    path: string,
    guard: Guard<T>,
    method = "GET",
    body?: unknown,
    key?: string,
  ): Promise<T> {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.token}`,
      Accept: "application/json",
    };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (key) headers["Idempotency-Key"] = key;
    let response: Response;
    try {
      response = await fetch(this.base + path, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        credentials: "omit",
        redirect: "error",
        cache: "no-store",
        signal: AbortSignal.timeout(20000),
      });
    } catch {
      throw new ApiError("NETWORK");
    }
    const requestId = response.headers
      .get("x-request-id")
      ?.replace(/[^a-zA-Z0-9_-]/g, "")
      .slice(0, 100);
    if (!response.ok) {
      await response.body?.cancel().catch(() => {});
      throw new ApiError(
        codeForStatus(response.status),
        response.status,
        requestId,
      );
    }
    const length = Number(response.headers.get("content-length"));
    if (length > 2_097_152) {
      await response.body?.cancel().catch(() => {});
      throw new ApiError("INVALID_RESPONSE");
    }
    const reader = response.body?.getReader();
    if (!reader) throw new ApiError("INVALID_RESPONSE");
    const decoder = new TextDecoder();
    let text = "";
    let bytes = 0;
    try {
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > 2_097_152) throw new ApiError("INVALID_RESPONSE");
        text += decoder.decode(chunk.value, { stream: true });
      }
      text += decoder.decode();
    } catch (caught) {
      throw caught instanceof ApiError ? caught : new ApiError("NETWORK");
    } finally {
      await reader.cancel().catch(() => {});
      reader.releaseLock();
    }
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch {
      throw new ApiError("INVALID_RESPONSE");
    }
    if (!guard(value)) throw new ApiError("INVALID_RESPONSE");
    return value;
  }
  get<T>(path: string, guard: Guard<T>): Promise<T> {
    return this.request(path, guard);
  }
  post<T>(
    path: string,
    body: unknown,
    guard: Guard<T>,
    key?: string,
  ): Promise<T> {
    return this.request(path, guard, "POST", body, key);
  }
  async stream(lastId: string, signal: AbortSignal): Promise<Response> {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.token}`,
      Accept: "text/event-stream",
    };
    if (lastId) headers["Last-Event-ID"] = lastId;
    const response = await fetch(this.base + "/events", {
      headers,
      signal,
      credentials: "omit",
      redirect: "error",
      cache: "no-store",
    });
    if (!response.ok) {
      await response.body?.cancel().catch(() => {});
      throw new ApiError(codeForStatus(response.status), response.status);
    }
    if (!response.headers.get("content-type")?.includes("text/event-stream")) {
      await response.body?.cancel().catch(() => {});
      throw new ApiError("INVALID_RESPONSE");
    }
    return response;
  }
}
