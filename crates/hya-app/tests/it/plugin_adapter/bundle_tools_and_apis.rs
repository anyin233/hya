//! Bundle tool capabilities, `api/request` leases, shared/private process
//! launch ownership, and crash-restart / disable budgets.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_app::plugin_adapter::{PluginHostAdapter, session_usage_reader};
use hya_engine::bundle_apis::{SessionUsageReport, UsageReport};
use hya_engine::hooks::{HookDispatcher, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};
use hya_engine::{
    ApiMethod, BundleApiProvider, BundleApiRequest, CoreError, HostSessionReads, UsageScope,
};
use hya_plugin::config::{PluginSpec, ProcessLaunch};
use hya_plugin::messages::{ActivationLifecycle, ActivationMetadata, PluginKindWire};
use hya_plugin::protocol::codes;
use hya_plugin::{PluginHost, PluginStatus};
use hya_proto::{MessageId, SessionId, ToolCallId, ToolName, ToolSchema};
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolError};
use serde_json::{Value, json};

use super::{ctx_with, host_info, py_spec};

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

struct FakeNested;

#[async_trait]
impl hya_tool::NestedTools for FakeNested {
    fn list(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: ToolName::new("echo"),
            description: "echo input".into(),
            input_schema: json!({"type": "object"}),
            output_schema: Some(json!({"type": "object"})),
        }]
    }

    async fn call(&self, name: &str, input: Value) -> Result<Value, ToolError> {
        match name {
            "echo" => Ok(json!({"echo": input})),
            "denied" => Err(ToolError::Permission(
                hya_tool::PermissionError::Unavailable,
            )),
            "big" => Ok(json!("x".repeat(800 * 1024))),
            _ => Err(ToolError::Other(format!("unknown tool: {name}"))),
        }
    }

    async fn workspace_state(&self) -> Result<Option<String>, ToolError> {
        Ok(Some("session:7".into()))
    }
}

const CAPABILITY_FIXTURE: &str = r#"import json,sys
seq=[100]
def cap(token, session, call, op, params):
  seq[0]+=1
  inner={'capability':token,'call':call,'method':op,'params':params}
  if session is not None: inner['session']=session
  print(json.dumps({'jsonrpc':'2.0','id':seq[0],'method':'host/capability','params':inner}),flush=True)
  return json.loads(sys.stdin.readline())
for line in sys.stdin:
  req=json.loads(line); m=req.get('method'); p=req.get('params',{})
  if m=='initialize':
    res={'protocol_version':1,'plugin':{'id':'apis','version':'0.1.0','kind':'bun'},'hooks':[],
         'tools':[{'name':'probe','description':'Probe capabilities','inputSchema':{'type':'object'}}],
         'apis':APIS}
  elif m=='tool/call':
    tok=p.get('host_capability'); mode=p['input'].get('mode','basic')
    if tok is None:
      res={'ok':True,'output':{'capability':None,'tool':p['tool'],'value':p['input'].get('value'),'session':p['session']}}
    elif mode=='nested':
      res={'ok':True,'output':{
        'listed':cap(tok,p['session'],p['call'],'tools.list',{}),
        'list_params':cap(tok,p['session'],p['call'],'tools.list',{'x':1}),
        'echo':cap(tok,p['session'],p['call'],'tool.call',{'name':'echo','input':{'a':1}}),
        'not_object':cap(tok,p['session'],p['call'],'tool.call',{'name':'echo','input':[1]}),
        'denied':cap(tok,p['session'],p['call'],'tool.call',{'name':'denied','input':{}}),
        'failed':cap(tok,p['session'],p['call'],'tool.call',{'name':'nope','input':{}}),
        'big':cap(tok,p['session'],p['call'],'tool.call',{'name':'big','input':{}}),
        'state':cap(tok,p['session'],p['call'],'workspace.state',{}),
        'state_invalid':cap(tok,p['session'],p['call'],'workspace.state',{'x':1}),
      }}
    else:
      ctx=cap(tok,p['session'],p['call'],'context.describe',{})
      usage=cap(tok,p['session'],p['call'],'session.usage',{'scope':p['input'].get('scope','tree')})
      denial=cap(tok,p['session'],p['call'],'permission.assert',{'action':'read','resource':{'kind':'path','value':'/private/blocked'}})
      res={'ok':True,'output':{'context':ctx.get('result'),'usage':usage.get('result'),'denial':denial.get('error')}}
  elif m=='api/request':
    tok=p['host_capability']; sess=p.get('session')
    ctx=cap(tok,sess,p['call'],'context.describe',{})
    usage=cap(tok,sess,p['call'],'session.usage',{'scope':p['query'].get('scope','tree')})
    body={'request':p,'context':ctx.get('result'),'usage':usage.get('result'),'usage_error':usage.get('error'),
          'denied':cap(tok,sess,p['call'],'permission.assert',{'action':'read','resource':{'kind':'path','value':'/x'}}).get('error'),
          'tools_list':cap(tok,sess,p['call'],'tools.list',{}).get('error'),
          'tool_call':cap(tok,sess,p['call'],'tool.call',{'name':'read','input':{}}).get('error'),
          'workspace_state':cap(tok,sess,p['call'],'workspace.state',{}).get('error')}
    if p['api']=='malformed': res={'body':body,'headers':{}}
    elif p['api']=='plain': res={'body':body}
    else: res={'status':201,'body':body}
  else: res={}
  if 'id' in req: print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':res}),flush=True)
