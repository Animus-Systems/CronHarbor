import { describe, expect, it } from "vitest";
import { isStatus } from "../src/guards";

const status = {
  sqlite_version: "3.51.3", workloads: 9, active: 9, unknown: 0,
  paused: false, restore_required: false, adapter_health: [],
};
const admission = {
  database_bytes: 939524096, wal_bytes: 4096, warning: true, paused: true,
  policy: { maxDatabaseBytes: 1073741824, warningBytes: 805306368,
    admissionPauseBytes: 939524096, maxWalBytes: 67108864 },
};

describe("metadata admission boundary", () => {
  it("accepts legacy status and explicit storage pressure", () => {
    expect(isStatus(status)).toBe(true);
    expect(isStatus({ ...status, admission })).toBe(true);
  });
  it("rejects malformed admission metadata before presentation", () => {
    for (const change of [
      { paused: "false" }, { warning: undefined }, { database_bytes: -1 },
      { wal_bytes: Number.MAX_SAFE_INTEGER + 1 }, { policy: {} },
      { policy: { ...admission.policy, maxDatabaseBytes: "large" } },
    ]) {
      expect(isStatus({ ...status, admission: { ...admission, ...change } })).toBe(false);
    }
  });
});
