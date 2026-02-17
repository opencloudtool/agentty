UPDATE session
SET summary = output
WHERE summary IS NULL
  AND status IN ('Done', 'Canceled');
