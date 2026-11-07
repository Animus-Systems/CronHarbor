import type { PrincipalDto } from "./generated/contracts";
export function permitted(
  principal: PrincipalDto | null,
  capability: string,
  target?: string,
): boolean {
  if (!principal) return false;
  const { role, capabilities } = principal;
  const read = ["workload.read", "execution.read"].includes(capability);
  const allowed =
    read ||
    capability === "logs.read" ||
    (["workload.run_preapproved", "service.control_preapproved"].includes(
      capability,
    ) &&
      ["operator", "editor", "admin"].includes(role)) ||
    ([
      "workload.edit_safe_config",
      "executor.create_command",
      "docker.manage",
      "systemd.manage",
    ].includes(capability) &&
      ["editor", "admin"].includes(role)) ||
    (["tokens.manage", "operations.manage"].includes(capability) &&
      role === "admin");
  return (
    allowed &&
    ((read && role !== "observe") ||
      capabilities.includes("*") ||
      capabilities.includes(capability) ||
      Boolean(target && capabilities.includes(`${capability}:${target}`)))
  );
}
