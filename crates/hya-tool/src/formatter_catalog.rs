#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckKind {
    Command(&'static str),
    Prettier,
    Oxfmt,
    Biome,
    Clang,
    Ruff,
    Air,
    Uv,
    Ocamlformat,
    Pint,
}

pub(crate) struct BuiltinSpec {
    pub(crate) name: &'static str,
    pub(crate) extensions: &'static [&'static str],
    pub(crate) check: CheckKind,
}

pub(crate) fn builtins() -> &'static [BuiltinSpec] {
    &[
        BuiltinSpec {
            name: "gofmt",
            extensions: &[".go"],
            check: CheckKind::Command("gofmt"),
        },
        BuiltinSpec {
            name: "mix",
            extensions: &[".ex", ".exs", ".eex", ".heex", ".leex", ".neex", ".sface"],
            check: CheckKind::Command("mix"),
        },
        BuiltinSpec {
            name: "prettier",
            extensions: &[
                ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".html", ".htm",
                ".css", ".scss", ".sass", ".less", ".vue", ".svelte", ".json", ".jsonc", ".yaml",
                ".yml", ".toml", ".xml", ".md", ".mdx", ".graphql", ".gql",
            ],
            check: CheckKind::Prettier,
        },
        BuiltinSpec {
            name: "oxfmt",
            extensions: &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"],
            check: CheckKind::Oxfmt,
        },
        BuiltinSpec {
            name: "biome",
            extensions: &[
                ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".html", ".htm",
                ".css", ".scss", ".sass", ".less", ".vue", ".svelte", ".json", ".jsonc", ".yaml",
                ".yml", ".toml", ".xml", ".md", ".mdx", ".graphql", ".gql",
            ],
            check: CheckKind::Biome,
        },
        BuiltinSpec {
            name: "zig",
            extensions: &[".zig", ".zon"],
            check: CheckKind::Command("zig"),
        },
        BuiltinSpec {
            name: "clang-format",
            extensions: &[
                ".c", ".cc", ".cpp", ".cxx", ".c++", ".h", ".hh", ".hpp", ".hxx", ".h++", ".ino",
                ".C", ".H",
            ],
            check: CheckKind::Clang,
        },
        BuiltinSpec {
            name: "ktlint",
            extensions: &[".kt", ".kts"],
            check: CheckKind::Command("ktlint"),
        },
        BuiltinSpec {
            name: "ruff",
            extensions: &[".py", ".pyi"],
            check: CheckKind::Ruff,
        },
        BuiltinSpec {
            name: "air",
            extensions: &[".R"],
            check: CheckKind::Air,
        },
        BuiltinSpec {
            name: "uv",
            extensions: &[".py", ".pyi"],
            check: CheckKind::Uv,
        },
        BuiltinSpec {
            name: "rubocop",
            extensions: &[".rb", ".rake", ".gemspec", ".ru"],
            check: CheckKind::Command("rubocop"),
        },
        BuiltinSpec {
            name: "standardrb",
            extensions: &[".rb", ".rake", ".gemspec", ".ru"],
            check: CheckKind::Command("standardrb"),
        },
        BuiltinSpec {
            name: "htmlbeautifier",
            extensions: &[".erb", ".html.erb"],
            check: CheckKind::Command("htmlbeautifier"),
        },
        BuiltinSpec {
            name: "dart",
            extensions: &[".dart"],
            check: CheckKind::Command("dart"),
        },
        BuiltinSpec {
            name: "ocamlformat",
            extensions: &[".ml", ".mli"],
            check: CheckKind::Ocamlformat,
        },
        BuiltinSpec {
            name: "terraform",
            extensions: &[".tf", ".tfvars"],
            check: CheckKind::Command("terraform"),
        },
        BuiltinSpec {
            name: "latexindent",
            extensions: &[".tex"],
            check: CheckKind::Command("latexindent"),
        },
        BuiltinSpec {
            name: "gleam",
            extensions: &[".gleam"],
            check: CheckKind::Command("gleam"),
        },
        BuiltinSpec {
            name: "shfmt",
            extensions: &[".sh", ".bash"],
            check: CheckKind::Command("shfmt"),
        },
        BuiltinSpec {
            name: "nixfmt",
            extensions: &[".nix"],
            check: CheckKind::Command("nixfmt"),
        },
        BuiltinSpec {
            name: "rustfmt",
            extensions: &[".rs"],
            check: CheckKind::Command("rustfmt"),
        },
        BuiltinSpec {
            name: "pint",
            extensions: &[".php"],
            check: CheckKind::Pint,
        },
        BuiltinSpec {
            name: "ormolu",
            extensions: &[".hs"],
            check: CheckKind::Command("ormolu"),
        },
        BuiltinSpec {
            name: "cljfmt",
            extensions: &[".clj", ".cljs", ".cljc", ".edn"],
            check: CheckKind::Command("cljfmt"),
        },
        BuiltinSpec {
            name: "dfmt",
            extensions: &[".d"],
            check: CheckKind::Command("dfmt"),
        },
    ]
}
