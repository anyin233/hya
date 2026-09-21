//! T2.19 — installed bundle schema declarations surface end to end.
//!
//! A `.hyabundle` fixture declaring `schemas:`, `extensions.process`, and
//! `resources.mcp` installs through the CLI, `bundle schemas` and `bundle info`
//! report the declarations, and after the first turn binds the installed
//! catalog the published runtime scheme table exposes the claim over
//! `GET /v1/runtime/schemas` (owner = the bundle source, canonical tool = the
//! owning tool's `bundle:{id}/tool/{local}` stable id). The `scheme://`
//! read-dispatch through the owning sidecar tool is covered by the hya-core
//! unit seam (`bundle_agent_view_with_sidecar_owner_reads_registered_scheme`),
//! because driving a real JS sidecar is out of scope for the FakeLlm harness.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_e2e::{E2eEnvBuilder, text_step};

const BUNDLE_ID: &str = "hya/schema-demo";

/// Copy the checked-in fixture into a unique temp path with the required
/// `.hyabundle` suffix (the CLI rejects packages without it).
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
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema_demo.hyabundle");
    let dest = dest_dir.join("schema-demo.hyabundle");
    std::fs::copy(&source, &dest)
        .unwrap_or_else(|error| panic!("copy fixture {}: {error}", source.display()));
    dest
}

#[tokio::test]
async fn t2_19_installed_bundle_schemas_surface_through_cli_and_runtime_api() {
    let package = materialized_package();
    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("SCHEMA_SURFACED")])
        .build()
        .await
        .expect("e2e env");

    let install = env
        .backend
        .bundle_cli(&["bundle", "install", package.to_str().unwrap()])
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
        "process=bun command=bun schema-demo-worker",
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
