# ImageField: модель и загрузка изображения

`ImageField` отвечает за **ссылку на сохранённый файл в модели**. Обработка
самого файла — задача `che-rest::media`. Этот пример показывает, что нужно
настроить в каждом слое.

## 1. Объявите поле модели

```rust
use che_orm::{ImageField, Model};

#[derive(Debug, Model)]
#[orm(table = "boards")]
pub struct Board {
    #[orm(primary_key)]
    pub id: i64,
    pub title: String,
    pub preview: Option<ImageField>,
}
```

`Option<ImageField>` создаёт nullable колонку `preview` типа `TEXT`. После
загрузки строка в базе может содержать `board-previews/a1b2.png`; пока картинки
нет — `NULL`. **В базу не записываются PNG-байты, абсолютный путь на сервере
или браузерный URL.** Для обязательного поля существует `ImageField` без
`Option`, но при создании записи сервер должен задать ему значение, например
через `ViewSet::prepare_create()`: JSON-запись ключа всё равно запрещена.

Папка в ключе — `board-previews/` — задаётся политикой загрузки на следующем
шаге. Остаток имени безопасно генерирует медиасервис, не пользователь.

## 2. Зарегистрируйте загрузку в che-rest

Установите `che_rest::auth::module()` и `che_rest::media::module()` в реестре
приложений. Фрагменты ниже относятся к тому же модулю, что и модель `Board`
из шага 1. Сначала определите сериализатор:

```rust
#[derive(che_orm::ModelSerializer)]
#[serializer(model = Board)]
pub struct BoardSerializer {
    #[serializer(read_only)]
    pub id: i64,
    pub title: String,
    pub preview: Option<ImageField>, // уже read-only благодаря типу
}
```

Затем объявите image-действие в ViewSet:

```rust
use che_rest::{
    AppResult, ViewSet, ViewSetConfig,
    auth::IsAuthenticated,
    media::{ImageCleanup, ImageFieldEndpoint, ImagePolicy},
};

#[derive(Clone, Copy, Default)]
pub struct BoardViewSet;

impl ViewSet for BoardViewSet {
    type Model = Board;
    type Serializer = BoardSerializer;
    type ListSerializer = Self::Serializer;
    type QuerySet = che_orm::DatabaseQuery<Board>;
    type FilterSet = che_rest::FilterSet<Board>;
    type Permission = IsAuthenticated;

    fn configure(&self, config: &mut ViewSetConfig<Self>) -> AppResult<()> {
        config.mutation_hook(ImageCleanup::<Self> {
            id: "board-preview",
            image: |board| board.preview.as_ref(),
        })?;
        config.image_field(ImageFieldEndpoint::<Self>::new(
            "preview",
            Board::PREVIEW,
            |board| board.preview.as_ref(),
            ImagePolicy::new("board-previews", 2 * 1024 * 1024, 2048).thumbnail(640),
        ))
    }

    fn get_queryset(&self) -> Self::QuerySet {
        che_orm::DatabaseQuery::new(Board::query())
    }

    fn path(&self) -> &'static str { "/boards" }
}
```

`"board-previews"` — подкаталог в хранилище; `2 * 1024 * 1024` — максимум
байтов входного файла; `2048` — максимум входной ширины и высоты;
`.thumbnail(640)` создаёт PNG размером не более 640 × 640. `Board::PREVIEW` —
типизированная константа, которую сгенерировал `#[derive(Model)]`.

Модуль приложения должен включить модель в свою схему и зарегистрировать
ViewSet через `context.viewset_with(BoardViewSet)`:

```rust
pub struct BoardsModule;

impl che_rest::AppModule for BoardsModule {
    fn name(&self) -> &'static str { "boards" }

    fn schema(&self) -> che_orm::SchemaSet {
        che_orm::SchemaSet::new().model::<Board>()
    }

    fn init(&self, context: &mut che_rest::ModuleContext) {
        context.viewset_with(BoardViewSet);
    }
}
```

И добавьте модули в реестр приложения:

```rust
let apps = che_rest::InstalledApps::new()
    .add(che_rest::auth::module())
    .add(che_rest::media::module())
    .add(BoardsModule);
```

Для обычного случая обработчики HTTP писать не нужно: фреймворк создаст
`PUT /api/boards/{id}/images/preview/` для загрузки и замены и
`DELETE /api/boards/{id}/images/preview/` для очистки. `PUT` принимает
multipart-поле `file`. Доступ наследуется от ViewSet; при особых правилах
используйте его callbacks `authorize`, `before` и `write`. Фреймворк также
добавляет действия в OpenAPI и генерируемый TypeScript-клиент.

```ts
await boardApi.preview.replace(boardId, file); // загрузить или заменить
await boardApi.preview.clear(boardId);         // очистить
```

Сгенерированный `clear()` отправляет multipart-запрос без файла. Если вы
обращаетесь к DELETE вручную, также отправьте пустую `FormData` (либо нужные
дополнительные поля для приложения).

Для `ModelSerializer` тип `ImageField` **автоматически read-only** в JSON-записи,
даже без `#[serializer(read_only)]`. Путь нельзя подставить через обычный
`POST`/`PATCH` модели: он появляется после проверенной загрузки.

## 3. Различайте ключ, путь на диске и URL

При корне `uploads` (значение по умолчанию) значения выглядят так:

| Что | Пример |
| --- | --- |
| Значение `Board.preview` и колонка SQLite | `board-previews/a1b2.png` |
| Файл на диске | `uploads/board-previews/a1b2.png` |
| URL для браузера | `/api/media/board-previews/a1b2.png` |

В `app.toml` можно задать другой корень:

```toml
[media]
root = "/var/lib/my-app/uploads"
```

Ключ в базе при этом не меняется; меняется место, относительно которого он
вычисляется. При смене корня нужно **перенести существующие файлы**, сохранив
подкаталоги. URL создаёт `che-rest::media::url(&image, "/api")` или клиент,
добавляя ключ к `/api/media/`; `che-orm` не знает адресов HTTP и каталогов.
Получение файла через `/api/media/{key}` требует авторизации.

В JSON чтения поле выглядит как `"preview": "board-previews/a1b2.png"` либо
`"preview": null`. Это **ключ**, не готовый URL. Генератор TypeScript выводит
тип `string | null` для nullable-поля.

`ImageField::new("board-previews/a1b2.png")` лишь оборачивает строку для
доверенного серверного кода. Метод **не проверяет путь, не загружает и не
создаёт файл**. Для загрузки используйте image-действие и `ImageStorage` в
`che-rest`.

## 4. Удаление и существующие данные

При замене медиасервис сверяет старый ключ с текущим в базе: устаревшая
загрузка получает `409`, её новый файл удаляется. Зарегистрированный выше
`ImageCleanup` удаляет файл после **прямого удаления записи через ViewSet**.
SQLite-каскады и raw SQL deletes не запускают ViewSet-хуки: для них нужна
отдельная очистка файлов. Сохраняйте каталог `uploads/` вместе с резервной
копией базы.

Для **новой модели** достаточно создать и применить обычную compiled
миграцию с nullable колонкой `preview`. Если вы только переименовываете Rust
поле поверх старой колонки, используйте
`#[orm(db_column = "preview_storage_name")]`. Это сохраняет имя SQL-колонки,
но **не преобразует её значения**: старые `a1b2.png` следует превратить в
`board-previews/a1b2.png` отдельной миграцией. Если старые файлы уже лежат в
`uploads/board-previews/`, переносить их не нужно; иначе перенесите файлы так,
чтобы путь от корня хранилища совпадал с новым ключом.
