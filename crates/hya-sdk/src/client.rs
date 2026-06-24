use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Result, SdkError};
use crate::store::StoredPart;
use crate::types::{Agent, Config, Message, Session};

/// Raw request transport beneath [`Client`].
///
/// One `request` carries every verb the TUI needs. Implementations error on non-2xx
/// responses and return [`Value::Null`] for an empty (e.g. `DELETE`/`PATCH`) body, so the
/// high-level [`Client`] logic is written ONCE and shared by the HTTP and native backends.
#[async_trait]
pub trait Transport: Send + Sync {
    fn base_url(&self) -> &str;
    fn directory(&self) -> &str;
    async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value>;
}

/// The backend server client surface used by the TUI.
///
/// FROZEN CONTRACT (W0). State workers hold `Arc<dyn Client>`. Backed by any [`Transport`]
/// (`HttpClient` over reqwest, or `NativeClient` over the in-process stdio bridge).
#[async_trait]
pub trait Client: Send + Sync {
    /// Base URL of the server this client targets, e.g. `http://127.0.0.1:NNNNN`.
    fn base_url(&self) -> &str;

    /// The directory this client scopes requests to (sent as the directory header).
    fn directory(&self) -> &str;

    /// `GET /config`
    async fn config_get(&self) -> Result<Config>;

    /// `GET /session`
    async fn session_list(&self) -> Result<Vec<Session>>;

    /// `GET /session/{id}`
    async fn session_get(&self, session_id: &str) -> Result<Session>;

    /// `GET /session/{id}/message` — full history as `{info, parts}` pairs (info → [`Message`],
    /// parts → [`StoredPart`]s). Hydrates the store when switching to a session not streamed live.
    async fn session_messages(&self, session_id: &str) -> Result<Vec<(Message, Vec<StoredPart>)>>;

    async fn session_todo(&self, session_id: &str) -> Result<Vec<serde_json::Value>>;

    async fn session_diff(&self, session_id: &str) -> Result<Vec<serde_json::Value>>;

    /// `GET /agent` — list configured agents (default selection + agent switch dialog).
    async fn agents(&self) -> Result<Vec<Agent>>;

    /// `GET /find/file?query=` — fuzzy file search for `@file` prompt mentions. Returns
    /// relative paths, best match first.
    async fn find_files(&self, query: &str) -> Result<Vec<String>>;

    /// `GET /command` — configured command names (for `/slash` validation). Slow (~12s) warmup,
    /// so callers fetch it in the background, not at startup.
    async fn commands(&self) -> Result<Vec<String>>;

    /// `GET /config/providers` — provider/model catalog for the model switch dialog. Returns
    /// `(value = "providerID/modelID", title, provider display name, context token limit, variant
    /// names)` tuples; deprecated models are skipped. Variants are the keys of the model's
    /// `variants` object (empty when the model has none). Fetched in the background like
    /// [`Client::commands`].
    async fn models(&self) -> Result<Vec<(String, String, String, i64, Vec<String>)>>;

    async fn mcp_status(&self) -> Result<Vec<(String, String)>>;

    /// `GET /lsp` — configured language servers as `(id, root, status)` tuples. Status is
    /// `"connected"` or `"error"` (other strings are passed through verbatim).
    async fn lsp_status(&self) -> Result<Vec<(String, String, String)>>;

    /// `GET /formatter` — configured formatters; only `enabled` entries are returned, in
    /// server order, as their display names.
    async fn formatter_status(&self) -> Result<Vec<String>>;

    /// `GET /config` `plugin` — installed plugins as `(name, optional version)`. Entries
    /// shaped `"name@version"` split on the last `@`; `"file://..."` URLs become the file
    /// basename (or its parent directory when the basename is `index`); `[name, options]`
    /// tuples take the first element. The list is sorted by name.
    async fn plugins(&self) -> Result<Vec<(String, Option<String>)>>;

    /// `POST /session` — create a session and return it.
    async fn session_create(&self) -> Result<Session>;

