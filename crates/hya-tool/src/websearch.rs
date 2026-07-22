use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{SessionId, ToolName, ToolSchema};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

const DEFAULT_TIMEOUT_SECS: u64 = 25;
const EXA_URL: &str = "https://mcp.exa.ai/mcp";
const PARALLEL_URL: &str = "https://search.parallel.ai/mcp";

#[derive(Clone)]
pub struct WebSearchPlane {
    config: Arc<WebSearchConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    pub provider: WebSearchProvider,
    pub endpoint: Option<String>,
    pub key: Option<String>,
    pub enabled: bool,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: WebSearchProvider::Exa,
            endpoint: None,
            key: None,
            enabled: true,
        }
    }
}

impl Default for WebSearchPlane {
    fn default() -> Self {
        Self::configured(WebSearchConfig::default())
    }
}

impl WebSearchPlane {
    #[must_use]
    pub fn new(provider: WebSearchProvider, url: String) -> Self {
        Self::configured(WebSearchConfig {
            provider,
            endpoint: Some(url),
            ..WebSearchConfig::default()
        })
    }

    #[must_use]
    pub fn configured(config: WebSearchConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    Exa,
    Parallel,
}

impl WebSearchProvider {
    const fn id(self) -> &'static str {
        match self {
            WebSearchProvider::Exa => "exa",
            WebSearchProvider::Parallel => "parallel",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            WebSearchProvider::Exa => "Exa Web Search",
            WebSearchProvider::Parallel => "Parallel Web Search",
        }
    }
}

pub(crate) struct WebSearchTool;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebSearchInput {
    query: String,
    num_results: Option<u64>,
    livecrawl: Option<LiveCrawlMode>,
    #[serde(rename = "type")]
    search_type: Option<SearchType>,
    context_max_characters: Option<u64>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum LiveCrawlMode {
    Fallback,
    Preferred,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum SearchType {
    Auto,
    Fast,
    Deep,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "websearch"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("websearch"),
            description: include_str!("websearch.txt")
                .replace("{{year}}", &OffsetDateTime::now_utc().year().to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Websearch query" },
                    "numResults": { "type": "number", "description": "Number of search results to return (default: 8)" },
                    "livecrawl": { "type": "string", "enum": ["fallback", "preferred"], "description": "Live crawl mode - 'fallback': use live crawling as backup if cached content unavailable, 'preferred': prioritize live crawling (default: 'fallback')" },
                    "type": { "type": "string", "enum": ["auto", "fast", "deep"], "description": "Search type - 'auto': balanced search (default), 'fast': quick results, 'deep': comprehensive search" },
                    "contextMaxCharacters": { "type": "number", "description": "Maximum characters for context string optimized for LLMs (default: 10000)" }
                },
                "required": ["query"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: WebSearchInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::WebSearch, Resource::WebSearch(input.query.clone()))
            .await?;
        let provider = ctx.websearch.config.provider;
        let result = call_provider(&ctx.websearch.config, provider, &input, ctx.session).await?;
        Ok(json!({
            "title": format!("{}: {}", provider.label(), input.query),
            "output": result.unwrap_or_else(|| "No search results found. Please try a different query.".to_string()),
            "metadata": { "provider": provider.id() },
        }))
    }
}

async fn call_provider(
    config: &WebSearchConfig,
    provider: WebSearchProvider,
    input: &WebSearchInput,
    session: Option<SessionId>,
) -> Result<Option<String>, ToolError> {
    let (default_url, tool, arguments) = match provider {
        WebSearchProvider::Exa => (EXA_URL, "web_search_exa", exa_arguments(input)),
        WebSearchProvider::Parallel => (
            PARALLEL_URL,
            "web_search",
            parallel_arguments(input, session),
        ),
    };
    let url = config.endpoint.as_deref().unwrap_or(default_url);
    let mut url = Url::parse(url).map_err(|e| ToolError::Input(e.to_string()))?;
    if provider == WebSearchProvider::Exa
        && let Some(key) = config.key.as_deref().filter(|key| !key.is_empty())
    {
        url.query_pairs_mut().append_pair("exaApiKey", key);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .user_agent("hya")
        .build()
        .map_err(|e| ToolError::Other(e.to_string()))?;
    let mut request = client
        .post(url)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": arguments,
            },
        }));
    if provider == WebSearchProvider::Parallel
        && let Some(key) = config.key.as_deref().filter(|key| !key.is_empty())
    {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|e| ToolError::Other(e.to_string()))?;
    if !response.status().is_success() {
        return Err(ToolError::Other(format!(
            "websearch request failed with HTTP {}",
            response.status()
        )));
    }
    let body = response
        .text()
        .await
        .map_err(|e| ToolError::Other(e.to_string()))?;
    crate::websearch_response::parse_response(&body)
}

fn exa_arguments(input: &WebSearchInput) -> Value {
    let mut value = json!({
        "query": input.query,
        "type": input.search_type.unwrap_or(SearchType::Auto),
        "numResults": input.num_results.unwrap_or(8),
        "livecrawl": input.livecrawl.unwrap_or(LiveCrawlMode::Fallback),
    });
    if let Some(limit) = input.context_max_characters {
        value["contextMaxCharacters"] = json!(limit);
    }
    value
}

fn parallel_arguments(input: &WebSearchInput, session: Option<SessionId>) -> Value {
    let mut value = json!({
        "objective": input.query,
        "search_queries": [input.query],
    });
    if let Some(session) = session {
        value["session_id"] = json!(session.to_string());
    }
    value
}
