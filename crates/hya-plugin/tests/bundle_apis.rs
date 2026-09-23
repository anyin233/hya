//! Bundle-process capabilities for every process kind plus `api/request`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_core::bundle_apis::{SessionUsageReport, UsageReport};
use hya_core::{
    ApiMethod, BundleApiProvider, BundleApiRequest, CoreError, HostSessionReads, UsageScope,
};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::SessionId;
use hya_tool::{
    FormatterPlane, InteractionPlane, LspPlane, PermissionPlane, PermissionRules, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, WebSearchPlane, handle::ArtifactPlane,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// A bundle process fixture: one tool `usage_tool` and the API endpoints
/// `APIS`. Tool calls and API requests answer by calling `context.describe`
/// then `session.usage` (scope from the input/query) on their lease; API
/// requests also try `permission.assert` and echo the routed request.
/// Endpoint `malformed` answers an unknown result field; `plain` omits
/// `status`; every other endpoint answers status 201.
const FIXTURE: &str = r#"import json,sys
seq=[100]
def capability(token, session, call, operation, params):
  seq[0]+=1
  inner={'capability':token,'call':call,'method':operation,'params':params}
  if session is not None:
    inner['session']=session
  print(json.dumps({'jsonrpc':'2.0','id':seq[0],'method':'host/capability','params':inner}),flush=True)
  return json.loads(sys.stdin.readline())
for line in sys.stdin:
  request=json.loads(line)
  method=request.get('method')
  p=request.get('params',{})
  if method=='initialize':
    result={'protocol_version':1,'plugin':{'id':'apis','version':'0.1.0','kind':'bun'},'hooks':[],'tools':[{'name':'usage_tool','description':'usage','inputSchema':{'type':'object'}}],'apis':APIS}
  elif method=='tool/call':
    token=p.get('host_capability')
    if token is None:
      result={'ok':True,'output':{'capability':None}}
    else:
      context=capability(token,p['session'],p['call'],'context.describe',{})
      usage=capability(token,p['session'],p['call'],'session.usage',{'scope':p['input'].get('scope','tree')})
      result={'ok':True,'output':{'context':context.get('result'),'usage':usage.get('result'),'error':usage.get('error')}}
  elif method=='api/request':
    token=p['host_capability']
    session=p.get('session')
    context=capability(token,session,p['call'],'context.describe',{})
    usage=capability(token,session,p['call'],'session.usage',{'scope':p['query'].get('scope','tree')})
    denied=capability(token,session,p['call'],'permission.assert',{'action':'read','resource':{'kind':'path','value':'/x'}})
    body={'request':p,'context':context.get('result'),'usage':usage.get('result'),'usage_error':usage.get('error'),'denied':denied.get('error')}
    if p['api']=='malformed':
      result={'body':body,'headers':{}}
    elif p['api']=='plain':
      result={'body':body}
    else:
      result={'status':201,'body':body}
  else:
    result={}
  if 'id' in request:
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#;

const APIS: &str = "[{'name':'usage','description':'Token usage'},{'name':'echo'},{'name':'plain'},{'name':'malformed'}]";

fn fixture(apis: &str) -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        FIXTURE.replace("APIS", apis),
    ]
}

fn spec(kind: PluginKindWire, apis: &str) -> PluginSpec {
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), std::env::var("PATH").unwrap());
    PluginSpec {
        id: "apis".to_string(),
        kind,
        command: fixture(apis),
        timeout_ms: Some(5000),
        env,
        posture_overrides: BTreeMap::new(),
        plugin_dir: None,
    }
}

fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: "0.0.0".to_string(),
    }
}

/// Records every `session.usage` call and answers a fixed report.
#[derive(Default)]
struct FakeReads {
    calls: Mutex<Vec<(SessionId, UsageScope)>>,
}

#[async_trait]
impl HostSessionReads for FakeReads {
    async fn session_usage(
        &self,
        session: SessionId,
        scope: UsageScope,
    ) -> Result<SessionUsageReport, CoreError> {
        self.calls.lock().unwrap().push((session, scope));
        Ok(SessionUsageReport {
            session,
            scope,
            root: session,
            sessions: Vec::new(),
            total: UsageReport::default(),
            truncated: false,
        })
    }
}

fn ctx_with(session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        formatter: FormatterPlane::default(),
        agents: Default::default(),
        lsp: LspPlane::default(),
        workdir: PathBuf::from("/tmp/bundle-apis"),
        cancel: CancellationToken::new(),
    }
}

