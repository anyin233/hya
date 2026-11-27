//! ACP v1 stdio frontend to the local database's backend.
mod mcp;
mod turn;

use crate::daemon;
use agent_client_protocol::{
    Client, Error, Result, Stdio,
    schema::ProtocolVersion,
    v1::{ConnectionTo, Responder, schema as acp},
};
use hya_api::v2 as pb;
use hya_sdk::Sdk;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type Editor = ConnectionTo<Client>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Session {
    sdk: Sdk,
    id: String,
    active: Mutex<Option<CancellationToken>>,
    peers: Option<mcp::Peers>,
}

#[derive(Default)]
struct Agent {
    url: String,
    model: Option<String>,
    initialized: AtomicBool,
    sessions: Mutex<HashMap<acp::SessionId, Arc<Session>>>,
    shutdown: CancellationToken,
    work: TaskTracker,
}

impl Agent {
    fn ready(&self) -> Result<()> {
        self.initialized
            .load(Ordering::Acquire)
            .then_some(())
            .ok_or_else(Error::invalid_request)
    }
    fn session(&self, id: &acp::SessionId) -> Result<Arc<Session>> {
        self.ready()?;
        lock(&self.sessions).get(id).cloned().ok_or_else(|| {
            Error::new(-32002, "Unknown session").data(serde_json::json!({"sessionId":id}))
        })
    }
    fn initialize(&self) -> Result<acp::InitializeResponse> {
        if self.initialized.swap(true, Ordering::AcqRel) {
            return Err(Error::invalid_request());
        }
        Ok(acp::InitializeResponse::new(ProtocolVersion::V1)
            .agent_info(acp::Implementation::new("hya", hya_version::VERSION))
            .agent_capabilities(acp::AgentCapabilities::new().session_capabilities(
                acp::SessionCapabilities::new().close(acp::SessionCloseCapabilities::new()),
            )))
    }
    /// Forget the session and cancel its turn. Its MCP peers stop when the
    /// turn task releases the last reference.
    fn close(&self, id: &acp::SessionId) -> Result<acp::CloseSessionResponse> {
        let session = self.session(id)?;
        lock(&self.sessions).remove(id);
        if let Some(token) = lock(&session.active).as_ref() {
            token.cancel();
        }
        Ok(acp::CloseSessionResponse::new())
    }
    async fn new_session(&self, args: acp::NewSessionRequest) -> Result<acp::NewSessionResponse> {
        self.ready()?;
        if !args.cwd.is_absolute()
            || !tokio::fs::metadata(&args.cwd)
                .await
                .is_ok_and(|m| m.is_dir())
        {
            return Err(Error::invalid_params()
                .data("cwd must be an existing absolute directory".to_owned()));
        }
        let dir = args.cwd.to_str().ok_or_else(Error::invalid_params)?;
        let sdk = Sdk::for_local_daemon(&self.url, dir).map_err(Error::into_internal_error)?;
        let model = self.model.clone().unwrap_or_default();
        let (id, peers) = if args.mcp_servers.is_empty() {
            let info = sdk
                .create_session(pb::CreateSessionRequest {
                    workdir: Some(dir.into()),
                    model,
                    ..Default::default()
                })
                .await
                .map_err(api)?;
            (info.id, None)
        } else {
            let (id, peers) = mcp::Peers::open(&self.url, &args.cwd, model, args.mcp_servers)
                .await
                .map_err(|e| Error::invalid_params().data(e.to_string()))?;
            (id, Some(peers))
        };
        let session_id = acp::SessionId::new(id.clone());
        lock(&self.sessions).insert(
            session_id.clone(),
            Arc::new(Session {
                sdk,
                id,
                active: Default::default(),
                peers,
            }),
        );
        Ok(acp::NewSessionResponse::new(session_id))
    }

