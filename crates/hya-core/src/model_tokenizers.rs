//! Real per-family tokenizers for the usage ledger fallback.
//!
//! When a provider reports token usage the ledger stores the reported numbers.
//! When it does not, the ledger estimates — and the estimate should come from
//! the model family's *actual* tokenizer whenever one can be obtained. This
//! module maps model names to six initially adapted families (GPT, Claude,
//! DeepSeek, GLM, Kimi, Qwen), resolves each family's `tokenizer.json`
//! lazily (hya cache dir → local HF cache → one-time download into the HF
//! cache), and counts with the real encoder. Anything unmatched — or any
//! resolution failure — falls back to the structure-aware
//! [`CalibratedTokenizer`] estimate, so the ledger is always recorded.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::tokens::{CalibratedTokenizer, Tokenizer};

/// The six initially adapted model families and their tokenizer sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenizerFamily {
    /// OpenAI GPT / o-series (o200k-style vocab).
    Gpt,
    /// Anthropic Claude.
    Claude,
    /// DeepSeek.
    Deepseek,
    /// Zhipu GLM.
    Glm,
    /// Moonshot Kimi.
    Kimi,
    /// Alibaba Qwen.
    Qwen,
}

impl TokenizerFamily {
    /// Match a model ref (any `provider/model` casing) to a family. The whole
    /// ref is considered — provider ids often carry the family name
    /// (`deepseek/deepseek-v4.1-flash`, `Qwen/Qwen3-8B`).
    #[must_use]
    pub fn match_model(model: &str) -> Option<Self> {
        let model = model.to_ascii_lowercase();
        let contains_any = |needles: &[&str]| needles.iter().any(|needle| model.contains(needle));
        if contains_any(&["gpt", "o1", "o3", "o4", "o200k", "chatgpt"]) {
            Some(Self::Gpt)
        } else if model.contains("claude") {
            Some(Self::Claude)
        } else if model.contains("deepseek") {
            Some(Self::Deepseek)
        } else if model.contains("glm") {
            Some(Self::Glm)
        } else if contains_any(&["kimi", "moonshot"]) {
            Some(Self::Kimi)
        } else if model.contains("qwen") {
            Some(Self::Qwen)
        } else {
            None
        }
    }

    /// HF repo holding this family's `tokenizer.json`.
    #[must_use]
    pub fn repo(self) -> &'static str {
        match self {
            Self::Gpt => "Xenova/gpt-4o",
            Self::Claude => "Xenova/claude-tokenizer",
            Self::Deepseek => "deepseek-ai/DeepSeek-V3",
            Self::Glm => "zai-org/GLM-4.5",
            Self::Kimi => "moonshotai/Kimi-K2-Instruct",
            Self::Qwen => "Qwen/Qwen3-8B",
        }
    }
}

/// One resolved counting backend: either the family's real HF tokenizer or
/// the calibrated estimator fallback.
pub enum LedgerTokenizer {
    /// Real `tokenizers::Tokenizer` loaded from a family `tokenizer.json`.
    Family(Box<FamilyTokenizer>),
    /// Structure-aware estimate when no real tokenizer is available.
    Calibrated(CalibratedTokenizer),
}

/// Boxed family encoder plus its source repo (diagnostics + confidence label).
pub struct FamilyTokenizer {
    /// Repo the tokenizer came from.
    pub repo: &'static str,
    /// Loaded encoder.
    pub tokenizer: tokenizers::Tokenizer,
}

impl Tokenizer for LedgerTokenizer {
    fn name(&self) -> &str {
        match self {
            Self::Family(family) => family.repo,
            Self::Calibrated(_) => CalibratedTokenizer.name(),
        }
    }

    fn count_text(&self, text: &str) -> usize {
        match self {
            Self::Family(family) => family
                .tokenizer
                .encode(text, false)
                .map(|encoding| encoding.get_ids().len())
                .unwrap_or_else(|_| CalibratedTokenizer.count_text(text)),
            Self::Calibrated(_) => CalibratedTokenizer.count_text(text),
        }
    }
}

/// Resolves family tokenizer bytes from somewhere local or the network.
///
/// The default implementation checks hya's cache directory first, then the
/// local Hugging Face cache, then downloads once through `hf-hub` (which
/// populates the HF cache). Tests inject a loader that returns fixture bytes
/// so CI never touches the network.
pub type TokenizerBytesLoader = Arc<dyn Fn(&str) -> Option<Arc<Vec<u8>>> + Send + Sync>;

/// Per-process cache of resolved family tokenizers.
pub struct ModelTokenizerSource {
    loader: TokenizerBytesLoader,
    cache: RwLock<HashMap<&'static str, Arc<dyn Tokenizer>>>,
}

impl Default for ModelTokenizerSource {
    fn default() -> Self {
        Self::with_loader(Arc::new(load_tokenizer_bytes))
    }
}

