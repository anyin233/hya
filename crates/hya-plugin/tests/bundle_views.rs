//! Bundle-process capabilities for every process kind plus `view/get`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_core::bundle_views::{SessionUsageReport, UsageReport};
use hya_core::{BundleViewProvider, CoreError, HostSessionReads, UsageScope};
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

/// A bundle process fixture: one tool `usage_tool` and one view `usage`, both
/// answering by calling `context.describe` then `session.usage` with the
/// scope named in the input/query.
const FIXTURE: &str = r#"import json,sys
seq=[100]
def capability(token, session, call, operation, params):
  seq[0]+=1
  outbound={'jsonrpc':'2.0','id':seq[0],'method':'host/capability','params':{'capability':token,'session':session,'call':call,'method':operation,'params':params}}
  print(json.dumps(outbound),flush=True)
  return json.loads(sys.stdin.readline())
for line in sys.stdin:
  request=json.loads(line)
  method=request.get('method')
  p=request.get('params',{})
  if method=='initialize':
    result={'protocol_version':1,'plugin':{'id':'viewer','version':'0.1.0','kind':'bun'},'hooks':[],'tools':[{'name':'usage_tool','description':'usage','inputSchema':{'type':'object'}}],'views':VIEWS}
  elif method=='tool/call':
    token=p.get('host_capability')
    if token is None:
      result={'ok':True,'output':{'capability':None}}
    else:
      context=capability(token,p['session'],p['call'],'context.describe',{})
      usage=capability(token,p['session'],p['call'],'session.usage',{'scope':p['input'].get('scope','tree')})
      result={'ok':True,'output':{'context':context.get('result'),'usage':usage.get('result'),'error':usage.get('error')}}
  elif method=='view/get':
    token=p['host_capability']
    context=capability(token,p['session'],p['call'],'context.describe',{})
    usage=capability(token,p['session'],p['call'],'session.usage',{'scope':p['query'].get('scope','tree')})
    denied=capability(token,p['session'],p['call'],'permission.assert',{'action':'read','resource':{'kind':'path','value':'/x'}})
    result={'body':{'view':p['view'],'query':p['query'],'context':context.get('result'),'usage':usage.get('result'),'usage_error':usage.get('error'),'denied':denied.get('error')}}
  else:
    result={}
  if 'id' in request:
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#;

fn fixture(views: &str) -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        FIXTURE.replace("VIEWS", views),
    ]
}

fn spec(kind: PluginKindWire, views: &str) -> PluginSpec {
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), std::env::var("PATH").unwrap());
    PluginSpec {
        id: "viewer".to_string(),
        kind,
        command: fixture(views),
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
        workdir: PathBuf::from("/tmp/bundle-views"),
        cancel: CancellationToken::new(),
    }
}

async fn bundle_host(kind: PluginKindWire, reads: Arc<FakeReads>) -> PluginHost {
    PluginHost::connect_bundle_with_reads(
        spec(kind, "[{'name':'usage','description':'Token usage'}]"),
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
    assert_eq!(output["context"]["workdir"], "/tmp/bundle-views");
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

#[tokio::test]
async fn view_request_gets_a_session_bound_read_only_capability() {
    let reads = Arc::new(FakeReads::default());
    let host = bundle_host(PluginKindWire::Bun, reads.clone()).await;
    assert_eq!(host.declared_views()[0].name, "usage");
    let session = SessionId::new();
    let query = BTreeMap::from([("scope".to_string(), "session".to_string())]);
    let body = host.get_view("usage", session, query).await.unwrap();
    assert_eq!(body["view"], "usage");
    assert_eq!(body["query"]["scope"], "session");
    assert_eq!(body["context"]["request"], "view");
    assert_eq!(body["context"]["view"], "usage");
    assert_eq!(
        body["context"]["session"],
        serde_json::to_value(session).unwrap()
    );
    assert_eq!(body["usage"]["scope"], "session");
    assert_eq!(
        body["denied"]["code"],
        hya_plugin::protocol::codes::CAPABILITY_DENIED,
        "view requests cannot assert permissions"
    );
    assert_eq!(
        *reads.calls.lock().unwrap(),
        vec![(session, UsageScope::Session)]
    );

    // A view request may not widen its read to the lineage root.
    let query = BTreeMap::from([("scope".to_string(), "root".to_string())]);
    let body = host.get_view("usage", session, query).await.unwrap();
    assert_eq!(body["usage"], Value::Null);
    assert_eq!(
        body["usage_error"]["code"],
        hya_plugin::protocol::codes::INVALID_PARAMS
    );
}

#[tokio::test]
async fn undeclared_view_is_refused_by_the_host() {
    let host = bundle_host(PluginKindWire::Rust, Arc::new(FakeReads::default())).await;
    let error = host
        .get_view("missing", SessionId::new(), BTreeMap::new())
        .await
        .unwrap_err();
    assert!(error.contains("missing"), "{error}");
}

#[tokio::test]
async fn duplicate_initialize_views_are_rejected() {
    let result = PluginHost::connect_bundle_with_reads(
        spec(PluginKindWire::Bun, "[{'name':'usage'},{'name':'usage'}]"),
        host_info(),
        std::env::temp_dir(),
        None,
    )
    .await;
    let Err(error) = result else {
        panic!("duplicate view declarations must fail initialize");
    };
    assert!(error.to_string().contains("usage"), "{error}");
}
