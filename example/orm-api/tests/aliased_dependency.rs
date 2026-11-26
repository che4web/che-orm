use orm::Model;

#[derive(Debug, orm::Model)]
#[orm(table = "aliased_models")]
struct AliasedModel {
    #[orm(primary_key)]
    id: i64,
    name: String,
}

#[test]
fn model_derive_resolves_the_renamed_dependency() {
    let row = orm::Row::new(vec![
        orm::DatabaseValue::Integer(7),
        orm::DatabaseValue::Text("Alice".into()),
    ]);
    let model = AliasedModel::from_row(&row).unwrap();

    assert_eq!(model.id, 7);
    assert_eq!(model.name, "Alice");
    assert_eq!(AliasedModel::table_name(), "aliased_models");
}
