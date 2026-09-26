-- Content-addressed file contents kept for session revert (`FilesChanged`,
-- `SessionReverted`). Scoped per session so `delete_session` removes them
-- and a session's total is cheap to cap. `hash` is the lowercase hex sha256
-- of `content`; events carry only the hash.
CREATE TABLE file_blob (
    session_id BLOB NOT NULL,
    hash       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    content    BLOB NOT NULL,
    PRIMARY KEY (session_id, hash)
) WITHOUT ROWID;
