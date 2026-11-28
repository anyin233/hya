//! Install every enabled native bundle's registrations the same way
//! (ADR-0040): one shape, one path, whoever wrote the bundle.

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_engine::SessionEngine;
use hya_runtime_abi::{
    HostRuntime, NativeBundleApiHandler, NativeHook, NativeRegistrations, NativeService,
    ProjectionReducer, UriSchemaHandler,
};

use crate::host_runtime::AppHostRuntime;

/// Registrations collected from the loaded native bundles, before the engine
/// exists. Tools are already in the tool registry.
#[derive(Default)]
pub(crate) struct NativeInstall {
    pub(crate) hooks: Vec<Arc<dyn NativeHook>>,
    pub(crate) uri_handlers: Vec<Arc<dyn UriSchemaHandler>>,
    reducers: Vec<Arc<dyn ProjectionReducer>>,
    apis: BTreeMap<String, Vec<Arc<dyn NativeBundleApiHandler>>>,
    services: Vec<Arc<dyn NativeService>>,
}

impl NativeInstall {
    /// Take one bundle's registrations. Drivers and declared hook points are
    /// registered with the host runtime now, namespaced by `bundle_id`.
    ///
    /// # Errors
    /// The bundle registers rewriters, which only the engine's own
    /// compaction bundle provides today.
    pub(crate) fn add(
        &mut self,
        host: &AppHostRuntime,
        bundle_id: &str,
        regs: NativeRegistrations,
    ) -> anyhow::Result<()> {
        let NativeRegistrations {
            tools: _,
            rewriters,
            services,
            hooks,
            api_handlers,
            projection_reducers,
            uri_schema_handlers,
            hook_points,
            drivers,
        } = regs;
        anyhow::ensure!(
            rewriters.is_empty(),
            "native bundle `{bundle_id}` registers context rewriters, which this host loads only from the engine's compaction bundle"
        );
        self.hooks.extend(hooks);
        self.uri_handlers.extend(uri_schema_handlers);
        self.reducers.extend(projection_reducers);
        if !api_handlers.is_empty() {
            self.apis
                .entry(bundle_id.to_string())
                .or_default()
                .extend(api_handlers);
        }
        self.services.extend(services);
        host.register_drivers(bundle_id, drivers);
        host.declare_hook_points(bundle_id, &hook_points);
        Ok(())
    }

    /// Register projection reducers and BundleApi handlers with the engine.
    ///
    /// # Errors
    /// A bundle's API endpoints are invalid or collide.
    pub(crate) fn register_with(&mut self, engine: &SessionEngine) -> anyhow::Result<()> {
        engine.register_native_projection_reducers(std::mem::take(&mut self.reducers));
        for (bundle_id, handlers) in std::mem::take(&mut self.apis) {
            engine
                .runtime_registry()
                .register_native_bundle_apis(&bundle_id, handlers)
                .map_err(|error| anyhow::anyhow!("register {bundle_id} native APIs: {error}"))?;
        }
        Ok(())
    }

    /// Start every service in load order; on a failure, stop the ones that
    /// started, in reverse.
    ///
    /// # Errors
    /// The failing service's id and error.
    pub(crate) async fn start_services(
        &mut self,
        runtime: &Arc<dyn HostRuntime>,
    ) -> anyhow::Result<Vec<Arc<dyn NativeService>>> {
        let mut started: Vec<Arc<dyn NativeService>> = Vec::new();
        for service in std::mem::take(&mut self.services) {
            if let Err(error) = service.start(Arc::clone(runtime)).await {
                for running in started.into_iter().rev() {
                    let _ = running.stop().await;
                }
                anyhow::bail!("start native bundle service `{}`: {error}", service.id());
            }
            started.push(service);
        }
        Ok(started)
    }
}
