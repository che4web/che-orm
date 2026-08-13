# che-orm-macros

Процедурные макросы для [`che-orm`](https://crates.io/crates/che-orm).

Этот crate обычно не подключается напрямую: `che-orm` реэкспортирует
`#[derive(Model)]` и `#[derive(Choice)]`.

```rust
use che_orm::Model;

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
}
```

Допустимы только ASCII SQL identifiers из букв, цифр и `_`; identifier не может
начинаться с цифры. Полная документация: <https://github.com/che4web/che-orm>.
