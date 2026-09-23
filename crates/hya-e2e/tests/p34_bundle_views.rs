//! T2.29 — an installed bundle process serves a read-only session view over
//! `GET /v1/sessions/{session}/views/{bundle}/{view}` using the request-scoped
//! `session.usage` capability, and a non-Rust (`kind: bun`) bundle process
//! also receives a capability on its tool calls.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_bundle::{BundleSource, SourceFile, write_public_package};
use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::{Value, json};

/// Prompt/completion/reasoning tokens FakeLlm reports on every response.
const PROMPT: u64 = 100;
const COMPLETION: u64 = 20;
const REASONING: u64 = 5;

/// The bundle process: declares tool `usage_probe` and view `usage`; both
/// answer through `host/capability` calls on the lease they were handed.
const PROCESS: &str = r#"import json,sys
seq=[1000]
def cap(p, op, params):
  seq[0]+=1
  out={'jsonrpc':'2.0','id':seq[0],'method':'host/capability','params':{'capability':p['host_capability'],'session':p['session'],'call':p['call'],'method':op,'params':params}}
  print(json.dumps(out),flush=True)
  return json.loads(sys.stdin.readline())
for line in sys.stdin:
  r=json.loads(line)
  m=r.get('method')
  p=r.get('params') or {}
  if m=='initialize':
    result={'protocol_version':1,'plugin':{'id':'usage-views','version':'1.0.0','kind':'bun'},'hooks':[],'tools':[{'name':'usage_probe','description':'usage probe','inputSchema':{'type':'object'}}],'views':[{'name':'usage'}]}
  elif m=='tool/call':
    ctx=cap(p,'context.describe',{})
    usage=cap(p,'session.usage',{'scope':'session'})
    result={'ok':True,'output':{'marker':'USAGE_PROBE_OK','request':ctx['result']['request'],'scope':usage['result']['scope']}}
  elif m=='view/get':
    q=p.get('query') or {}
    usage=cap(p,'session.usage',{'scope':q.get('scope','tree')})
    result={'body':usage['result'] if 'result' in usage else {'error':usage.get('error')}}
  else:
    result={}
  if 'id' in r:
    print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;

const BUNDLE_SEGMENT: &str = "acme%2Fusage-views";

fn package(root: &std::path::Path) -> std::path::PathBuf {
    let source = BundleSource::new(
        "usage-views",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/usage-views, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: bun, command: [python3, '${BUNDLE_ROOT}/views.py'] }
  files:
    - { id: runtime, path: views.py }
resources:
  tools: [{ id: usage_probe, path: tool.json }]
views:
  - { id: usage, description: Token usage of the session tree }
"#,
            ),
            SourceFile::new("views.py", PROCESS),
            SourceFile::new("tool.json", "{}"),
        ],
    );
    let package = root.join("usage-views.hyabundle");
    std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
    package
}

/// GET a backend path, returning the status and JSON body.
async fn get(env: &hya_e2e::E2eEnv, path: &str) -> (u16, Value) {
    let response = env
        .http
        .get(format!("{}{path}", env.backend.url))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("not a u64: {value}"))
}