"#;

fn api_spec(kind: PluginKindWire, apis: &str) -> PluginSpec {
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), std::env::var("PATH").unwrap());
    PluginSpec {
        id: "apis".into(),
        kind,
        command: vec![
            "python3".into(),
            "-c".into(),
            CAPABILITY_FIXTURE.replace("APIS", apis),
        ],
        timeout_ms: Some(5000),
        env,
        plugin_dir: None,
        launch: Default::default(),
    }
}

fn api_req(api: &str, session: Option<SessionId>, query: &[(&str, &str)]) -> BundleApiRequest {
    BundleApiRequest {
        api: api.into(),
        method: ApiMethod::Post,
        path: "/items/a%2Fb".into(),
        path_params: BTreeMap::from([("id".into(), "a/b".into())]),
        query: query
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        body: json!({"value": 7}),
        session,
    }
}

#[tokio::test]
async fn bundle_tool_and_api_capabilities_enforced_over_lease() {
    let apis = "[{'name':'usage','description':'Token usage'},{'name':'echo'},{'name':'plain'},{'name':'malformed'}]";
    let reads = Arc::new(FakeReads::default());
    let bundle_host = Arc::new(
        PluginHost::connect(
            api_spec(PluginKindWire::Bun, apis).in_bundle(std::env::temp_dir()),
            host_info(),
            Some(session_usage_reader(reads.clone())),
        )
        .await
        .unwrap(),
    );
    assert_eq!(bundle_host.declared_apis()[0].name, "usage");

    let session = SessionId::new();
    let session_json = serde_json::to_value(session).unwrap();
    let mut ctx = ctx_with(session, "/tmp/bundle-apis");
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/private/blocked",
        Mode::Deny,
    )]));
    ctx.permission = permission.for_session(session);
    let tool = &bundle_host.tools()[0];
    assert_eq!(tool.name(), "probe");
    assert_eq!(tool.schema().description, "Probe capabilities");

    // 1. Bundle tool call: context.describe, session.usage (root scope), permission.assert denial.
    let basic = tool.execute(&ctx, json!({"scope": "root"})).await.unwrap();
    assert_eq!(basic["context"]["request"], "tool_call");
    assert_eq!(basic["context"]["workdir"], "/tmp/bundle-apis");
    assert_eq!(basic["context"]["session"], session_json);
    assert_eq!(basic["usage"]["scope"], "root");
    assert_eq!(basic["denial"]["code"], codes::PERMISSION_DENIED);
    assert_eq!(
        *reads.calls.lock().unwrap(),
        vec![(session, UsageScope::Root)]
    );

    // 2. Nested tools + workspace.state (detached vs attached).
    let detached = tool.execute(&ctx, json!({"mode": "nested"})).await.unwrap();
    assert_eq!(detached["state"]["result"], json!({"state": null}));
    assert_eq!(
        detached["state_invalid"]["error"]["code"],
        codes::INVALID_PARAMS
    );

    ctx.nested = hya_tool::NestedToolPlane::new(Arc::new(FakeNested));
    let nested = tool.execute(&ctx, json!({"mode": "nested"})).await.unwrap();
    assert_eq!(
        nested["listed"]["result"],
        json!({"tools": [{"name": "echo", "description": "echo input", "input_schema": {"type": "object"}}]})
    );
    assert_eq!(
        nested["list_params"]["error"]["code"],
        codes::INVALID_PARAMS
    );
    assert_eq!(
        nested["echo"]["result"],
        json!({"output": {"echo": {"a": 1}}})
    );
    assert_eq!(nested["not_object"]["error"]["code"], codes::INVALID_PARAMS);
    assert_eq!(nested["denied"]["error"]["code"], codes::PERMISSION_DENIED);
    assert_eq!(nested["failed"]["error"]["code"], codes::TOOL_FAILED);
    assert_eq!(nested["failed"]["error"]["message"], "unknown tool: nope");
    assert_eq!(nested["big"]["error"]["code"], codes::TOOL_FAILED);
    assert!(
        nested["big"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("too large")
    );
    assert_eq!(nested["state"]["result"], json!({"state": "session:7"}));

    // 3. Session API request (read-only capability, scope clamped to session), global API request,
    //    default 200 status, malformed reply, and undeclared API refusal.
    let adapter = PluginHostAdapter::new(bundle_host.clone());
    let reply = adapter
        .request(api_req("usage", Some(session), &[("scope", "session")]))
        .await
        .unwrap();
    assert_eq!(reply.status, 201);
    assert_eq!(reply.body["request"]["path_params"], json!({"id": "a/b"}));
    assert_eq!(reply.body["context"]["scope"], "session");
    assert_eq!(reply.body["context"]["call"], reply.body["request"]["call"]);
    assert_eq!(reply.body["usage"]["scope"], "session");
    for denied in ["denied", "tools_list", "tool_call", "workspace_state"] {
        assert_eq!(
            reply.body[denied]["code"],
            codes::CAPABILITY_DENIED,
            "{denied}"
        );
    }
    let widened = adapter
        .request(api_req("usage", Some(session), &[("scope", "root")]))
        .await
        .unwrap()
        .body;
    assert_eq!(widened["usage"], Value::Null);
    assert_eq!(widened["usage_error"]["code"], codes::INVALID_PARAMS);

    let global = adapter
        .request(api_req("echo", None, &[]))
        .await
        .unwrap()
        .body;
    assert!(global["request"].get("session").is_none());
    assert_eq!(global["context"]["scope"], "global");
    assert_eq!(global["usage_error"]["code"], codes::CAPABILITY_DENIED);

    assert_eq!(
        adapter
            .request(api_req("plain", None, &[]))
            .await
            .unwrap()
            .status,
        200
    );
    assert!(
        adapter
            .request(api_req("malformed", None, &[]))
            .await
            .unwrap_err()
            .contains("headers")
    );
    assert!(
        adapter
            .request(api_req("missing", None, &[]))
            .await
            .unwrap_err()
            .contains("missing")
    );
    bundle_host.shutdown().await.unwrap();

    // 4. Configured (non-bundle) plugin tool gets no host capability, and duplicate initialize APIs fail.
    let configured =
        PluginHost::connect_all(vec![api_spec(PluginKindWire::Bun, "[]")], host_info()).await;
    let out = configured.tools()[0]
        .execute(&ctx_with(session, "."), json!({"value": "ship it"}))
        .await
        .unwrap();
    assert_eq!(out["capability"], Value::Null);
    assert_eq!(out["value"], "ship it");

    let dup = PluginHost::connect(
        api_spec(PluginKindWire::Bun, "[{'name':'usage'},{'name':'usage'}]")
            .in_bundle(std::env::temp_dir()),
        host_info(),
        None,
    )
    .await;
    assert!(dup.err().unwrap().to_string().contains("usage"));
}

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

