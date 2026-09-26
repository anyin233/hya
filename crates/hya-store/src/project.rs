//! Projects (ADR-0024): a named, ordered, non-empty list of absolute
//! workspace roots on the backend machine. Mutable configuration in the
//! `project` / `project_root` tables, managed with CRUD like
//! `saved_permission`; never event-sourced. Sessions name their Project in
//! `session_created`, mirrored in `session.project_id` / `session.kind`.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use hya_proto::{ProjectId, now_millis};
use sqlx::Row;

use crate::{SessionStore, StoreError, decode_session_key};

/// One Project row with its ordered roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// Stable Project id (`prj_…`).
    pub id: ProjectId,
    /// Display name (non-empty).
    pub name: String,
    /// Absolute, normalized, distinct roots in order; `roots[0]` is the
    /// primary root. Never empty.
    pub roots: Vec<String>,
    /// Creation time, unix epoch milliseconds.
    pub created_at_ms: i64,
    /// Last rename / root replacement, unix epoch milliseconds. Strictly
    /// increases on every update.
    pub updated_at_ms: i64,
    /// Archived Projects are hidden from [`SessionStore::list_projects`] and
    /// never match [`SessionStore::resolve_project_by_path`].
    pub archived: bool,
}

/// One [`SessionStore::list_projects`] entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSummary {
    /// The Project.
    pub project: Project,
    /// Root sessions (not subagent sessions) that belong to the Project and
    /// still have an event log, archived ones included.
    pub session_count: u64,
}

/// Normalize one Project root, or a path to resolve against roots: it must be
/// absolute; `.` components, repeated and trailing separators are dropped.
/// A `..` component is rejected rather than resolved, because resolving it
/// lexically can disagree with the filesystem when a symlink precedes it.
/// The path is not canonicalized and need not exist.
///
/// # Errors
/// [`StoreError::ProjectRootNotAbsolute`] for a relative or empty path;
/// [`StoreError::ProjectRootInvalid`] for a `..` component.
pub fn normalize_project_path(path: &str) -> Result<String, StoreError> {
    let raw = Path::new(path);
    if !raw.is_absolute() {
        return Err(StoreError::ProjectRootNotAbsolute {
            path: path.to_string(),
        });
    }
    let mut normalized = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(StoreError::ProjectRootInvalid {
                    path: path.to_string(),
                    reason: "a `..` component is not allowed",
                });
            }
        }
    }
    normalized
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| StoreError::ProjectRootInvalid {
            path: path.to_string(),
            reason: "not valid UTF-8",
        })
}

/// Normalize and de-duplicate an ordered root list (first occurrence wins).
fn normalize_roots(roots: &[String]) -> Result<Vec<String>, StoreError> {
    let mut out: Vec<String> = Vec::with_capacity(roots.len());
    for root in roots {
        let root = normalize_project_path(root)?;
        if !out.contains(&root) {
            out.push(root);
        }
    }
    if out.is_empty() {
        return Err(StoreError::ProjectRootsEmpty);
    }
    Ok(out)
}

fn validate_name(name: &str) -> Result<String, StoreError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(StoreError::ProjectNameEmpty);
    }
    Ok(name.to_string())
}

fn parse_project_id(raw: &str) -> Result<ProjectId, StoreError> {
    ProjectId::from_str(raw)
        .map_err(|error| StoreError::ProjectData(format!("id {raw:?}: {error}")))
}

/// `FROM … WHERE` clause selecting the root sessions of the Project named by
/// the SQL expression `project` that still have an event log.
/// `delete_session` keeps the materialized `session` row, so the log decides
/// whether a session still exists.
fn live_root_sessions(project: &str) -> String {
    format!(
        "FROM session s WHERE s.project_id = {project} AND s.parent_id IS NULL \
         AND EXISTS (SELECT 1 FROM event_log e WHERE e.session_id = s.id)"
    )
}

