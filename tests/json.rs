use che_orm::serde_json::json;
use che_orm::{DatabaseValue, FromDatabaseValue, JsonField, Model, ModelSerializer};
#[cfg(feature = "sqlite")]
use che_orm::{ModelWriteSerializer, WriteMode};

#[derive(Debug, Model)]
#[orm(table = "json_records")]
struct JsonRecord {
    #[orm(primary_key)]
    id: i64,
    config: JsonField,
}

#[derive(ModelSerializer)]
#[serializer(model = JsonRecord)]
struct JsonRecordSerializer {
    #[serializer(read_only)]
    id: i64,
    config: JsonField,
}

#[test]
fn json_values_decode_and_reject_corrupt_storage() {
    let value = json!({"nested":{"port":null},"items":[true,12,"текст"]});
    let field = JsonField::from_database_value(&DatabaseValue::Text(value.to_string())).unwrap();
    assert_eq!(che_orm::serde_json::to_value(&field).unwrap(), value);
    assert!(JsonField::from_database_value(&DatabaseValue::Text("bad json".into())).is_err());
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn json_field_round_trips_through_typed_writes_and_serializers() {
    let db = che_orm::Database::connect(":memory:").await.unwrap();
    db.create_table::<JsonRecord>().await.unwrap();
    let value = json!({"source_path":"/srv/данные","options":[1,null,false]});
    let write = JsonRecordSerializer::is_valid(json!({"config":value}), WriteMode::Create).unwrap();
    let record = write.save(&db).await.unwrap().unwrap();
    let loaded = db.get::<JsonRecord>(record.id).await.unwrap().unwrap();
    assert_eq!(loaded.config.0, value);
    assert_eq!(
        che_orm::serde_json::to_value(JsonRecordSerializer::from_model(loaded)).unwrap()["config"],
        value
    );
}
