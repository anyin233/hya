//! Core/bundle boundary guard (ADR-0040), run by `cargo test -p xtask`.
//!
//! Fails when a crate under `crates/` depends on a crate from `bundles/`, or
//! when core source names a bundle domain identifier. Both rules start from
//! the allowlist in `crates/xtask/boundary.toml`; an allowlist entry that no
//! longer matches a violation also fails, so the list only shrinks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use anyhow::{Context as _, bail};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use serde::Deserialize;

/// The guard configuration file, relative to the workspace root.
pub const CONFIG: &str = "crates/xtask/boundary.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    scan: Scan,
    identifiers: Identifiers,
    /// The ring map; absent in configs that only check the bundle boundary.
    rings: Option<crate::rings::Rings>,
    #[serde(default)]
    allow: Allow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scan {
    roots: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identifiers {
    deny: Vec<String>,
    #[serde(default)]
    exempt: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Allow {
    #[serde(default)]
    dependencies: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    identifiers: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    rings: BTreeMap<String, BTreeSet<String>>,
}

/// Violations found in a tree, keyed like the allowlist.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Findings {
    /// Crate directory → bundle crates it depends on.
    pub dependencies: BTreeMap<String, BTreeSet<String>>,
    /// Source file → denied words it names.
    pub identifiers: BTreeMap<String, BTreeSet<String>>,
    /// Crate directory → workspace crates of an outer ring it depends on.
    pub rings: BTreeMap<String, BTreeSet<String>>,
}

impl Config {
    /// Read the guard configuration from `root`.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(root.join(CONFIG)).with_context(|| format!("read {CONFIG}"))?;
        toml::from_str(&text).with_context(|| format!("parse {CONFIG}"))
    }
}

/// `cargo run -p xtask -- boundary-check [--print-findings]`.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = Config::load(&root)?;
    match args.as_slice() {
        [] => check(&root, &config),
        [flag] if flag == "--print-findings" => {
            print!("{}", scan(&root, &config)?.to_toml());
            Ok(())
        }
        _ => bail!("usage: boundary-check [--print-findings]"),
    }
}

/// `cargo run -p xtask -- ring-report`: production lines per ring (ADR-0040).
pub fn ring_report(args: Vec<String>) -> anyhow::Result<()> {
    anyhow::ensure!(args.is_empty(), "usage: ring-report");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = Config::load(&root)?;
    let rings = config
        .rings
        .as_ref()
        .with_context(|| format!("{CONFIG} has no [rings] map"))?;
    print!("{}", crate::rings::report(&root, rings)?);
    Ok(())
}

impl Findings {
    /// The findings in allowlist form, for seeding `boundary.toml`.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        for (table, entries) in [
            ("allow.dependencies", &self.dependencies),
            ("allow.identifiers", &self.identifiers),
            ("allow.rings", &self.rings),
        ] {
            out.push_str(&format!("[{table}]\n"));
            for (path, names) in entries {
                let names: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
                out.push_str(&format!("\"{path}\" = [{}]\n", names.join(", ")));
            }
        }
        out
    }
}

/// Scan `root` and compare the findings with the allowlist.
pub fn check(root: &Path, config: &Config) -> anyhow::Result<()> {
    let found = scan(root, config)?;
    let mut problems = Vec::new();
    diff(
        "depends on bundle crate",
        &found.dependencies,
        &config.allow.dependencies,
        &mut problems,
    );
    diff(
        "names bundle domain word",
        &found.identifiers,
        &config.allow.identifiers,
        &mut problems,
    );
    diff(
        "depends on an outer ring's crate",
        &found.rings,
        &config.allow.rings,
        &mut problems,
    );
    if problems.is_empty() {
        return Ok(());
    }
    bail!(
        "core/bundle boundary guard ({CONFIG}):\n  {}",
        problems.join("\n  ")
    )
}

fn diff(
    what: &str,
    found: &BTreeMap<String, BTreeSet<String>>,
    allowed: &BTreeMap<String, BTreeSet<String>>,
    problems: &mut Vec<String>,
) {
    let empty = BTreeSet::new();
    for (path, names) in found {
        let allow = allowed.get(path).unwrap_or(&empty);
        for name in names.difference(allow) {
            problems.push(format!("{path} {what} `{name}`"));
        }
    }
    for (path, names) in allowed {
        let have = found.get(path).unwrap_or(&empty);
        for name in names.difference(have) {
            problems.push(format!(
                "{path}: stale allowlist entry `{name}`; remove it from {CONFIG}"
            ));
        }
    }
}

