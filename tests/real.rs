use che_orm::{ColumnType, DatabaseValue, Model, PostgresDialect, SqlCompiler, SqliteDialect};

#[derive(Debug, Model)]
#[orm(table = "balances")]
struct Balance {
    #[orm(primary_key)]
    id: i64,
    amount: f64,
    optional_amount: Option<f64>,
}

#[test]
fn real_schema_and_sql_keep_fractional_values() {
    let schema = Balance::schema();
    assert_eq!(schema.columns[1].column_type, ColumnType::Real);
    assert!(schema.columns[2].nullable);
    assert!(
        SqlCompiler::<SqliteDialect>::compile_schema(&schema)
            .table
            .contains("amount REAL")
    );
    assert!(
        SqlCompiler::<PostgresDialect>::compile_schema(&schema)
            .table
            .contains(r#""amount" DOUBLE PRECISION"#)
    );
    let ast = Balance::insert()
        .set(Balance::AMOUNT, 12.75)
        .into_ast()
        .unwrap();
    assert_eq!(
        SqlCompiler::<SqliteDialect>::compile(&ast).params,
        vec![DatabaseValue::Real(12.75)]
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn real_fields_round_trip_and_filter_numerically() {
    let db = che_orm::Database::connect_in_memory().await.unwrap();
    db.create_table::<Balance>().await.unwrap();
    let result = db
        .insert(&Balance {
            id: 0,
            amount: 12.75,
            optional_amount: Some(-0.125),
        })
        .await
        .unwrap();
    let id = result.last_insert_rowid.unwrap();
    let row = db.get::<Balance>(id).await.unwrap().unwrap();
    assert_eq!(row.amount, 12.75);
    assert_eq!(row.optional_amount, Some(-0.125));
    let rows = db
        .fetch_all(Balance::query().filter(Balance::AMOUNT.gt(12.5)))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    db.execute(
        Balance::update()
            .set(Balance::AMOUNT, 0.25)
            .set(Balance::OPTIONAL_AMOUNT, None::<f64>)
            .filter(Balance::ID.eq(id))
            .into_ast()
            .unwrap(),
    )
    .await
    .unwrap();
    let row = db.get::<Balance>(id).await.unwrap().unwrap();
    assert_eq!(row.amount, 0.25);
    assert_eq!(row.optional_amount, None);
}
