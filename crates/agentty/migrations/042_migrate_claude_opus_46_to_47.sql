-- Consolidate legacy Claude Opus usage rows into `claude-opus-4-7` before
-- renaming models in place. This avoids violating the `(session_id, model)`
-- uniqueness constraint when one session already has both legacy and current
-- Opus usage rows.
INSERT INTO session_usage (session_id, model, created_at, input_tokens, invocation_count, output_tokens)
SELECT
    session_id,
    'claude-opus-4-7',
    MIN(created_at),
    SUM(input_tokens),
    SUM(invocation_count),
    SUM(output_tokens)
FROM session_usage
WHERE session_id IS NOT NULL
  AND model IN ('claude-opus-4-7', 'claude-opus-4-6')
GROUP BY session_id
HAVING SUM(CASE WHEN model = 'claude-opus-4-6' THEN 1 ELSE 0 END) > 0
ON CONFLICT(session_id, model) DO UPDATE SET
    created_at = excluded.created_at,
    input_tokens = excluded.input_tokens,
    invocation_count = excluded.invocation_count,
    output_tokens = excluded.output_tokens;

DELETE FROM session_usage
WHERE session_id IS NOT NULL
  AND model = 'claude-opus-4-6';

UPDATE session
SET model = 'claude-opus-4-7'
WHERE model = 'claude-opus-4-6';

UPDATE session_usage
SET model = 'claude-opus-4-7'
WHERE session_id IS NULL
  AND model = 'claude-opus-4-6';

UPDATE setting
SET value = 'claude-opus-4-7'
WHERE name IN ('DefaultSmartModel', 'DefaultFastModel', 'DefaultReviewModel')
  AND value = 'claude-opus-4-6';

UPDATE project_setting
SET value = 'claude-opus-4-7'
WHERE name IN ('DefaultSmartModel', 'DefaultFastModel', 'DefaultReviewModel')
  AND value = 'claude-opus-4-6';