#[tokio::test]
async fn shared_process_launch_policies_respawn_through_owner() {
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
        let ctx = ctx_with(SessionId::new(), ".");
        let before = alpha.execute(&ctx, json!({})).await.unwrap();
        assert!(alpha.execute(&ctx, json!({"crash": true})).await.is_err());
        let after = alpha.execute(&ctx, json!({})).await.unwrap();
        assert_ne!(before["pid"], after["pid"]);
        assert_eq!(after["env"], "retained");
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
        assert!(alpha.execute(&ctx, json!({})).await.is_err());
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
async fn private_activation_reports_loss_without_respawning() {
    let root = std::env::temp_dir().join(format!("hya-owner-{}", SessionId::new()));
    std::fs::create_dir(&root).unwrap();
    let host = Arc::new(
        PluginHost::connect(
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
        .unwrap(),
    );
    let adapter = PluginHostAdapter::new(host.clone());
    let plugin = host.prepared_plugins().remove(0);
    let loss = plugin.loss_token().unwrap();
    let alpha = plugin.tool("alpha", "selected-alpha").unwrap();
    let mut ctx = ctx_with(SessionId::new(), ".");
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
        message: MessageId::new(),
        call: ToolCallId::new(),
        tool: "selected-alpha".into(),
        input: json!({"crash": true}),
    };
    assert!(matches!(
        adapter.tool_execute_before(input()).await,
        ToolExecuteBeforeOutcome::Veto { .. }
    ));
    assert!(loss.is_cancelled());
    assert!(!HookDispatcher::is_healthy(&adapter));
    assert!(matches!(
        adapter.tool_execute_before(input()).await,
        ToolExecuteBeforeOutcome::Veto { .. }
    ));
    assert!(matches!(
        alpha.execute(&ctx, json!({})).await,
        Err(ToolError::Cancelled)
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
async fn crash_marks_dead_then_respawns_then_disables() {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        res = {"protocol_version":1,"plugin":{"id":"crasher","version":"0.1.0","kind":"rust"},
               "hooks":[{"name":"tool.execute.before"}],"tools":[]}
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":res}), flush=True)
    elif msg.get("method") == "hook/tool.execute.before":
        sys.exit(1)
    elif "id" in msg:
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{}}), flush=True)
"#;
    let host =
        Arc::new(PluginHost::connect_all(vec![py_spec("crasher", script)], host_info()).await);
    let adapter = PluginHostAdapter::new(host.clone());
    assert_eq!(
        host.plugin_status("crasher").await,
        Some(PluginStatus::Alive)
    );

    let poke = || {
        adapter.tool_execute_before(ToolExecuteBeforeInput {
            session: SessionId::new(),
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".into(),
            input: json!({"command": "ls"}),
        })
    };

    let _ = poke().await;
    assert_eq!(
        host.plugin_status("crasher").await,
        Some(PluginStatus::Dead)
    );
    let _ = poke().await;
    assert_eq!(
        host.plugin_status("crasher").await,
        Some(PluginStatus::Dead)
    );

    let mut disabled = false;
    for _ in 0..10 {
        let _ = poke().await;
        if host.plugin_status("crasher").await == Some(PluginStatus::Disabled) {
            disabled = true;
            break;
        }
    }
    assert!(
        disabled,
        "exceeding the restart budget must disable the plugin"
    );
}
