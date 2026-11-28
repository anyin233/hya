//! `hya/cli-driver-example`: a minimal native bundle that adds `hya`
//! commands (ADR-0040). `hya example-echo` runs this bundle's `echo` driver;
//! `hya example-status` calls its Global `status` BundleApi endpoint on an
//! in-memory store, `hya example-db-status` the same endpoint on `--db`, and
//! `hya example-session` its Session-scope `session` endpoint on `--db`.

use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::{
    DriverReport, DriverRequest, HostError, HostRuntime, NativeApiEndpoint, NativeBundleApiHandler,
    NativeBundleApiReply, NativeBundleApiRequest, NativeDriver,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Bundle identity.
pub const BUNDLE_ID: &str = "hya/cli-driver-example";

/// Driver `echo`: prints `args.text` `args.times` times, upper-cased with
/// `args.loud`.
pub struct EchoDriver;

#[async_trait]
impl NativeDriver for EchoDriver {
    fn name(&self) -> &str {
        "echo"
    }

    async fn run(
        &self,
        _runtime: Arc<dyn HostRuntime>,
        request: DriverRequest,
        _cancel: CancellationToken,
    ) -> Result<DriverReport, HostError> {
        let text = request.args["text"]
            .as_str()
            .ok_or_else(|| HostError::InvalidInput("`text` is required".into()))?;
        let text = if request.args["loud"].as_bool() == Some(true) {
            text.to_uppercase()
        } else {
            text.to_string()
        };
        let times = request.args["times"].as_u64().unwrap_or(1);
        let mut stdout = String::new();
        for _ in 0..times {
            stdout.push_str(&text);
            stdout.push('\n');
        }
        Ok(DriverReport {
            stdout,
            ..DriverReport::default()
        })
    }
}

/// BundleApi endpoints `status` (Global `POST /status`): a status line;
/// `session` (Session `POST /session`): the bound session and the caller's
/// `cwd`, after blocking for `args.delay_ms` (default 0).
pub struct StatusApi {
    endpoints: Vec<NativeApiEndpoint>,
}

impl StatusApi {
    /// The handler with its two endpoints.
    #[must_use]
    pub fn new() -> Self {
        Self {
            endpoints: vec![
                NativeApiEndpoint {
                    id: "status".into(),
                    method: hya_runtime_abi::ApiMethod::Post,
                    scope: hya_runtime_abi::ApiScope::Global,
                    path: "/status".into(),
                    description: "Report the example bundle's status.".into(),
                },
                NativeApiEndpoint {
                    id: "session".into(),
                    method: hya_runtime_abi::ApiMethod::Post,
                    scope: hya_runtime_abi::ApiScope::Session,
                    path: "/session".into(),
                    description: "Report the session the call is bound to.".into(),
                },
            ],
        }
    }
}

impl Default for StatusApi {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NativeBundleApiHandler for StatusApi {
    fn id(&self) -> &str {
        BUNDLE_ID
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        &self.endpoints
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        let args = &request.body["args"];
        let cwd = request.body["cwd"].as_str().unwrap_or_default();
        let stdout = if request.api == "session" {
            let delay = args["delay_ms"].as_u64().unwrap_or(0);
            // No async runtime dependency in this example: block the handler.
            std::thread::sleep(std::time::Duration::from_millis(delay));
            let session = request
                .session
                .map(|session| session.to_string())
                .unwrap_or_default();
            format!("session {session}\ncwd {cwd}\n")
        } else {
            let name = args["name"].as_str().unwrap_or("example");
            format!("{name}: ok\n")
        };
        Ok(NativeBundleApiReply {
            status: 200,
            body: json!({ "stdout": stdout, "stderr": "", "exit_code": 0 }),
        })
    }
}

hya_runtime_abi::export_native_bundle_v2!(|out| {
    out.drivers.push(Arc::new(EchoDriver));
    out.api_handlers.push(Arc::new(StatusApi::new()));
});
