import { describe, expect, it } from "vitest";
import { SseParser } from "../src/sse";

describe("bounded SSE parser", () => {
  it("handles split CRLF, multiple data lines, and last event id", () => {
    const parser = new SseParser();
    expect(parser.push("id: 41\r")).toEqual([]);
    expect(
      parser.push("\nevent: update\r\ndata: first\r\ndata: second\r\n\r\n"),
    ).toEqual([{ id: "41", type: "update", data: "first\nsecond" }]);
  });
  it("ignores comments and rejects unbounded lines and event bodies", () => {
    const parser = new SseParser(64);
    expect(parser.push(": heartbeat\n\n")).toEqual([]);
    expect(() => parser.push("a".repeat(65))).toThrow("stream limit");
    const aggregate = new SseParser(64);
    expect(() => aggregate.push("data: abcdefghijklmnop\n".repeat(10))).toThrow(
      "stream limit",
    );
  });
  it("preserves incomplete events between network chunks", () => {
    const parser = new SseParser();
    expect(parser.push('event: resync\ndata: {"res')).toEqual([]);
    expect(parser.push('ync":true}\n\n')).toEqual([
      { id: "", type: "resync", data: '{"resync":true}' },
    ]);
  });
});
