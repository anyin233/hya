use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use crate::formatter_catalog::CheckKind;
use crate::formatter_definition::FormatterDefinition;

#[derive(Clone, Debug)]
pub(crate) struct FormatterCommand {
    pub(crate) argv: Vec<String>,
    pub(crate) environment: BTreeMap<String, String>,
}

pub(crate) fn builtin_enabled(kind: CheckKind, workdir: &Path) -> bool {
    builtin_command(kind, workdir).is_some()
}

pub(crate) fn builtin_environment(kind: CheckKind) -> BTreeMap<String, String> {
    match kind {
        CheckKind::Prettier | CheckKind::Oxfmt | CheckKind::Biome => {
            BTreeMap::from([("BUN_BE_BUN".to_string(), "1".to_string())])
        }
        CheckKind::Command(_)
        | CheckKind::Clang
        | CheckKind::Ruff
        | CheckKind::Air
        | CheckKind::Uv
        | CheckKind::Ocamlformat
        | CheckKind::Pint => BTreeMap::new(),
    }
}

pub(crate) fn command_for_definition(
    item: &FormatterDefinition,
    workdir: &Path,
) -> Option<FormatterCommand> {
    let argv = match &item.command {
        Some(command) if !command.is_empty() => command.clone(),
        Some(_command) => return None,
        None => builtin_command(item.check?, workdir)?,
    };
    Some(FormatterCommand {
        argv,
        environment: item.environment.clone(),
    })
}

fn builtin_command(kind: CheckKind, workdir: &Path) -> Option<Vec<String>> {
    match kind {
        CheckKind::Command(command) => command_formatter(command, workdir),
        CheckKind::Prettier => prettier_command(workdir),
        CheckKind::Oxfmt => None,
        CheckKind::Biome => biome_command(workdir),
        CheckKind::Clang => clang_command(workdir),
        CheckKind::Ruff => ruff_command(workdir),
        CheckKind::Uv => {
            if uv_enabled(workdir) {
                uv_command(workdir)
            } else {
                None
            }
        }
        CheckKind::Air => air_command(workdir),
        CheckKind::Ocamlformat => ocamlformat_command(workdir),
        CheckKind::Pint => pint_command(workdir),
    }
}

fn prettier_command(workdir: &Path) -> Option<Vec<String>> {
    if !package_mentions(workdir, "prettier") {
        return None;
    }
    Some(vec![
        find_command("prettier", workdir)?,
        "--write".into(),
        "$FILE".into(),
    ])
}

fn biome_command(workdir: &Path) -> Option<Vec<String>> {
    find_up(workdir, &["biome.json", "biome.jsonc"])?;
    Some(vec![
        find_command("biome", workdir)?,
        "format".into(),
        "--write".into(),
        "$FILE".into(),
    ])
}

fn clang_command(workdir: &Path) -> Option<Vec<String>> {
    find_up(workdir, &[".clang-format"])?;
    Some(vec![
        find_command("clang-format", workdir)?,
        "-i".into(),
        "$FILE".into(),
    ])
}

fn ocamlformat_command(workdir: &Path) -> Option<Vec<String>> {
    find_up(workdir, &[".ocamlformat"])?;
    Some(vec![
        find_command("ocamlformat", workdir)?,
        "-i".into(),
        "$FILE".into(),
    ])
}

fn pint_command(workdir: &Path) -> Option<Vec<String>> {
    file_mentions(workdir, "composer.json", "laravel/pint")
        .then(|| vec!["./vendor/bin/pint".into(), "$FILE".into()])
}

fn command_formatter(command: &str, workdir: &Path) -> Option<Vec<String>> {
    let binary = find_command(command, workdir)?;
    let command = match command {
        "gofmt" => vec![binary, "-w".into(), "$FILE".into()],
        "mix" => vec![binary, "format".into(), "$FILE".into()],
        "zig" => vec![binary, "fmt".into(), "$FILE".into()],
        "ktlint" => vec![binary, "-F".into(), "$FILE".into()],
        "rubocop" => vec![binary, "--autocorrect".into(), "$FILE".into()],
        "standardrb" => vec![binary, "--fix".into(), "$FILE".into()],
        "dart" => vec![binary, "format".into(), "$FILE".into()],
        "terraform" => vec![binary, "fmt".into(), "$FILE".into()],
        "latexindent" => vec![binary, "-w".into(), "-s".into(), "$FILE".into()],
        "gleam" => vec![binary, "format".into(), "$FILE".into()],
        "shfmt" => vec![binary, "-w".into(), "$FILE".into()],
        "ormolu" => vec![binary, "-i".into(), "$FILE".into()],
        "cljfmt" => vec![binary, "fix".into(), "--quiet".into(), "$FILE".into()],
        "dfmt" => vec![binary, "-i".into(), "$FILE".into()],
        "htmlbeautifier" | "nixfmt" | "rustfmt" => vec![binary, "$FILE".into()],
        _ => vec![binary, "$FILE".into()],
    };
    Some(command)
}

fn air_command(workdir: &Path) -> Option<Vec<String>> {
    let air = find_command("air", workdir)?;
    let output = std::process::Command::new(&air)
        .arg("--help")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let first_line = text.lines().next().unwrap_or_default();
    if output.status.success()
        && first_line.contains("R language")
        && first_line.contains("formatter")
    {
        Some(vec![air, "format".into(), "$FILE".into()])
    } else {
        None
    }
}

fn ruff_command(workdir: &Path) -> Option<Vec<String>> {
    if !ruff_enabled(workdir) {
        return None;
    }
    Some(vec![
        find_command("ruff", workdir)?,
        "format".into(),
        "$FILE".into(),
    ])
}

fn uv_command(workdir: &Path) -> Option<Vec<String>> {
    Some(vec![
        find_command("uv", workdir)?,
        "format".into(),
        "--".into(),
        "$FILE".into(),
    ])
}

fn uv_enabled(workdir: &Path) -> bool {
    if ruff_enabled(workdir) {
        return false;
    }
    let Some(uv) = find_command("uv", workdir) else {
        return false;
    };
    std::process::Command::new(uv)
        .args(["format", "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn ruff_enabled(workdir: &Path) -> bool {
    if find_command("ruff", workdir).is_none() {
        return false;
    }
    if file_mentions(workdir, "pyproject.toml", "[tool.ruff]")
        || find_up(workdir, &["ruff.toml", ".ruff.toml"]).is_some()
    {
        return true;
    }
    ["requirements.txt", "pyproject.toml", "Pipfile"]
        .iter()
        .any(|file| file_mentions(workdir, file, "ruff"))
}

fn package_mentions(workdir: &Path, package: &str) -> bool {
    file_mentions(workdir, "package.json", package)
}

fn file_mentions(workdir: &Path, name: &str, needle: &str) -> bool {
    find_up(workdir, &[name])
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|content| content.contains(needle))
}

fn find_up(start: &Path, names: &[&str]) -> Option<std::path::PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        for name in names {
            let path = dir.join(name);
            if path.exists() {
                return Some(path);
            }
        }
        current = dir.parent();
    }
    None
}

fn find_command(command: &str, workdir: &Path) -> Option<String> {
    let command_path = Path::new(command);
    if command.contains('/') {
        let path = if command_path.is_absolute() {
            command_path.to_path_buf()
        } else {
            workdir.join(command_path)
        };
        return path.exists().then(|| command.to_string());
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(command))
        .find(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
}
