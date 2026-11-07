import { describe, expect, it } from "vitest";
import { permitted } from "../src/permissions";
import type { PrincipalDto } from "../src/generated/contracts";
function identity(overrides: Partial<PrincipalDto> = {}): PrincipalDto {
  return {
    actor_id: "test",
    role: "admin",
    namespaces: ["default"],
    capabilities: [],
    ...overrides,
  };
}
describe("permission presentation mirrors daemon policy", () => {
  it("requires explicit grants even for admins", () => {
    expect(permitted(identity(), "workload.read")).toBe(true);
    expect(permitted(identity(), "tokens.manage")).toBe(false);
    expect(permitted(identity({ capabilities: ["*"] }), "tokens.manage")).toBe(
      true,
    );
    expect(
      permitted(
        identity({ role: "viewer", capabilities: ["*"] }),
        "workload.run_preapproved",
      ),
    ).toBe(false);
  });
  it("respects target-specific grants and observation-only reads", () => {
    const principal = identity({
      role: "operator",
      capabilities: ["workload.run_preapproved:job-1"],
    });
    expect(permitted(principal, "workload.run_preapproved", "job-1")).toBe(
      true,
    );
    expect(permitted(principal, "workload.run_preapproved", "job-2")).toBe(
      false,
    );
    expect(permitted(identity({ role: "observe" }), "workload.read")).toBe(
      false,
    );
    expect(
      permitted(
        identity({ role: "observe", capabilities: ["workload.read"] }),
        "workload.read",
      ),
    ).toBe(true);
  });
});
