ALTER TABLE session ADD COLUMN review_diff_hash TEXT;

UPDATE session
SET review_diff_hash = focused_review_diff_hash
WHERE focused_review_diff_hash IS NOT NULL;
