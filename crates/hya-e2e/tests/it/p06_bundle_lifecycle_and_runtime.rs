//! T2.7, T2.8, T2.19, T2.21, T2.22, T2.24, T2.29 — Hyabundle CLI lifecycle,
//! installed package agents, agentless plugin skills, executable bundle schemas,
//! Claude Code import, packaged native/JS processes + owner-scoped MCP/hooks +
//! live config reload, and bundle-declared HTTP APIs with request-scoped capabilities.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_bundle::{BundleSource, SourceFile, write_public_package};
use hya_e2e::{
    E2eEnv, E2eEnvBuilder, fake_requests_from, materialize_public_bundle, mcp_echo_script,
    text_step, tool_step, unique_temp_dir,
};
use hya_proto::Event;
use serde_json::{Value, json};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        Self(unique_temp_dir(label).expect("create temp dir"))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_and_install(env: &E2eEnv, dir: &Path, filename: &str, source: &BundleSource) -> String {
    let package = dir.join(filename);
    std::fs::write(&package, write_public_package(source).unwrap()).unwrap();
    let install = env
        .backend
        .bundle_cli(&[
            "--db",
            env.backend.db.to_str().unwrap(),
            "bundle",
            "install",
            "-y",
            package.to_str().unwrap(),
        ])
        .unwrap();
    assert!(
        install.status.success(),
        "install {filename} failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let _ = std::fs::remove_file(&package);
    String::from_utf8_lossy(&install.stdout).into_owned()
}

fn uninstall(env: &E2eEnv, id: &str) {
    let out = env
        .backend
        .bundle_cli(&[
            "--db",
            env.backend.db.to_str().unwrap(),
            "bundle",
            "uninstall",
            "-y",
            id,
        ])
        .unwrap();
    assert!(
        out.status.success(),
        "uninstall {id} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// T2.7, T2.8, T2.19, T2.21, T2.24 on one booted backend.
#[tokio::test]
async fn bundle_cli_agent_plugin_schemas_and_claude() {
    let tmp = TempDir::new("bundle-lifecycle");
    let env = E2eEnvBuilder::new().build().await.expect("e2e env");

    // 1. T2.7 + T2.8: public bundle install / list / info / spawn installed agent / uninstall.
    const PUB_ID: &str = "hya/valid-public";
    const PUB_AGENT: &str = "valid-public-lead";
    let pub_pkg = materialize_public_bundle(&tmp.path().join("pub")).unwrap();
    let install_out = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", pub_pkg.to_str().unwrap()])
        .unwrap();
    assert!(install_out.status.success());
    assert!(String::from_utf8_lossy(&install_out.stdout).contains(PUB_ID));
    let list_out =
        String::from_utf8_lossy(&env.backend.bundle_cli(&["bundle", "list"]).unwrap().stdout)
            .into_owned();
    assert!(list_out.contains(PUB_ID));
    let info_out = String::from_utf8_lossy(
        &env.backend
            .bundle_cli(&["bundle", "info", PUB_ID])
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(info_out.contains(PUB_AGENT) && info_out.contains("origin=installed"));

    assert!(
        env.list_agents()
            .await
            .unwrap()
            .to_string()
            .contains(PUB_AGENT)
    );
    env.fake
        .push_scripts([text_step("BUNDLE_AGENT_OK")])
        .unwrap();
    let pub_session = env.create_session_with_agent(PUB_AGENT).await.unwrap();
    env.prompt(pub_session, "hello from bundle agent")
        .await
        .unwrap();
    let pub_text: String = env
        .events(pub_session, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::TextDelta { delta, .. } => Some(delta),
            Event::TextReplace { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert!(pub_text.contains("BUNDLE_AGENT_OK"));

    uninstall(&env, PUB_ID);
    let after_list =
        String::from_utf8_lossy(&env.backend.bundle_cli(&["bundle", "list"]).unwrap().stdout)
            .into_owned();
    assert!(!after_list.contains(PUB_ID));

    // 2. T2.21: agentless Plugin installs and publishes a static skill, then uninstalls cleanly.
    let plugin_source = BundleSource::new(
        "plugin-e2e",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: acme/plugin-e2e, version: 1.0.0, publisher: acme }
resources:
  skills:
    - id: plugin-help
      path: resources/skills/plugin-help.md
"#,
            ),
            SourceFile::new(
                "resources/skills/plugin-help.md",
                b"---\nname: plugin-help\ndescription: Static Plugin skill\n---\nPLUGIN_STATIC_SKILL_MARKER\n",
            ),
        ],
    );
    write_and_install(&env, tmp.path(), "plugin-e2e.hyabundle", &plugin_source);
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("skill", json!({"name": "plugin-help"})),
            text_step("PLUGIN_SKILL_OK"),
        ])
        .unwrap();
    let s_plug = env.create_session().await.unwrap();
    env.prompt(s_plug, "load plugin-help").await.unwrap();
    assert!(
        fake_requests_from(&env.fake.requests().unwrap(), before + 1)
            .contains("PLUGIN_STATIC_SKILL_MARKER")
    );
    uninstall(&env, "acme/plugin-e2e");
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("skill", json!({"name": "plugin-help"})),
            text_step("PLUGIN_SKILL_GONE"),
        ])
        .unwrap();
    let s_plug2 = env.create_session().await.unwrap();
    env.prompt(s_plug2, "load removed plugin-help")
        .await
        .unwrap();
    assert!(
        fake_requests_from(&env.fake.requests().unwrap(), before + 1)
            .contains("skill not found: plugin-help")
    );

    // 3. T2.19: bundle schemas surface through `bundle schema`, `bundle info`, `read(db://...)`,
    // and `GET /v2/runtime/schemas`.
    let schema_source = BundleSource::new(
        "schema-demo",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: hya/schema-demo, version: 1.0.0, publisher: hya }
