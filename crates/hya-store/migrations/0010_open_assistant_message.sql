-- Write-through index of assistant messages that have started but not
-- finished (or been deleted). Maintained inside the event-append transaction
-- (`materialize.rs`); read only by startup crash recovery, so recovery touches
-- only sessions a dead process left mid-turn instead of replaying every log.
CREATE TABLE open_assistant_message (
    session_id BLOB NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (session_id, message_id)
) WITHOUT ROWID;

-- Backfill from logs written before the index existed. One pass over the
-- message lifecycle rows; the LIKE prefix skips JSON parsing for every other
-- event (serde writes the `type` tag first).
INSERT OR IGNORE INTO open_assistant_message (session_id, message_id)
SELECT session_id, json_extract(payload, '$.message') AS message_id
FROM event_log
WHERE payload LIKE '{"type":"message_%'
GROUP BY session_id, message_id
HAVING SUM(
           json_extract(payload, '$.type') = 'message_started'
           AND json_extract(payload, '$.role') = 'assistant'
       ) > 0
   AND SUM(json_extract(payload, '$.type') IN ('message_finished', 'message_deleted')) = 0;
