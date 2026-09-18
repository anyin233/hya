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
import re
import sys

FEATURES = [
    "word_runs",
    "word_extra",
    "word_long",
    "digit_chars",
    "case_changes",
    "number_runs",
    "number_chars",
    "punct_runs",
    "newline_runs",
    "cjk",
    "other",
]

# Sixteenths, as committed in `tokens.rs::weight`.
QUANTIZED = [15, 1, 8, 7, 5, 11, 6, 15, 9, 13, 17]
SCALE = 16
CJK_START = 0x2E80

# Alpha runs longer than this stop behaving like vocabulary words. Up to here
# BPE folds a run into one or two tokens; past it the merges run out and the
# cost settles near half a token per character. That is the regime of base64
# blobs, long digests and minified bundles in tool output, and charging them the
# prose rate under-counts them fourfold. Digits need no such split: BPE groups
# them in threes at every length, so one per-character rate covers both regimes.
LONG_RUN = 12

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
        if char.isascii() and char.isalnum():
            # One maximal alphanumeric run, not separate letter and digit runs.
            # BPE merges across the letter/digit boundary inside `sha256`, `utf8`
            # or a base64 blob, so splitting there invents a per-run cost that
            # the encoder never charges.
            start = i
            digits = changes = 0
            while i < length and text[i].isascii() and text[i].isalnum():
                digits += text[i].isdigit()
                # Case changes are what separate a base64 blob from a lowercase
                # hex digest of the same length: alternating case has few merges
                # in the vocabulary. Counting capitals instead would charge
                # SCREAMING_SNAKE_CASE, which BPE merges as happily as prose.
                # The first pair is skipped so an ordinary initial capital is
                # free.
                if i - start >= 2 and text[i].isalpha() and text[i - 1].isalpha():
                    changes += text[i].isupper() != text[i - 1].isupper()
                i += 1
            run = i - start
            if digits == run:
                # A bare number, which BPE splits into three-digit groups at
                # every length. Digits embedded in an identifier are far cheaper,
                # so the two cannot share one weight.
                counts["number_runs"] += 1
                counts["number_chars"] += run
            else:
                counts["word_runs"] += 1
                counts["word_extra"] += max(0, min(run, LONG_RUN) - 4)
                counts["word_long"] += max(0, run - LONG_RUN)
                counts["digit_chars"] += digits
                counts["case_changes"] += changes
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