schemas: [{ scheme: db, tool: query, writable: false }]
resources:
  tools: [{ id: query, path: tool.json }]
  mcp: [{ id: vecdb, path: mcp.json }]
runtime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
assets:
  - { id: runtime, path: runtime.py }
  - { id: mcp-runtime, path: mcp.py }
agents:
  - id: schema-lead
    role: main
    resource_view: { allow: [query, 'harness:tool/read'] }
"#,
            ),
            SourceFile::new("tool.json", "{}"),
            SourceFile::new(
                "mcp.json",
                r#"{"command":["python3","${BUNDLE_ROOT}/mcp.py"]}"#,
            ),
            SourceFile::new("mcp.py", mcp_echo_script()),
            SourceFile::new(
                "runtime.py",
                r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize': result={'protocol_version':1,'plugin':{'id':'schema-demo','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'query','description':'Read db URI','inputSchema':{'type':'object'}}]}
 elif r.get('method') == 'tool/call': result={'ok':True,'output':{'output':'BUNDLE_SCHEMA_READ_EXECUTED','input':r['params']['input']}}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#,
            ),
        ],
    );
    write_and_install(&env, tmp.path(), "schema-demo.hyabundle", &schema_source);
    let schema_cli = env
        .backend
        .bundle_cli(&["bundle", "schema", "hya/schema-demo"])
        .unwrap();
    assert!(schema_cli.status.success());
    assert_eq!(
        String::from_utf8_lossy(&schema_cli.stdout)
            .lines()
            .collect::<Vec<_>>(),
        ["SCHEME TOOL WRITABLE", "db query false"]
    );
    let info_schema = String::from_utf8_lossy(
        &env.backend
            .bundle_cli(&["bundle", "info", "hya/schema-demo"])
            .unwrap()
            .stdout,
    )
    .into_owned();
    for expected in [
        "schema=db tool=query writable=false",
        "process=bun command=python3 ${BUNDLE_ROOT}/runtime.py",
        "mcp=bundle:hya/schema-demo/mcp/vecdb",
    ] {
        assert!(info_schema.lines().any(|l| l == expected), "{info_schema}");
    }
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("read", json!({"path": "db://rows/42"})),
            text_step("SCHEMA_SURFACED"),
        ])
        .unwrap();
    let s_schema = env.create_session_with_agent("schema-lead").await.unwrap();
    env.prompt(s_schema, "surface schema").await.unwrap();
    assert!(
        fake_requests_from(&env.fake.requests().unwrap(), before + 1)
            .contains("BUNDLE_SCHEMA_READ_EXECUTED")
    );
    let runtime_schemas = env.get_json("/v2/runtime/schemas").await.unwrap();
    let claim = runtime_schemas["schemas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["scheme"] == "db")
        .expect("db claim");
    assert_eq!(claim["owner"], "bundle:hya/schema-demo");
    assert_eq!(claim["canonicalTool"], "bundle:hya/schema-demo/tool/query");
    uninstall(&env, "hya/schema-demo");

    // 4. T2.24: Claude plugin directory import executes packaged hook & skill after source removal.
    let claude_root = tmp.path().join("claude-src");
    for dir in [".claude-plugin", "hooks", "skills/claude-help"] {
        std::fs::create_dir_all(claude_root.join(dir)).unwrap();
    }
    std::fs::write(
        claude_root.join(".claude-plugin/plugin.json"),
        r#"{"name":"claude-e2e","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        claude_root.join("skills/claude-help/SKILL.md"),
        "---\nname: claude-help\ndescription: Imported help\n---\nCLAUDE_PACKAGED_SKILL\n",
    )
    .unwrap();
    std::fs::write(
        claude_root.join("hooks/guard.py"),
        "import json\nprint(json.dumps({'decision':'block','reason':'CLAUDE_PACKAGED_GUARD'}))\n",
    )
    .unwrap();
    std::fs::write(
        claude_root.join("hooks/hooks.json"),
        serde_json::to_vec(&json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Read",
                    "hooks": [{"type": "command", "command": "python3 ${CLAUDE_PLUGIN_ROOT}/hooks/guard.py"}]
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let claude_install = env
        .backend
        .bundle_cli(&[
            "bundle",
            "install",
            "-y",
            "--claude",
            claude_root.to_str().unwrap(),
        ])
        .unwrap();
    assert!(
        claude_install.status.success(),
        "{}",
        String::from_utf8_lossy(&claude_install.stderr)
    );
    std::fs::remove_dir_all(&claude_root).unwrap();

    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("read", json!({"path": "README.md"})),
            tool_step("skill", json!({"name": "claude-help"})),
            text_step("CLAUDE_IMPORTED"),
        ])
        .unwrap();
    let s_claude = env.create_session().await.unwrap();
    env.prompt(s_claude, "use imported hook and skill")
        .await
        .unwrap();
    let claude_followup = fake_requests_from(&env.fake.requests().unwrap(), before + 2);
    assert!(
        claude_followup.contains("CLAUDE_PACKAGED_GUARD")
            && claude_followup.contains("CLAUDE_PACKAGED_SKILL"),
        "{claude_followup}"
    );
    uninstall(&env, "claude/claude-e2e");
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("skill", json!({"name": "claude-help"})),
            text_step("CLAUDE_REMOVED"),
        ])
        .unwrap();
    let s_claude2 = env.create_session().await.unwrap();
    env.prompt(s_claude2, "use removed claude skill")
        .await
        .unwrap();
    assert!(
        fake_requests_from(&env.fake.requests().unwrap(), before + 1)
            .contains("skill not found: claude-help")
    );
}

