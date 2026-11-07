CREATE INDEX audit_auth_rejections ON audit_events(seq) WHERE action='auth.reject';
-- Only anonymous rejection samples expire; privileged audit history is preserved.
DELETE FROM audit_events WHERE action='auth.reject' AND seq < COALESCE(
  (SELECT seq FROM audit_events WHERE action='auth.reject' ORDER BY seq DESC LIMIT 1 OFFSET 999), 0
);
