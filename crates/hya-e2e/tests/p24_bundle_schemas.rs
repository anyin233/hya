//! T2.19 — executable bundle schema declarations and scheme reads surface end to end.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{BundleSource, SourceFile, write_public_package};
use hya_e2e::{E2eEnvBuilder, fake_requests_from, mcp_echo_script, text_step, tool_step};
use serde_json::json;

const BUNDLE_ID: &str = "hya/schema-demo";

/// Prepare a real process fixture into a unique public package.
fn materialized_package() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock follows the Unix epoch")
        .as_nanos();
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let dest_dir = std::env::temp_dir().join(format!(
        "hya-e2e-schema-demo-{}-{nanos}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dest_dir).expect("create fixture dir");
    let source = BundleSource::new(
        "schema-demo",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity: { id: hya/schema-demo, version: 1.0.0, publisher: hya }
schemas: [{ scheme: db, tool: query, writable: false }]
resources:
  tools: [{ id: query, path: tool.json }]
  mcp: [{ id: vecdb, path: mcp.json }]
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files:
    - { id: runtime, path: runtime.py }
    - { id: mcp-runtime, path: mcp.py }
agent:
  id: schema-lead
  role: main
  spawn_lifecycle: transient
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
    let dest = dest_dir.join("schema-demo.hyabundle");
    std::fs::write(&dest, write_public_package(&source).unwrap()).unwrap();
    dest
}

#[tokio::test]
async fn t2_19_installed_bundle_schemas_surface_through_cli_and_runtime_api() {
    let package = materialized_package();
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("read", json!({"path":"db://rows/42"})),
            text_step("SCHEMA_SURFACED"),
        ])
        .build()
        .await
        .expect("e2e env");

    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .expect("install");
    assert!(
        install.status.success(),
        "install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let schemas = env
        .backend
        .bundle_cli(&["bundle", "schemas"])
        .expect("bundle schemas");
    assert!(schemas.status.success(), "bundle schemas failed");
    let schemas_out = String::from_utf8_lossy(&schemas.stdout);
    let row = schemas_out
        .lines()
        .find(|line| line.split_whitespace().next() == Some(BUNDLE_ID))
        .expect("installed bundle must be listed in bundle schemas");
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        [BUNDLE_ID, "db", "query", "false"],
        "the schema row must carry scheme, owner tool, and writable flag:\n{schemas_out}"
    );

    let info = env
        .backend
        .bundle_cli(&["bundle", "info", BUNDLE_ID])
        .expect("bundle info");
    assert!(info.status.success(), "bundle info failed");
    let info_out = String::from_utf8_lossy(&info.stdout);
    for expected in [
        "schema=db tool=query writable=false",
        "process=rust command=python3 ${BUNDLE_ROOT}/runtime.py",
        "mcp=bundle:hya/schema-demo/mcp/vecdb",
    ] {
        assert!(
            info_out.lines().any(|line| line == expected),
            "bundle info omitted {expected:?}:\n{info_out}"
        );
    }

    // One bound turn publishes the installed catalog generation, including its
    // scheme claims.
    let session = env
        .create_session_with_agent("schema-lead")
        .await
        .expect("create bundle agent session");
    let _ = env
        .prompt(session, "surface the schema")
        .await
        .expect("prompt");

    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 1);
    assert!(
        followup.contains("BUNDLE_SCHEMA_READ_EXECUTED"),
        "{followup}; {}",
        env.diagnostics()
    );

    let runtime_schemas = env
        .get_json("/v1/runtime/schemas")
        .await
        .expect("GET /v1/runtime/schemas");
    let rows = runtime_schemas
        .get("schemas")
        .and_then(serde_json::Value::as_array)
        .expect("runtime schemas response carries a schemas array")
        .clone();
    let claim = rows
        .iter()
        .find(|row| row.get("scheme").and_then(serde_json::Value::as_str) == Some("db"))
        .unwrap_or_else(|| {
            panic!("db scheme must be published over the runtime API: {runtime_schemas}")
        });
    assert_eq!(
        claim.get("owner").and_then(serde_json::Value::as_str),
        Some("bundle:hya/schema-demo"),
        "the winning binding is owned by the bundle source: {claim}"
    );
    assert_eq!(
        claim
            .get("canonicalTool")
            .and_then(serde_json::Value::as_str),
        Some("bundle:hya/schema-demo/tool/query"),
        "the binding names the owning tool by its stable id: {claim}"
    );

    let _ = std::fs::remove_dir_all(package.parent().expect("fixture parent"));
}
