use std::time::Duration;

use hya_api::v2 as pb;
use reqwest::{Client as HttpClient, Method, RequestBuilder};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ClientError;

pub(crate) struct HttpTransport {
    base: String,
    http: HttpClient,
    request_timeout: Option<Duration>,
}

impl HttpTransport {
    pub(crate) fn new(
        base: impl Into<String>,
        http: HttpClient,
        request_timeout: Option<Duration>,
    ) -> Self {
        Self {
            base: base.into(),
            http,
            request_timeout,
        }
    }

    /// The same transport with no request timeout.
    pub(crate) fn unbounded(&self) -> Self {
        Self::new(self.base.clone(), self.http.clone(), None)
    }

    pub(crate) fn request(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        timeout: Option<Duration>,
    ) -> RequestBuilder {
        let mut url = format!("{}{path}", self.base);
        if let Some(query) = query {
            url.push('?');
            url.push_str(query);
        }
        let request = self.http.request(method, url);
        match timeout {
            Some(timeout) => request.timeout(timeout),
            None => request,
        }
    }

    pub(crate) async fn call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Option<&Req>,
    ) -> Result<Resp, ClientError> {
        self.call_with_timeout(method, path, query, body, self.request_timeout)
            .await
    }

    async fn call_with_timeout<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Option<&Req>,
        timeout: Option<Duration>,
    ) -> Result<Resp, ClientError> {
        let mut request = self.request(method, path, query, timeout);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&bytes)
                && let Some(error) = map.get("error")
                && let (Some(code), Some(message)) = (
                    error.get("code").and_then(Value::as_str),
                    error.get("message").and_then(Value::as_str),
                )
            {
                return Err(ClientError::Api {
                    code: code.to_owned(),
                    message: message.to_owned(),
                });
            }
            return Err(ClientError::Api {
                code: status.as_u16().to_string(),
                message: format!("{status}: {}", String::from_utf8_lossy(&bytes))
                    .trim_end()
                    .to_owned(),
            });
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(crate) async fn call_optional_json(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Value, ClientError> {
        let mut request = self.request(method, path, query, self.request_timeout);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&bytes)
                && let Some(error) = map.get("error")
                && let (Some(code), Some(message)) = (
                    error.get("code").and_then(Value::as_str),
                    error.get("message").and_then(Value::as_str),
                )
            {
                return Err(ClientError::Api {
                    code: code.to_owned(),
                    message: message.to_owned(),
                });
            }
            return Err(ClientError::Api {
                code: status.as_u16().to_string(),
                message: format!("{status}: {}", String::from_utf8_lossy(&bytes))
                    .trim_end()
                    .to_owned(),
            });
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(crate) async fn list_bundle_apis(
        &self,
        session: Option<&str>,
        directory: Option<&str>,
    ) -> Result<pb::ListBundleApisResponse, ClientError> {
        let mut params = Vec::new();
        if let Some(session) = session {
            params.push(format!("session={}", encode_component(session)));
        }
        if let Some(directory) = directory {
            params.push(format!("directory={}", encode_component(directory)));
        }
        let query = (!params.is_empty()).then(|| params.join("&"));
        self.call(
            Method::GET,
            "/v2/bundle-apis",
            query.as_deref(),
            None::<&Value>,
        )
        .await
    }

    pub(crate) async fn invoke_session_bundle_api(
        &self,
        session: &str,
        bundle: &str,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Value, ClientError> {
        let rel = path.trim_start_matches('/');
        let route = format!(
            "/v2/sessions/{}/bundles/{}/{}",
            encode_component(session),
            encode_component(bundle),
            rel,
        );
        self.call_optional_json(method, &route, query, body).await
    }

    pub(crate) async fn invoke_global_bundle_api(
        &self,
        bundle: &str,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Value, ClientError> {
        let rel = path.trim_start_matches('/');
        let route = format!("/v2/bundles/{}/api/{}", encode_component(bundle), rel,);
        self.call_optional_json(method, &route, query, body).await
    }

    pub(crate) async fn list_sessions(
        &self,
        project: Option<&str>,
        page: Option<&pb::PageRequest>,
    ) -> Result<pb::ListSessionsResponse, ClientError> {
        let mut params = Vec::new();
        if let Some(project) = project {
            params.push(format!("projectId={}", encode_component(project)));
        }
        if let Some(page) = page {
            params.push(format!(
                "page.cursor={}&page.limit={}",
                encode_component(&page.cursor),
                page.limit
            ));
        }
        let query = (!params.is_empty()).then(|| params.join("&"));
        self.call(
            Method::GET,
            "/v2/sessions",
            query.as_deref(),
            None::<&Value>,
        )
        .await
    }

    pub(crate) async fn create_session(
        &self,
        req: &pb::CreateSessionRequest,
    ) -> Result<pb::CreateSessionResponse, ClientError> {
        self.call(Method::POST, "/v2/sessions", None, Some(req))
            .await
    }

    pub(crate) async fn list_projects(&self) -> Result<pb::ListProjectsResponse, ClientError> {
        self.call(Method::GET, "/v2/projects", None, None::<&Value>)
            .await
    }

    pub(crate) async fn get_project(&self, project: &str) -> Result<pb::ProjectInfo, ClientError> {
        self.call(
            Method::GET,
            &format!("/v2/projects/{}", encode_component(project)),
            None,
            None::<&Value>,
        )
        .await
    }

    pub(crate) async fn create_project(
        &self,
        req: &pb::CreateProjectRequest,
    ) -> Result<pb::ProjectInfo, ClientError> {
        self.call(Method::POST, "/v2/projects", None, Some(req))
            .await
    }

    pub(crate) async fn update_project(
        &self,
        req: &pb::UpdateProjectRequest,
    ) -> Result<pb::ProjectInfo, ClientError> {
        self.call(
            Method::PATCH,
            &format!("/v2/projects/{}", encode_component(&req.project)),
            None,
            Some(req),
        )
        .await
    }

    pub(crate) async fn delete_project(
        &self,
        project: &str,
    ) -> Result<pb::DeleteProjectResponse, ClientError> {
        self.call(
            Method::DELETE,
            &format!("/v2/projects/{}", encode_component(project)),
            None,
            None::<&Value>,
        )
        .await
    }

    pub(crate) async fn resolve_project(
        &self,
        path: &str,
    ) -> Result<pb::ResolveProjectResponse, ClientError> {
        let query = format!("path={}", encode_component(path));
        self.call(
            Method::GET,
            "/v2/projects/resolve",
            Some(&query),
            None::<&Value>,
        )
        .await
    }

    pub(crate) async fn ensure_project_for_path(
        &self,
        path: &str,
    ) -> Result<pb::EnsureProjectForPathResponse, ClientError> {
        let req = pb::EnsureProjectForPathRequest {
            path: path.to_owned(),
        };
        self.call(Method::POST, "/v2/projects/ensure", None, Some(&req))
            .await
    }

    pub(crate) async fn create_turn_request(
        &self,
        request: &pb::CreateTurnRequest,
    ) -> Result<pb::CreateTurnResponse, ClientError> {
        self.call(
            Method::POST,
            &format!("/v2/sessions/{}/turns", encode_component(&request.session)),
            None,
            Some(request),
        )
        .await
    }

    pub(crate) async fn wait_turn_request(
        &self,
        request: &pb::WaitTurnRequest,
    ) -> Result<pb::TurnInfo, ClientError> {
        let timeout_ms = request.timeout_ms.min(600_000);
        self.call_with_timeout(
            Method::POST,
            &format!(
                "/v2/sessions/{}/turns/{}/wait",
                encode_component(&request.session),
                encode_component(&request.turn)
            ),
            Some(&format!("timeoutMs={timeout_ms}")),
            None::<&Value>,
            None,
        )
        .await
    }

    pub(crate) async fn prompt_text(
        &self,
        session: &str,
        text: impl Into<String>,
    ) -> Result<pb::TurnInfo, ClientError> {
        let admitted = self
            .create_turn_request(&pb::CreateTurnRequest {
                session: session.to_owned(),
                kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                    text: text.into(),
                    ..Default::default()
                })),
            })
            .await?;
        let turn = admitted
            .turn
            .filter(|turn| !turn.id.is_empty())
            .ok_or_else(|| ClientError::Api {
                code: "invalid_response".into(),
                message: "create turn response missing a turn ID".into(),
            })?;
        self.wait_turn_request(&pb::WaitTurnRequest {
            session: session.to_owned(),
            turn: turn.id,
            timeout_ms: 120_000,
        })
        .await
    }

    pub(crate) async fn list_events_request(
        &self,
        request: &pb::ListEventsRequest,
    ) -> Result<pb::ListEventsResponse, ClientError> {
        self.replay_events(
            &request.session,
            (request.since_seq != 0).then_some(request.since_seq),
            request.limit,
            request.include_raw,
        )
        .await
    }

    pub(crate) async fn replay_events(
        &self,
        session: &str,
        since_seq: Option<u64>,
        limit: u32,
        include_raw: bool,
    ) -> Result<pb::ListEventsResponse, ClientError> {
        let mut params = Vec::new();
        if let Some(seq) = since_seq {
            params.push(format!("sinceSeq={seq}"));
        }
        if limit != 0 {
            params.push(format!("limit={limit}"));
        }
        if include_raw {
            params.push("includeRaw=true".into());
        }
        let query = (!params.is_empty()).then(|| params.join("&"));
        self.call(
            Method::GET,
            &format!("/v2/sessions/{}/events", encode_component(session)),
            query.as_deref(),
            None::<&Value>,
        )
        .await
    }

    pub(crate) async fn list_interactions(
        &self,
        kind: Option<pb::InteractionType>,
    ) -> Result<Vec<pb::Interaction>, ClientError> {
        let query = kind.map(|kind| match kind {
            pb::InteractionType::Permission => "type=PERMISSION",
            pb::InteractionType::Question => "type=QUESTION",
            _ => "",
        });
        let response: pb::ListInteractionsResponse = self
            .call(Method::GET, "/v2/interactions", query, None::<&Value>)
            .await?;
        Ok(response.interactions)
    }

    pub(crate) async fn respond_interaction(
        &self,
        id: &str,
        response: &pb::RespondInteractionRequest,
    ) -> Result<pb::RespondInteractionResponse, ClientError> {
        self.call(
            Method::POST,
            &format!("/v2/interactions/{}/respond", encode_component(id)),
            None,
            Some(response),
        )
        .await
    }
}

/// Percent-encode one path segment or query value (RFC 3986 unreserved
/// characters pass through).
pub(crate) fn encode_component(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
