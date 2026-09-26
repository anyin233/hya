//! Project-root path boundary shared by the builtin file tools (ADR-0026).
//!
//! A session may touch files inside any of its Project's roots without an
//! `ExternalDirectory` ask. [`ProjectScope`] answers "is this path inside?"
//! after resolving symlinks, so a link inside a root that points elsewhere is
//! judged by where it lands, and a link elsewhere that points into a root is
//! inside.

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::permission::{Action, PermissionError, PermissionPlane, Resource};
use crate::tool::ToolCtx;

/// Maximum number of dangling symlinks followed while judging one path.
const MAX_SYMLINK_HOPS: usize = 40;

/// The set of directories a session may touch without asking.
///
/// Built from a call's [`ToolCtx::roots`] (or its workdir when a hand-built
/// context carries no roots). Roots are canonicalized once at construction;
/// a root that cannot be resolved keeps its lexical absolute form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectScope {
    workdir: PathBuf,
    roots: Vec<PathBuf>,
    lexical_roots: Vec<PathBuf>,
}

impl ProjectScope {
    /// Build a scope from a workdir and ordered roots.
    ///
    /// # Parameters
    /// - `workdir`: Directory relative paths resolve against; a relative
    ///   workdir resolves against the process working directory.
    /// - `roots`: Workspace roots; when empty, the workdir is the only root.
    ///
    /// # Returns
    /// A scope whose roots are canonical where they exist.
    #[must_use]
    pub fn new(workdir: &Path, roots: &[PathBuf]) -> Self {
        let workdir = lexical_absolute(&process_absolute(workdir));
        let roots = if roots.is_empty() {
            std::slice::from_ref(&workdir)
        } else {
            roots
        };
        let lexical_roots: Vec<PathBuf> = roots
            .iter()
            .map(|root| lexical_absolute(&workdir.join(root)))
            .collect();
        let roots = lexical_roots
            .iter()
            .map(|root| resolve(root, MAX_SYMLINK_HOPS).unwrap_or_else(|_| root.clone()))
            .collect();
        Self {
            workdir,
            roots,
            lexical_roots,
        }
    }

    /// Build the scope of one tool call from its context.
    #[must_use]
    pub fn for_ctx(ctx: &ToolCtx) -> Self {
        Self::new(&ctx.workdir, &ctx.roots)
    }

    /// Resolved roots, in the order they were given.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Whether `path` lies inside any root once symlinks are resolved.
    ///
    /// A relative `path` resolves against the workdir. A path that does not
    /// exist yet is judged by its nearest existing ancestor with the missing
    /// remainder re-appended; a `..` in that remainder or an unreadable
    /// ancestor makes the path outside. A symlink loop names no file at all,
    /// so it is judged lexically and left to the tool to report.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        let candidate = self.workdir.join(path);
        match resolve(&candidate, MAX_SYMLINK_HOPS) {
            Ok(resolved) => self.roots.iter().any(|root| resolved.starts_with(root)),
            Err(Unresolved::Loop) => {
                let lexical = lexical_absolute(&candidate);
                self.roots
                    .iter()
                    .chain(&self.lexical_roots)
                    .any(|root| lexical.starts_with(root))
            }
            Err(Unresolved::Other) => false,
        }
    }

    /// The `ExternalDirectory` resource for a file path: the canonical
    /// directory the file lives in, followed by `/*`.
    ///
    /// The path is resolved like [`Self::contains`] (symlinks followed, a
    /// missing tail re-appended to its nearest existing ancestor) before its
    /// parent is taken, so a file reached through a symlinked directory, or a
    /// file that is itself a symlink, names the directory it really lives in.
    /// A path that cannot be resolved falls back to its lexical form.
    #[must_use]
    pub fn outside_dir_pattern(&self, path: &Path) -> String {
        let resolved = self.canonical(path);
        let parent = resolved
            .parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
        display(&parent.join("*"))
    }

    /// The `ExternalDirectory` resource for a directory path: the canonical
    /// directory itself followed by `/*`.
    #[must_use]
    pub fn outside_directory_pattern(&self, directory: &Path) -> String {
        display(&self.canonical(directory).join("*"))
    }

    /// `path` resolved against the workdir with symlinks followed, or its
    /// lexical absolute form when it cannot be resolved.
    fn canonical(&self, path: &Path) -> PathBuf {
        let candidate = self.workdir.join(path);
        resolve(&candidate, MAX_SYMLINK_HOPS).unwrap_or_else(|_| lexical_absolute(&candidate))
    }

    /// Ask `ExternalDirectory` for `pattern` unless `path` is inside the scope.
    ///
    /// # Errors
    /// Returns the permission plane's denial or unavailability.
    pub async fn authorize(
        &self,
        permission: &PermissionPlane,
        path: &Path,
        pattern: impl FnOnce(&Self) -> String,
    ) -> Result<(), PermissionError> {
        if self.contains(path) {
            return Ok(());
        }
        permission
            .assert(Action::ExternalDirectory, Resource::Path(pattern(self)))
            .await
    }
}

