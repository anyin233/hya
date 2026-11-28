//! Install every enabled native bundle's registrations the same way
//! (ADR-0040): one shape, one path, whoever wrote the bundle.

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_engine::SessionEngine;
use hya_runtime_abi::{HostRuntime, NativeBundleApiHandler, NativeRegistrations, NativeService};

use crate::host_runtime::AppHostRuntime;

/// Registrations collected from the loaded native bundles, before the engine
/// exists. Tools are already in the tool registry.
#[derive(Default)]
pub(crate) struct NativeInstall {
    pub(crate) registrations: NativeRegistrations,
    apis: BTreeMap<String, Vec<Arc<dyn NativeBundleApiHandler>>>,
}

impl NativeInstall {
    /// Take one bundle's registrations. Drivers and declared hook points are
    /// registered with the host runtime now, namespaced by `bundle_id`.
    pub(crate) fn add(
        &mut self,
        host: &AppHostRuntime,
        bundle_id: &str,
        regs: NativeRegistrations,
    ) {
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
            // Provider kinds register at config resolution; the engine never
            // routes by kind.
            providers: _,
        } = regs;
        self.registrations.rewriters.extend(rewriters);
        self.registrations.hooks.extend(hooks);
        self.registrations
            .uri_schema_handlers
            .extend(uri_schema_handlers);
        self.registrations
            .projection_reducers
            .extend(projection_reducers);
        if !api_handlers.is_empty() {
            self.apis
                .entry(bundle_id.to_string())
                .or_default()
                .extend(api_handlers);
        }
        self.registrations.services.extend(services);
        host.register_drivers(bundle_id, drivers);
        host.declare_hook_points(bundle_id, &hook_points);
    }

    /// Register projection reducers and BundleApi handlers with the engine.
    ///
    /// # Errors
    /// A bundle's API endpoints are invalid or collide.
    pub(crate) fn register_with(&mut self, engine: &SessionEngine) -> anyhow::Result<()> {
        engine.register_native_projection_reducers(std::mem::take(
            &mut self.registrations.projection_reducers,
        ));
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
        for service in std::mem::take(&mut self.registrations.services) {
            if let Err(error) = service.start(Arc::clone(runtime)).await {
                for running in started.into_iter().rev() {
                    let _ = running.stop().await;
                }
                anyhow::bail!("start native bundle service `{}`: {error}", service.id());
            }
            crate::startup_trace::mark("native_service", Some(service.id()));
            started.push(service);
        }
        Ok(started)
    }
}
