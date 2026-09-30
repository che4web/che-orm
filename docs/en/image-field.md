# ImageField: from model to image upload

`ImageField` stores a **reference to a saved image** in a model. `che-rest::media`
handles the actual file. The steps below show what belongs in each layer.

## 1. Declare the field

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

`Option<ImageField>` creates a nullable SQLite `TEXT` column named `preview`.
After upload, that column can hold `board-previews/a1b2.png`; before upload, it
is `NULL`. **The database does not hold image bytes, a server's absolute path,
or a browser URL.** Use `ImageField` without `Option` only if server-side code
can supply a key when creating every record (for example in
`ViewSet::prepare_create()`); writing a key in JSON remains disallowed.

The `board-previews/` directory comes from the upload policy below. The media
service generates the rest of the file name; the user does not choose it.

## 2. Register upload handling with che-rest

Install `che_rest::auth::module()` and `che_rest::media::module()` in the app
registry. The following snippets belong to the same module as the `Board`
model above. First, define a serializer:

```rust
#[derive(che_orm::ModelSerializer)]
#[serializer(model = Board)]
pub struct BoardSerializer {
    #[serializer(read_only)]
    pub id: i64,
    pub title: String,
    pub preview: Option<ImageField>, // automatically read-only
}
```

Then declare the image action once in the owning ViewSet:

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

`"board-previews"` is the subdirectory within the storage root;
`2 * 1024 * 1024` is the maximum upload size in bytes; `2048` is the maximum
input width and height; `.thumbnail(640)` outputs a PNG bounded to 640 × 640.
`#[derive(Model)]` generates the typed `Board::PREVIEW` field constant.

The app module must include the model in its schema and register the ViewSet:

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

Add the modules to your application registry:

```rust
let apps = che_rest::InstalledApps::new()
    .add(che_rest::auth::module())
    .add(che_rest::media::module())
    .add(BoardsModule);
```

The framework creates `PUT /api/boards/{id}/images/preview/` to upload or
replace the file and `DELETE /api/boards/{id}/images/preview/` to clear it.
`PUT` receives a multipart `file` part. Both operations appear in OpenAPI and the generated
TypeScript client. By default, they use the ViewSet's permissions; use the
`authorize`, `before`, or `write` callbacks for application-specific rules.

```ts
await boardApi.preview.replace(boardId, file); // upload or replace
await boardApi.preview.clear(boardId);         // clear
```

The generated `clear()` sends a multipart request without a file. If calling
DELETE manually, send an empty `FormData` (or any application-specific extra
fields) as well.

`#[derive(ModelSerializer)]` makes image fields **read-only in JSON model
writes**, even without an explicit `#[serializer(read_only)]`. Clients cannot
choose a storage path using ordinary model `POST` or `PATCH` requests.

## 3. Key, disk path, and URL are different values

With the default media root, `uploads`:

| Value | Example |
| --- | --- |
| `Board.preview` / SQLite column | `board-previews/a1b2.png` |
| File on disk | `uploads/board-previews/a1b2.png` |
| Browser URL | `/api/media/board-previews/a1b2.png` |

The media root can be changed in `app.toml`:

```toml
[media]
root = "/var/lib/my-app/uploads"
```

The database key stays the same; it is resolved against the new root. Move
existing files, preserving subdirectories, when changing the root. Use
`che_rest::media::url(&image, "/api")` or append the key to `/api/media/` on
the client to form a URL. `che-orm` does not know your HTTP prefix or storage
directory. Fetching `/api/media/{key}` requires authentication.

A JSON read response contains `"preview": "board-previews/a1b2.png"`, or
`"preview": null`. This is a **key**, not a ready-to-use URL. The generated
TypeScript type for a nullable image field is `string | null`.

`ImageField::new("board-previews/a1b2.png")` only wraps a trusted string. It
**does not validate a path, upload, or create a file**. Use the image action
and `che-rest` storage for new uploads.

## 4. Cleanup and existing data

Replacement checks that the previously loaded key still matches the database
key. A stale request gets `409` and its new file is discarded. The registered
`ImageCleanup` hook cleans up the file when its record is **directly deleted
through a ViewSet**. SQLite cascades and raw SQL deletes bypass ViewSet hooks;
those paths need separate file cleanup. Preserve `uploads/` alongside database
backups.

For a **new model**, create and apply a compiled migration with a nullable
`preview` column. If an existing physical column has another name, keep it
with `#[orm(db_column = "preview_storage_name")]`. This does not transform
stored values: old `a1b2.png` keys must be changed to
`board-previews/a1b2.png` in a separate migration. If the old files already
live under `uploads/board-previews/`, no file move is needed; otherwise move
them so their paths relative to the media root match the new keys.
