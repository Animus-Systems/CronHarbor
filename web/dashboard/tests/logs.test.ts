import { describe, expect, it } from "vitest";
import { isLog } from "../src/guards";
import type { LogDto } from "../src/generated/contracts";
describe("raw log cursors", () => {
  const log: LogDto = {
    attempt_id: "attempt",
    text: "\uFFFD😀",
    next_offset: 5,
    response_truncated: true,
    log_truncated: false,
    untrusted: true,
  };
  it("accepts the canonical raw offset independently of encoded display text length", () => {
    expect(new TextEncoder().encode(log.text).length).toBe(7);
    expect(isLog(log)).toBe(true);
    expect(log.next_offset).toBe(5);
  });
  it("rejects missing, fractional, negative and unsafe offsets", () => {
    for (const offset of [undefined, -1, 0.5, Number.MAX_SAFE_INTEGER + 1, "5"])
      expect(isLog({ ...log, next_offset: offset })).toBe(false);
  });
});
