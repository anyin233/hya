#!/usr/bin/env python3
"""Provenance for the `CalibratedTokenizer` weight table.

`crates/hya-core/src/tokens.rs` estimates tokens by classifying text into run
categories and weighting them. This script is how those weights were derived and
how they are re-derived if the table ever needs to change:

    pip install tiktoken numpy
    python3 scripts/tokenizer-calibration.py fit        # refit the weights
    python3 scripts/tokenizer-calibration.py evaluate   # score against bytes/4
    python3 scripts/tokenizer-calibration.py fixtures   # regenerate test truths

`fit` solves for weights by relative-error least squares against `o200k_base`
over repository files, then quantizes to sixteenths so the Rust implementation
is exact integer arithmetic. `evaluate` scores the quantized table on held-out
files against the `str::len() / 4` baseline it replaces. `fixtures` prints the
ground-truth counts baked into `crates/hya-core/tests/token_accounting.rs`.

The Python featurizer below must stay a faithful mirror of `count_text` in
`tokens.rs`; the fixture accuracy test is what catches divergence.
"""

import glob
import json
import random
import sys

FEATURES = [
    "word_runs",
    "word_extra",
    "digit_runs",
    "digit_extra",
    "punct_runs",
    "punct_extra",
    "newline_runs",
    "newline_extra",
    "cjk",
    "other",
]

# Sixteenths, as committed in `tokens.rs::weight`.
QUANTIZED = [12, 2, 26, 3, 15, 2, 15, 0, 13, 14]
SCALE = 16
CJK_START = 0x2E80

CORPUS_GLOBS = [
    "crates/**/*.rs",
    "docs/**/*.md",
    "packages/**/*.ts",
    "packages/**/*.tsx",
    "**/*.json",
    "**/*.yml",
    "**/*.toml",
]


def featurize(text):
    """Mirror of `CalibratedTokenizer::count_text`'s classification pass."""
    counts = dict.fromkeys(FEATURES, 0)
    i, length = 0, len(text)
    while i < length:
        char = text[i]
        if char.isascii() and char.isalpha():
            start = i
            while i < length and text[i].isascii() and text[i].isalpha():
                i += 1
            counts["word_runs"] += 1
            counts["word_extra"] += max(0, (i - start) - 4)
        elif char.isascii() and char.isdigit():
            start = i
            while i < length and text[i].isascii() and text[i].isdigit():
                i += 1
            counts["digit_runs"] += 1
            counts["digit_extra"] += max(0, (i - start) - 1)
        elif char == "\n":
            # A newline and the indentation after it collapse into one token.
            i += 1
            while i < length and text[i] in "\n\t ":
                i += 1
            counts["newline_runs"] += 1
        elif char in " \t":
            start = i
            while i < length and text[i] in " \t":
                i += 1
            run = i - start
            # A lone separator merges into the neighbouring word token.
            if run > 1:
                counts["punct_runs"] += 1
                counts["punct_extra"] += run - 2
        elif char.isascii():
            start = i
            while (
                i < length
                and text[i].isascii()
                and not text[i].isalnum()
                and text[i] not in "\n\t "
            ):
                i += 1
            counts["punct_runs"] += 1
            counts["punct_extra"] += max(0, (i - start) - 2)
        elif ord(char) >= CJK_START:
            counts["cjk"] += 1
            i += 1
        else:
            counts["other"] += 1
            i += 1
    return [counts[key] for key in FEATURES]


