import { afterEach, describe, expect, it, vi } from "vitest";
import { dashboard } from "../src/store";
import type {
  EventDto,
  OperationDto,
  PrincipalDto,
  StatusDto,
} from "../src/generated/contracts";
const status: StatusDto = {
  sqlite_version: "3",
  workloads: 0,
  active: 0,
  unknown: 0,
  paused: false,
  restore_required: false,
  adapter_health: [],
};
const principal: PrincipalDto = {
  actor_id: "admin",
  role: "admin",
  namespaces: ["default"],
  capabilities: ["*"],
};
const operation: OperationDto = {
  id: "op_1",
  state: "accepted",
  target_id: "default/test",
  kind: "run",
  created_at_us: 1,
  error_code: null,
};
function json(value: unknown, code = 200): Response {
  return new Response(JSON.stringify(value), { status: code });
}
afterEach(() => {
  dashboard.disconnect();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
describe("operation recovery", () => {
  it("retains one exact intent after ambiguous disconnect and reuses its key only on manual retry", async () => {
    const submitted: {
      key: string | null;
      body: BodyInit | null | undefined;
    }[] = [];
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events"))
        return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener(
            "abort",
            () => reject(new Error("aborted")),
            { once: true },
          );
        });
      if (init?.method === "POST") {
        submitted.push({
          key: new Headers(init.headers).get("Idempotency-Key"),
          body: init.body,
        });
        if (submitted.length === 1)
          throw new TypeError("network lost after commit");
        return json(operation, 202);
      }
      if (input.endsWith("/status")) return json(status);
      if (input.endsWith("/session")) return json(principal);
      return json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    expect(dashboard.canAct.value).toBe(true);
    expect(await dashboard.mutate("/jobs/test/run", {})).toBeNull();
    expect(dashboard.connected.value).toBe(false);
    expect(dashboard.uncertain.value).not.toBeNull();
    expect(submitted).toHaveLength(1);
    expect(await dashboard.mutate("/jobs/test/run", {})).toBeNull();
    expect(submitted).toHaveLength(1);
    dashboard.connected.value = true;
    await dashboard.retryUncertain();
    expect(submitted).toHaveLength(2);
    expect(submitted[1]).toEqual(submitted[0]);
    expect(dashboard.operations.value[0]).toEqual(operation);
    expect(dashboard.uncertain.value).toBeNull();
    expect(dashboard.notice.value).toContain(
      "Acceptance does not mean completion",
    );
  });
  it("drops credentials and cached operational data on explicit disconnect", async () => {
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events"))
        return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener(
            "abort",
            () => reject(new Error("aborted")),
            { once: true },
          );
        });
      return input.endsWith("/session")
        ? json(principal)
        : input.endsWith("/status")
          ? json(status)
          : json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    dashboard.disconnect();
    expect(dashboard.api.value).toBeNull();
    expect(dashboard.principal.value).toBeNull();
    expect(dashboard.operations.value).toEqual([]);
    expect(dashboard.lastRefresh.value).toBeNull();
  });
});

describe("asynchronous session boundaries", () => {
  function stubSession(post: () => Promise<Response>): void {
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events"))
        return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener(
            "abort",
            () => reject(new Error("aborted")),
            { once: true },
          );
        });
      if (init?.method === "POST") return post();
      return input.endsWith("/session")
        ? json(principal)
        : input.endsWith("/status")
          ? json(status)
          : json({ items: [], next_cursor: null });
    });
  }
  it("keeps action failures visible after a successful background snapshot refresh", async () => {
    stubSession(async () => json({}, 409));
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    expect(await dashboard.mutate("/jobs/test/run")).toBeNull();
    const message = dashboard.error.value;
    expect(message).toContain("changed");
    await dashboard.refresh();
    expect(dashboard.error.value).toBe(message);
  });
  it("discards late mutation responses after the authenticated session ends", async () => {
    let finish: ((response: Response) => void) | undefined;
    stubSession(
      () =>
        new Promise<Response>((resolve) => {
          finish = resolve;
        }),
    );
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    const response = dashboard.mutate("/jobs/test/run");
    dashboard.disconnect();
    finish?.(json(operation, 202));
    expect(await response).toBeNull();
    expect(dashboard.operations.value).toEqual([]);
    expect(dashboard.notice.value).toBe("");
  });
});

