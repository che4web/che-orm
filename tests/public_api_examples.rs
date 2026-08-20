use che_orm::{DbEnum, Model, ModelSerializer, SchemaSet, SqliteDialect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, DbEnum)]
enum TaskStatus {
    Draft,
    #[db_enum(rename = "in_progress")]
    InProgress,
}

#[derive(Debug, Model)]
#[orm(table = "example_users")]
struct User {
    #[orm(primary_key)]
    id: i64,
    email: String,
}

#[derive(Debug, Model)]
#[orm(table = "example_tasks")]
struct Task {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "set null")]
    assignee_id: Option<i64>,
    status: TaskStatus,
}

#[derive(ModelSerializer)]
#[serializer(model = User)]
struct UserSerializer {
    #[serializer(read_only)]
    id: i64,
    email: String,
}

#[test]
fn public_api_examples_compile() {
    let query = User::query()
        .filter(User::EMAIL.eq("alice@example.test"))
        .order_by(User::ID.asc())
        .limit(20)
        .into_ast()
        .unwrap();
    assert!(
        che_orm::SqlCompiler::<SqliteDialect>::compile(&query)
            .sql
            .contains("example_users")
    );

    let schema = SchemaSet::new().model::<User>().model::<Task>();
    assert!(schema.to_sql::<SqliteDialect>().contains("example_tasks"));

    let serializer = UserSerializer::from_model(User {
        id: 1,
        email: "alice@example.test".into(),
    });
    assert_eq!(serializer.id, 1);
    assert_eq!(
        <TaskStatus as che_orm::DbEnum>::from_str("draft"),
        Some(TaskStatus::Draft)
    );
}
