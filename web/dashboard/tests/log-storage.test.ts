import { describe, expect, it } from "vitest";
import { isLog, isLogStoragePolicy } from "../src/guards";

const stream = {
  attempt_id: "attempt",
  capture_mode: "ROLLING",
  capture_state: "CAPTURING",
  retained_from_offset: 65536,
  written_end_offset: 131072,
  retained_bytes: 65536,
  known_discarded_bytes: 0,
  history_evicted: true,
  log_truncated: false,
  last_error_code: null,
};
const response = {
  attempt_id: "attempt",
  text: "recent output",
  next_offset: 65549,
  response_truncated: true,
  log_truncated: false,
  untrusted: true,
  stream,
};

describe("rotated log trust boundary", () => {
  it("requires explicit import provenance on the canonical installation policy", () => {
    const policy = { revision: 1, max_disk_bytes: 1048576, max_segments: 16, retention_seconds: 604800 };
    expect(isLogStoragePolicy(policy)).toBe(false);
    expect(isLogStoragePolicy({ ...policy, imported_conservative: true })).toBe(true);
    expect(isLogStoragePolicy({ ...policy, imported_conservative: "false" })).toBe(false);
  });
  it("accepts bounded stream metadata and an absolute byte cursor", () => {
    expect(isLog(response)).toBe(true);
  });

  it("rejects missing, backwards, fractional and unsafe stream ranges", () => {
    expect(isLog({ ...response, stream: undefined })).toBe(false);
    for (const update of [
      { retained_from_offset: -1 },
      { written_end_offset: 1 },
      { retained_bytes: -1 },
      { known_discarded_bytes: 0.5 },
      { written_end_offset: Number.MAX_SAFE_INTEGER + 1 },
      { capture_mode: "guess" },
      { capture_state: "ready-ish" },
    ]) {
      expect(isLog({ ...response, stream: { ...stream, ...update } })).toBe(false);
    }
  });

  it("rejects a cursor or attempt outside the verified retained stream", () => {
    expect(isLog({ ...response, next_offset: 0 })).toBe(false);
    expect(isLog({ ...response, next_offset: 131073 })).toBe(false);
    expect(isLog({ ...response, stream: { ...stream, attempt_id: "other" } })).toBe(false);
  });
});
