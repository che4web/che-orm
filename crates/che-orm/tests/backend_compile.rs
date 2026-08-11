use che_orm::Model;

#[derive(Debug, Clone, che_orm::Choice)]
enum BackendCompileStatus {
    Active,
}

#[derive(Clone, Model)]
#[model(table = "backend_compile_models")]
struct BackendCompileModel {
    #[field(primary_key)]
    id: i64,
    path: che_orm::FilePath,
    status: BackendCompileStatus,
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_derive_and_file_path_codecs_compile() {
    fn assert_model<T: che_orm::SqliteModel>() {}
    fn assert_codec<T>()
    where
        T: che_orm::__private::sqlx::Type<che_orm::__private::sqlx::Sqlite>,
        for<'q> T: che_orm::__private::sqlx::Encode<'q, che_orm::__private::sqlx::Sqlite>,
        for<'r> T: che_orm::__private::sqlx::Decode<'r, che_orm::__private::sqlx::Sqlite>,
    {
    }

    assert_model::<BackendCompileModel>();
    assert_codec::<che_orm::FilePath>();
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_derive_and_file_path_codecs_compile() {
    fn assert_model<T: che_orm::PostgresModel>() {}
    fn assert_codec<T>()
    where
        T: che_orm::__private::sqlx::Type<che_orm::__private::sqlx::Postgres>,
        for<'q> T: che_orm::__private::sqlx::Encode<'q, che_orm::__private::sqlx::Postgres>,
        for<'r> T: che_orm::__private::sqlx::Decode<'r, che_orm::__private::sqlx::Postgres>,
    {
    }

    assert_model::<BackendCompileModel>();
    assert_codec::<che_orm::FilePath>();
}