    fn prompt(
        &self,
        request: acp::PromptRequest,
        responder: Responder<acp::PromptResponse>,
        client: Editor,
    ) -> Result<()> {
        let session = match self.session(&request.session_id) {
            Ok(s) => s,
            Err(e) => return responder.respond_with_error(e),
        };
        if session
            .peers
            .as_ref()
            .is_some_and(|p| p.disconnected.is_cancelled())
        {
            return responder
                .respond_with_error(Error::new(-32011, "Session MCP connection closed"));
        }
        let text = match prompt_text(request.prompt) {
            Ok(t) => t,
            Err(e) => return responder.respond_with_error(e),
        };
        let cancel = self.shutdown.child_token();
        {
            let mut active = lock(&session.active);
            if active.is_some() {
                return responder.respond_with_error(busy());
            }
            *active = Some(cancel.clone());
        }
        self.work.spawn(async move {
            let request_cancel = responder.cancellation();
            let run = turn::run(&session, client, text, &cancel);
            tokio::pin!(run);
            let result = tokio::select! {
                result = &mut run => result,
                () = request_cancel.cancelled() => {cancel.cancel(); run.await}
            };
            lock(&session.active).take();
            let _ = responder.respond_with_result(result.map(acp::PromptResponse::new));
        });
        Ok(())
    }
}
fn busy() -> Error {
    Error::new(-32010, "Session is busy")
}

fn turn_error(code: String, message: String) -> Error {
    if code == "provider_auth" {
        return Error::auth_required().data(serde_json::json!({"code":code,"message":message,"hint":"Configure the provider or run hya login in a terminal."}));
    }
    Error::new(-32011, message).data(serde_json::json!({"code":code}))
}

fn api(error: hya_sdk::SdkError) -> Error {
    match error {
        hya_sdk::SdkError::Api { ref code, .. } if code == "session_busy" => busy(),
        other => Error::into_internal_error(other),
    }
}

fn prompt_text(blocks: Vec<acp::ContentBlock>) -> Result<String> {
    let mut text = String::new();
    for block in blocks {
        match block {
            acp::ContentBlock::Text(chunk) => text.push_str(&chunk.text),
            acp::ContentBlock::ResourceLink(link) => {
                text.push_str(&format!(" [{}]({}) ", link.name, link.uri))
            }
            _ => return Err(Error::invalid_params()),
        }
    }
    Ok(text)
}

pub(super) async fn run(db: String, model: Option<String>, pure: bool) -> anyhow::Result<()> {
    let spec = daemon::DaemonSpec {
        db,
        model: model.clone(),
        pure,
        yolo: false,
        allow_hosts: Vec::new(),
        exe: std::env::current_exe()?,
    };
    let url = daemon::start(&spec, daemon::START_WAIT)
        .await?
        .discovery
        .url;
    let bootstrap = Sdk::for_local_daemon(&url, "")?.bootstrap().await?;
    let version = bootstrap
        .location
        .as_ref()
        .map(|loc| loc.version.as_str())
        .unwrap_or_default();
    anyhow::ensure!(
        semver::Version::parse(version).is_ok_and(|v| v >= semver::Version::new(0, 50, 0)),
        "ACP requires hya backend 0.50.0 or newer (found {version}). Run hya serve restart for this database."
    );
    let agent = Arc::new(Agent {
        url,
        model,
        ..Default::default()
    });
    let init = agent.clone();
    let new = agent.clone();
    let prompt = agent.clone();
    let close = agent.clone();
    let cancel = agent.clone();
    let result = agent_client_protocol::Agent
        .v1()
        .name("hya")
        .on_receive_request(
            async move |_: acp::InitializeRequest, responder, _| {
                responder.respond_with_result(init.initialize())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::NewSessionRequest, responder, _| {
                responder.respond_with_result(new.new_session(request).await)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::PromptRequest, responder, client| {
                prompt.prompt(request, responder, client)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::CloseSessionRequest, responder, _| {
                responder.respond_with_result(close.close(&request.session_id))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |request: acp::CancelNotification, _| {
                if let Ok(session) = cancel.session(&request.session_id)
                    && let Some(token) = lock(&session.active).as_ref()
                {
                    token.cancel()
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Stdio::new())
        .await;
    agent.shutdown.cancel();
    agent.work.close();
    agent.work.wait().await;
    result?;
    Ok(())
}
