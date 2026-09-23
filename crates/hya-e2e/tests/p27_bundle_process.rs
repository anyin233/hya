//! Installed Plugin processes and MCP servers execute from packaged resources.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_bundle::{BundleSource, SourceFile, write_public_package};
use hya_e2e::{E2eEnvBuilder, fake_requests_from, mcp_echo_script, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t2_22_plugin_process_and_mcp_execute_from_package() {
    let root = std::env::temp_dir().join(format!("hya-process-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'process-fixture','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'echo','description':'echo fixture','inputSchema':{'type':'object'}}]}
 elif r.get('method') == 'tool/call': result={'ok':True,'output':{'marker':'BUNDLE_PROCESS_EXECUTED','input':r['params']['input']}}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let source = BundleSource::new(
        "process-e2e",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/process-fixture, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files:
    - { id: runtime, path: runtime.py }
    - { id: mcp-runtime, path: mcp.py }
resources:
  tools: [{ id: echo, path: tool.json }]
  mcp: [{ id: echo, path: mcp.json }]
"#,
            ),
            SourceFile::new("runtime.py", script),
            SourceFile::new(
                "mcp.py",
                mcp_echo_script().replace("\"name\": \"ping\"", "\"name\": \"group__ping\""),
            ),
            SourceFile::new("tool.json", "{}"),
            SourceFile::new(
                "mcp.json",
                r#"{"command":["python3","${BUNDLE_ROOT}/mcp.py"]}"#,
            ),
        ],
    );
    let package = root.join("process.hyabundle");
    std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("process-fixture__echo", json!({"msg":"native"})),
            tool_step(
                "process-fixture__mcp__echo__group__ping",
                json!({"msg":"BUNDLE_MCP_EXECUTED"}),
            ),
            text_step("BOTH_FINISHED"),
            tool_step("process-fixture__echo", json!({})),
            text_step("REMOVED"),
        ])
        .build()
        .await
        .unwrap();
    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_file(&package).unwrap();
    let session = env.create_session().await.unwrap();
    env.prompt(session, "execute installed native and MCP tools")
        .await
        .unwrap();
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 2);
    assert!(
        followup.contains("BUNDLE_PROCESS_EXECUTED"),
        "{followup}; {}",
        env.diagnostics()
    );
    assert!(
        followup.contains("echo:BUNDLE_MCP_EXECUTED"),
        "{followup}; {}",
        env.diagnostics()
    );
    let uninstall = env
        .backend
        .bundle_cli(&["bundle", "uninstall", "-y", "acme/process-fixture"])
        .unwrap();
    assert!(
        uninstall.status.success(),
        "{}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let session = env.create_session().await.unwrap();
    env.prompt(session, "try removed tool").await.unwrap();
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 4);
    assert!(!followup.contains("BUNDLE_PROCESS_EXECUTED"), "{followup}");
    assert!(
        followup.contains("unknown tool")
            || followup.contains("Unknown tool")
            || followup.contains("not found"),
        "{followup}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn agent_bundle_process_mcp_and_selected_hooks_are_owner_scoped() {
    let root = std::env::temp_dir().join(format!("hya-private-process-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 method=r.get('method')
 if method == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'private-process','version':'1.0.0','kind':'rust'},'hooks':[{'name':'tool.execute.before','posture':'safe'}],'tools':[{'name':'echo','description':'echo input','inputSchema':{'type':'object'}}]}
 elif method == 'hook/tool.execute.before':
  inp=r['params']['input']; inp['hook_marker']='OWNER_HOOK_EXECUTED'; result={'outcome':'continue','input':inp}
 elif method == 'tool/call': result={'ok':True,'output':r['params']['input']}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let source = BundleSource::new(
        "private-process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentSetBundle
identity: { id: acme/private-process, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files:
    - { id: runtime, path: runtime.py }
    - { id: mcp-runtime, path: mcp.py }
resources:
  tools: [{ id: echo, path: declaration.json }]
  hooks: [{ id: tool.execute.before, path: declaration.json }]
  mcp: [{ id: remote, path: mcp.json }]
agents:
  - id: owner-agent
    role: main
    resource_view: { allow: [echo, remote] }
    hook_refs: [tool.execute.before]
  - id: quiet-agent
    role: main
    resource_view: { allow: [echo] }
"#,
            ),
            SourceFile::new("runtime.py", script),
            SourceFile::new("mcp.py", mcp_echo_script()),
            SourceFile::new("declaration.json", "{}"),
            SourceFile::new(
                "mcp.json",
                r#"{"command":["python3","${BUNDLE_ROOT}/mcp.py"]}"#,
            ),
        ],
    );
    let package = root.join("private.hyabundle");
    std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("echo", json!({"msg":"PRIVATE_PROCESS_EXECUTED"})),
            tool_step("remote__ping", json!({"msg":"PRIVATE_MCP_EXECUTED"})),
            text_step("OWNER_DONE"),
            tool_step("echo", json!({"msg":"QUIET_PROCESS_EXECUTED"})),
            text_step("QUIET_DONE"),
        ])
        .build()
        .await
        .unwrap();
    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_file(package).unwrap();
    let owner = env.create_session_with_agent("owner-agent").await.unwrap();
    env.prompt(owner, "execute own process and MCP")
        .await
        .unwrap();
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 2);
    assert!(
        followup.contains("OWNER_HOOK_EXECUTED"),
        "{followup}; {}",
        env.diagnostics()
    );
    assert!(
        followup.contains("echo:PRIVATE_MCP_EXECUTED"),
        "{followup}; {}",
        env.diagnostics()
    );
    let quiet = env.create_session_with_agent("quiet-agent").await.unwrap();
    env.prompt(quiet, "execute echo without hooks")
        .await
        .unwrap();
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 4);
    assert!(followup.contains("QUIET_PROCESS_EXECUTED"), "{followup}");
    assert!(
        !followup.contains("OWNER_HOOK_EXECUTED"),
        "unselected hook ran: {followup}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn agentless_javascript_plugin_executes_without_a_synthetic_agent() {
    let root = std::env::temp_dir().join(format!("hya-js-plugin-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let source = BundleSource::new(
        "js-plugin",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/js-plugin, version: 1.0.0, publisher: acme }
resources:
  tools: [{ id: echo, path: extensions/runtime.js }]
extensions:
  js: [{ id: runtime, path: extensions/runtime.js }]
"#,
            ),
            SourceFile::new(
                "extensions/runtime.js",
                "export default { id: 'runtime', server: async () => ({ tool: { echo: { description: 'Echo marker', execute: async () => 'AGENTLESS_JS_EXECUTED' } } }) };",
            ),
        ],
    );
    let package = root.join("js-plugin.hyabundle");
    std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("js-plugin__echo", json!({})),
            text_step("DONE"),
        ])
        .build()
        .await
        .unwrap();
    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_file(package).unwrap();
    let session = env.create_session().await.unwrap();
    env.prompt(session, "execute agentless JavaScript plugin")
        .await
        .unwrap();
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 1);
    assert!(
        followup.contains("AGENTLESS_JS_EXECUTED"),
        "{followup}; {}",
        env.diagnostics()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn plugin_chat_params_reaches_bundle_agents_with_request_lineage() {
    let root = std::env::temp_dir().join(format!("hya-lineage-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 method=r.get('method')
 if method == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'lineage','version':'1.0.0','kind':'rust'},'hooks':[{'name':'chat.params'}],'tools':[]}
 elif method == 'hook/chat.params':
  p=r['params']; q=p['request']
  q['system']=(q.get('system') or '')+' LINEAGE agent=%s root_is_self=%s' % (p.get('agent'), p.get('root_session')==p['session'])
  result={'outcome':'continue','request':q}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let plugin = BundleSource::new(
        "lineage",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/lineage, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files:
    - { id: runtime, path: runtime.py }
resources:
  hooks: [{ id: chat.params, path: hook.json }]
"#,
            ),
            SourceFile::new("runtime.py", script),
            SourceFile::new("hook.json", "{}"),
        ],
    );
    let team = BundleSource::new(
        "team",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: AgentSetBundle
identity: { id: acme/team, version: 1.0.0, publisher: acme }
agents:
  - id: team-agent
    role: main
"#,
        )],
    );
    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("TEAM_DONE"), text_step("BUILD_DONE")])
        .build()
        .await
        .unwrap();
    for (name, source) in [("lineage", plugin), ("team", team)] {
        let package = root.join(format!("{name}.hyabundle"));
        std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
        let install = env
            .backend
            .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
            .unwrap();
        assert!(
            install.status.success(),
            "{}",
            String::from_utf8_lossy(&install.stderr)
        );
        std::fs::remove_file(package).unwrap();
    }
    let session = env.create_session_with_agent("team-agent").await.unwrap();
    env.prompt(session, "bundle agent turn").await.unwrap();
    let session = env.create_session().await.unwrap();
    env.prompt(session, "built-in agent turn").await.unwrap();
    let requests = env.fake.requests().unwrap();
    let team_request = fake_requests_from(&requests[..1], 0);
    assert!(
        team_request.contains("LINEAGE agent=team-agent root_is_self=True"),
        "installed Plugin chat.params must reach the bundle agent: {team_request}; {}",
        env.diagnostics()
    );
    let build_request = fake_requests_from(&requests, 1);
    assert!(
        build_request.contains("LINEAGE agent=build root_is_self=True"),
        "{build_request}; {}",
        env.diagnostics()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn plugin_model_fallback_recovers_an_unrouted_model_before_the_stream() {
    let root = std::env::temp_dir().join(format!("hya-fallback-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // chat.params routes the turn to a model no provider serves; the
    // pre-stream UnknownModel failure then asks model.fallback, which names
    // the fake model the turn started on.
    let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 method=r.get('method')
 if method == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'fallback','version':'1.0.0','kind':'rust'},'hooks':[{'name':'chat.params'},{'name':'model.fallback'}],'tools':[]}
 elif method == 'hook/chat.params':
  q=r['params']['request']; original=q['model']; q['model']='ghost/unrouted'
  q['system']=(q.get('system') or '')+' ORIGINAL_MODEL='+original
  result={'outcome':'continue','request':q}
 elif method == 'hook/model.fallback':
  p=r['params']
  if p['model']=='ghost/unrouted' and p['error']['class']=='unknown_model' and p['attempt']==1 and p['tried']==['ghost/unrouted']:
   result={'outcome':'retry','model':'fake/model'}
  else:
   result={'outcome':'give_up'}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let source = BundleSource::new(
        "fallback",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/fallback, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files:
    - { id: runtime, path: runtime.py }
resources:
  hooks:
    - { id: chat.params, path: hook.json }
    - { id: model.fallback, path: hook.json }
"#,
            ),
            SourceFile::new("runtime.py", script),
            SourceFile::new("hook.json", "{}"),
        ],
    );
    let package = root.join("fallback.hyabundle");
    std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("RECOVERED")])
        .build()
        .await
        .unwrap();
    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_file(package).unwrap();
    let session = env.create_session().await.unwrap();
    let turn = env.prompt(session, "route me").await.unwrap();
    assert!(
        turn.error_message.is_empty(),
        "model.fallback must recover the unrouted model: {}; {}",
        turn.error_message,
        env.diagnostics()
    );
    let requests = env.fake.requests().unwrap();
    assert_eq!(requests.len(), 1, "{}", env.diagnostics());
    assert!(
        fake_requests_from(&requests, 0).contains("ORIGINAL_MODEL=fake/model"),
        "{}",
        env.diagnostics()
    );
    std::fs::remove_dir_all(root).unwrap();
}
