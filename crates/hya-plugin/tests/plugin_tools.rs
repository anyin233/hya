//! Integration tests for `hya-plugin`: plugin tools.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::config::ProcessLaunch;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::SessionId;
use hya_tool::{
    Action, FormatterPlane, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules,
    Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, WebSearchPlane, handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn owner_fixture(root: &std::path::Path, launch: ProcessLaunch) -> PluginSpec {
    let script = r#"
import json, os, sys
with open(os.path.join(os.environ['OWNER_ROOT'], 'starts'), 'a') as f:
    f.write(str(os.getpid())+'\n')
metadata = None
for line in sys.stdin:
    req = json.loads(line)
    method = req.get('method')
    if method == 'initialize':
        metadata = req['params']
        result = {'protocol_version':1,'plugin':{'id':'toolbox','version':'1','kind':'bun'},
                  'hooks':[{'name':'tool.execute.before'}],
                  'tools':[{'name':name,'description':name,'inputSchema':{'type':'object'}} for name in ['alpha','beta']]}
    elif method == 'tool/call':
        if req['params']['input'].get('crash'): os._exit(1)
        result = {'ok':True,'output':{'tool':req['params']['tool'],'pid':os.getpid(),
                  'metadata':metadata,'cwd':os.getcwd(),'env':os.environ.get('OWNER_VALUE')},'time_ms':0}
    elif method == 'hook/tool.execute.before':
        if req['params']['input'].get('crash'): os._exit(1)
        result = {'outcome':'continue','input':req['params']['input']}
    elif method == 'shutdown':
        with open(os.path.join(os.environ['OWNER_ROOT'], 'shutdown'), 'w') as f: f.write('done')
        print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':{}}), flush=True)
        break
    else: continue
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}), flush=True)
"#;
    PluginSpec {
        id: "toolbox".into(),
        kind: PluginKindWire::Bun,
        command: vec!["python3".into(), "-c".into(), script.into()],
        timeout_ms: Some(1000),
        env: BTreeMap::from([
            ("OWNER_ROOT".into(), root.to_string_lossy().into_owned()),
            ("OWNER_VALUE".into(), "retained".into()),
        ]),
        plugin_dir: None,
        launch,
    }
}

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
        plugin_dir: None,
        launch: Default::default(),
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
        roots: vec![PathBuf::from(".")],
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

#[tokio::test]
async fn shared_process_launch_policies_respawn_through_the_same_owner() {
    for mode in ["config", "project", "bundle"] {
        let root = std::env::temp_dir().join(format!("hya-owner-{}", SessionId::new()));
        std::fs::create_dir(&root).unwrap();
        let launch = match mode {
            "config" => ProcessLaunch::Config,
            "project" => ProcessLaunch::Project(root.clone()),
            _ => ProcessLaunch::Bundle {
                root: root.clone(),
                activation: None,
            },
        };
        let host = PluginHost::connect(owner_fixture(&root, launch), host_info(), None)
            .await
            .unwrap();
        let plugin = host.prepared_plugins().remove(0);
        assert_eq!(plugin.contributions().tools.len(), 2);
        let alpha = plugin.tool("alpha", "selected-alpha").unwrap();
        let ctx = ctx_with(SessionId::new());
        let before = alpha.execute(&ctx, json!({})).await.unwrap();
        assert!(alpha.execute(&ctx, json!({"crash":true})).await.is_err());
        let after = alpha.execute(&ctx, json!({})).await.unwrap();
        assert_ne!(before["pid"], after["pid"]);
        assert_eq!(after["env"], "retained");
        assert_eq!(after["tool"], "alpha");
        assert!(after["metadata"].get("activation_id").is_none());
        if mode != "config" {
            assert_eq!(after["cwd"], root.to_string_lossy().as_ref());
        }
        assert_eq!(
            std::fs::read_to_string(root.join("starts"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        host.shutdown().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("shutdown")).unwrap(),
            "done"
        );
        assert!(
            alpha.execute(&ctx, json!({})).await.is_err(),
            "a retained view must never restart a stopped owner"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("starts"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn private_activation_keeps_metadata_and_reports_hook_loss_without_respawning() {
    use hya_core::hooks::{HookDispatcher, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};
    use hya_plugin::messages::{ActivationLifecycle, ActivationMetadata};
    let root = std::env::temp_dir().join(format!("hya-owner-{}", SessionId::new()));
    std::fs::create_dir(&root).unwrap();
    let host = PluginHost::connect(
        owner_fixture(
            &root,
            ProcessLaunch::Bundle {
                root: root.clone(),
                activation: Some(ActivationMetadata {
                    activation_id: "private".into(),
                    lifecycle: ActivationLifecycle::Resident,
                }),
            },
        ),
        host_info(),
        None,
    )
    .await
    .unwrap();
    let plugin = host.prepared_plugins().remove(0);
    let loss = plugin.loss_token().unwrap();
    let alpha = plugin.tool("alpha", "selected-alpha").unwrap();
    let mut ctx = ctx_with(SessionId::new());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "selected-alpha",
        Mode::Allow,
    )]));
    ctx.permission = permission;
    let output = alpha.execute(&ctx, json!({})).await.unwrap();
    assert_eq!(output["metadata"]["activation_id"], "private");
    assert_eq!(output["metadata"]["lifecycle"], "resident");
    let input = || ToolExecuteBeforeInput {
        session: ctx.session.unwrap(),
        message: hya_proto::MessageId::new(),
        call: hya_proto::ToolCallId::new(),
        tool: "selected-alpha".into(),
        input: json!({"crash":true}),
    };
    assert!(matches!(
        host.tool_execute_before(input()).await,
        ToolExecuteBeforeOutcome::Veto { .. }
    ));
    assert!(loss.is_cancelled());
    assert!(!HookDispatcher::is_healthy(&host));
    assert!(matches!(
        host.tool_execute_before(input()).await,
        ToolExecuteBeforeOutcome::Veto { .. }
    ));
    assert!(matches!(
        alpha.execute(&ctx, json!({})).await,
        Err(hya_tool::ToolError::Cancelled)
    ));
    host.terminate().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("starts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
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
    let host = PluginHost::connect(bundle_spec.in_bundle(root), host_info(), None)
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
