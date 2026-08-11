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

    async fn facade_methods(
        db: &che_orm::Database,
        model: &BackendCompileModel,
    ) -> che_orm::Result<()> {
        db.create_table::<BackendCompileModel>().await?;
        db.get::<BackendCompileModel>(1).await?;
        db.all::<BackendCompileModel>().await?;
        db.update::<BackendCompileModel>(
            1,
            BackendCompileModelUpdate {
                path: None,
                status: None,
            },
        )
        .await?;
        db.save(model).await?;
        db.delete::<BackendCompileModel>(1).await?;
        db.query::<BackendCompileModel>()
            .filter(
                BackendCompileModelFields::ID
                    .gte(1_i64)
                    .and(BackendCompileModelFields::ID.gt(0_i64))
                    .or(BackendCompileModelFields::ID.lt(10_i64).not())
                    .and(BackendCompileModelFields::STATUS.eq(BackendCompileStatus::Active)),
            )
            .order_by(BackendCompileModelFields::ID)
            .order_by_desc(BackendCompileModelFields::PATH)
            .distinct()
            .limit(10)
            .offset(0)
            .all()
            .await?;
        db.query::<BackendCompileModel>()
            .filter(BackendCompileModelFields::ID.eq(1_i64))
            .first()
            .await?;
        db.query::<BackendCompileModel>()
            .filter(BackendCompileModelFields::ID.lte(10_i64))
            .count()
            .await?;
        Ok(())
    }

    let _ = facade_methods;
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

    async fn facade_methods(
        db: &che_orm::Database,
        model: &BackendCompileModel,
    ) -> che_orm::Result<()> {
        db.get::<BackendCompileModel>(1).await?;
        db.all::<BackendCompileModel>().await?;
        db.update::<BackendCompileModel>(
            1,
            BackendCompileModelUpdate {
                path: None,
                status: None,
            },
        )
        .await?;
        db.save(model).await?;
        db.delete::<BackendCompileModel>(1).await?;
        db.query::<BackendCompileModel>()
            .filter(
                BackendCompileModelFields::ID
                    .gte(1_i64)
                    .and(BackendCompileModelFields::ID.gt(0_i64))
                    .or(BackendCompileModelFields::ID.lt(10_i64).not())
                    .and(BackendCompileModelFields::STATUS.eq(BackendCompileStatus::Active)),
            )
            .order_by(BackendCompileModelFields::ID)
            .order_by_desc(BackendCompileModelFields::PATH)
            .distinct()
            .limit(10)
            .offset(0)
            .all()
            .await?;
        db.query::<BackendCompileModel>()
            .filter(BackendCompileModelFields::ID.eq(1_i64))
            .first()
            .await?;
        db.query::<BackendCompileModel>()
            .filter(BackendCompileModelFields::ID.lte(10_i64))
            .count()
            .await?;
        Ok(())
    }

    let _ = facade_methods;
}
