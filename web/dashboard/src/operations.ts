export function operationFinished(state: string): boolean {
  return [
    "succeeded",
    "completed",
    "failed",
    "rejected",
    "cancelled",
    "canceled",
  ].includes(state.toLowerCase());
}
export function operationSucceeded(state: string): boolean {
  return ["succeeded", "completed"].includes(state.toLowerCase());
}
export function retryDelay(attempt: number): number {
  return Math.min(30000, 1000 * 2 ** Math.min(attempt, 5));
}
