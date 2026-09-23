//! T2.29 — an installed bundle process registers its own HTTP endpoints
//! (manifest `apis:`): a session-scoped `GET` that reads the session tree's
//! usage through the request-scoped `session.usage` capability, a global
//! `POST` that echoes its body and path parameters with status 201, and a
//! global `PUT`/`GET`/`DELETE` trio over state the process keeps itself.
//! A non-Rust (`kind: bun`) bundle process also receives a capability on its
//! tool calls.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_bundle::{BundleSource, SourceFile, write_public_package};
use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::{Value, json};

/// Prompt/completion/reasoning tokens FakeLlm reports on every response.
const PROMPT: u64 = 100;
const COMPLETION: u64 = 20;
const REASONING: u64 = 5;

/// The bundle process: declares tool `usage_probe` and five endpoints. The
/// tool and the `usage` endpoint answer through `host/capability` calls on
/// the lease they were handed (a global request has no session, so it sends
/// none back); `echo` returns its routed request with status 201; `put-item`
/// / `get-item` / `delete-item` keep a dictionary in the process.
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

const BUNDLE_SEGMENT: &str = "acme%2Fusage-apis";

fn package(root: &std::path::Path) -> std::path::PathBuf {
    let source = BundleSource::new(
        "usage-apis",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/usage-apis, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: bun, command: [python3, '${BUNDLE_ROOT}/apis.py'] }
  files:
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
    let package = root.join("usage-apis.hyabundle");
    std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
    package
}

/// Send one request, returning the status and JSON body (`Null` when empty).
async fn call(
    env: &hya_e2e::E2eEnv,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = env
        .http
        .request(method, format!("{}{path}", env.backend.url));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, body)
}

/// GET a backend path, returning the status and JSON body.
async fn get(env: &hya_e2e::E2eEnv, path: &str) -> (u16, Value) {
    call(env, reqwest::Method::GET, path, None).await
}

fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("not a u64: {value}"))
}

#[tokio::test]
async fn t2_29_bundle_apis_serve_session_and_global_endpoints() {
    let root = std::env::temp_dir().join(format!("hya-apis-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let package = package(&root);
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
                        "subagent_type": "general",
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

    // Discovery lists the declared endpoints with their schemas.
    let (status, listed) = get(&env, "/v1/bundle-apis").await;
    assert_eq!(status, 200, "{listed}");
    let ours = listed["apis"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|api| api["bundle"] == "acme/usage-apis")
        .collect::<Vec<_>>();
    assert_eq!(ours.len(), 5, "{listed}");
    assert!(
        ours.iter().any(|api| **api
            == json!({
                "bundle": "acme/usage-apis",
                "api": "usage",
                "method": "GET",
                "scope": "session",
                "path": "/usage",
                "description": "Token usage of the session tree",
            })),
        "{listed}"
    );
    assert!(
        ours.iter().any(
            |api| api["api"] == "put-item" && api["requestSchema"] == json!({"type": "object"})
        ),
        "{listed}"
    );

    // Wait until both sessions exist and every FakeLlm call is folded.
    let path = format!("/v1/sessions/{session}/bundles/{BUNDLE_SEGMENT}/usage");
    let mut report = Value::Null;
    for _ in 0..600 {
        let (status, body) = get(&env, &path).await;
        assert_eq!(status, 200, "{body}; {}", env.diagnostics());
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The HTTP body is the process's own JSON (no envelope).
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

    // `?scope=session` narrows to the requested session only; a scope the
    // capability refuses becomes the process's own 400.
    let (status, single) = get(&env, &format!("{path}?scope=session")).await;
    assert_eq!(status, 200, "{single}");
    assert_eq!(single["scope"], "session");
    assert_eq!(single["sessions"].as_array().unwrap().len(), 1);
    let (status, refused) = get(&env, &format!("{path}?scope=root")).await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"]["code"], -32602, "{refused}");

    // A global POST: body, decoded path params, and query reach the process,
    // whose status (201) passes through; its capability has no session.
    let global = format!("/v1/bundles/{BUNDLE_SEGMENT}/api");
    let (status, echoed) = call(
        &env,
        reqwest::Method::POST,
        &format!("{global}/echo/a%20b/7?x=1"),
        Some(json!({"hello": [1, 2]})),
    )
    .await;
    assert_eq!(status, 201, "{echoed}");
    assert_eq!(echoed["method"], "POST");
    assert_eq!(echoed["path"], "/echo/a%20b/7");
    assert_eq!(echoed["path_params"], json!({"name": "a b", "n": "7"}));
    assert_eq!(echoed["query"], json!({"x": "1"}));
    assert_eq!(echoed["body"], json!({"hello": [1, 2]}));
    assert_eq!(echoed["context"]["scope"], "global");
    assert!(echoed["context"].get("session").is_none(), "{echoed}");
    assert_eq!(echoed["usage_error"]["code"], -32001, "{echoed}");

    // PUT / GET / DELETE over state the process keeps itself.
    let item = format!("{global}/items/alpha");
    let (status, created) = call(&env, reqwest::Method::PUT, &item, Some(json!({"n": 1}))).await;
    assert_eq!(
        (status, &created["value"]),
        (201, &json!({"n": 1})),
        "{created}"
    );
    let (status, updated) = call(&env, reqwest::Method::PUT, &item, Some(json!({"n": 2}))).await;
    assert_eq!(status, 200, "{updated}");
    let (status, read) = get(&env, &item).await;
    assert_eq!((status, &read["value"]), (200, &json!({"n": 2})), "{read}");
    let (status, deleted) = call(&env, reqwest::Method::DELETE, &item, None).await;
    assert_eq!((status, deleted), (204, Value::Null));
    let (status, gone) = get(&env, &item).await;
    assert_eq!(status, 404, "the process's own 404: {gone}");
    assert_eq!(gone["error"], "no item alpha");

    // Host-side failures use the stable codes.
    for (method, path, status, code) in [
        (
            reqwest::Method::GET,
            format!("/v1/sessions/{session}/bundles/{BUNDLE_SEGMENT}/missing"),
            404,
            "bundle_api_not_found",
        ),
        (
            reqwest::Method::GET,
            format!("/v1/sessions/{session}/bundles/acme%2Fabsent/usage"),
            404,
            "bundle_api_not_found",
        ),
        (
            reqwest::Method::GET,
            format!(
                "/v1/sessions/{}/bundles/{BUNDLE_SEGMENT}/usage",
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
        let (actual, body) = call(&env, method.clone(), &path, None).await;
        assert_eq!(actual, status, "{method} {path}: {body}");
        assert_eq!(body["error"]["code"], code, "{method} {path}: {body}");
    }
    let response = env
        .http
        .post(format!("{}{global}/echo/a/1", env.backend.url))
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "bundle_api_bad_request");
    std::fs::remove_dir_all(root).unwrap();
}
