//! Axum integration for the typed `che-orm2` API.

mod error;
mod filters;
mod openapi;
mod state;
mod views;

pub use error::{RestError, RestResult};
pub use filters::{Filter, FilterError, FilterSet, FilterSetSpec, Lookup};
pub use openapi::{OpenApiOptions, openapi_json_for};
pub use state::RestState;
pub use views::{CrudViewSet, ViewSet, router, router_with_openapi};