/// T2.22 — native bundle process + MCP, owner-scoped private tools/hooks, agentless JS
/// plugin, `chat.params` request lineage, `model.fallback` recovery, and live `config.yaml`
/// plugin reload — all on one running backend PID.
#[tokio::test]
async fn bundle_process_mcp_hooks_js_and_live_config() {
    let tmp = TempDir::new("bundle-process");
    let env = E2eEnvBuilder::new().build().await.unwrap();
    let original_pid = env.backend.pid();
    let running_pid = || {
        let discovery: Value = serde_json::from_slice(
            &std::fs::read(format!("{}.server.json", env.backend.db.display())).unwrap(),
        )
        .unwrap();
        discovery["pid"].as_u64().unwrap() as u32
    };
    assert_eq!(running_pid(), original_pid);

    // Phase A: packaged native process + MCP server, hot activation & uninstall on same PID.
    let proc_script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'process-fixture','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'echo','description':'echo fixture','inputSchema':{'type':'object'}}]}
 elif r.get('method') == 'tool/call': result={'ok':True,'output':{'marker':'BUNDLE_PROCESS_EXECUTED','input':r['params']['input']}}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let proc_source = BundleSource::new(
        "process-e2e",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: acme/process-fixture, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
assets:
  - { id: runtime, path: runtime.py }
  - { id: mcp-runtime, path: mcp.py }
resources:
  tools: [{ id: echo, path: tool.json }]
  mcp: [{ id: echo, path: mcp.json }]
"#,
            ),
            SourceFile::new("runtime.py", proc_script),
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
    let activation = write_and_install(&env, tmp.path(), "process.hyabundle", &proc_source);
    assert!(activation.contains(&format!(
        "activation: active in the backend (pid {original_pid})"
    )));
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("process-fixture__echo", json!({"msg": "native"})),
            tool_step(
                "process-fixture__mcp__echo__group__ping",
                json!({"msg": "BUNDLE_MCP_EXECUTED"}),
            ),
            text_step("BOTH_FINISHED"),
        ])
        .unwrap();
    let s1 = env.create_session().await.unwrap();
    env.prompt(s1, "execute native + MCP").await.unwrap();
    let followup = fake_requests_from(&env.fake.requests().unwrap(), before + 2);
    assert!(
        followup.contains("BUNDLE_PROCESS_EXECUTED")
            && followup.contains("echo:BUNDLE_MCP_EXECUTED")
    );
    uninstall(&env, "acme/process-fixture");

    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("process-fixture__echo", json!({})),
            text_step("REMOVED"),
        ])
        .unwrap();
    let s1_after = env.create_session().await.unwrap();
    env.prompt(s1_after, "removed tool").await.unwrap();
    let after_rm = fake_requests_from(&env.fake.requests().unwrap(), before + 1);
    assert!(
        !after_rm.contains("BUNDLE_PROCESS_EXECUTED")
            && (after_rm.contains("unknown tool") || after_rm.contains("not found"))
    );

    // Phase B: owner-scoped process, MCP, and selected `hook_refs` (`owner-agent` vs `quiet-agent`).
    let private_source = BundleSource::new(
        "private-process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: acme/private-process, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
assets:
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
            SourceFile::new(
                "runtime.py",
                r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 m=r.get('method')
 if m == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'private-process','version':'1.0.0','kind':'rust'},'hooks':[{'name':'tool.execute.before'}],'tools':[{'name':'echo','description':'echo input','inputSchema':{'type':'object'}}]}
 elif m == 'hook/tool.execute.before':
  inp=r['params']['input']; inp['hook_marker']='OWNER_HOOK_EXECUTED'; result={'outcome':'continue','input':inp}
 elif m == 'tool/call': result={'ok':True,'output':r['params']['input']}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#,
            ),
            SourceFile::new("mcp.py", mcp_echo_script()),
            SourceFile::new("declaration.json", "{}"),
            SourceFile::new(
                "mcp.json",
                r#"{"command":["python3","${BUNDLE_ROOT}/mcp.py"]}"#,
            ),
        ],
    );
    write_and_install(&env, tmp.path(), "private.hyabundle", &private_source);
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("echo", json!({"msg": "PRIVATE_PROCESS_EXECUTED"})),
            tool_step("remote__ping", json!({"msg": "PRIVATE_MCP_EXECUTED"})),
            text_step("OWNER_DONE"),
            tool_step("echo", json!({"msg": "QUIET_PROCESS_EXECUTED"})),
            text_step("QUIET_DONE"),
        ])
        .unwrap();
    let owner = env.create_session_with_agent("owner-agent").await.unwrap();
    env.prompt(owner, "owner turn").await.unwrap();
    let owner_f = fake_requests_from(&env.fake.requests().unwrap(), before + 2);
    assert!(
        owner_f.contains("OWNER_HOOK_EXECUTED") && owner_f.contains("echo:PRIVATE_MCP_EXECUTED")
    );
    let quiet_before = env.fake.requests().unwrap().len();
    let quiet = env.create_session_with_agent("quiet-agent").await.unwrap();
    env.prompt(quiet, "quiet turn").await.unwrap();
    let quiet_f = fake_requests_from(&env.fake.requests().unwrap(), quiet_before + 1);
    assert!(quiet_f.contains("QUIET_PROCESS_EXECUTED") && !quiet_f.contains("OWNER_HOOK_EXECUTED"));
    uninstall(&env, "acme/private-process");

    // Phase C: agentless JavaScript (Bun) plugin executes without a synthetic agent.
    let js_source = BundleSource::new(
        "js-plugin",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: acme/js-plugin, version: 1.0.0, publisher: acme }
