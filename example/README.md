# Runnable examples

All three examples are workspace members and use the local `che-orm` API.
The modular example also has a workspace member for its `tasks` app crate.

| Directory | Cargo package | What it shows |
| --- | --- | --- |
| [`orm-api`](orm-api/) | `che-orm-examples` | Typed queries, CRUD, transactions, serializers, and compiled migrations. |
| [`template-single`](template-single/) | `template-single` | The `single` application layout, with `Task` in `src/models.rs` and its registration in `src/lib.rs`. |
| [`template-modular`](template-modular/) | `template-modular` | The `modular` layout, with the model, `AppConfig`, and migrations in the separate `apps/tasks/` crate. |

Run the template apps from the repository root:

```bash
cargo run -p template-single --bin template-single-manage -- migrate
cargo run -p template-single

cargo run -p template-modular --bin template-modular-manage -- migrate
cargo run -p template-modular
```

Both apps ship the same initial task migration and use separate SQLite files by
default. They do not migrate at startup. When overriding `CHE_ORM_DATABASE_URL`,
use a separate disposable database for each app. See the
[template guide](../docs/en/template.md) to generate a standalone project instead;
its migration history starts empty.
