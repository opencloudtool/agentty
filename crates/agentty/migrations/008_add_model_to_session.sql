ALTER TABLE session ADD COLUMN model TEXT NOT NULL DEFAULT '';

UPDATE session
SET model = CASE agent
    WHEN 'gemini' THEN 'gemini-3-flash-preview'
    WHEN 'codex' THEN 'gpt-5.3-codex'
    WHEN 'claude' THEN 'claude-opus-4-6'
    ELSE 'gemini-3-flash-preview'
END
WHERE model = '';
