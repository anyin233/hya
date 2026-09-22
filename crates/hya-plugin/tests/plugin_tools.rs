//! Integration tests for `hya-plugin`: plugin tools.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::SessionId;
use hya_tool::{
    Action, FormatterPlane, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules,
    Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, WebSearchPlane, handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn tool_fixture() -> Vec<String> {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "toolbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Remember a fact",
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"]
                },
            }],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "tool/call":
        params = msg["params"]
        result = {
            "ok": True,
            "output": {
                "tool": params["tool"],
                "value": params["input"]["value"],
                "session": params["session"],
            },
            "time_ms": 4,
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

fn spec() -> PluginSpec {
    PluginSpec {
        id: "toolbox".to_string(),
        kind: PluginKindWire::Rust,
        command: tool_fixture(),
        timeout_ms: Some(3000),
        env: BTreeMap::new(),
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
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn declared_plugin_tool_is_callable() {
    let host = PluginHost::connect_all(vec![spec()], host_info()).await;
    let tools = host.tools();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "remember");
    assert_eq!(tools[0].schema().description, "Remember a fact");

    let session = SessionId::new();
    let out = tools[0]
        .execute(&ctx_with(session), json!({"value": "ship it"}))
        .await
        .unwrap();

    assert_eq!(out["tool"], "remember");
    assert_eq!(out["value"], "ship it");
    assert_eq!(out["session"], serde_json::to_value(session).unwrap());
}

#[tokio::test]
async fn bundle_native_tool_receives_call_scoped_context_and_resource_denial() {
    let script = r#"import json,sys
for line in sys.stdin:
 request=json.loads(line)
 method=request.get('method')
 if method=='initialize':
  result={'protocol_version':1,'plugin':{'id':'toolbox','version':'0.1.0','kind':'rust'},'hooks':[],'tools':[{'name':'native','description':'native test','inputSchema':{'type':'object'}}]}
 elif method=='tool/call':
  p=request['params']; token=p['host_capability']
  def capability(seq, operation, params):
   outbound={'jsonrpc':'2.0','id':seq,'method':'host/capability','params':{'capability':token,'session':p['session'],'call':p['call'],'method':operation,'params':params}}
   print(json.dumps(outbound),flush=True)
   return json.loads(sys.stdin.readline())
  context=capability(51,'context.describe',{})
  denial=capability(52,'permission.assert',{'action':'read','resource':{'kind':'path','value':'/private/blocked'}})
  result={'ok':True,'output':{'context':context.get('result'),'denial':denial.get('error')}}
 else: result={}
 if 'id' in request: print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#;
    let mut bundle_spec = spec();
    bundle_spec.command = vec!["python3".into(), "-c".into(), script.into()];
    bundle_spec
        .env
        .insert("PATH".into(), std::env::var("PATH").unwrap());
    let root = std::env::temp_dir();
    let host = PluginHost::connect_bundle(bundle_spec, host_info(), root)
        .await
        .unwrap();
    let session = SessionId::new();
    let mut ctx = ctx_with(session);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/private/blocked",
        Mode::Deny,
    )]));
    ctx.permission = permission.for_session(session);
    ctx.workdir = PathBuf::from("/tmp/native-tool-context");
    let output = host.tools()[0].execute(&ctx, json!({})).await.unwrap();
    assert_eq!(output["context"]["workdir"], "/tmp/native-tool-context");
    assert_eq!(
        output["context"]["session"],
        serde_json::to_value(session).unwrap()
    );
    assert_eq!(
        output["denial"]["code"],
        hya_plugin::protocol::codes::PERMISSION_DENIED
    );
}