describe("snapshot and stream recovery", () => {
  it("shares one in-flight snapshot between reconnect and refresh callers", async () => {
    let complete: ((value: Response) => void) | undefined;
    let statusRequests = 0;
    let delayStatus = false;
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events"))
        return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener(
            "abort",
            () => reject(new Error("aborted")),
            { once: true },
          );
        });
      if (input.endsWith("/session")) return json(principal);
      if (input.endsWith("/status")) {
        if (delayStatus) {
          statusRequests++;
          return new Promise<Response>((resolve) => {
            complete = resolve;
          });
        }
        return json(status);
      }
      return json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    delayStatus = true;
    const first = dashboard.refresh();
    const second = dashboard.refresh();
    expect(first).toBe(second);
    await vi.waitFor(() => expect(statusRequests).toBe(1));
    complete?.(json(status));
    await Promise.all([first, second]);
    expect(dashboard.connected.value).toBe(true);
  });
  it("cancels an opened SSE body when its required authoritative snapshot fails", async () => {
    let streamOpened = false;
    let cancelled = 0;
    vi.stubGlobal("fetch", async (input: string) => {
      if (input.endsWith("/events")) {
        streamOpened = true;
        return new Response(
          new ReadableStream<Uint8Array>({
            cancel() {
              cancelled++;
            },
          }),
          { headers: { "Content-Type": "text/event-stream" } },
        );
      }
      if (input.endsWith("/session")) return json(principal);
      if (input.endsWith("/status")) {
        if (streamOpened) throw new TypeError("network lost");
        return json(status);
      }
      return json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    await vi.waitFor(() => expect(cancelled).toBe(1));
    expect(dashboard.connected.value).toBe(false);
  });
});

describe("offline snapshots with an existing stream", () => {
  it("recovers authoritative state without waiting for a new SSE event and never retries a mutation", async () => {
    vi.useFakeTimers();
    let online = true;
    let streamCount = 0;
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events")) {
        streamCount++;
        return new Response(
          new ReadableStream<Uint8Array>({
            start(controller) {
              init?.signal?.addEventListener(
                "abort",
                () => controller.error(new Error("aborted")),
                { once: true },
              );
            },
          }),
          { headers: { "Content-Type": "text/event-stream" } },
        );
      }
      if (!online) throw new TypeError("network offline");
      if (input.endsWith("/session")) return json(principal);
      if (input.endsWith("/status")) return json(status);
      return json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    await dashboard.refresh();
    online = false;
    await expect(dashboard.refresh()).rejects.toMatchObject({
      code: "NETWORK",
    });
    expect(dashboard.connected.value).toBe(false);
    online = true;
    await vi.advanceTimersByTimeAsync(1000);
    expect(dashboard.connected.value).toBe(true);
    expect(streamCount).toBe(1);
    expect(dashboard.operations.value).toEqual([]);
  });
});

describe("session ownership during delayed requests", () => {
  it("still rejects its own invalid credential while clearing the session", async () => {
    vi.stubGlobal("fetch", async () => json({}, 401));
    await expect(dashboard.connect("http://127.0.0.1/api/v1", "rejected")).rejects.toMatchObject({ code: "UNAUTHENTICATED" });
    expect(dashboard.api.value).toBeNull();
    expect(dashboard.principal.value).toBeNull();
    expect(dashboard.connecting.value).toBe(false);
  });
  it("does not start background work when a pending connection finishes after disconnect", async () => {
    vi.useFakeTimers();
    let finish: ((response: Response) => void) | undefined;
    vi.stubGlobal("fetch", () => new Promise<Response>((resolve) => {
      finish = resolve;
    }));
    const connection = dashboard.connect("http://127.0.0.1/api/v1", "old");
    dashboard.disconnect();
    finish?.(json(principal));
    await connection;
    expect(dashboard.api.value).toBeNull();
    expect(dashboard.connecting.value).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("lets only the newest connection open its credential-owned stream", async () => {
    let finishOld: ((response: Response) => void) | undefined;
    const streams: (string | null)[] = [];
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      const credential = new Headers(init?.headers).get("Authorization");
      if (input.endsWith("/events")) {
        streams.push(credential);
        return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
        });
      }
      if (input.endsWith("/session")) {
        if (credential === "Bearer old") return new Promise<Response>((resolve) => {
          finishOld = resolve;
        });
        return json({ ...principal, actor_id: "new" });
      }
      return input.endsWith("/status") ? json(status) : json({ items: [], next_cursor: null });
    });
    const oldConnection = dashboard.connect("http://127.0.0.1/api/v1", "old");
    await dashboard.connect("http://127.0.0.1/api/v1", "new");
    finishOld?.(json(principal));
    await oldConnection;
    expect(dashboard.principal.value?.actor_id).toBe("new");
    expect(streams).toEqual(["Bearer new"]);
  });

  it("discards a failed pagination response from the previous credential", async () => {
    let finishPage: ((response: Response) => void) | undefined;
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events")) return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
      });
      if (input.includes("cursor=")) return new Promise<Response>((resolve) => {
        finishPage = resolve;
      });
      if (input.endsWith("/session")) return json(principal);
      return input.endsWith("/status") ? json(status) : json({ items: [], next_cursor: "next" });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "old");
    const page = dashboard.loadMore("workloads");
    await dashboard.connect("http://127.0.0.1/api/v1", "new");
    finishPage?.(json({}, 403));
    await page;
    expect(dashboard.error.value).toBe("");
    expect(dashboard.connected.value).toBe(true);
  });
});

