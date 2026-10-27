pub mod models;

use che_orm::{AppConfig, SchemaSet};

pub const APP_NAME: &str = "tasks";

pub struct App;

impl AppConfig for App {
    fn name() -> &'static str {
        APP_NAME
    }

    fn schema() -> SchemaSet {
        SchemaSet::new().model::<models::Task>()
    }
}
