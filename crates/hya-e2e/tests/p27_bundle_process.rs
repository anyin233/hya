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
    spawn_lifecycle: transient
    resource_view: { allow: [echo, remote] }
    hook_refs: [tool.execute.before]
  - id: quiet-agent
    role: main
    spawn_lifecycle: transient
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