async fn bundle_host(kind: PluginKindWire, reads: Arc<FakeReads>) -> PluginHost {
    PluginHost::connect_bundle_with_reads(
        spec(kind, APIS),
        host_info(),
        std::env::temp_dir(),
        Some(reads),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn bun_bundle_tool_call_receives_capability_with_session_usage() {
    let reads = Arc::new(FakeReads::default());
    let host = bundle_host(PluginKindWire::Bun, reads.clone()).await;
    let session = SessionId::new();
    let output = host.tools()[0]
        .execute(&ctx_with(session), json!({"scope": "root"}))
        .await
        .unwrap();
    assert_eq!(output["context"]["request"], "tool_call");
    assert_eq!(output["context"]["workdir"], "/tmp/bundle-apis");
    assert_eq!(
        output["context"]["session"],
        serde_json::to_value(session).unwrap()
    );
    assert_eq!(output["usage"]["scope"], "root");
    assert_eq!(
        output["usage"]["session"],
        serde_json::to_value(session).unwrap()
    );
    assert_eq!(
        *reads.calls.lock().unwrap(),
        vec![(session, UsageScope::Root)],
        "a tool call reads its own session tree, rooted as requested"
    );
}

#[tokio::test]
async fn configured_plugin_tool_call_gets_no_capability() {
    let host = PluginHost::connect_all(vec![spec(PluginKindWire::Bun, "[]")], host_info()).await;
    let output = host.tools()[0]
        .execute(&ctx_with(SessionId::new()), json!({}))
        .await
        .unwrap();
    assert_eq!(output["capability"], Value::Null);
}

fn request(api: &str, session: Option<SessionId>, query: &[(&str, &str)]) -> BundleApiRequest {
    BundleApiRequest {
        api: api.to_string(),
        method: ApiMethod::Post,
        path: "/items/a%2Fb".to_string(),
        path_params: BTreeMap::from([("id".to_string(), "a/b".to_string())]),
        query: query
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        body: json!({"value": 7}),
        session,
    }
}

#[tokio::test]
async fn session_api_request_gets_a_session_bound_read_only_capability() {
    let reads = Arc::new(FakeReads::default());
    let host = bundle_host(PluginKindWire::Bun, reads.clone()).await;
    assert_eq!(host.declared_apis()[0].name, "usage");
    let session = SessionId::new();
    let reply = host
        .request(request("usage", Some(session), &[("scope", "session")]))
        .await
        .unwrap();
    assert_eq!(reply.status, 201, "the process status passes through");
    let body = reply.body;
    let session_json = serde_json::to_value(session).unwrap();
    // The routed request reaches the process verbatim.
    let sent = &body["request"];
    assert_eq!(sent["api"], "usage");
    assert_eq!(sent["method"], "POST");
    assert_eq!(sent["path"], "/items/a%2Fb");
    assert_eq!(sent["path_params"], json!({"id": "a/b"}));
    assert_eq!(sent["query"], json!({"scope": "session"}));
    assert_eq!(sent["body"], json!({"value": 7}));
    assert_eq!(sent["session"], session_json);
    // The capability describes the request and is bound to the session.
    assert_eq!(body["context"]["request"], "api");
    assert_eq!(body["context"]["api"], "usage");
    assert_eq!(body["context"]["scope"], "session");
    assert_eq!(body["context"]["session"], session_json);
    assert_eq!(
        body["context"]["call"], sent["call"],
        "context.describe reports the call id the request carried"
    );
    assert_eq!(body["usage"]["scope"], "session");
    assert_eq!(
        body["denied"]["code"],
        hya_plugin::protocol::codes::CAPABILITY_DENIED,
        "API requests cannot assert permissions"
    );
    assert_eq!(
        *reads.calls.lock().unwrap(),
        vec![(session, UsageScope::Session)]
    );

    // A session API request may not widen its read to the lineage root.
    let body = host
        .request(request("usage", Some(session), &[("scope", "root")]))
        .await
        .unwrap()
        .body;
    assert_eq!(body["usage"], Value::Null);
    assert_eq!(
        body["usage_error"]["code"],
        hya_plugin::protocol::codes::INVALID_PARAMS
    );
}

#[tokio::test]
async fn global_api_request_gets_a_sessionless_capability() {
    let reads = Arc::new(FakeReads::default());
    let host = bundle_host(PluginKindWire::Rust, reads.clone()).await;
    let body = host.request(request("echo", None, &[])).await.unwrap().body;
    assert!(
        body["request"].get("session").is_none(),
        "a global request carries no session: {body}"
    );
    assert_eq!(body["context"]["request"], "api");
    assert_eq!(body["context"]["scope"], "global");
    assert!(body["context"].get("session").is_none(), "{body}");
    assert_eq!(body["usage"], Value::Null);
    assert_eq!(
        body["usage_error"]["code"],
        hya_plugin::protocol::codes::CAPABILITY_DENIED,
        "session.usage needs a session"
    );
    assert!(reads.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn api_reply_status_defaults_to_200_and_unknown_fields_fail() {
    let host = bundle_host(PluginKindWire::Bun, Arc::new(FakeReads::default())).await;
    let reply = host.request(request("plain", None, &[])).await.unwrap();
    assert_eq!(reply.status, 200);
    let error = host
        .request(request("malformed", None, &[]))
        .await
        .unwrap_err();
    assert!(error.contains("headers"), "{error}");
}

#[tokio::test]
async fn undeclared_api_is_refused_by_the_host() {
    let host = bundle_host(PluginKindWire::Rust, Arc::new(FakeReads::default())).await;
    let error = host
        .request(request("missing", None, &[]))
        .await
        .unwrap_err();
    assert!(error.contains("missing"), "{error}");
}

#[tokio::test]
async fn duplicate_initialize_apis_are_rejected() {
    let result = PluginHost::connect_bundle_with_reads(
        spec(PluginKindWire::Bun, "[{'name':'usage'},{'name':'usage'}]"),
        host_info(),
        std::env::temp_dir(),
        None,
    )
    .await;
    let Err(error) = result else {
        panic!("duplicate API declarations must fail initialize");
    };
    assert!(error.to_string().contains("usage"), "{error}");
}