def predict(text, weights=QUANTIZED):
    """Mirror of the full `count_text`, including its rounding."""
    total = sum(f * w for f, w in zip(featurize(text), weights))
    return (total + SCALE // 2) // SCALE


def load_corpus(patterns, limit, seed):
    paths = []
    for pattern in patterns:
        paths += glob.glob(pattern, recursive=True)
    paths = [p for p in paths if "/target/" not in p and "/node_modules/" not in p]
    random.seed(seed)
    random.shuffle(paths)
    corpus = []
    for path in paths[:limit]:
        try:
            text = open(path, encoding="utf-8").read()
        except (OSError, UnicodeDecodeError):
            continue
        if 200 <= len(text) <= 200_000:
            corpus.append(text)
    return corpus


def fit():
    import numpy as np
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    corpus = load_corpus(CORPUS_GLOBS, 900, seed=7)
    features = np.array([featurize(t) for t in corpus], dtype=float)
    truth = np.array(
        [len(enc.encode(t, disallowed_special=())) for t in corpus], dtype=float
    )
    # Weight each file so relative, not absolute, error is minimised: a 200-line
    # file must not be drowned out by a 5000-line one.
    inverse = 1.0 / np.maximum(truth, 1.0)
    coefficients, *_ = np.linalg.lstsq(
        features * inverse[:, None], truth * inverse, rcond=None
    )
    coefficients = np.maximum(coefficients, 0.0)
    print(f"fitted on {len(corpus)} files")
    for name, raw in zip(FEATURES, coefficients):
        print(f"  {name:14s} {raw:.4f}  ->  {round(raw * SCALE)}/{SCALE}")
    print(f"\nquantized: {[round(c * SCALE) for c in coefficients]}")
    print(f"committed: {QUANTIZED}")


def score(label, predicted, truth):
    import numpy as np

    errors = np.sort((predicted - truth) / np.maximum(truth, 1))
    n = len(errors)
    print(
        f"  {label:20s} bias={errors.mean():+.3f} "
        f"p05={errors[int(n * 0.05)]:+.3f} p95={errors[int(n * 0.95)]:+.3f} "
        f"worst={np.abs(errors).max():.3f} "
        f"within15%={(np.abs(errors) <= 0.15).mean():.1%} "
        f"under15%={(errors < -0.15).mean():.1%}"
    )


def evaluate():
    import numpy as np
    import tiktoken

    cjk_line = "本次我们开始开发上下文管理功能，首先阅读当前仓库，分析系统如何管理上下文。"
    suites = [
        ("held-out code", ["crates/**/*.rs", "packages/**/*.ts"], "o200k_base"),
        ("held-out docs/json", ["docs/**/*.md", "**/*.json"], "o200k_base"),
        ("held-out code vs cl100k", ["crates/**/*.rs"], "cl100k_base"),
    ]
    for label, patterns, encoding in suites:
        enc = tiktoken.get_encoding(encoding)
        corpus = load_corpus(patterns, 4000, seed=99)
        truth = np.array(
            [len(enc.encode(t, disallowed_special=())) for t in corpus], dtype=float
        )
        print(f"{label}  n={len(corpus)}")
        score(
            "bytes/4 (baseline)",
            np.array([len(t.encode("utf-8")) // 4 for t in corpus], dtype=float),
            truth,
        )
        score(
            "calibrated",
            np.array([predict(t) for t in corpus], dtype=float),
            truth,
        )
    enc = tiktoken.get_encoding("o200k_base")
    cjk = [cjk_line * k for k in (2, 6, 20)]
    truth = np.array(
        [len(enc.encode(t, disallowed_special=())) for t in cjk], dtype=float
    )
    print(f"CJK  n={len(cjk)}")
    score(
        "bytes/4 (baseline)",
        np.array([len(t.encode("utf-8")) // 4 for t in cjk], dtype=float),
        truth,
    )
    score("calibrated", np.array([predict(t) for t in cjk], dtype=float), truth)


# Mirrors the `FIXTURES` table in `crates/hya-core/tests/token_accounting.rs`.
TEST_FIXTURES = {
    "rust_code": (
        "pub fn resolved_threshold(cfg: &CompactionConfig, max_context: Option<u32>) -> usize {\n"
        "    let Some(window) = max_context.filter(|w| *w > 0) else {\n"
        "        return cfg.token_threshold;\n"
        "    };\n"
        "    if !(cfg.context_fraction > 0.0 && cfg.context_fraction <= 1.0) {\n"
        "        return cfg.token_threshold;\n"
        "    }\n"
        "    let scaled = f64::from(window) * f64::from(cfg.context_fraction);\n"
        "    (scaled as usize).max(MIN_RESOLVED_THRESHOLD)\n"
        "}\n"
    ),
    "json_blob": json.dumps(
        {
            "session": "01JQ7X8Z9ABCDEFGHJKMNPQRST",
            "messages": [
                {
                    "role": "user",
                    "tokens": {"input": 18422, "output": 512, "cache_read": 16384},
                },
                {
                    "role": "assistant",
                    "finish": "tool_calls",
                    "tools": ["read", "grep", "bash"],
                },
            ],
            "paths": [
                "crates/hya-core/src/compaction.rs",
                "crates/hya-tool/src/handle/artifact.rs",
            ],
        },
        indent=2,
    ),
    "markdown_prose": (
        "## Compaction ladder\n\n"
        "The turn loop walks a fixed escalation order, cheapest and most recoverable\n"
        "first, and stops at the first rung that brings the transcript under the\n"
        "trigger. Spilling tool output is lossless because the body moves to an\n"
        "`artifact://` handle and only the pointer stays in the transcript.\n"
    ),
    "chinese": (
        "本次我们开始开发上下文管理功能，首先阅读当前仓库，分析系统如何管理上下文。"
        "压缩工具的实现完全参考分级设计进行，使用多个工具分级依次尝试，"
        "从而实现当高效压缩工具有效时直接调用高效的压缩工具。"
    ),
    "mixed_cjk": (
        "接下来我们需要为 hya 引入类似 omp 的 artifact:// 等路径工具，"
        "即引入包括 skill:// 在内的这类基于 url 的索引工具，"
        "替换现有的索引工具，为 hya 带来统一化的外部工具调用体验。"
    ),
    "indented_code": (
        "impl Tool for ShellTool {\n"
        "    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {\n"
        "        let args: ShellArgs = serde_json::from_value(input)?;\n"
        "        let artifacts = ctx.handles.artifacts();\n"
        "        let mut sink = OutputSink::new(artifacts.clone());\n"
        "        for chunk in stream {\n"
        "            sink.push(&chunk)?;\n"
        "        }\n"
        "        Ok(sink.finish()?)\n"
        "    }\n"
        "}\n"
    ),
    "numeric_table": (
        "threshold=150000 reserve=16384 window=200000 fraction=0.75\n"
        "input=18422 output=512 cache_read=16384 cache_write=0\n"
        "1234567890 987654321 42 3.14159265358979 0xDEADBEEF\n"
    ),
    "log_output": (
        "error[E0433]: failed to resolve: use of undeclared crate or module `handle`\n"
        "  --> crates/hya-tool/src/shell.rs:354:41\n"
        "   |\n"
        "354 |         let artifact_root = normalize(&absolutize(&ctx.workdir));\n"
        "   |                                         ^^^^^^^^^^ not found in this scope\n"
        "warning: unused import: `std::sync::Arc`\n"
    ),
}


def fixtures():
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    header = f"{'fixture':16s} {'truth':>6s} {'calib':>6s} {'bytes/4':>8s}"
    print(f"{header}   err_calib   err_bytes4")
    for name, text in TEST_FIXTURES.items():
        truth = len(enc.encode(text, disallowed_special=()))
        calibrated = predict(text)
        baseline = len(text.encode("utf-8")) // 4
        err_c = (calibrated - truth) / truth
        err_b = (baseline - truth) / truth
        print(
            f"{name:16s} {truth:6d} {calibrated:6d} {baseline:8d}"
            f"   {err_c:+8.1%}   {err_b:+8.1%}"
        )


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else "evaluate"
    handlers = {"fit": fit, "evaluate": evaluate, "fixtures": fixtures}
    handler = handlers.get(command)
    if handler is None:
        sys.exit(f"usage: {sys.argv[0]} [{'|'.join(handlers)}]")
    handler()