resources:
  tools: [{ id: echo, path: extensions/runtime.js }]
runtime: { kind: js, mode: managed, entry: extensions/runtime.js }
"#,
            ),
            SourceFile::new(
                "extensions/runtime.js",
                "export default { id: 'runtime', server: async () => ({ tool: { echo: { description: 'Echo marker', execute: async () => 'AGENTLESS_JS_EXECUTED' } } }) };",
            ),
        ],
    );
    write_and_install(&env, tmp.path(), "js-plugin.hyabundle", &js_source);
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("js-plugin__echo", json!({})),
            text_step("JS_DONE"),
        ])
        .unwrap();
    let s_js = env.create_session().await.unwrap();
    env.prompt(s_js, "run js plugin").await.unwrap();
    assert!(
        fake_requests_from(&env.fake.requests().unwrap(), before + 1)
            .contains("AGENTLESS_JS_EXECUTED")
    );
    uninstall(&env, "acme/js-plugin");

    // Phase D: `chat.params` reaches both bundle agents and built-in agents with lineage.
    let lineage_source = BundleSource::new(
        "lineage",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: acme/lineage, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
assets: [{ id: runtime, path: runtime.py }]
resources:
  hooks: [{ id: chat.params, path: hook.json }]
"#,
            ),
            SourceFile::new(
                "runtime.py",
                r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 m=r.get('method')
 if m == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'lineage','version':'1.0.0','kind':'rust'},'hooks':[{'name':'chat.params'}],'tools':[]}
 elif m == 'hook/chat.params':
  p=r['params']; q=p['request']
  q['system']=(q.get('system') or '')+' LINEAGE agent=%s root_is_self=%s' % (p.get('agent'), p.get('root_session')==p['session'])
  result={'outcome':'continue','request':q}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#,
            ),
            SourceFile::new("hook.json", "{}"),
        ],
    );
    let team_source = BundleSource::new(
        "team",
        vec![SourceFile::new(
            "bundle.yaml",
            b"schema_version: 2\nkind: Bundle\nidentity: { id: acme/team, version: 1.0.0, publisher: acme }\nagents: [{ id: team-agent, role: main }]\n",
        )],
    );
    write_and_install(&env, tmp.path(), "lineage.hyabundle", &lineage_source);
    write_and_install(&env, tmp.path(), "team.hyabundle", &team_source);
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([text_step("TEAM_DONE"), text_step("MAIN_DONE")])
        .unwrap();
    let s_team = env.create_session_with_agent("team-agent").await.unwrap();
    env.prompt(s_team, "bundle agent turn").await.unwrap();
    let s_main = env.create_session().await.unwrap();
    env.prompt(s_main, "main agent turn").await.unwrap();
    let reqs = env.fake.requests().unwrap();
    assert!(
        reqs[before]
            .to_string()
            .contains("LINEAGE agent=team-agent root_is_self=True")
    );
    assert!(
        reqs[before + 1]
            .to_string()
            .contains("LINEAGE agent=hya-main root_is_self=True")
    );
    uninstall(&env, "acme/lineage");
    uninstall(&env, "acme/team");

    // Phase E: plugin `model.fallback` recovers pre-stream `unknown_model` from `chat.params`.
    let fallback_source = BundleSource::new(
        "fallback",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: acme/fallback, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
assets: [{ id: runtime, path: runtime.py }]
resources:
  hooks:
    - { id: chat.params, path: hook.json }
    - { id: model.fallback, path: hook.json }
"#,
            ),
            SourceFile::new(
                "runtime.py",
                r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 m=r.get('method')
 if m == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'fallback','version':'1.0.0','kind':'rust'},'hooks':[{'name':'chat.params'},{'name':'model.fallback'}],'tools':[]}
 elif m == 'hook/chat.params':
  q=r['params']['request']; orig=q['model']; q['model']='ghost/unrouted'
  q['system']=(q.get('system') or '')+' ORIGINAL_MODEL='+orig
  result={'outcome':'continue','request':q}
 elif m == 'hook/model.fallback':
  p=r['params']
  if p['model']=='ghost/unrouted' and p['error']['class']=='unknown_model' and p['attempt']==1 and p['tried']==['ghost/unrouted']:
   result={'outcome':'retry','model':'fake/model'}
  else: result={'outcome':'give_up'}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#,
            ),
            SourceFile::new("hook.json", "{}"),
        ],
    );
    write_and_install(&env, tmp.path(), "fallback.hyabundle", &fallback_source);
    let before = env.fake.requests().unwrap().len();
    env.fake.push_scripts([text_step("RECOVERED")]).unwrap();
    let s_fb = env.create_session().await.unwrap();
    let turn = env.prompt(s_fb, "route me").await.unwrap();
    assert!(turn.error_message.is_empty(), "{}", turn.error_message);
    assert!(
        env.fake.requests().unwrap()[before]
            .to_string()
            .contains("ORIGINAL_MODEL=fake/model")
    );
    uninstall(&env, "acme/fallback");

    // Phase F: live `config.yaml` plugin edit publishes tools + hooks without restarting backend.
    let config_path = env.backend.xdg_config_home.join("hya/config.yaml");
    let original_config = std::fs::read_to_string(&config_path).unwrap();
    let plugin_path = env.backend.project.join("live-plugin.py");
    let live_script = proc_script
        .replace("process-fixture", "live-config")
        .replace("'hooks':[]", "'hooks':[{'name':'message.user.before'}]")
        .replace(
            " elif r.get('method') == 'tool/call':",
            " elif r.get('method') == 'hook/message.user.before': result={'outcome':'continue','text':r['params']['text'] + ' CONFIG_HOOK_ACTIVE'}\n elif r.get('method') == 'tool/call':",
        );
    std::fs::write(&plugin_path, live_script).unwrap();
    let declaration = format!(
        "plugins:\n  live-config:\n    command: [python3, {}]",
        serde_json::to_string(&plugin_path).unwrap()
    );
    std::fs::write(
        &config_path,
        original_config.replace("plugins: {}", &declaration),
    )
    .unwrap();
    let before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("live-config__echo", json!({})),
            text_step("CONFIG_RELOADED"),
            text_step("CONFIG_DISABLED"),
        ])
        .unwrap();
    env.prompt(s1_after, "use newly configured tool")
        .await
        .unwrap();
    let req_with_cfg = &env.fake.requests().unwrap()[before];
    assert!(
        req_with_cfg["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "live-config__echo")
    );
    assert!(
        serde_json::to_string(&req_with_cfg["messages"])
            .unwrap()
            .contains("use newly configured tool CONFIG_HOOK_ACTIVE")
    );

    std::fs::write(&config_path, original_config).unwrap();
    env.prompt(s1_after, "after config removal").await.unwrap();
    let last_req = env.fake.requests().unwrap().last().unwrap().clone();
    assert!(
        !last_req["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "live-config__echo")
    );
    let msgs = serde_json::to_string(&last_req["messages"]).unwrap();
    assert!(
        msgs.contains("after config removal")
            && !msgs.contains("after config removal CONFIG_HOOK_ACTIVE")
    );
    assert_eq!(running_pid(), original_pid);
}

/// T2.29 — bundle-declared HTTP APIs (`apis:`): session-scoped `session.usage` tree &
/// single-session reads, global `POST`/`PUT`/`GET`/`DELETE`, `kind: bun` tool capability,
/// `/v2/bundle-apis` schema discovery, and stable `bundle_api_*` host error codes.
#[tokio::test]
async fn bundle_apis_session_usage_and_global_state() {
    const PROMPT: u64 = 100;
    const COMPLETION: u64 = 20;
    const REASONING: u64 = 5;
    const BUNDLE_SEGMENT: &str = "acme%2Fusage-apis";
    const PROCESS: &str = r#"import json,sys
seq=[1000]
items={}
def cap(p, op, params):
  seq[0]+=1
  inner={'capability':p['host_capability'],'call':p['call'],'method':op,'params':params}
  if p.get('session') is not None:
    inner['session']=p['session']
  print(json.dumps({'jsonrpc':'2.0','id':seq[0],'method':'host/capability','params':inner}),flush=True)
  return json.loads(sys.stdin.readline())
def api(p):
  a=p['api']
  if a=='usage':
    q=p.get('query') or {}
    usage=cap(p,'session.usage',{'scope':q.get('scope','tree')})
    if 'result' in usage:
      return {'body':usage['result']}
    return {'status':400,'body':{'error':usage.get('error')}}
  if a=='echo':
    ctx=cap(p,'context.describe',{})
    usage=cap(p,'session.usage',{})
    return {'status':201,'body':{'method':p['method'],'path':p['path'],'path_params':p['path_params'],'query':p['query'],'body':p['body'],'context':ctx.get('result'),'usage_error':usage.get('error')}}
  key=p['path_params']['key']
  if a=='put-item':
    created=key not in items
    items[key]=p['body']
    return {'status':201 if created else 200,'body':{'key':key,'value':p['body']}}
  if a=='get-item':
    if key in items:
      return {'body':{'key':key,'value':items[key]}}
    return {'status':404,'body':{'error':'no item '+key}}
  if a=='delete-item':
    if items.pop(key,None) is None:
      return {'status':404,'body':{'error':'no item '+key}}
    return {'status':204}
  return {'status':500,'body':{'error':'unknown api '+a}}
for line in sys.stdin:
  r=json.loads(line)
  m=r.get('method')
  p=r.get('params') or {}
  if m=='initialize':
    result={'protocol_version':1,'plugin':{'id':'usage-apis','version':'1.0.0','kind':'bun'},'hooks':[],'tools':[{'name':'usage_probe','description':'usage probe','inputSchema':{'type':'object'}}],'apis':[{'name':n} for n in ['usage','echo','put-item','get-item','delete-item']]}
  elif m=='tool/call':
    ctx=cap(p,'context.describe',{})
    usage=cap(p,'session.usage',{'scope':'session'})
    result={'ok':True,'output':{'marker':'USAGE_PROBE_OK','request':ctx['result']['request'],'scope':usage['result']['scope']}}
  elif m=='api/request':
    result=api(p)
  else:
    result={}
  if 'id' in r:
    print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;

    async fn call(
        env: &E2eEnv,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = env
            .http
            .request(method, format!("{}{path}", env.backend.url));
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        let val = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, val)
    }

    let tmp = TempDir::new("apis-e2e");
    let source = BundleSource::new(
        "usage-apis",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"schema_version: 2
kind: Bundle
identity: { id: acme/usage-apis, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/apis.py'] }
assets:
  - { id: runtime, path: apis.py }
  - { id: item-schema, path: schemas/item.json }
resources:
  tools: [{ id: usage_probe, path: tool.json }]
apis:
  - { id: usage, method: GET, scope: session, path: /usage, description: Token usage of the session tree }
  - { id: echo, method: POST, scope: global, path: '/echo/{name}/{n}' }
  - { id: put-item, method: PUT, scope: global, path: '/items/{key}', request_schema: schemas/item.json }
  - { id: get-item, method: GET, scope: global, path: '/items/{key}', response_schema: schemas/item.json }
  - { id: delete-item, method: DELETE, scope: global, path: '/items/{key}' }
"#,
            ),
            SourceFile::new("apis.py", PROCESS),
            SourceFile::new("schemas/item.json", r#"{"type":"object"}"#),
            SourceFile::new("tool.json", "{}"),
        ],
    );

    let env = E2eEnvBuilder::new()
        .route(
            "You are hya",
            vec![
                tool_step("usage-apis__usage_probe", json!({})),
                tool_step(
                    "task",
                    json!({
                        "description": "apis child",
                        "prompt": "do the child work",
                        "subagent_type": "hya-task",
                        "inline_agent": {
                            "description": "",
                            "category": "",
                            "model": "",
                            "name": "",
                            "prompt": "MARKER_APIS_CHILD do the child work",
                            "resident": false
                        }
                    }),
                ),
                text_step("PARENT_AFTER_TASK"),
            ],
        )
        .route("MARKER_APIS_CHILD", vec![text_step("CHILD_APIS_OK")])
        .build()
        .await
        .expect("e2e env");
    env.fake.set_usage(PROMPT, COMPLETION, REASONING).unwrap();
    write_and_install(&env, tmp.path(), "usage-apis.hyabundle", &source);

    let session = env.create_session().await.unwrap();
    env.prompt(session, "probe usage then spawn child")
        .await
        .unwrap();

    let followup = fake_requests_from(&env.fake.requests().unwrap(), 1);
    assert!(followup.contains("USAGE_PROBE_OK") && followup.contains("tool_call"));

    let (status, listed) = call(&env, reqwest::Method::GET, "/v2/bundle-apis", None).await;
    assert_eq!(status, 200);
    let ours: Vec<_> = listed["apis"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|api| api["bundle"] == "acme/usage-apis")
        .collect();
    assert_eq!(ours.len(), 5);
    assert!(ours.iter().any(|api| {
        api["api"] == "put-item" && api["requestSchema"] == json!({"type": "object"})
    }));

    let usage_path = format!("/v2/sessions/{session}/bundles/{BUNDLE_SEGMENT}/usage");
    let mut report = Value::Null;
    for _ in 0..300 {
        let (status, body) = call(&env, reqwest::Method::GET, &usage_path, None).await;
        assert_eq!(status, 200, "{body}");
        report = body;
        let rows = report["sessions"].as_array().map_or(0, Vec::len);
        let rounds = report["total"]["total"]["rounds"].as_u64().unwrap_or(0);
        let calls = env.fake.requests().unwrap().len() as u64;
        let child_done = env
            .fake
            .route_remaining("MARKER_APIS_CHILD")
            .unwrap()
            .is_some_and(|left| left == 0);
        if rows == 2 && child_done && rounds == calls {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let calls = env.fake.requests().unwrap().len() as u64;
    assert_eq!(report["scope"], "tree");
    let rows = report["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["session"], session.to_string());
    assert_eq!(rows[1]["parent"], session.to_string());
    for row in rows {
        let fake = &row["usage"]["by_model"]["fake/model"];
        let r = fake["rounds"].as_u64().unwrap();
        assert_eq!(fake["input"].as_u64().unwrap(), r * PROMPT);
        assert_eq!(
            fake["split"],
            json!({ "thinking": r * REASONING, "visible": r * (COMPLETION - REASONING), "unknown": 0 })
        );
    }
    assert_eq!(report["total"]["total"]["rounds"].as_u64().unwrap(), calls);

    let (status, single) = call(
        &env,
        reqwest::Method::GET,
        &format!("{usage_path}?scope=session"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(single["sessions"].as_array().unwrap().len(), 1);
    let (status, refused) = call(
        &env,
        reqwest::Method::GET,
        &format!("{usage_path}?scope=root"),
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(refused["error"]["code"], -32602);

    let global = format!("/v2/bundles/{BUNDLE_SEGMENT}/api");
    let (status, echoed) = call(
        &env,
        reqwest::Method::POST,
        &format!("{global}/echo/a%20b/7?x=1"),
        Some(json!({"hello": [1, 2]})),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(echoed["path_params"], json!({"name": "a b", "n": "7"}));
    assert_eq!(echoed["context"]["scope"], "global");
    assert_eq!(echoed["usage_error"]["code"], -32001);

    let item = format!("{global}/items/alpha");
    assert_eq!(
        call(&env, reqwest::Method::PUT, &item, Some(json!({"n": 1})))
            .await
            .0,
        201
    );
    assert_eq!(
        call(&env, reqwest::Method::PUT, &item, Some(json!({"n": 2})))
            .await
            .0,
        200
    );
    assert_eq!(
        call(&env, reqwest::Method::GET, &item, None).await.1["value"],
        json!({"n": 2})
    );
    assert_eq!(
        call(&env, reqwest::Method::DELETE, &item, None).await,
        (204, Value::Null)
    );
    assert_eq!(call(&env, reqwest::Method::GET, &item, None).await.0, 404);

    for (method, path, expected_status, expected_code) in [
        (
            reqwest::Method::GET,
            format!("/v2/sessions/{session}/bundles/{BUNDLE_SEGMENT}/missing"),
            404,
            "bundle_api_not_found",
        ),
        (
            reqwest::Method::GET,
            format!("/v2/sessions/{session}/bundles/acme%2Fabsent/usage"),
            404,
            "bundle_api_not_found",
        ),
        (
            reqwest::Method::GET,
            format!(
                "/v2/sessions/{}/bundles/{BUNDLE_SEGMENT}/usage",
                hya_proto::SessionId::new()
            ),
            404,
            "session_not_found",
        ),
        (
            reqwest::Method::PATCH,
            item.clone(),
            405,
            "bundle_api_method_not_allowed",
        ),
    ] {
        let (st, body) = call(&env, method, &path, None).await;
        assert_eq!(st, expected_status, "{body}");
        assert_eq!(body["error"]["code"], expected_code, "{body}");
    }
    let bad = env
        .http
        .post(format!("{}{global}/echo/a/1", env.backend.url))
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400);
    assert_eq!(
        bad.json::<Value>().await.unwrap()["error"]["code"],
        "bundle_api_bad_request"
    );
}
