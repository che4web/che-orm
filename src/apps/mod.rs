pub mod accounts;
pub mod content;

use crate::AppRegistry;

/// Application registry used by the built-in `manage` binary.
pub fn registry() -> AppRegistry {
    AppRegistry::new()
        .register::<accounts::App>()
        .register::<content::App>()
}