describe("manual ambiguous submission retry", () => {
  it.each([
    [202, '{"id":'],
    [503, "private server detail"],
  ])("retains an uncertain intent after an unreadable or failed HTTP %i receipt", async (code, body) => {
    let posts = 0;
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events")) return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
      });
      if (init?.method === "POST") {
        posts++;
        return new Response(body, { status: code });
      }
      if (input.endsWith("/session")) return json(principal);
      return input.endsWith("/status") ? json(status) : json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    await dashboard.mutate("/jobs/test/run");
    expect(dashboard.canAct.value).toBe(false);
    expect(dashboard.uncertain.value?.path).toBe("/jobs/test/run");
    expect(dashboard.operations.value).toEqual([]);
    expect(dashboard.error.value).not.toContain("private server detail");
    await dashboard.refresh();
    expect(dashboard.connected.value).toBe(true);
    expect(dashboard.canAct.value).toBe(false);
    expect(posts).toBe(1);
  });
  it("rejects an unserializable intent before sending or retaining it", async () => {
    const fetcher = vi.fn();
    vi.stubGlobal("fetch", fetcher);
    dashboard.connected.value = true;
    expect(await dashboard.mutate("/jobs/test/run", { invalid: 1n })).toBeNull();
    expect(fetcher).not.toHaveBeenCalled();
    expect(dashboard.uncertain.value).toBeNull();
    expect(dashboard.pending.value).toBe(false);
    expect(dashboard.error.value).toContain("not submitted");
  });
  it("keeps the original request bytes even if the caller later edits its object", async () => {
    const bodies: (BodyInit | null | undefined)[] = [];
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events")) return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
      });
      if (init?.method === "POST") {
        bodies.push(init.body);
        if (bodies.length === 1) throw new TypeError("lost receipt");
        return json(operation, 202);
      }
      if (input.endsWith("/session")) return json(principal);
      return input.endsWith("/status") ? json(status) : json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    const body = { definition: { command: "/bin/echo", args: ["reviewed"] } };
    await dashboard.mutate("/config/apply", body);
    body.definition.args[0] = "unreviewed";
    await dashboard.refresh();
    await dashboard.retryUncertain();
    expect(bodies).toHaveLength(2);
    expect(bodies[1]).toBe(bodies[0]);
  });

  it("does not submit a second manual retry while the first retry is pending", async () => {
    let posts = 0;
    let finish: ((response: Response) => void) | undefined;
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events")) return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
      });
      if (init?.method === "POST") {
        posts++;
        if (posts === 1) throw new TypeError("lost receipt");
        if (posts > 2) return json(operation, 202);
        return new Promise<Response>((resolve) => { finish = resolve; });
      }
      if (input.endsWith("/session")) return json(principal);
      return input.endsWith("/status") ? json(status) : json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    await dashboard.mutate("/jobs/test/run");
    await dashboard.refresh();
    const retry = dashboard.retryUncertain();
    const repeated = dashboard.retryUncertain();
    finish?.(json(operation, 202));
    await Promise.all([retry, repeated]);
    expect(posts).toBe(2);
  });
});