    /// `POST /session/{id}/message` — submit a normal prompt (text + parts).
    async fn session_prompt(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value>;

    /// `POST /session/{id}/shell` — run a shell command (prompt `!` mode). `agent`/`model`
    /// are optional (server defaults); body is `{"command": "..."}`.
    async fn session_shell(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value>;

    /// `POST /session/{id}/command` — run a `/slash` command. Requires `agent`; body is
    /// `{"command": name, "arguments": args, "agent": ...}`.
    async fn session_command(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value>;

    /// `POST /permission/{requestID}/reply` — answer a pending permission request. `reply`
    /// is `"once"`, `"always"`, or `"reject"`; `message` is optional rejection feedback.
    async fn permission_reply(
        &self,
        request_id: &str,
        reply: &str,
        message: Option<&str>,
    ) -> Result<()>;

    /// `POST /question/{requestID}/reply` — answer a pending assistant question.
    async fn question_reply(
        &self,
        request_id: &str,
        answers: &[Vec<String>],
        directory: Option<&str>,
    ) -> Result<()>;

    /// `POST /question/{requestID}/reject` — dismiss a pending assistant question.
    async fn question_reject(&self, request_id: &str, directory: Option<&str>) -> Result<()>;

    /// `PATCH /session/{id}` with `{title}` — rename a session.
    async fn session_rename(&self, session_id: &str, title: &str) -> Result<()>;

    /// `DELETE /session/{id}` — delete a session.
    async fn session_delete(&self, session_id: &str) -> Result<()>;

    /// `POST /session/{id}/summarize` — compact (AI-summarize) a session with the given model.
    async fn session_compact(
        &self,
        session_id: &str,
        provider_id: &str,
        model_id: &str,
    ) -> Result<()>;

    /// `POST /session/{id}/revert` with `{messageID}` — revert messages from `messageID` onward.
    async fn session_revert(&self, session_id: &str, message_id: &str) -> Result<()>;

    /// `POST /session/{id}/unrevert` — restore all reverted messages.
    async fn session_unrevert(&self, session_id: &str) -> Result<()>;

    /// `POST /session/{id}/abort` — interrupt the running turn.
    async fn session_abort(&self, session_id: &str) -> Result<()>;
}

/// [`Client`] implemented once over any [`Transport`].
pub struct ApiClient<T: Transport> {
    transport: T,
}

impl<T: Transport> ApiClient<T> {
    pub fn with_transport(transport: T) -> Self {
        Self { transport }
    }

    async fn get<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        Ok(serde_json::from_value(
            self.transport.request("GET", path, None).await?,
        )?)
    }

    async fn post<R: DeserializeOwned>(&self, path: &str, body: serde_json::Value) -> Result<R> {
        Ok(serde_json::from_value(
            self.transport.request("POST", path, Some(&body)).await?,
        )?)
    }
}

/// Concrete reqwest-backed [`Transport`]. Injects the directory header on every request.
pub struct HttpTransport {
    http: reqwest::Client,
    base_url: String,
    directory: String,
}

#[async_trait]
impl Transport for HttpTransport {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn directory(&self) -> &str {
        &self.directory
    }

    async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let mut request = match method {
            "GET" => self.http.get(url),
            "POST" => self.http.post(url),
            "PATCH" => self.http.patch(url),
            "DELETE" => self.http.delete(url),
            other => return Err(SdkError::Http(format!("unsupported method {other}"))),
        }
        .header(crate::DIRECTORY_HEADER, &self.directory);
        if let Some(body) = body {
            request = request.json(body);
        }
        let bytes = request
            .send()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?
            .error_for_status()
            .map_err(|e| SdkError::Http(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| SdkError::Http(e.to_string()))
    }
}

/// Concrete reqwest-backed [`Client`] (HTTP transport). Public constructors are preserved so
/// the binary can attach to an external server with `--server <url>`.
pub type HttpClient = ApiClient<HttpTransport>;

impl HttpClient {
    #[must_use]
    pub fn new(base_url: impl Into<String>, directory: impl Into<String>) -> Self {
        Self::with_http(reqwest::Client::new(), base_url, directory)
    }

    #[must_use]
    pub fn with_http(
        http: reqwest::Client,
        base_url: impl Into<String>,
        directory: impl Into<String>,
    ) -> Self {
        ApiClient::with_transport(HttpTransport {
            http,
            base_url: base_url.into(),
            directory: directory.into(),
        })
    }
}

#[async_trait]
impl<T: Transport> Client for ApiClient<T> {
    fn base_url(&self) -> &str {
        self.transport.base_url()
    }

    fn directory(&self) -> &str {
        self.transport.directory()
    }

    async fn config_get(&self) -> Result<Config> {
        self.get("/config").await
    }

    async fn session_list(&self) -> Result<Vec<Session>> {
        self.get("/session").await
    }

    async fn session_get(&self, session_id: &str) -> Result<Session> {
        self.get(&format!("/session/{session_id}")).await
    }

    async fn session_messages(&self, session_id: &str) -> Result<Vec<(Message, Vec<StoredPart>)>> {
        let raw: Vec<serde_json::Value> =
            self.get(&format!("/session/{session_id}/message")).await?;
        Ok(raw
            .into_iter()
            .filter_map(|entry| {
                let message = serde_json::from_value::<Message>(entry.get("info")?.clone()).ok()?;
                let parts = entry
                    .get("parts")
                    .and_then(|value| value.as_array())
                    .map(|parts| parts.iter().filter_map(StoredPart::from_value).collect())
                    .unwrap_or_default();
                Some((message, parts))
            })
            .collect())
    }

    async fn agents(&self) -> Result<Vec<Agent>> {
        let raw: Vec<serde_json::Value> = self.get("/agent").await?;
        Ok(raw
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect())
    }

    async fn session_todo(&self, session_id: &str) -> Result<Vec<serde_json::Value>> {
        self.get(&format!("/session/{session_id}/todo")).await
    }

    async fn session_diff(&self, session_id: &str) -> Result<Vec<serde_json::Value>> {
        self.get(&format!("/session/{session_id}/diff")).await
    }

    async fn find_files(&self, query: &str) -> Result<Vec<String>> {
        self.get(&format!(
            "/find/file?query={}",
            encode_query_component(query)
        ))
        .await
    }

    async fn commands(&self) -> Result<Vec<String>> {
        let raw: Vec<serde_json::Value> = self.get("/command").await?;
        Ok(raw
            .into_iter()
            .filter_map(|value| {
                value
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(String::from)
            })
            .collect())
    }

    async fn mcp_status(&self) -> Result<Vec<(String, String)>> {
        let raw: serde_json::Value = self.get("/mcp").await?;
        let Some(map) = raw.as_object() else {
            return Ok(Vec::new());
        };
        Ok(map
            .iter()
            .map(|(name, value)| {
                let status = value
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| value.as_str())
                    .unwrap_or_default()
                    .to_owned();
                (name.clone(), status)
            })
            .collect())
    }

    async fn lsp_status(&self) -> Result<Vec<(String, String, String)>> {
        let raw: Vec<serde_json::Value> = self.get("/lsp").await?;
        Ok(raw
            .into_iter()
            .filter_map(|value| {
                let id = value.get("id")?.as_str()?.to_owned();
                let root = value
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let status = value
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                Some((id, root, status))
            })
            .collect())
    }

    async fn formatter_status(&self) -> Result<Vec<String>> {
        let raw: Vec<serde_json::Value> = self.get("/formatter").await?;
        Ok(raw
            .into_iter()
            .filter_map(|value| {
                value
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .filter(|enabled| *enabled)?;
                value
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(String::from)
            })
            .collect())
    }

    async fn plugins(&self) -> Result<Vec<(String, Option<String>)>> {
        let config: Config = self.get("/config").await?;
        let Some(list) = config.rest.get("plugin").and_then(|value| value.as_array()) else {
            return Ok(Vec::new());
        };
        let mut entries: Vec<(String, Option<String>)> = list
            .iter()
            .filter_map(|item| {
                let value = match item {
                    serde_json::Value::String(s) => s.as_str(),
                    serde_json::Value::Array(arr) => arr.first().and_then(|v| v.as_str())?,
                    _ => return None,
                };
                Some(parse_plugin_entry(value))
            })
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    async fn models(&self) -> Result<Vec<(String, String, String, i64, Vec<String>)>> {
        let raw: serde_json::Value = self.get("/config/providers").await?;
        let mut out = Vec::new();
        let Some(providers) = raw.get("providers").and_then(|value| value.as_array()) else {
            return Ok(out);
        };
        for provider in providers {
            let Some(provider_id) = provider.get("id").and_then(|value| value.as_str()) else {
                continue;
            };
            let provider_label = provider
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(provider_id)
                .to_owned();
            let Some(models) = provider.get("models").and_then(|value| value.as_object()) else {
                continue;
            };
            for (model_id, info) in models {
                if info.get("status").and_then(|value| value.as_str()) == Some("deprecated") {
                    continue;
                }
                let title = info
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or(model_id)
                    .to_owned();
                let context_limit = info
                    .get("limit")
                    .and_then(|limit| limit.get("context"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                let variants = info
                    .get("variants")
                    .and_then(serde_json::Value::as_object)
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default();
                out.push((
                    format!("{provider_id}/{model_id}"),
                    title,
                    provider_label.clone(),
                    context_limit,
                    variants,
                ));
            }
        }
        Ok(out)
    }

    async fn session_create(&self) -> Result<Session> {
        self.post("/session", serde_json::json!({})).await
    }

    async fn session_prompt(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.post(&format!("/session/{session_id}/message"), body)
            .await
    }

    async fn session_shell(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.post(&format!("/session/{session_id}/shell"), body)
            .await
    }

    async fn session_command(
        &self,
        session_id: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.post(&format!("/session/{session_id}/command"), body)
            .await
    }

    async fn permission_reply(
        &self,
        request_id: &str,
        reply: &str,
        message: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({ "reply": reply });
        if let Some(message) = message {
            body["message"] = serde_json::Value::String(message.to_owned());
        }
        self.transport
            .request(
                "POST",
                &format!("/permission/{request_id}/reply"),
                Some(&body),
            )
            .await
            .map(|_| ())
    }

    async fn question_reply(
        &self,
        request_id: &str,
        answers: &[Vec<String>],
        directory: Option<&str>,
    ) -> Result<()> {
        self.transport
            .request(
                "POST",
                &question_path(request_id, "reply", directory),
                Some(&serde_json::json!({ "answers": answers })),
            )
            .await
            .map(|_| ())
    }

    async fn question_reject(&self, request_id: &str, directory: Option<&str>) -> Result<()> {
        self.transport
            .request(
                "POST",
                &question_path(request_id, "reject", directory),
                Some(&serde_json::json!({})),
            )
            .await
            .map(|_| ())
    }

    async fn session_rename(&self, session_id: &str, title: &str) -> Result<()> {
        self.transport
            .request(
                "PATCH",
                &format!("/session/{session_id}"),
                Some(&serde_json::json!({ "title": title })),
            )
            .await
            .map(|_| ())
    }

    async fn session_delete(&self, session_id: &str) -> Result<()> {
        self.transport
            .request("DELETE", &format!("/session/{session_id}"), None)
            .await
            .map(|_| ())
    }

    async fn session_compact(
        &self,
        session_id: &str,
        provider_id: &str,
        model_id: &str,
    ) -> Result<()> {
        self.transport
            .request(
                "POST",
                &format!("/session/{session_id}/summarize"),
                Some(&serde_json::json!({ "providerID": provider_id, "modelID": model_id })),
            )
            .await
            .map(|_| ())
    }

    async fn session_revert(&self, session_id: &str, message_id: &str) -> Result<()> {
        self.transport
            .request(
                "POST",
                &format!("/session/{session_id}/revert"),
                Some(&serde_json::json!({ "messageID": message_id })),
            )
            .await
            .map(|_| ())
    }

    async fn session_unrevert(&self, session_id: &str) -> Result<()> {
        self.transport
            .request(
                "POST",
                &format!("/session/{session_id}/unrevert"),
                Some(&serde_json::json!({})),
            )
            .await
            .map(|_| ())
    }

    async fn session_abort(&self, session_id: &str) -> Result<()> {
        self.transport
            .request(
                "POST",
                &format!("/session/{session_id}/abort"),
                Some(&serde_json::json!({})),
            )
            .await
            .map(|_| ())
    }
}

fn parse_plugin_entry(value: &str) -> (String, Option<String>) {
    if let Some(rest) = value.strip_prefix("file://") {
        let mut parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
        let filename = parts.pop().unwrap_or(rest);
        if !filename.contains('.') {
            return (filename.to_owned(), None);
        }
        let basename = filename.split('.').next().unwrap_or(filename);
        if basename == "index" {
            let dirname = parts.pop().unwrap_or(basename);
            return (dirname.to_owned(), None);
        }
        return (basename.to_owned(), None);
    }
    match value.rfind('@') {
        Some(index) if index > 0 => (
            value[..index].to_owned(),
            Some(value[index + 1..].to_owned()),
        ),
        _ => (value.to_owned(), Some("latest".to_owned())),
    }
}

fn question_path(request_id: &str, action: &str, directory: Option<&str>) -> String {
    let path = format!("/question/{request_id}/{action}");
    match directory {
        Some(directory) => format!("{path}?directory={}", encode_query_component(directory)),
        None => path,
    }
}

fn encode_query_component(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_plugin_entry, question_path};

    #[test]
    fn parse_plugin_entry_handles_name_at_version_and_file_urls() {
        assert_eq!(
            parse_plugin_entry("oh-my-openagent@latest"),
            ("oh-my-openagent".to_owned(), Some("latest".to_owned())),
        );
        assert_eq!(
            parse_plugin_entry("@scope/pkg@1.2.3"),
            ("@scope/pkg".to_owned(), Some("1.2.3".to_owned())),
        );
        assert_eq!(
            parse_plugin_entry("bare-name"),
            ("bare-name".to_owned(), Some("latest".to_owned())),
        );
        assert_eq!(
            parse_plugin_entry("@only-scope"),
            ("@only-scope".to_owned(), Some("latest".to_owned())),
        );
        assert_eq!(
            parse_plugin_entry("file:///home/me/plugins/my-plugin.ts"),
            ("my-plugin".to_owned(), None),
        );
        assert_eq!(
            parse_plugin_entry("file:///home/me/plugins/awesome/index.ts"),
            ("awesome".to_owned(), None),
        );
        assert_eq!(
            parse_plugin_entry("file:///home/me/plugins/standalone"),
            ("standalone".to_owned(), None),
        );
    }

    #[test]
    fn question_path_carries_optional_directory_query() {
        assert_eq!(
            question_path("que_1", "reply", Some("/tmp/hya project")),
            "/question/que_1/reply?directory=/tmp/hya%20project"
        );
        assert_eq!(
            question_path("que_1", "reject", None),
            "/question/que_1/reject"
        );
    }
}