#[tokio::test]
async fn t2_29_bundle_view_reports_session_tree_usage() {
    let root = std::env::temp_dir().join(format!("hya-views-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let package = package(&root);
    let env = E2eEnvBuilder::new()
        .route(
            "You are hya",
            vec![
                tool_step("usage-views__usage_probe", json!({})),
                tool_step(
                    "task",
                    json!({
                        "description": "views child",
                        "prompt": "do the child work",
                        "subagent_type": "general",
                        "inline_agent": {
                            "description": "",
                            "category": "",
                            "model": "",
                            "name": "",
                            "prompt": "MARKER_VIEWS_CHILD do the child work",
                            "resident": false
                        }
                    }),
                ),
                text_step("PARENT_AFTER_TASK"),
            ],
        )
        .route("MARKER_VIEWS_CHILD", vec![text_step("CHILD_VIEWS_OK")])
        .build()
        .await
        .expect("e2e env");
    env.fake.set_usage(PROMPT, COMPLETION, REASONING).unwrap();
    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );

    let session = env.create_session().await.unwrap();
    env.prompt(session, "probe usage then spawn a child")
        .await
        .unwrap();

    // The bun-kind process's tool call received a capability.
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 1);
    assert!(
        followup.contains("USAGE_PROBE_OK")
            && followup.contains("tool_call")
            && followup.contains("session"),
        "{followup}; {}",
        env.diagnostics()
    );

    // Discovery lists the declared view.
    let (status, listed) = get(&env, &format!("/v1/sessions/{session}/views")).await;
    assert_eq!(status, 200, "{listed}");
    assert!(
        listed["views"].as_array().unwrap().iter().any(|view| view
            == &json!({
                "bundle": "acme/usage-views",
                "view": "usage",
                "description": "Token usage of the session tree",
            })),
        "{listed}"
    );

    // Wait until both sessions exist and every FakeLlm call is folded.
    let path = format!("/v1/sessions/{session}/views/{BUNDLE_SEGMENT}/usage");
    let mut view = Value::Null;
    for _ in 0..600 {
        let (status, body) = get(&env, &path).await;
        assert_eq!(status, 200, "{body}; {}", env.diagnostics());
        view = body;
        let rows = view["body"]["sessions"].as_array().map_or(0, Vec::len);
        let rounds = view["body"]["total"]["total"]["rounds"]
            .as_u64()
            .unwrap_or(0);
        let calls = env.fake.requests().unwrap().len() as u64;
        let child_done = env
            .fake
            .route_remaining("MARKER_VIEWS_CHILD")
            .unwrap()
            .is_some_and(|left| left == 0);
        if rows == 2 && child_done && rounds == calls {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(view["bundle"], "acme/usage-views");
    assert_eq!(view["view"], "usage");
    assert_eq!(view["contentType"], "application/json");
    let report = &view["body"];
    assert_eq!(report["scope"], "tree", "{report}");
    assert_eq!(report["session"], session.to_string());
    let rows = report["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "root and its subagent: {report}");
    assert_eq!(rows[0]["session"], session.to_string());
    assert_eq!(rows[1]["parent"], session.to_string());

    // Every FakeLlm call is billed exactly once, to one session of the tree,
    // and each row's per-model numbers follow the reported usage.
    let calls = env.fake.requests().unwrap().len() as u64;
    let mut summed_rounds = 0;
    for row in rows {
        let fake = &row["usage"]["by_model"]["fake/model"];
        let rounds = number(&fake["rounds"]);
        assert!(rounds >= 1, "{row}");
        assert_eq!(number(&fake["input"]), rounds * PROMPT, "{row}");
        assert_eq!(number(&fake["output"]), rounds * COMPLETION, "{row}");
        assert_eq!(
            fake["split"],
            json!({
                "thinking": rounds * REASONING,
                "visible": rounds * (COMPLETION - REASONING),
                "unknown": 0,
            }),
            "{row}"
        );
        summed_rounds += number(&row["usage"]["total"]["rounds"]);
    }
    let total = &report["total"]["total"];
    assert_eq!(number(&total["rounds"]), summed_rounds);
    assert_eq!(number(&total["rounds"]), calls, "{report}");
    assert_eq!(number(&total["input"]), calls * PROMPT);

    // `?scope=session` narrows to the requested session only.
    let (status, single) = get(&env, &format!("{path}?scope=session")).await;
    assert_eq!(status, 200, "{single}");
    assert_eq!(single["body"]["scope"], "session");
    assert_eq!(single["body"]["sessions"].as_array().unwrap().len(), 1);

    // Stable errors: unknown view, unknown bundle, unknown session.
    for (path, code) in [
        (
            format!("/v1/sessions/{session}/views/{BUNDLE_SEGMENT}/missing"),
            "view_not_found",
        ),
        (
            format!("/v1/sessions/{session}/views/acme%2Fabsent/usage"),
            "view_not_found",
        ),
        (
            format!(
                "/v1/sessions/{}/views/{BUNDLE_SEGMENT}/usage",
                hya_proto::SessionId::new()
            ),
            "session_not_found",
        ),
    ] {
        let (status, body) = get(&env, &path).await;
        assert_eq!(status, 404, "{path}: {body}");
        assert_eq!(body["error"]["code"], code, "{path}: {body}");
    }
    std::fs::remove_dir_all(root).unwrap();
}