impl ModelTokenizerSource {
    /// Build with a custom byte loader (tests, offline deployments).
    #[must_use]
    pub fn with_loader(loader: TokenizerBytesLoader) -> Self {
        Self {
            loader,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// The counting backend for a model ref: the family tokenizer when one
    /// resolves, otherwise the calibrated estimator. Never fails.
    #[must_use]
    pub fn tokenizer_for_model(&self, model: &str) -> Arc<dyn Tokenizer> {
        let Some(family) = TokenizerFamily::match_model(model) else {
            return Arc::new(LedgerTokenizer::Calibrated(CalibratedTokenizer));
        };
        if let Some(cached) = self.cached(family.repo()) {
            return cached;
        }
        let resolved = (self.loader)(family.repo())
            .and_then(|bytes| tokenizers::Tokenizer::from_bytes(bytes.as_ref()).ok())
            .map(|tokenizer| {
                Arc::new(LedgerTokenizer::Family(Box::new(FamilyTokenizer {
                    repo: family.repo(),
                    tokenizer,
                })))
            });
        let backend: Arc<dyn Tokenizer> = match resolved {
            Some(backend) => backend,
            None => Arc::new(LedgerTokenizer::Calibrated(CalibratedTokenizer)),
        };
        self.store(family.repo(), backend.clone());
        backend
    }

    fn cached(&self, repo: &str) -> Option<Arc<dyn Tokenizer>> {
        let guard = self.cache.read().ok()?;
        guard.get(repo).cloned()
    }

    fn store(&self, repo: &'static str, backend: Arc<dyn Tokenizer>) {
        if let Ok(mut guard) = self.cache.write() {
            guard.insert(repo, backend);
        }
    }
}

/// Default loader: hya cache file → HF cache lookup → one-time HF download,
/// mirroring successful downloads into the hya cache directory.
fn load_tokenizer_bytes(repo: &str) -> Option<Arc<Vec<u8>>> {
    if let Ok(bytes) = std::fs::read(hya_cache_tokenizer_path(repo)) {
        return Some(Arc::new(bytes));
    }
    let hf_file = hf_hub::Cache::default()
        .repo(hf_hub::Repo::with_revision(
            repo.to_string(),
            hf_hub::RepoType::Model,
            "main".to_string(),
        ))
        .get("tokenizer.json");
    let downloaded = hf_hub::api::sync::Api::new()
        .ok()?
        .model(repo.to_string())
        .get("tokenizer.json")
        .ok();
    let path = hf_file.or(downloaded)?;
    let bytes = std::fs::read(path).ok()?;
    let cache_path = hya_cache_tokenizer_path(repo);
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&cache_path, bytes.as_slice());
    Some(Arc::new(bytes))
}

fn hya_cache_tokenizer_path(repo: &str) -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
        .unwrap_or_else(|| PathBuf::from(".cache"));
    let sanitized = repo.replace('/', "_");
    base.join("hya")
        .join("tokenizers")
        .join(format!("{sanitized}.json"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Minimal valid WordLevel tokenizer.json: whitespace pre-tokenization,
    /// vocabulary a/b/c + [UNK]. "a b c" → 3 tokens.
    const FIXTURE: &str = r#"{
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [],
        "normalizer": null,
        "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": null,
        "decoder": null,
        "model": {"type": "WordLevel", "vocab": {"a": 0, "b": 1, "c": 2, "[UNK]": 3}, "unk_token": "[UNK]"}
    }"#;

    fn fixture_source() -> ModelTokenizerSource {
        let bytes: Arc<Vec<u8>> = Arc::new(FIXTURE.as_bytes().to_vec());
        ModelTokenizerSource::with_loader(Arc::new(move |_| Some(bytes.clone())))
    }

    #[test]
    fn family_matching_covers_the_initial_six() {
        assert_eq!(
            TokenizerFamily::match_model("12th/gpt-5.3"),
            Some(TokenizerFamily::Gpt)
        );
        assert_eq!(
            TokenizerFamily::match_model("anthropic/claude-sonnet-4-6"),
            Some(TokenizerFamily::Claude)
        );
        assert_eq!(
            TokenizerFamily::match_model("deepseek/deepseek-v4.1-flash"),
            Some(TokenizerFamily::Deepseek)
        );
        assert_eq!(
            TokenizerFamily::match_model("12th/glm-5.3"),
            Some(TokenizerFamily::Glm)
        );
        assert_eq!(
            TokenizerFamily::match_model("moonshotai/Kimi-K2"),
            Some(TokenizerFamily::Kimi)
        );
        assert_eq!(
            TokenizerFamily::match_model("Qwen/Qwen3.8-27B"),
            Some(TokenizerFamily::Qwen)
        );
        assert_eq!(TokenizerFamily::match_model("hya/offline"), None);
    }

    #[test]
    fn matched_model_counts_with_the_real_tokenizer() {
        let source = fixture_source();
        let tokenizer = source.tokenizer_for_model("12th/glm-5.3");
        assert_eq!(tokenizer.name(), TokenizerFamily::Glm.repo());
        assert_eq!(tokenizer.count_text("a b c"), 3);
        assert_eq!(
            tokenizer.count_text("zzz a"),
            2,
            "unknown words count as [UNK]"
        );
    }

    #[test]
    fn unmatched_model_falls_back_to_the_calibrated_estimator() {
        let source = fixture_source();
        let tokenizer = source.tokenizer_for_model("hya/offline");
        assert_eq!(tokenizer.name(), "calibrated");
        assert!(tokenizer.count_text("hello world from the fallback estimator") > 0);
    }

    #[test]
    fn loader_failure_falls_back_without_caching_poison_across_families() {
        let source = ModelTokenizerSource::with_loader(Arc::new(|_| None));
        let tokenizer = source.tokenizer_for_model("Qwen/Qwen3-8B");
        assert_eq!(tokenizer.name(), "calibrated");
    }
}
