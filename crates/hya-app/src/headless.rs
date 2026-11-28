//! Headless run-to-completion of a bundle driver.

use std::future::Future;
use std::sync::Arc;

use hya_runtime_abi::{DriverReport, DriverRequest, HostError, NativeDriver};
use tokio_util::sync::CancellationToken;

use crate::BuiltSessionEngine;

/// Prepare the cancellable run of the driver registered as `name`
/// (`<bundle-id>:<name>`).
///
/// # Errors
/// Returns an error naming the bundle when no enabled bundle exports `name`.
pub fn prepare_driver(
    built: &BuiltSessionEngine,
    name: &str,
    request: DriverRequest,
    cancel: CancellationToken,
) -> anyhow::Result<impl Future<Output = Result<DriverReport, HostError>> + use<>> {
    let driver: Arc<dyn NativeDriver> = built.driver(name).ok_or_else(|| {
        let bundle = name.split_once(':').map_or(name, |(bundle, _)| bundle);
        anyhow::anyhow!(
            "driver `{name}` is unavailable: its bundle `{bundle}` is not enabled (running \
             with `--bare`, or `{bundle}` is disabled under `bundles.builtin` in \
             `config.yaml`)"
        )
    })?;
    let runtime = built.host_runtime();
    Ok(async move { driver.run(runtime, request, cancel).await })
}