/// Why a path could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unresolved {
    /// A symlink loop (or too many dangling hops): the path names no file.
    Loop,
    /// Anything else, such as permission denied.
    Other,
}

/// Resolve `path` to a canonical absolute path, following symlinks.
///
/// Missing trailing components are re-appended to the canonical nearest
/// existing ancestor.
fn resolve(path: &Path, hops: usize) -> Result<PathBuf, Unresolved> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(error) if is_missing(error.kind()) => resolve_missing(path, hops),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => Err(Unresolved::Loop),
        Err(_) => Err(Unresolved::Other),
    }
}

/// Resolve a path whose canonicalization reported a missing component.
fn resolve_missing(path: &Path, hops: usize) -> Result<PathBuf, Unresolved> {
    let Some(Component::Normal(name)) = path.components().next_back() else {
        return Err(Unresolved::Other);
    };
    let parent = path.parent().ok_or(Unresolved::Other)?;
    match std::fs::symlink_metadata(path) {
        // A dangling symlink: judge where it would land, not where it sits.
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let hops = hops.checked_sub(1).ok_or(Unresolved::Loop)?;
            let target = std::fs::read_link(path).map_err(|_| Unresolved::Other)?;
            let base = resolve(parent, hops)?;
            resolve(&base.join(target), hops)
        }
        Ok(_) => Err(Unresolved::Other),
        Err(error) if is_missing(error.kind()) => Ok(resolve(parent, hops)?.join(name)),
        Err(_) => Err(Unresolved::Other),
    }
}

fn is_missing(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::NotFound | ErrorKind::NotADirectory)
}

fn process_absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    }
}

