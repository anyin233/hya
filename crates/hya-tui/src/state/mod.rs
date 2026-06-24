pub mod data;
pub mod project;
pub mod route;
pub mod sync;

use std::sync::Arc;

use tokio::sync::RwLock;

pub use data::DataState;
pub use project::ProjectState;
pub use route::Route;
pub use sync::SyncState;

#[derive(Debug, Clone)]
pub struct AppState {
    pub data: Arc<RwLock<hya_sdk::MessageStore>>,
    pub sync: SyncState,
    pub project: ProjectState,
    pub route: Route,
}

impl AppState {
    #[must_use]
    pub fn new(project: ProjectState, route: Route) -> Self {
        Self {
            data: DataState::default().into_inner(),
            sync: SyncState::default(),
            project,
            route,
        }
    }

    pub fn navigate(&mut self, route: Route) {
        self.route = route;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(ProjectState::default(), Route::default())
    }
}
