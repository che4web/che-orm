//! Axum integration for the typed `che-orm2` API.

mod error;
mod openapi;
mod state;
mod views;

pub use error::{RestError, RestResult};
pub use openapi::{OpenApiOptions, openapi_json, openapi_json_for};
pub use state::RestState;
pub use views::{CrudViewSet, ViewSet, router, router_with_openapi};