/// Collect every violation under `root`, ignoring the allowlist.
pub fn scan(root: &Path, config: &Config) -> anyhow::Result<Findings> {
    let bundle_crates = bundle_crates(root)?;
    let mut findings = Findings::default();
    for manifest in crate_manifests(root)? {
        let crate_dir = relative(root, manifest.parent().unwrap_or(root));
        let deps = bundle_dependencies(&manifest, &bundle_crates)?;
        if !deps.is_empty() {
            findings.dependencies.insert(crate_dir, deps);
        }
    }
    let deny: Vec<Vec<String>> = config.identifiers.deny.iter().map(|d| words(d)).collect();
    for file in source_files(root, &config.scan)? {
        let path = relative(root, &file);
        let text = std::fs::read_to_string(&file).with_context(|| format!("read {path}"))?;
        let identifiers = if path.ends_with(".rs") {
            rust_identifiers(&text).with_context(|| format!("tokenize {path}"))?
        } else {
            c_like_identifiers(&text)
        };
        let mut hits = BTreeSet::new();
        for (qualifier, ident) in identifiers {
            if exempt(&config.identifiers.exempt, qualifier.as_deref(), &ident) {
                continue;
            }
            let ident_words = words(&ident);
            for (name, word_seq) in config.identifiers.deny.iter().zip(&deny) {
                if contains_run(&ident_words, word_seq) {
                    hits.insert(name.clone());
                }
            }
        }
        if !hits.is_empty() {
            findings.identifiers.insert(path, hits);
        }
    }
    if let Some(rings) = &config.rings {
        findings.rings = crate::rings::violations(root, rings)?;
    }
    Ok(findings)
}

fn exempt(exempt: &[String], qualifier: Option<&str>, ident: &str) -> bool {
    exempt.iter().any(|entry| match entry.rsplit_once("::") {
        Some((path, name)) => name == ident && qualifier == Some(path),
        None => entry == ident,
    })
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Package names of every crate under `bundles/`.
fn bundle_crates(root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for manifest in walk(&root.join("bundles"), &|p| p.ends_with("Cargo.toml"))? {
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(&manifest)?)
            .with_context(|| format!("parse {}", manifest.display()))?;
        if let Some(name) = value
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
        {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

fn crate_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).context("read crates/")? {
        let manifest = entry?.path().join("Cargo.toml");
        if manifest.is_file() {
            manifests.push(manifest);
        }
    }
    manifests.sort();
    Ok(manifests)
}

fn bundle_dependencies(
    manifest: &Path,
    bundle_crates: &BTreeSet<String>,
) -> anyhow::Result<BTreeSet<String>> {
    let value: toml::Value = toml::from_str(&std::fs::read_to_string(manifest)?)
        .with_context(|| format!("parse {}", manifest.display()))?;
    let mut found = BTreeSet::new();
    let mut tables = Vec::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        tables.extend(value.get(key).and_then(toml::Value::as_table));
    }
    if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                tables.extend(target.get(key).and_then(toml::Value::as_table));
            }
        }
    }
    for table in tables {
        for (key, spec) in table {
            let name = spec
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            let path = spec.get("path").and_then(toml::Value::as_str);
            if bundle_crates.contains(name) || path.is_some_and(|p| p.contains("bundles/")) {
                found.insert(name.to_owned());
            }
        }
    }
    Ok(found)
}

