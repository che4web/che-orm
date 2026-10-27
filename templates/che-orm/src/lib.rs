pub mod migrations;
pub mod models;
pub mod settings;

use che_orm::{AppConfig, AppRegistry, SchemaSet};
use models::Task;

pub struct App;

impl AppConfig for App {
    fn name() -> &'static str {
        "tasks"
    }

    fn schema() -> SchemaSet {
        SchemaSet::new().model::<Task>()
    }
}

pub fn registry() -> AppRegistry {
    AppRegistry::new().register::<App>()
}