def blob_corpus(seed=11):
    """Long-run payloads that repository files barely contain.

    Base64 attachments, hex digests, JWTs and minified bundles arrive through
    tool output rather than checked-in source, so a corpus of `.rs` and `.md`
    files gives the long-run weights almost no signal. These samples supply it.
    """
    rng = random.Random(seed)
    b64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    hexits = "0123456789abcdef"

    def draw(alphabet, n):
        return "".join(rng.choice(alphabet) for _ in range(n))

    samples = []
    # Vary run lengths within every sample. Fitting a per-character weight
    # against runs that are all the same length leaves it collinear with the
    # per-run weight, and least squares answers with a nonsense split.
    for n in (512, 2048, 8192):
        samples.append(draw(b64, n))
        samples.append("\n".join(draw(b64, rng.randint(40, 96)) for _ in range(n // 76)))
    for count in (20, 40, 80):
        samples.append(" ".join(draw(hexits, rng.randint(8, 128)) for _ in range(count)))
    samples.append(".".join(draw(b64, k) for k in (36, 210, 43)))  # JWT-shaped
    samples.append(
        ";".join(
            f"function {draw('abcdefghijklmnopqrstuvwxyz', rng.randint(6, 40))}"
            f"(a,b){{return a.{draw('abcdefghijklmnopqrstuvwxyz', rng.randint(4, 30))}(b)}}"
            for _ in range(60)
        )
    )
    for count in (50, 200):
        samples.append(
            " ".join(draw("0123456789", rng.randint(1, 40)) for _ in range(count))
        )
    return samples


# A token covers at least one character, so a per-character weight above 1.0 is
# unphysical; a per-run weight covers the run's first few characters. Bounding
# the solve keeps a collinear corpus from answering with an absurd split that
# happens to fit, such as two tokens per digit.
UPPER_BOUND = {
    "word_runs": 4.0,
    "word_extra": 1.0,
    "word_long": 1.0,
    "digit_chars": 1.0,
    "case_changes": 2.0,
    # A number costs about `ceil(len / 3)` tokens. The fixed part of that is the
    # rounding, worth at most two thirds of a token — never the 1.6 an
    # unconstrained solve reaches for by leaning on digits as a proxy for the
    # punctuation around them, which over-charges every short number by half.
    "number_runs": 2.0 / 3.0,
    "number_chars": 1.0,
    "punct_runs": 2.0,
    "newline_runs": 2.0,
    "cjk": 1.5,
    "other": 1.5,
}

# Zero is a wrong answer for a length term even when the corpus tolerates it:
# `case_changes` and `word_long` can absorb its variance, and the fit then
# claims a twelve-character word costs what a five-character one does. These
# floors are the marginal rates measured on runs of those lengths in isolation.
#
# `word_long` needs a floor for a second reason. What a long run really costs
# depends on whether the vocabulary happens to contain its merges: 100 `r`s cost
# 0.25 per character, 100 `R`s cost 0.50, and a concatenation of dictionary
# words costs 0.08. No run-length feature can tell those apart, so the weight is
# pinned to the rate measured on high-entropy runs — random letters, base64, hex
# — at 0.50. Those are the shapes that actually arrive in tool output, and the
# residual error then lands on the safe side: over-counting compacts a turn
# early, while under-counting overflows the window.
LOWER_BOUND = {
    "word_extra": 0.05,
    "word_long": 0.50,
}


def bounded_lstsq(design, target, lower, upper, iterations=20_000):
    """Least squares over a box, by projected gradient descent.

    `numpy.linalg.lstsq` is unconstrained and scipy is not a dependency here,
    so this projects onto `[lower, upper]` after every step. The objective is
    convex and the box is convex, so the fixed point is the constrained
    optimum.
    """
    import numpy as np

    gram = design.T @ design
    moment = design.T @ target
    step = 1.0 / np.linalg.eigvalsh(gram).max()
    weights = lower.copy()
    for _ in range(iterations):
        weights = np.clip(weights - step * (gram @ weights - moment), lower, upper)
    return weights


def fit():
    import numpy as np
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    corpus = load_corpus(CORPUS_GLOBS, 900, seed=7) + blob_corpus()
    features = np.array([featurize(t) for t in corpus], dtype=float)
    truth = np.array(
        [len(enc.encode(t, disallowed_special=())) for t in corpus], dtype=float
    )
    # Weight each sample so relative, not absolute, error is minimised: a
    # 200-line file must not be drowned out by a 5000-line one.
    inverse = 1.0 / np.maximum(truth, 1.0)
    lower = np.array([LOWER_BOUND.get(name, 0.0) for name in FEATURES])
    upper = np.array([UPPER_BOUND[name] for name in FEATURES])
    coefficients = bounded_lstsq(
        features * inverse[:, None], truth * inverse, lower, upper
    )
    print(f"fitted on {len(corpus)} samples")
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


def cjk_spans(minimum=8):
    """Contiguous CJK spans found in repository files.

    Scoring one sentence repeated N times measures BPE merging across the
    repeats, not Chinese prose, and reports an over-count that real text never
    shows. Distinct spans are the honest sample.
    """
    span = re.compile(r"[\u2e80-\u9fff\uf900-\ufaff\uff00-\uffef]{%d,}" % minimum)
    spans = []
    for pattern in ("docs/**/*.md", "docs/**/*.json", "crates/**/*.rs"):
        for path in glob.glob(pattern, recursive=True):
            if "/target/" in path:
                continue
            try:
                spans += span.findall(open(path, encoding="utf-8").read())
            except (OSError, UnicodeDecodeError):
                continue
    return spans


def evaluate():
    import numpy as np
    import tiktoken

    suites = [
        ("held-out code", ["crates/**/*.rs", "packages/**/*.ts"], "o200k_base"),
        ("held-out docs/json", ["docs/**/*.md", "**/*.json"], "o200k_base"),
        ("held-out code vs cl100k", ["crates/**/*.rs"], "cl100k_base"),
    ]
    for label, patterns, encoding in suites:
        enc = tiktoken.get_encoding(encoding)
        corpus = load_corpus(patterns, 4000, seed=99)
        report(label, corpus, enc)
    report("CJK spans", cjk_spans(), tiktoken.get_encoding("o200k_base"))
    report("long runs", blob_corpus(seed=23), tiktoken.get_encoding("o200k_base"))


def report(label, corpus, enc):
    import numpy as np

    truth = np.array(
        [len(enc.encode(t, disallowed_special=())) for t in corpus], dtype=float
    )
    print(f"{label}  n={len(corpus)}")
    score(
        "bytes/4 (baseline)",
        np.array([len(t.encode("utf-8")) // 4 for t in corpus], dtype=float),
        truth,
    )
    score("calibrated", np.array([predict(t) for t in corpus], dtype=float), truth)


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

# Mirrors the `BLOB_FIXTURES` table in the same test file. These are weighted
# one-sidedly on purpose; see `weight::WORD_LONG` in `tokens.rs`.
BLOB_FIXTURES = {
    "base64_attachment": (
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA"
        "60e6kgAAAABJRU5ErkJggg=="
    ),
    "hex_digest_table": (
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n"
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n"
    ),
    "long_identifier_run": (
        "averyveryverylongunbrokenidentifiernamethatkeepsgoingwithoutanyseparators"
    ),
}


def fixtures():
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    header = f"{'fixture':20s} {'truth':>6s} {'calib':>6s} {'bytes/4':>8s}"
    for label, table in (("prose/code", TEST_FIXTURES), ("blobs", BLOB_FIXTURES)):
        print(f"\n{label}\n{header}   err_calib   err_bytes4")
        for name, text in table.items():
            truth = len(enc.encode(text, disallowed_special=()))
            calibrated = predict(text)
            baseline = len(text.encode("utf-8")) // 4
            print(
                f"{name:20s} {truth:6d} {calibrated:6d} {baseline:8d}"
                f"   {(calibrated - truth) / truth:+8.1%}"
                f"   {(baseline - truth) / truth:+8.1%}"
            )


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else "evaluate"
    handlers = {"fit": fit, "evaluate": evaluate, "fixtures": fixtures}
    handler = handlers.get(command)
    if handler is None:
        sys.exit(f"usage: {sys.argv[0]} [{'|'.join(handlers)}]")
    handler()
