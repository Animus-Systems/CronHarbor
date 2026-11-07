import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiClient, ApiError, apiBaseUrl, formatError } from "../src/api";
import { isOperation, isPrincipal, isToken, pageOf } from "../src/guards";

const operation = {
  id: "op_1",
  state: "accepted",
  target_id: "job_1",
  kind: "run",
  created_at_us: 1,
  error_code: null,
};
afterEach(() => vi.unstubAllGlobals());
describe("API trust boundaries", () => {
  it("accepts HTTPS and loopback, rejects credentials and insecure remote origins", () => {
    expect(
      apiBaseUrl("https://example.com/api/v1", "https://console.example.com"),
    ).toBe("https://example.com/api/v1");
    expect(apiBaseUrl("/api/v1", "http://localhost:5173")).toBe(
      "http://localhost:5173/api/v1",
    );
    for (const bad of [
      "http://example.com/api/v1",
      "https://user:secret@example.com/api/v1",
      "https://example.com/api/v1?token=bad",
    ]) {
      expect(() => apiBaseUrl(bad, "https://example.com")).toThrow();
    }
  });
  it("sends credentials in headers, validates responses, and does not auto-retry mutations", async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify(operation), { status: 202 }),
      );
    vi.stubGlobal("fetch", fetcher);
    const api = new ApiClient("https://example.com/api/v1", "secret");
    expect(
      await api.post("/jobs/job_1/run", {}, isOperation, "intent_1"),
    ).toEqual(operation);
    expect(fetcher).toHaveBeenCalledTimes(1);
    const options: unknown = fetcher.mock.calls[0]?.[1];
    expect(options).toMatchObject({
      headers: {
        Authorization: "Bearer secret",
        "Idempotency-Key": "intent_1",
      },
      redirect: "error",
      credentials: "omit",
    });
  });
  it("rejects malformed responses and suppresses raw backend errors", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          new Response('{"password":"secret"}', { status: 500 }),
        ),
    );
    const api = new ApiClient("https://example.com/api/v1", "secret");
    await expect(api.get("/operations/op", isOperation)).rejects.toMatchObject({
      code: "SERVER_ERROR",
    });
    expect(formatError(new Error("secret database path"))).not.toContain(
      "secret",
    );
    expect(formatError(new ApiError("CONFLICT", 409))).toContain("changed");
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response('{"id":5}', { status: 200 })),
    );
    await expect(api.get("/operations/op", isOperation)).rejects.toMatchObject({
      code: "INVALID_RESPONSE",
    });
  });
  it.each([
    [401, "UNAUTHENTICATED"],
    [403, "FORBIDDEN"],
    [409, "CONFLICT"],
    [429, "RATE_LIMITED"],
    [503, "UNAVAILABLE"],
  ])("maps HTTP %i without displaying its untrusted body", async (status, code) => {
    vi.stubGlobal("fetch", async () => new Response("private internal error", {
      status,
      headers: { "x-request-id": "request/<unsafe>" },
    }));
    const api = new ApiClient("https://example.com/api/v1", "private");
    await expect(api.get("/operations/op", isOperation)).rejects.toMatchObject({
      code,
      status,
      requestId: "requestunsafe",
    });
  });
  it.each([
    "{",
    JSON.stringify({ items: [{ ...operation, created_at_us: "yesterday" }], next_cursor: null }),
    JSON.stringify({ items: [operation, { ...operation, state: null }], next_cursor: null }),
    JSON.stringify({ items: [], next_cursor: {} }),
    JSON.stringify({ items: Array.from({ length: 1001 }, () => operation), next_cursor: null }),
  ])("rejects malformed or oversized page payload %$", async (body) => {
    vi.stubGlobal("fetch", async () => new Response(body));
    const api = new ApiClient("https://example.com/api/v1", "private");
    await expect(api.get("/operations", pageOf(isOperation))).rejects.toMatchObject({ code: "INVALID_RESPONSE" });
  });
  it("cancels a body rejected by its advertised size before consuming it", async () => {
    let cancelled = false;
    vi.stubGlobal("fetch", async () => new Response(new ReadableStream<Uint8Array>({
      cancel() { cancelled = true; },
    }), { headers: { "Content-Length": "2097153" } }));
    const api = new ApiClient("https://example.com/api/v1", "private");
    await expect(api.get("/operations/op", isOperation)).rejects.toMatchObject({ code: "INVALID_RESPONSE" });
    expect(cancelled).toBe(true);
  });
  it.each([false, true])("cancels rejected HTTP and event-stream bodies (stream=%s)", async (stream) => {
    let cancelled = false;
    vi.stubGlobal("fetch", async () => new Response(new ReadableStream<Uint8Array>({
      cancel() { cancelled = true; },
    }), { status: stream ? 200 : 500, headers: { "Content-Type": "application/json" } }));
    const api = new ApiClient("https://example.com/api/v1", "private");
    const request = stream
      ? api.stream("", new AbortController().signal)
      : api.get("/operations/op", isOperation);
    await expect(request).rejects.toMatchObject({ code: stream ? "INVALID_RESPONSE" : "SERVER_ERROR" });
    expect(cancelled).toBe(true);
  });
  it("enforces the response byte limit across chunks without trusting Content-Length", async () => {
    let cancelled = false;
    vi.stubGlobal("fetch", async () => new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(1_048_576));
        controller.enqueue(new Uint8Array(1_048_577));
      },
      cancel() { cancelled = true; },
    })));
    const api = new ApiClient("https://example.com/api/v1", "private");
    await expect(api.get("/operations/op", isOperation)).rejects.toMatchObject({ code: "INVALID_RESPONSE" });
    expect(cancelled).toBe(true);
  });
  it("treats a body lost after response headers as an uncertain network outcome", async () => {
    vi.stubGlobal("fetch", async () => new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('{"id":"op_'));
      },
      pull(controller) { controller.error(new Error("private transport detail")); },
    }), { status: 202 }));
    const api = new ApiClient("https://example.com/api/v1", "private");
    await expect(api.post("/jobs/job/run", {}, isOperation, "same_intent")).rejects.toMatchObject({ code: "NETWORK" });
  });
});

describe("credential metadata shape", () => {
  it("rejects role arrays and objects without coercing untrusted values", () => {
    const principal = {
      actor_id: "viewer",
      role: "viewer",
      namespaces: ["fixture"],
      capabilities: [],
    };
    expect(isPrincipal(principal)).toBe(true);
    for (const role of [
      ["viewer"],
      { toString: "viewer" },
      1,
      true,
      "toString",
    ]) {
      expect(isPrincipal({ ...principal, role })).toBe(false);
      expect(
        isToken({
          ...principal,
          role,
          id: "token",
          expires_at_us: null,
          revoked: false,
        }),
      ).toBe(false);
    }
  });
});
