use axum::Router;
use che_orm2::{Model, ModelSerializer};
use che_orm2_rest::{CrudViewSet, RestState, router_with_openapi};

#[derive(Debug, Model)]
#[orm(table = "tasks")]
struct Task {
    #[orm(primary_key)]
    id: i64,
    title: String,
    completed: bool,
}

#[derive(ModelSerializer)]
#[serializer(model = Task)]
struct TaskSerializer {
    #[serializer(read_only)]
    id: i64,
    title: String,
    completed: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = che_orm2::Database::connect("rest-api.db")?;
    let table_exists = database
        .transaction(|connection| {
            connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tasks')",
                [],
                |row| row.get::<_, bool>(0),
            )
        })
        .await?;
    if !table_exists {
        database.create_table::<Task>().await?;
    }

    let state = RestState::new(database);
    let app: Router =
        router_with_openapi(state, CrudViewSet::<Task, TaskSerializer>::new("/tasks"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!("REST API listening on http://{}", listener.local_addr()?);
    println!("OpenAPI: http://127.0.0.1:3000/openapi.json");
    axum::serve(listener, app).await?;
    Ok(())
}
