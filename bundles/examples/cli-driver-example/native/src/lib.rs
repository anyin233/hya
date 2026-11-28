//! `hya/cli-driver-example`: a minimal native bundle that adds two `hya`
//! commands (ADR-0040). `hya example-echo` runs this bundle's `echo` driver;
//! `hya example-status` calls its `status` BundleApi endpoint.

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

/// BundleApi endpoint `status` (`POST /status`): replies with a status line.
pub struct StatusApi {
    endpoints: Vec<NativeApiEndpoint>,
}

impl StatusApi {
    /// The handler with its one endpoint.
    #[must_use]
    pub fn new() -> Self {
        Self {
            endpoints: vec![NativeApiEndpoint {
                id: "status".into(),
                method: hya_runtime_abi::ApiMethod::Post,
                scope: hya_runtime_abi::ApiScope::Global,
                path: "/status".into(),
                description: "Report the example bundle's status.".into(),
            }],
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
        let name = request.body["args"]["name"].as_str().unwrap_or("example");
        Ok(NativeBundleApiReply {
            status: 200,
            body: json!({ "stdout": format!("{name}: ok\n"), "stderr": "", "exit_code": 0 }),
        })
    }
}

hya_runtime_abi::export_native_bundle_v2!(|out| {
    out.drivers.push(Arc::new(EchoDriver));
    out.api_handlers.push(Arc::new(StatusApi::new()));
});
