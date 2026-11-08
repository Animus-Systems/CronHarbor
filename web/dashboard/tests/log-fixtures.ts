import type { LogStreamDto } from "../src/generated/contracts";

export function logStream(
  attemptId: string,
  end = 0,
  overrides: Partial<LogStreamDto> = {},
): LogStreamDto {
  return {
    attempt_id: attemptId,
    capture_mode: "PREFIX",
    capture_state: "SEALED",
    retained_from_offset: 0,
    written_end_offset: end,
    retained_bytes: end,
    known_discarded_bytes: 0,
    history_evicted: false,
    log_truncated: false,
    last_error_code: null,
    ...overrides,
  };
}