fn source_files(root: &Path, scan: &Scan) -> anyhow::Result<Vec<PathBuf>> {
    let excluded: Vec<PathBuf> = scan.exclude.iter().map(|e| root.join(e)).collect();
    let mut files = Vec::new();
    for scan_root in &scan.roots {
        let dir = root.join(scan_root);
        let include = |path: &Path| {
            let source = path
                .extension()
                .is_some_and(|ext| ["rs", "ts", "tsx", "proto"].iter().any(|e| ext == *e));
            source
                && !excluded.iter().any(|e| path.starts_with(e))
                && !is_test_or_generated(path.strip_prefix(root).unwrap_or(path))
        };
        files.extend(walk(&dir, &include)?);
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Tests, fixtures, and generated code are not core production source.
pub(crate) fn is_test_or_generated(path: &Path) -> bool {
    let components: Vec<_> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if components.iter().any(|c| {
        matches!(
            c.as_str(),
            "tests" | "test" | "benches" | "examples" | "fixtures" | "gen" | "target"
        )
    }) {
        return true;
    }
    let name = components.last().map_or("", String::as_str);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".gen.ts")
        || name.ends_with(".generated.ts")
}

fn walk(dir: &Path, include: &dyn Fn(&Path) -> bool) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            if entry.file_type()?.is_dir() {
                if name != "target" && name != "node_modules" && name != ".git" {
                    stack.push(path);
                }
            } else if include(&path) {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Split an identifier into lowercase words at `_` and camelCase boundaries.
fn words(ident: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in ident.split('_').filter(|p| !p.is_empty()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = c.is_uppercase()
                && !word.is_empty()
                && (chars[i - 1].is_lowercase()
                    || chars[i - 1].is_ascii_digit()
                    || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
            if boundary {
                out.push(std::mem::take(&mut word));
            }
            word.extend(c.to_lowercase());
        }
        if !word.is_empty() {
            out.push(word);
        }
    }
    out
}

fn contains_run(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Identifiers of a Rust file with their `path::` qualifier, outside
/// `#[cfg(test)]` items. Comments and literals are not tokens.
fn rust_identifiers(text: &str) -> anyhow::Result<Vec<(Option<String>, String)>> {
    let stream = TokenStream::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut out = Vec::new();
    collect_rust(stream, &mut out);
    Ok(out)
}

fn collect_rust(stream: TokenStream, out: &mut Vec<(Option<String>, String)>) {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let mut i = 0;
    let mut skip_next_item = false;
    while i < tokens.len() {
        match &tokens[i] {
            TokenTree::Punct(p) if p.as_char() == '#' => {
                if let Some(TokenTree::Group(attr)) = tokens.get(i + 1)
                    && attr.delimiter() == Delimiter::Bracket
                {
                    let attr = attr.stream().to_string().replace(' ', "");
                    if attr.starts_with("cfg(test")
                        || attr == "test"
                        || attr.starts_with("tokio::test")
                    {
                        skip_next_item = true;
                    }
                    i += 2;
                    continue;
                }
            }
            TokenTree::Group(group) => {
                if skip_next_item && group.delimiter() == Delimiter::Brace {
                    skip_next_item = false;
                } else {
                    collect_rust(group.stream(), out);
                }
            }
            TokenTree::Ident(ident) => {
                if !skip_next_item {
                    let qualifier = qualifier_before(&tokens, i);
                    out.push((qualifier, ident.to_string()));
                }
            }
            TokenTree::Punct(p) if skip_next_item && p.as_char() == ';' => skip_next_item = false,
            _ => {}
        }
        i += 1;
    }
}

fn qualifier_before(tokens: &[TokenTree], i: usize) -> Option<String> {
    if i < 3 {
        return None;
    }
    match (&tokens[i - 3], &tokens[i - 2], &tokens[i - 1]) {
        (TokenTree::Ident(q), TokenTree::Punct(a), TokenTree::Punct(b))
            if a.as_char() == ':' && b.as_char() == ':' =>
        {
            Some(q.to_string())
        }
        _ => None,
    }
}

/// Identifiers of a TypeScript or protobuf file, skipping comments and string
/// literals (template literal `${…}` bodies are code).
fn c_like_identifiers(text: &str) -> Vec<(Option<String>, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut template_depth: Vec<usize> = Vec::new();
    let mut brace_depth = 0usize;
    let mut last: Vec<String> = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '"' || c == '\'' {
            i = skip_quoted(&chars, i, c);
        } else if c == '`' {
            i = skip_template(&chars, i + 1, &mut template_depth, brace_depth);
        } else if c == '}' && template_depth.last() == Some(&brace_depth) {
            template_depth.pop();
            i = skip_template(&chars, i + 1, &mut template_depth, brace_depth);
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            let qualifier = (last.len() >= 2 && last[last.len() - 1] == ".")
                .then(|| last[last.len() - 2].clone());
            last.push(ident.clone());
            out.push((qualifier, ident));
            continue;
        } else {
            if c == '{' {
                brace_depth += 1;
            } else if c == '}' {
                brace_depth = brace_depth.saturating_sub(1);
            }
            if !c.is_whitespace() {
                last.push(c.to_string());
            }
            i += 1;
        }
        if last.len() > 4 {
            last.drain(..last.len() - 4);
        }
    }
    out
}

fn skip_quoted(chars: &[char], start: usize, quote: char) -> usize {
    let mut i = start + 1;
    while i < chars.len() && chars[i] != quote && chars[i] != '\n' {
        if chars[i] == '\\' {
            i += 1;
        }
        i += 1;
    }
    i + 1
}

/// Skip template literal text from `i`; stop after the closing backtick, or
/// after `${` (recording the brace depth that closes the substitution).
fn skip_template(
    chars: &[char],
    mut i: usize,
    template_depth: &mut Vec<usize>,
    brace_depth: usize,
) -> usize {
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '`' => return i + 1,
            '$' if chars.get(i + 1) == Some(&'{') => {
                template_depth.push(brace_depth);
                return i + 2;
            }
            _ => i += 1,
        }
    }
    i
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn repository_respects_the_boundary_allowlist() {
        let root = workspace_root();
        let config = Config::load(&root).unwrap();
        check(&root, &config).unwrap();
    }

    #[test]
    fn splits_identifier_words() {
        assert_eq!(words("SubagentGovernor"), ["subagent", "governor"]);
        assert_eq!(words("run_mailbox_service"), ["run", "mailbox", "service"]);
        assert_eq!(words("LSPPlane"), ["lsp", "plane"]);
        assert_eq!(words("HostLoopDriver"), ["host", "loop", "driver"]);
        assert!(contains_run(
            &words("NativeLoopDriver"),
            &words("loop_driver")
        ));
        assert!(!contains_run(&words("steam"), &words("team")));
    }

    #[test]
    fn rust_scan_ignores_comments_literals_tests_and_exemptions() {
        let src = r#"
            // subagent in a comment
            /// Doc about the workflow.
            fn plain(f: &mut fmt::Formatter<'_>) { let s = "mailbox"; let (tx, rx) = mpsc::channel(1); }
            fn real_hit() { let goal_driver = 1; }
            #[cfg(test)]
            mod tests { fn team() {} }
            #[test]
            fn resident_case() {}
        "#;
        let exempt = vec!["fmt::Formatter".to_owned(), "mpsc::channel".to_owned()];
        let words_found: BTreeSet<String> = rust_identifiers(src)
            .unwrap()
            .into_iter()
            .filter(|(q, i)| !exempt_check(&exempt, q, i))
            .flat_map(|(_, i)| words(&i))
            .collect();
        assert!(words_found.contains("goal"));
        for absent in [
            "subagent",
            "workflow",
            "mailbox",
            "team",
            "resident",
            "formatter",
            "channel",
        ] {
            assert!(!words_found.contains(absent), "{absent} leaked");
        }
    }

    fn exempt_check(exempt: &[String], q: &Option<String>, i: &str) -> bool {
        super::exempt(exempt, q.as_deref(), i)
    }

    #[test]
    fn c_like_scan_skips_strings_comments_and_template_text() {
        let src = "// team\nconst a = 'subagent'; const b = `workflow ${goalName} text`; /* lsp */ const mailboxView = 1";
        let idents: Vec<String> = c_like_identifiers(src)
            .into_iter()
            .map(|(_, i)| i)
            .collect();
        assert!(idents.contains(&"goalName".to_owned()));
        assert!(idents.contains(&"mailboxView".to_owned()));
        for absent in ["team", "subagent", "workflow", "lsp", "text"] {
            assert!(!idents.iter().any(|i| i == absent), "{absent} leaked");
        }
    }

    /// A fresh violation, and an allowlist entry that no longer matches, both fail.
    #[test]
    fn new_violation_and_stale_entry_fail() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str, text: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "bundles/builtin/demo/native/Cargo.toml",
            "[package]\nname = \"hya-demo\"\n",
        );
        write(
            "crates/core/Cargo.toml",
            "[package]\nname = \"core\"\n[dependencies]\nhya-demo = { workspace = true }\n",
        );
        write("crates/core/src/lib.rs", "pub struct MailboxPlane;\n");
        let config = |allow: &str| -> Config {
            toml::from_str(&format!(
                "[scan]\nroots = [\"crates\"]\n[identifiers]\ndeny = [\"mailbox\", \"team\"]\n{allow}"
            ))
            .unwrap()
        };
        let allowed = "[allow.dependencies]\n\"crates/core\" = [\"hya-demo\"]\n\
                       [allow.identifiers]\n\"crates/core/src/lib.rs\" = [\"mailbox\"]\n";
        check(root, &config(allowed)).unwrap();

        let error = check(root, &config("")).unwrap_err().to_string();
        assert!(
            error.contains("crates/core depends on bundle crate `hya-demo`"),
            "{error}"
        );
        assert!(
            error.contains("crates/core/src/lib.rs names bundle domain word `mailbox`"),
            "{error}"
        );

        write("crates/core/src/lib.rs", "pub struct Plane;\n");
        let error = check(root, &config(allowed)).unwrap_err().to_string();
        assert!(error.contains("stale allowlist entry `mailbox`"), "{error}");
    }
}
