export function time(value: number | null | undefined, utc = false): string {
  if (value == null) return "—";
  const date = new Date(value / 1000);
  if (Number.isNaN(date.getTime())) return "Unavailable";
  return utc
    ? date.toISOString().replace("T", " ").replace(".000Z", " UTC")
    : new Intl.DateTimeFormat(undefined, {
        dateStyle: "medium",
        timeStyle: "medium",
      }).format(date);
}
export function shortId(value: string): string {
  return value.length > 22 ? `${value.slice(0, 10)}…${value.slice(-8)}` : value;
}
export function isService(kind: string): boolean {
  return ["service", "managed_service"].includes(kind.toLowerCase());
}
export function tone(state: string): string {
  const value = state.toLowerCase();
  if (["unknown", "uncertain", "unhealthy"].includes(value)) return "warning";
  if (
    ["failed", "timed_out", "rejected", "error", "unavailable"].includes(value)
  )
    return "danger";
  if (
    [
      "succeeded",
      "passed",
      "completed",
      "ready",
      "running",
      "enabled",
      "connected",
    ].includes(value)
  )
    return "success";
  if (["accepted", "starting", "stopping", "pending", "paused"].includes(value))
    return "info";
  return "neutral";
}
