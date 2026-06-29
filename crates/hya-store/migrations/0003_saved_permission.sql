CREATE TABLE saved_permission (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    action     TEXT NOT NULL,
    resource   TEXT NOT NULL,
    UNIQUE(project_id, action, resource)
);

CREATE INDEX saved_permission_project ON saved_permission(project_id);