/// Lexically drop `.` and fold `..`, never above the root.
fn lexical_absolute(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tempdir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-scope-{}-{}-{id}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn scope(workdir: &Path, roots: &[&Path]) -> ProjectScope {
        let roots: Vec<PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
        ProjectScope::new(workdir, &roots)
    }

    #[test]
    fn file_in_any_root_is_inside_and_other_paths_are_outside() {
        let one = tempdir();
        let two = tempdir();
        let other = tempdir();
        std::fs::write(two.join("b.txt"), "b").unwrap();
        let scope = scope(&one, &[&one, &two]);

        assert!(scope.contains(&two.join("b.txt")));
        assert!(scope.contains(&one));
        assert!(!scope.contains(&other.join("c.txt")));
    }

    #[test]
    fn symlink_inside_a_root_pointing_outside_is_outside() {
        let root = tempdir();
        let outside = tempdir();
        std::fs::write(outside.join("secret"), "s").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        symlink(outside.join("secret"), root.join("file-link")).unwrap();
        let scope = scope(&root, &[&root]);

        assert!(!scope.contains(&root.join("link/secret")));
        assert!(!scope.contains(&root.join("file-link")));
        assert!(!scope.contains(&root.join("link/new-file")));
    }

    #[test]
    fn dangling_symlink_inside_a_root_is_judged_by_its_target() {
        let root = tempdir();
        let outside = tempdir();
        symlink(outside.join("missing"), root.join("dangling-out")).unwrap();
        symlink(root.join("missing"), root.join("dangling-in")).unwrap();
        let scope = scope(&root, &[&root]);

        assert!(!scope.contains(&root.join("dangling-out")));
        assert!(scope.contains(&root.join("dangling-in")));
    }

    #[test]
    fn symlink_outside_pointing_into_a_root_is_inside() {
        let root = tempdir();
        let outside = tempdir();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        symlink(&root, outside.join("into-root")).unwrap();
        let scope = scope(&root, &[&root]);

        assert!(scope.contains(&outside.join("into-root/a.txt")));
    }

    #[test]
    fn parent_traversal_out_of_a_root_is_outside() {
        let parent = tempdir();
        let root = parent.join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(parent.join("sibling"), "s").unwrap();
        let scope = scope(&root, &[&root]);

        assert!(!scope.contains(Path::new("../sibling")));
        assert!(!scope.contains(&root.join("sub/../../sibling")));
        assert!(scope.contains(&root.join("sub/../x")));
        assert!(!scope.contains(&root.join("missing/../../sibling")));
    }

    #[test]
    fn missing_file_in_a_root_is_inside() {
        let root = tempdir();
        let scope = scope(&root, &[&root]);

        assert!(scope.contains(&root.join("new/dir/file.rs")));
        assert!(scope.contains(Path::new("new.rs")));
    }

    #[test]
    fn nested_roots_are_both_inside() {
        let root = tempdir();
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let scope = scope(&nested, &[&nested, &root]);

        assert!(scope.contains(&root.join("top.txt")));
        assert!(scope.contains(&nested.join("deep.txt")));
    }

    #[test]
    fn containment_is_component_wise() {
        let parent = tempdir();
        let root = parent.join("b");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(parent.join("bc")).unwrap();
        let scope = scope(&root, &[&root]);

        assert!(!scope.contains(&parent.join("bc/file")));
        assert!(scope.contains(&root.join("file")));
    }

    #[test]
    fn root_that_is_a_symlink_is_resolved() {
        let real = tempdir();
        let holder = tempdir();
        let linked_root = holder.join("root-link");
        symlink(&real, &linked_root).unwrap();
        let scope = scope(&linked_root, &[&linked_root]);

        assert!(scope.contains(&real.join("a.txt")));
        assert!(scope.contains(&linked_root.join("a.txt")));
        assert!(scope.contains(Path::new("a.txt")));
    }

    #[test]
    fn relative_paths_resolve_against_the_workdir() {
        let one = tempdir();
        let two = tempdir();
        let scope = scope(&two, &[&one, &two]);

        assert!(scope.contains(Path::new("sub/file.txt")));
        assert!(!scope.contains(Path::new("../elsewhere")));
    }

    #[test]
    fn non_canonical_temp_spelling_matches_its_canonical_form() {
        // On macOS the temp dir is `/var/folders/...`, a symlink to
        // `/private/var/folders/...`. Either spelling of a root or a path
        // must meet the other.
        let canonical = tempdir();
        let raw_temp = std::env::temp_dir();
        let name = canonical.file_name().unwrap();
        let raw = raw_temp.join(name);

        let raw_root = scope(&raw, &[&raw]);
        assert!(raw_root.contains(&canonical.join("x")));
        let canonical_root = scope(&canonical, &[&canonical]);
        assert!(canonical_root.contains(&raw.join("x")));
    }

    #[test]
    fn missing_root_keeps_its_lexical_form() {
        let parent = tempdir();
        let missing = parent.join("not-yet");
        let scope = scope(&parent, &[&missing]);

        assert!(scope.contains(&missing.join("file")));
        assert!(!scope.contains(&parent.join("file")));
    }

    #[test]
    fn empty_roots_fall_back_to_the_workdir() {
        let root = tempdir();
        let scope = ProjectScope::new(&root, &[]);

        assert!(scope.contains(&root.join("x")));
        assert_eq!(scope.roots(), [root]);
    }

    #[test]
    fn unreadable_ancestor_is_outside() {
        let root = tempdir();
        let locked = root.join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        let scope = scope(&root, &[&root]);
        let target = locked.join("inner/file");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = std::fs::metadata(locked.join("inner")).is_err();
        let inside = scope.contains(&target);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root bypasses the permission bits; nothing to check then.
        if blocked {
            assert!(!inside);
        }
    }

    #[test]
    fn symlink_loop_is_judged_lexically() {
        let root = tempdir();
        let outside = tempdir();
        symlink("second", root.join("first")).unwrap();
        symlink("first", root.join("second")).unwrap();
        symlink("b", outside.join("a")).unwrap();
        symlink("a", outside.join("b")).unwrap();
        let scope = scope(&root, &[&root]);

        assert!(scope.contains(&root.join("first")));
        assert!(!scope.contains(&outside.join("a")));
    }

    #[test]
    fn outside_patterns_name_the_concrete_directory() {
        let root = tempdir();
        let outside = tempdir();
        std::fs::write(outside.join("hosts"), "h").unwrap();
        let scope = scope(&root, &[&root]);
        let expected = format!("{}/*", display(&outside));

        assert_eq!(scope.outside_dir_pattern(&outside.join("hosts")), expected);
        assert_eq!(
            scope.outside_dir_pattern(&outside.join("new.txt")),
            expected
        );
        assert_eq!(scope.outside_directory_pattern(&outside), expected);
        assert_eq!(scope.outside_dir_pattern(Path::new("/")), "/*".to_string());
    }

    #[test]
    fn outside_patterns_name_the_canonical_directory() {
        let root = tempdir();
        let real = tempdir();
        let holder = tempdir();
        let secret = tempdir();
        std::fs::write(real.join("a.txt"), "a").unwrap();
        std::fs::write(secret.join("id"), "k").unwrap();
        symlink(&real, holder.join("link")).unwrap();
        symlink(secret.join("id"), holder.join("notes.txt")).unwrap();
        let scope = scope(&root, &[&root]);
        let real_pattern = format!("{}/*", display(&real));

        // A directory reached through a symlink names where it lands.
        assert_eq!(
            scope.outside_dir_pattern(&holder.join("link/a.txt")),
            real_pattern
        );
        assert_eq!(
            scope.outside_dir_pattern(&holder.join("link/missing/new.txt")),
            format!("{}/missing/*", display(&real))
        );
        assert_eq!(
            scope.outside_directory_pattern(&holder.join("link")),
            real_pattern
        );
        // A file that is itself a symlink names its target's directory.
        assert_eq!(
            scope.outside_dir_pattern(&holder.join("notes.txt")),
            format!("{}/*", display(&secret))
        );
        // `..` through a symlink is judged physically.
        assert_eq!(
            scope.outside_directory_pattern(&holder.join("link/..")),
            format!("{}/*", display(real.parent().unwrap()))
        );
    }
}