impl SessionStore {
    /// Create a Project with a name and ordered roots.
    ///
    /// Roots are normalized ([`normalize_project_path`]) and de-duplicated,
    /// keeping the first occurrence; the first root is the primary root.
    ///
    /// # Errors
    /// [`StoreError::ProjectNameEmpty`], [`StoreError::ProjectRootsEmpty`],
    /// [`StoreError::ProjectRootNotAbsolute`], [`StoreError::ProjectRootInvalid`],
    /// or SQLite failures.
    pub async fn create_project(
        &self,
        name: &str,
        roots: &[String],
    ) -> Result<Project, StoreError> {
        let name = validate_name(name)?;
        let roots = normalize_roots(roots)?;
        let id = ProjectId::new();
        let now = now_millis();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO project (id, name, created_at, updated_at, archived) \
             VALUES (?, ?, ?, ?, 0)",
        )
        .bind(id.to_string())
        .bind(&name)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        insert_roots(&mut tx, id, &roots).await?;
        tx.commit().await?;
        Ok(Project {
            id,
            name,
            roots,
            created_at_ms: now,
            updated_at_ms: now,
            archived: false,
        })
    }

    /// Read one Project (archived or not); `None` when it does not exist.
    ///
    /// # Errors
    /// SQLite failures or corrupt rows ([`StoreError::ProjectData`]).
    pub async fn get_project(&self, id: ProjectId) -> Result<Option<Project>, StoreError> {
        let mut conn = self.pool.acquire().await?;
        load_project(&mut conn, id).await
    }

    /// Non-archived Projects, most recently updated first, each with its
    /// root-session count.
    ///
    /// # Errors
    /// SQLite failures or corrupt rows ([`StoreError::ProjectData`]).
    pub async fn list_projects(&self) -> Result<Vec<ProjectSummary>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT p.id, p.name, p.created_at, p.updated_at, p.archived, \
             (SELECT count(*) {}) AS session_count \
             FROM project p WHERE p.archived = 0 \
             ORDER BY p.updated_at DESC, p.id DESC",
            live_root_sessions("p.id")
        ))
        .fetch_all(&self.pool)
        .await?;
        let root_rows = sqlx::query(
            "SELECT r.project_id, r.path FROM project_root r \
             JOIN project p ON p.id = r.project_id WHERE p.archived = 0 \
             ORDER BY r.project_id, r.ord",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut roots: HashMap<String, Vec<String>> = HashMap::new();
        for row in root_rows {
            roots
                .entry(row.try_get("project_id")?)
                .or_default()
                .push(row.try_get("path")?);
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let raw_id: String = row.try_get("id")?;
            let project_roots = roots.remove(&raw_id).unwrap_or_default();
            let session_count: i64 = row.try_get("session_count")?;
            out.push(ProjectSummary {
                project: project_from_row(&row, project_roots)?,
                session_count: session_count.max(0) as u64,
            });
        }
        Ok(out)
    }

    /// Rename a Project; bumps `updated_at`.
    ///
    /// # Errors
    /// [`StoreError::ProjectNameEmpty`], [`StoreError::ProjectNotFound`], or
    /// SQLite failures.
    pub async fn rename_project(&self, id: ProjectId, name: &str) -> Result<Project, StoreError> {
        let name = validate_name(name)?;
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE project SET name = ?, updated_at = max(?, updated_at + 1) WHERE id = ?",
        )
        .bind(&name)
        .bind(now_millis())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::ProjectNotFound { project: id });
        }
        let project = load_project(&mut tx, id)
            .await?
            .ok_or(StoreError::ProjectNotFound { project: id })?;
        tx.commit().await?;
        Ok(project)
    }

    /// Replace a Project's whole root list (validated like
    /// [`SessionStore::create_project`]); bumps `updated_at`. Running sessions
    /// see the new roots from their next turn (ADR-0024).
    ///
    /// # Errors
    /// [`StoreError::ProjectRootsEmpty`], [`StoreError::ProjectRootNotAbsolute`],
    /// [`StoreError::ProjectRootInvalid`], [`StoreError::ProjectNotFound`], or
    /// SQLite failures.
    pub async fn replace_project_roots(
        &self,
        id: ProjectId,
        roots: &[String],
    ) -> Result<Project, StoreError> {
        let roots = normalize_roots(roots)?;
        let mut tx = self.pool.begin().await?;
        let updated =
            sqlx::query("UPDATE project SET updated_at = max(?, updated_at + 1) WHERE id = ?")
                .bind(now_millis())
                .bind(id.to_string())
                .execute(&mut *tx)
                .await?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::ProjectNotFound { project: id });
        }
        sqlx::query("DELETE FROM project_root WHERE project_id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        insert_roots(&mut tx, id, &roots).await?;
        let project = load_project(&mut tx, id)
            .await?
            .ok_or(StoreError::ProjectNotFound { project: id })?;
        tx.commit().await?;
        Ok(project)
    }

    /// Delete a Project and its roots. Returns whether a Project was removed.
    ///
    /// Refused while any non-archived root session that still has an event
    /// log belongs to the Project. Archived and deleted sessions keep the
    /// Project id in their log; after the delete it names no Project.
    ///
    /// # Errors
    /// [`StoreError::ProjectInUse`] or SQLite failures.
    pub async fn delete_project(&self, id: ProjectId) -> Result<bool, StoreError> {
        let keys: Vec<Vec<u8>> =
            sqlx::query_scalar(&format!("SELECT s.id {}", live_root_sessions("?")))
                .bind(id.to_string())
                .fetch_all(&self.pool)
                .await?;
        let mut live = 0_u64;
        for key in keys {
            let Some(session) = decode_session_key(&key) else {
                continue;
            };
            if !self
                .with_projection(session, |projection| projection.session.is_archived())
                .await?
            {
                live += 1;
            }
        }
        if live > 0 {
            return Err(StoreError::ProjectInUse {
                project: id,
                sessions: live,
            });
        }
        let deleted = sqlx::query("DELETE FROM project WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(deleted.rows_affected() > 0)
    }

    /// The non-archived Project whose root contains `path` (ADR-0024 cwd
    /// matching), or `None`.
    ///
    /// Containment is component-wise (`/a/b` contains `/a/b/c` but not
    /// `/a/bc`), after normalizing `path` like a root. When several roots
    /// contain it, the longest root wins; among equally long roots, the most
    /// recently updated Project wins.
    ///
    /// # Errors
    /// [`StoreError::ProjectRootNotAbsolute`] / [`StoreError::ProjectRootInvalid`]
    /// for `path`, SQLite failures, or corrupt rows.
    pub async fn resolve_project_by_path(&self, path: &str) -> Result<Option<Project>, StoreError> {
        let path = normalize_project_path(path)?;
        let path = Path::new(&path);
        let rows = sqlx::query(
            "SELECT r.project_id, r.path, p.updated_at FROM project_root r \
             JOIN project p ON p.id = r.project_id WHERE p.archived = 0",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut best: Option<(usize, i64, String)> = None;
        for row in rows {
            let root: String = row.try_get("path")?;
            let root = Path::new(&root);
            if !path.starts_with(root) {
                continue;
            }
            let candidate = (
                root.components().count(),
                row.try_get::<i64, _>("updated_at")?,
                row.try_get::<String, _>("project_id")?,
            );
            if best.as_ref().is_none_or(|current| candidate > *current) {
                best = Some(candidate);
            }
        }
        let Some((_, _, raw_id)) = best else {
            return Ok(None);
        };
        self.get_project(parse_project_id(&raw_id)?).await
    }
}

async fn insert_roots(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ProjectId,
    roots: &[String],
) -> Result<(), StoreError> {
    for (ord, root) in roots.iter().enumerate() {
        sqlx::query("INSERT INTO project_root (project_id, path, ord) VALUES (?, ?, ?)")
            .bind(id.to_string())
            .bind(root)
            .bind(i64::try_from(ord).unwrap_or(i64::MAX))
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn load_project(
    conn: &mut sqlx::SqliteConnection,
    id: ProjectId,
) -> Result<Option<Project>, StoreError> {
    let key = id.to_string();
    let Some(row) =
        sqlx::query("SELECT id, name, created_at, updated_at, archived FROM project WHERE id = ?")
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(None);
    };
    let roots: Vec<String> =
        sqlx::query_scalar("SELECT path FROM project_root WHERE project_id = ? ORDER BY ord")
            .bind(&key)
            .fetch_all(&mut *conn)
            .await?;
    project_from_row(&row, roots).map(Some)
}

fn project_from_row(
    row: &sqlx::sqlite::SqliteRow,
    roots: Vec<String>,
) -> Result<Project, StoreError> {
    let raw_id: String = row.try_get("id")?;
    let archived: i64 = row.try_get("archived")?;
    Ok(Project {
        id: parse_project_id(&raw_id)?,
        name: row.try_get("name")?,
        roots,
        created_at_ms: row.try_get("created_at")?,
        updated_at_ms: row.try_get("updated_at")?,
        archived: archived != 0,
    })
}
