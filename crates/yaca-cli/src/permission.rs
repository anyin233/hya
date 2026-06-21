use std::path::{Component, Path, PathBuf};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use yaca_tool::{Action, AskRequest, Decision, Resource};

#[derive(Clone, Debug)]
pub enum PermissionPolicy {
    ReadOnly,
    Scoped { workdir: PathBuf },
    Yolo,
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| p.to_path_buf(), |cwd| cwd.join(p))
    }
}

/// Resolve symlinks on the deepest existing ancestor, then re-attach the
/// not-yet-existing tail lexically. This stops a symlinked component (e.g.
/// `workdir/link -> /etc`) from lexically appearing inside the workdir.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => tail.push(name.to_os_string()),
            None => break,
        }
        if !existing.pop() {
            break;
        }
    }
    let mut base = std::fs::canonicalize(&existing).unwrap_or_else(|_| normalize(&existing));
    for name in tail.iter().rev() {
        base.push(name);
    }
    normalize(&base)
}

pub fn path_in_workdir(workdir: &Path, candidate: &str) -> bool {
    let base = absolutize(workdir);
    let cand = Path::new(candidate);
    let joined = if cand.is_absolute() {
        cand.to_path_buf()
    } else {
        base.join(cand)
    };
    resolve(&joined).starts_with(resolve(&base))
}

pub fn decide(policy: &PermissionPolicy, action: Action, resource: &Resource) -> Decision {
    match policy {
        PermissionPolicy::Yolo => Decision::AllowOnce,
        PermissionPolicy::ReadOnly => Decision::Reject { feedback: None },
        PermissionPolicy::Scoped { workdir } => match (action, resource) {
            (Action::Bash, _) => Decision::AllowOnce,
            (Action::Edit, Resource::Path(p)) => {
                if path_in_workdir(workdir, p) {
                    Decision::AllowOnce
                } else {
                    Decision::Reject { feedback: None }
                }
            }
            _ => Decision::AllowOnce,
        },
    }
}

pub fn spawn_auto_responder(
    mut asks: mpsc::UnboundedReceiver<AskRequest>,
    policy: PermissionPolicy,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(req) = asks.recv().await {
            let decision = decide(&policy, req.action, &req.resource);
            let _ = req.reply.send(decision);
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tempdir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("yaca-perm-{nanos}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wd() -> PathBuf {
        PathBuf::from("/home/u/proj")
    }

    #[test]
    fn path_in_workdir_inside_and_outside() {
        let w = wd();
        assert!(path_in_workdir(&w, "src/main.rs"));
        assert!(path_in_workdir(&w, "/home/u/proj/a.txt"));
        assert!(path_in_workdir(&w, "./x"));
        assert!(!path_in_workdir(&w, "../other/x"));
        assert!(!path_in_workdir(&w, "/etc/passwd"));
        assert!(!path_in_workdir(&w, "src/../../escape"));
        assert!(!path_in_workdir(&w, "/home/u/proj2/x"));
    }

    #[test]
    fn scoped_allows_in_dir_edit_and_any_bash_rejects_out_dir_edit() {
        let p = PermissionPolicy::Scoped { workdir: wd() };
        assert_eq!(
            decide(&p, Action::Edit, &Resource::Path("src/a.rs".into())),
            Decision::AllowOnce
        );
        assert_eq!(
            decide(&p, Action::Edit, &Resource::Path("/etc/passwd".into())),
            Decision::Reject { feedback: None }
        );
        assert_eq!(
            decide(&p, Action::Bash, &Resource::Command("cargo test".into())),
            Decision::AllowOnce
        );
    }

    #[test]
    fn readonly_rejects_mutations() {
        let p = PermissionPolicy::ReadOnly;
        assert_eq!(
            decide(&p, Action::Edit, &Resource::Path("src/a.rs".into())),
            Decision::Reject { feedback: None }
        );
        assert_eq!(
            decide(&p, Action::Bash, &Resource::Command("ls".into())),
            Decision::Reject { feedback: None }
        );
    }

    #[test]
    fn yolo_allows_everything() {
        let p = PermissionPolicy::Yolo;
        assert_eq!(
            decide(&p, Action::Edit, &Resource::Path("/etc/passwd".into())),
            Decision::AllowOnce
        );
        assert_eq!(
            decide(&p, Action::Bash, &Resource::Command("rm -rf /".into())),
            Decision::AllowOnce
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cannot_escape_workdir() {
        let tmp = tempdir();
        let workdir = tmp.join("wd");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, workdir.join("link")).unwrap();
        assert!(!path_in_workdir(&workdir, "link/secret.txt"));
        assert!(path_in_workdir(&workdir, "real.txt"));
    }
}
