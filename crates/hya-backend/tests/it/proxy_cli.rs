//! `hya proxy`: readiness line, `404 hya relay` body on a non-relay path, and clean SIGTERM exit.

use crate::common::{ForegroundServer, Scratch, TestResult};

#[tokio::test]
async fn proxy_prints_readiness_line_serves_404_and_exits_cleanly_on_sigterm() -> TestResult {
    let scratch = Scratch::new("hya-proxy-cli")?;
    let mut cmd = scratch.hya();
    cmd.args(["proxy", "--host", "127.0.0.1", "--port", "0"]);
    let mut server = ForegroundServer::start(cmd, "hya proxy listening on ", None)?;

    assert!(
        server.url.starts_with("http://127.0.0.1:"),
        "unexpected readiness line: {}",
        server.url
    );
    let response = reqwest::get(&server.url).await?;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(response.text().await?, "hya relay");

    let status = server.terminate()?;
    assert!(status.success(), "{status:?}");
    Ok(())
}
