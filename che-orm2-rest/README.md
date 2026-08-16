# che-orm2-rest example

Run the standalone CRUD API example from the workspace root:

```bash
cargo run -p che-orm2-rest --example rest_api
```

Routes:

```text
GET    /tasks/
POST   /tasks/
GET    /tasks/{id}/
PATCH  /tasks/{id}/
DELETE /tasks/{id}/
GET    /openapi.json
```

Create a task:

```bash
curl -X POST http://127.0.0.1:3000/tasks/ \
  -H 'content-type: application/json' \
  -d '{"title":"Write documentation","completed":false}'
```

Patch a task:

```bash
curl -X PATCH http://127.0.0.1:3000/tasks/1/ \
  -H 'content-type: application/json' \
  -d '{"completed":true}'
```
