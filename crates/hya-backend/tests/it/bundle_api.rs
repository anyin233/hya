//! Bundle management over the v1 API against a real daemon (`ListBundles`,
//! `InstallBundle`, `UninstallBundle`, `SetBundleEnabled`, `RefreshBundles`,
//! `ListTuiExtensions`).

use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{Daemons, Scratch, TestResult};

const BUNDLE: &str = "e2e/tui-panel";

struct Api {
    client: reqwest::Client,
    url: String,
}

impl Api {
    async fn post(
        &self,
        path: &str,
        body: Value,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let resp = self
            .client
            .post(format!("{}{path}", self.url))
            .json(&body)
            .send()
            .await?;
        Ok((resp.status().as_u16(), resp.json().await?))
    }

    async fn status(&self, path: &str, body: Value) -> Result<u16, Box<dyn std::error::Error>> {
        Ok(self.post(path, body).await?.0)
    }

    async fn row(
        &self,
        dir: &str,
        id: &str,
        scope: &str,
    ) -> Result<Option<Value>, Box<dyn std::error::Error>> {
        let body: Value = self
            .client
            .get(format!("{}/v2/bundles", self.url))
            .query(&[("directory", dir)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body["bundles"]
            .as_array()
            .ok_or("no bundles")?
            .iter()
            .find(|r| r["id"] == id && r["scope"] == scope)
            .cloned())
    }

    async fn has_tui_ext(&self, id: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let body: Value = self
            .client
            .get(format!("{}/v2/tui-extensions", self.url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body["extensions"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["bundleId"] == id)))
    }
}

#[tokio::test]
async fn bundles_api_lifecycle_and_project_root_inventory_refresh() -> TestResult {
    let scratch = Scratch::new("hya-bundle-api")?;
    let root = &scratch.0;
    let started = scratch
        .hya()
        .args(["serve", "start", "--json", "--db"])
        .arg(root.join("s.db"))
        .output()?;
    let info: Value = serde_json::from_slice(&started.stdout)?;
    let _daemon = Daemons(vec![i32::try_from(info["pid"].as_i64().ok_or("no pid")?)?]);
    let api = Api {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?,
        url: info["url"].as_str().ok_or("no url")?.into(),
    };

    let src = hya_bundle::BundleSource::new(
        "tui-panel",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                b"schema_version: 2\nkind: Bundle\nidentity: { id: e2e/tui-panel, version: 1.0.0, publisher: e2e }\nassets: [{ id: panel, path: tui/main.ts }]\ntui: { api_version: 1, entry: tui/main.ts, sdk: 1.0.0, permissions: [tui.panel] }\n".to_vec(),
            ),
            hya_bundle::SourceFile::new("tui/main.ts", b"export default {}\n".to_vec()),
        ],
    );
    let pkg = root.join("tui-panel.hyabundle");
    std::fs::write(&pkg, hya_bundle::write_public_package(&src)?)?;

    let basic = api
        .row("", "hya/basic-tui-components", "first_party")
        .await?
        .ok_or("first-party")?;
    assert!(basic["removable"] != true && basic["components"]["tui"] == true);
    assert_eq!(
        api.status(
            "/v2/bundles:install",
            json!({"path": "tui-panel.hyabundle"})
        )
        .await?,
        400
    );

    let (st, body) = api
        .post("/v2/bundles:install", json!({"path": pkg}))
        .await?;
    assert_eq!((st, body["bundleId"].as_str()), (200, Some(BUNDLE)));
    assert!(api.has_tui_ext(BUNDLE).await?);

    assert_eq!(
        api.status(
            "/v2/bundles:set-enabled",
            json!({"bundleId": BUNDLE, "enabled": false})
        )
        .await?,
        200
    );
    assert_eq!(
        api.row("", BUNDLE, "user").await?.ok_or("disabled")?["state"],
        "disabled"
    );
    assert!(!api.has_tui_ext(BUNDLE).await?);
    assert_eq!(
        api.status(
            "/v2/bundles:set-enabled",
            json!({"bundleId": BUNDLE, "enabled": true})
        )
        .await?,
        200
    );
    assert!(api.has_tui_ext(BUNDLE).await?);

    assert_eq!(
        api.status(
            "/v2/bundles:set-enabled",
            json!({"bundleId": "hya/core-agents", "enabled": false})
        )
        .await?,
        409
    );
    assert_eq!(
        api.status("/v2/bundles:uninstall", json!({"bundleId": "hya/goal"}))
            .await?,
        409
    );
    assert_eq!(
        api.status("/v2/bundles:uninstall", json!({"bundleId": BUNDLE}))
            .await?,
        200
    );
    assert_eq!(
        api.status("/v2/bundles:uninstall", json!({"bundleId": BUNDLE}))
            .await?,
        404
    );

    let work = root.join("work");
    std::fs::create_dir_all(&work)?;
    let wd = work.to_string_lossy().into_owned();
    assert_eq!(
        api.status(
            "/v2/bundles:install",
            json!({"directory": wd, "path": pkg, "project": true})
        )
        .await?,
        200
    );
    assert_eq!(
        api.row(&wd, BUNDLE, "project").await?.ok_or("project")?["state"],
        "active"
    );
    assert!(api.row("", BUNDLE, "project").await?.is_none());
    assert_eq!(
        api.status(
            "/v2/bundles:uninstall",
            json!({"directory": wd, "bundleId": BUNDLE, "project": true})
        )
        .await?,
        200
    );

    // Multi-root project inventory vs refresh resolution.
    let (first, second) = (root.join("first"), root.join("second"));
    let nested = first.join("nested");
    for (dir, name, ns) in [
        (&first, "first", "s-inv"),
        (&second, "second", "s-inv"),
        (&second, "disabled", "d-inv"),
        (&nested, "nested", "n-inv"),
    ] {
        let b = dir.join(".hya/bundles").join(name);
        std::fs::create_dir_all(&b)?;
        std::fs::write(
            b.join("bundle.yaml"),
            format!(
                "schema_version: 2\nkind: Bundle\nidentity: {{ id: inventory/{name}, version: 1.0.0, publisher: inventory }}\nnamespace: {ns}\nresources:\n  skills: [{{ id: inv-demo, path: skill.md }}]\n"
            ),
        )?;
        std::fs::write(
            b.join("skill.md"),
            "---\nname: inv-demo\ndescription: d\n---\nbody\n",
        )?;
    }
    let broken = nested.join(".hya/bundles/broken");
    std::fs::create_dir_all(&broken)?;
    std::fs::write(broken.join("bundle.yaml"), "kind: [\n")?;

    assert_eq!(
        api.status(
            "/v2/projects",
            json!({"name": "inventory", "roots": [first, second]})
        )
        .await?,
        200
    );
    assert_eq!(
        api.status(
            "/v2/bundles:set-enabled",
            json!({"bundleId": "inventory/disabled", "enabled": false})
        )
        .await?,
        200
    );
    let nested_req = format!(" {} ", nested.display());
    assert_eq!(
        api.row(&nested_req, "inventory/nested", "project")
            .await?
            .ok_or("nested")?["state"],
        "active"
    );
    assert_eq!(
        api.row(&nested_req, broken.to_str().ok_or("b")?, "project")
            .await?
            .ok_or("broken")?["state"],
        "unreadable"
    );
    assert!(
        api.row(&nested_req, "inventory/first", "project")
            .await?
            .is_none()
    );
    assert_eq!(
        api.row(second.to_str().ok_or("s")?, "inventory/disabled", "project")
            .await?
            .ok_or("dis")?["state"],
        "disabled"
    );

    let (st, pub_resp) = api
        .post("/v2/bundles:refresh", json!({"directory": nested}))
        .await?;
    assert_eq!((st, pub_resp["scope"].as_str()), (200, Some("project")));
    let ids: Vec<&str> = pub_resp["bundles"]
        .as_array()
        .ok_or("bundles")?
        .iter()
        .filter_map(|r| r["id"].as_str())
        .filter(|id| id.starts_with("inventory/"))
        .collect();
    assert_eq!(ids, ["inventory/first"], "{pub_resp}");
    Ok(())
}
