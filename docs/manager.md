# Runtime manager

`Application` объявляет модельный реестр и runtime settings. `Manager<App>`
предоставляет `connect`, `migrate`, `status` и, при включённом feature создания
миграций, `makemigrations`. Приложение само владеет разбором аргументов и может
добавлять собственные команды.

Запускаемый пример использует `example.sqlite` и каталог `migrations` в текущей
директории:

```bash
cargo run -p che-orm-examples --bin manager -- --help
cargo run -p che-orm-examples --bin manager -- ping
cargo run -p che-orm-examples --bin manager -- makemigrations initial
cargo run -p che-orm-examples --bin manager -- migrate
cargo run -p che-orm-examples --bin manager -- status
```

Для TOML-настроек используйте `RuntimeSettings`; формат `[database]` и
`[migrations]` приведён в [migrations.md](migrations.md). Общий `Database`
содержит только выбранный при компиляции backend. Используйте `as_sqlite()` или
`as_postgres()` только для его специализированного API.
