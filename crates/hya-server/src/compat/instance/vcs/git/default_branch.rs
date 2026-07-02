use std::path::Path;

use super::output;

pub(super) fn get(workdir: &Path) -> Option<String> {
    remote(workdir).or_else(|| local(workdir))
}

fn remote(workdir: &Path) -> Option<String> {
    let remote = primary_remote(workdir)?;
    let ref_name = format!("refs/remotes/{remote}/HEAD");
    let branch = output(workdir, &["symbolic-ref", "--short", &ref_name])?;
    let prefix = format!("{remote}/");
    Some(branch.strip_prefix(&prefix).unwrap_or(&branch).to_string())
}

fn primary_remote(workdir: &Path) -> Option<String> {
    let remotes = output(workdir, &["remote"])?;
    let list = remotes.lines().collect::<Vec<_>>();
    if list.contains(&"origin") {
        return Some("origin".to_string());
    }
    if list.len() == 1 {
        return list.first().map(|remote| (*remote).to_string());
    }
    if list.contains(&"upstream") {
        return Some("upstream".to_string());
    }
    list.first().map(|remote| (*remote).to_string())
}

fn local(workdir: &Path) -> Option<String> {
    let refs = output(
        workdir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    let list = refs.lines().collect::<Vec<_>>();
    if let Some(name) = output(workdir, &["config", "init.defaultBranch"])
        && list.contains(&name.as_str())
    {
        return Some(name);
    }
    for name in ["main", "master"] {
        if list.contains(&name) {
            return Some(name.to_string());
        }
    }
    None
}
