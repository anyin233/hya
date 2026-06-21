use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

const DEFAULT_TIMEOUT_SECS: u64 = 25;
const EXA_URL: &str = "https://mcp.exa.ai/mcp";
const PARALLEL_URL: &str = "https://search.parallel.ai/mcp";

#[derive(Clone)]
pub struct WebSearchPlane {
    config: Arc<WebSearchConfig>,
}

#[derive(Clone)]
struct WebSearchConfig {
    provider: WebSearchProvider,
    exa_url: String,
    parallel_url: String,
}

impl Default for WebSearchPlane {
    fn default() -> Self {
        let provider = match std::env::var("OPENCODE_WEBSEARCH_PROVIDER").as_deref() {
            Ok("parallel") => WebSearchProvider::Parallel,
            _ => WebSearchProvider::Exa,
        };
        let exa_url = exa_url_from_env();
        Self {
            config: Arc::new(WebSearchConfig {
                provider,
                exa_url,
                parallel_url: PARALLEL_URL.to_string(),
            }),
        }
    }
}

impl WebSearchPlane {
    #[must_use]
    pub fn new(provider: WebSearchProvider, url: String) -> Self {
        let exa_url = if provider == WebSearchProvider::Exa {
            url.clone()
        } else {
            EXA_URL.to_string()
        };
        let parallel_url = if provider == WebSearchProvider::Parallel {
            url
        } else {
            PARALLEL_URL.to_string()
        };
        Self {
            config: Arc::new(WebSearchConfig {
                provider,
                exa_url,
                parallel_url,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
            description: "Search the web using an OpenCode-compatible MCP search provider."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Websearch query" },
                    "numResults": { "type": "number", "description": "Number of search results to return (default: 8)" },
                    "livecrawl": { "type": "string", "enum": ["fallback", "preferred"] },
                    "type": { "type": "string", "enum": ["auto", "fast", "deep"] },
                    "contextMaxCharacters": { "type": "number" }
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
        let result = call_provider(&ctx.websearch.config, provider, &input).await?;
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
) -> Result<Option<String>, ToolError> {
    let (url, tool, arguments) = match provider {
        WebSearchProvider::Exa => (&config.exa_url, "web_search_exa", exa_arguments(input)),
        WebSearchProvider::Parallel => (
            &config.parallel_url,
            "web_search",
            json!({
                "objective": input.query,
                "search_queries": [input.query],
            }),
        ),
    };
    let url = Url::parse(url).map_err(|e| ToolError::Input(e.to_string()))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .user_agent("yaca")
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
        && let Ok(key) = std::env::var("PARALLEL_API_KEY")
        && !key.is_empty()
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
    parse_response(&body)
}

fn exa_url_from_env() -> String {
    let Ok(key) = std::env::var("EXA_API_KEY") else {
        return EXA_URL.to_string();
    };
    if key.is_empty() {
        return EXA_URL.to_string();
    }
    let Ok(mut url) = Url::parse(EXA_URL) else {
        return EXA_URL.to_string();
    };
    url.query_pairs_mut().append_pair("exaApiKey", &key);
    url.into()
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

fn parse_response(body: &str) -> Result<Option<String>, ToolError> {
    let trimmed = body.trim();
    if let Some(text) = parse_payload(trimmed)? {
        return Ok(Some(text));
    }
    for line in body.lines() {
        if let Some(payload) = line.strip_prefix("data: ")
            && let Some(text) = parse_payload(payload)?
        {
            return Ok(Some(text));
        }
    }
    Ok(None)
}

fn parse_payload(payload: &str) -> Result<Option<String>, ToolError> {
    let trimmed = payload.trim();
    if !trimmed.starts_with('{') {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed)?;
    Ok(value["result"]["content"]
        .as_array()
        .and_then(|items| items.iter().find_map(|item| item["text"].as_str()))
        .map(ToString::to_string))
}