describe("stream framing and recovery cursors", () => {
  const event: EventDto = {
    seq: 41,
    created_at_us: 1,
    event_kind: "finished",
    payload_json: "emoji 😀 evidence",
    workload_id: "default/test",
    execution_id: null,
  };
  it("discards a pending action when split stream frames revoke the session", async () => {
    let stream: ReadableStreamDefaultController<Uint8Array> | undefined;
    let finish: ((response: Response) => void) | undefined;
    let cancelled = false;
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (input.endsWith("/events")) return new Response(new ReadableStream<Uint8Array>({
        start(controller) { stream = controller; },
        cancel() { cancelled = true; },
      }), { headers: { "Content-Type": "text/event-stream" } });
      if (init?.method === "POST") return new Promise<Response>((resolve) => { finish = resolve; });
      if (input.endsWith("/session")) return json(principal);
      return input.endsWith("/status") ? json(status) : json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    await dashboard.refresh();
    const action = dashboard.mutate("/jobs/test/run");
    stream?.enqueue(new TextEncoder().encode("event: auth_ex"));
    stream?.enqueue(new TextEncoder().encode(`pired\ndata: {}\n\nid: 41\ndata: ${JSON.stringify(event)}\n\n`));
    await vi.waitFor(() => expect(dashboard.api.value).toBeNull());
    finish?.(json(operation, 202));
    expect(await action).toBeNull();
    expect(dashboard.principal.value).toBeNull();
    expect(dashboard.events.value).toEqual([]);
    expect(dashboard.operations.value).toEqual([]);
    expect(dashboard.pending.value).toBe(false);
    expect(cancelled).toBe(true);
    expect(dashboard.error.value).toContain("token was rejected");
  });
  it.each([
    ["malformed", 'id: 42\ndata: {"seq":"bad"}\n\n'],
    ["truncated", 'id: 42\ndata: {"seq":42'],
    ["invalid cursor", `id: invalid-cursor\ndata: ${JSON.stringify({ ...event, seq: 42 })}\n\n`],
    ["mismatched cursor", `id: 43\ndata: ${JSON.stringify({ ...event, seq: 42 })}\n\n`],
    ["noncanonical cursor", `id: 042\ndata: ${JSON.stringify({ ...event, seq: 42 })}\n\n`],
    ["repeated sequence", `id: 41\ndata: ${JSON.stringify(event)}\n\n`],
    ["backwards sequence", `id: 40\ndata: ${JSON.stringify({ ...event, seq: 40 })}\n\n`],
    ["negative sequence", `id: -1\ndata: ${JSON.stringify({ ...event, seq: -1 })}\n\n`],
    ["fractional sequence", `id: 42.5\ndata: ${JSON.stringify({ ...event, seq: 42.5 })}\n\n`],
    ["unsafe sequence", `id: 9007199254740992\ndata: ${JSON.stringify({ ...event, seq: 9007199254740992 })}\n\n`],
    ["valid successor", `id: 42\ndata: ${JSON.stringify({ ...event, seq: 42 })}\n\n`],
  ])("reconnects after a %s event from the last complete validated event", async (_kind, ending) => {
    vi.useFakeTimers();
    const cursors: (string | null)[] = [];
    let posts = 0;
    let cancelled = 0;
    vi.stubGlobal("fetch", async (input: string, init?: RequestInit) => {
      if (init?.method === "POST") posts++;
      if (input.endsWith("/events")) {
        cursors.push(new Headers(init?.headers).get("Last-Event-ID"));
        if (cursors.length > 1) return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
        });
        const encoded = new TextEncoder().encode(`id: 41\nevent: change\ndata: ${JSON.stringify(event)}\n\n${ending}`);
        const emoji = encoded.findIndex((byte) => byte === 0xf0);
        return new Response(new ReadableStream<Uint8Array>({
          start(controller) {
            controller.enqueue(encoded.slice(0, emoji + 1));
            controller.enqueue(encoded.slice(emoji + 1, emoji + 3));
            controller.enqueue(encoded.slice(emoji + 3));
            if (_kind === "truncated" || _kind === "valid successor") controller.close();
          },
          cancel() { cancelled++; },
        }), { headers: { "Content-Type": "text/event-stream" } });
      }
      if (input.endsWith("/session")) return json(principal);
      return input.endsWith("/status") ? json(status) : json({ items: [], next_cursor: null });
    });
    await dashboard.connect("http://127.0.0.1/api/v1", "private");
    await vi.advanceTimersByTimeAsync(0);
    expect(dashboard.events.value).toEqual(_kind === "valid successor" ? [{ ...event, seq: 42 }, event] : [event]);
    expect(dashboard.connected.value).toBe(false);
    if (_kind !== "truncated" && _kind !== "valid successor") expect(cancelled).toBe(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(cursors).toEqual([null, _kind === "valid successor" ? "42" : "41"]);
    expect(posts).toBe(0);
  });
});
