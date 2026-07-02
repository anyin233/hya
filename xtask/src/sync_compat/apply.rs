use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::sync_compat::config_merge::{existing_mcp_names, merge_managed_mcp, remove_managed_mcp};
use crate::sync_compat::discover::{McpCandidate, SkillCandidate};

#[derive(Debug, Serialize, Deserialize)]
struct Lockfile {
    version: u32,
    mcp_ids: Vec<String>,
    skills: Vec<LockfileSkill>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LockfileSkill {
    name: String,
    target: String,
}

pub(crate) fn apply(
    hya_config_path: &Path,
    hya_skills_root: &Path,
    mcp: &[McpCandidate],
    skills: &[SkillCandidate],
) -> Result<()> {
    std::fs::create_dir_all(hya_skills_root)
        .with_context(|| format!("create skill root {}", hya_skills_root.display()))?;

    let mut lockfile_skills = Vec::new();
    for skill in skills {
        let link_path = hya_skills_root.join(&skill.name);
        match link_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::remove_file(&link_path)
                    .with_context(|| format!("remove existing symlink {}", link_path.display()))?;
            }
            Ok(_) => {
                // A real file/dir under this name is a user-authored skill; never overwrite it.
                eprintln!(
                    "skill conflict: {} already exists and is not a managed symlink; skipping",
                    link_path.display()
                );
                continue;
            }
            Err(_) => {}
        }
        symlink_dir(&skill.dir, &link_path)?;
        lockfile_skills.push(LockfileSkill {
            name: skill.name.clone(),
            target: skill.dir.display().to_string(),
        });
    }

    let current = if hya_config_path.exists() {
        std::fs::read_to_string(hya_config_path)
            .with_context(|| format!("read hya config {}", hya_config_path.display()))?
    } else {
        String::new()
    };
    let prior_lockfile = read_lockfile(hya_config_path).ok();
    let existing_names = existing_mcp_names(&current);
    let mcp_to_manage = mcp
        .iter()
        .filter(|candidate| {
            let name = &candidate.name;
            if let Some(lockfile) = &prior_lockfile
                && lockfile.mcp_ids.iter().any(|managed| managed == name)
            {
                return true;
            }
            !existing_names.iter().any(|existing| existing == name)
        })
        .collect::<Vec<_>>();
    let rendered = merge_managed_mcp(&current, &mcp_to_manage);
    std::fs::write(hya_config_path, rendered)
        .with_context(|| format!("write hya config {}", hya_config_path.display()))?;

    let lockfile = Lockfile {
        version: 1,
        mcp_ids: mcp_to_manage
            .iter()
            .map(|candidate| candidate.name.clone())
            .collect(),
        skills: lockfile_skills,
    };
    let lockfile_path = lockfile_path(hya_config_path)?;
    std::fs::write(
        &lockfile_path,
        serde_json::to_string_pretty(&lockfile).context("serialize lockfile")?,
    )
    .with_context(|| format!("write lockfile {}", lockfile_path.display()))?;

    println!(
        "applied {} mcp entries and {} skills",
        mcp_to_manage.len(),
        skills.len()
    );
    Ok(())
}

pub(crate) fn prune(hya_config_path: &Path, hya_skills_root: &Path) -> Result<()> {
    let lockfile_path = lockfile_path(hya_config_path)?;
    let raw = std::fs::read_to_string(&lockfile_path)
        .with_context(|| format!("read lockfile {}", lockfile_path.display()))?;
    let lockfile: Lockfile = serde_json::from_str(&raw).context("parse lockfile")?;

    for skill in lockfile.skills {
        let path = hya_skills_root.join(skill.name);
        if path.exists() || path.symlink_metadata().is_ok() {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove managed skill {}", path.display()))?;
        }
    }

    let current = if hya_config_path.exists() {
        std::fs::read_to_string(hya_config_path)
            .with_context(|| format!("read hya config {}", hya_config_path.display()))?
    } else {
        String::new()
    };
    let rendered = remove_managed_mcp(&current, &lockfile.mcp_ids);
    std::fs::write(hya_config_path, rendered)
        .with_context(|| format!("write hya config {}", hya_config_path.display()))?;

    std::fs::remove_file(&lockfile_path)
        .with_context(|| format!("remove lockfile {}", lockfile_path.display()))?;
    println!("pruned managed state");
    Ok(())
}

pub(crate) fn print_dry_run(
    hya_config_path: &Path,
    hya_skills_root: &Path,
    mcp: &[McpCandidate],
    skills: &[SkillCandidate],
) {
    println!("dry-run: target config {}", hya_config_path.display());
    println!("dry-run: target skills {}", hya_skills_root.display());
    for candidate in mcp {
        println!("mcp {}: {}", candidate.name, candidate.command.join(" "));
    }
    for skill in skills {
        println!("skill {} <- {}", skill.name, skill.dir.display());
    }
}

fn lockfile_path(hya_config_path: &Path) -> Result<PathBuf> {
    Ok(hya_config_path
        .parent()
        .context("hya config should have a parent directory")?
        .join("compat-sync-lock.json"))
}

fn read_lockfile(hya_config_path: &Path) -> Result<Lockfile> {
    let path = lockfile_path(hya_config_path)?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read lockfile {}", path.display()))?;
    serde_json::from_str(&raw).context("parse lockfile")
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link_path: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link_path).with_context(|| {
        format!(
            "create symlink {} -> {}",
            link_path.display(),
            target.display()
        )
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn symlink_dir(target: &Path, link_path: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(target, link_path).with_context(|| {
        format!(
            "create symlink {} -> {}",
            link_path.display(),
            target.display()
        )
    })?;
    Ok(())
}
