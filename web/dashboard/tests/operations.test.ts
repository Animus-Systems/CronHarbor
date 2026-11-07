import { describe, expect, it } from "vitest";
import {
  operationFinished,
  operationSucceeded,
  retryDelay,
} from "../src/operations";
describe("durable operation presentation", () => {
  it("never treats acceptance or a reconnect as completion", () => {
    for (const state of ["ACCEPTED", "RUNNING", "pending", "starting"])
      expect(operationFinished(state)).toBe(false);
    for (const state of ["SUCCEEDED", "failed", "rejected", "cancelled"])
      expect(operationFinished(state)).toBe(true);
    expect(operationSucceeded("accepted")).toBe(false);
    expect(operationSucceeded("SUCCEEDED")).toBe(true);
  });
  it("bounds reconnect backoff", () => {
    expect(retryDelay(0)).toBe(1000);
    expect(retryDelay(8)).toBe(30000);
  });
});
