use che_orm2::{ColumnType, Model, ModelSerializer};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone)]
pub struct OpenApiOptions {
    pub title: String,
    pub version: String,
}

impl Default for OpenApiOptions {
    fn default() -> Self {
        Self {
            title: "che-orm2 API".into(),
            version: "0.1.0".into(),
        }
    }
}

pub fn openapi_json_for<M, S>(path: &str, options: OpenApiOptions) -> Value
where
    M: Model,
    S: ModelSerializer<Model = M, Input = M>,
{
    let model_name = std::any::type_name::<M>()
        .rsplit("::")
        .next()
        .unwrap_or("Model");
    let schema = M::schema();
    let mut response_properties = Map::new();
    let mut create_properties = Map::new();
    let mut response_required = Vec::new();
    let mut create_required = Vec::new();

    for field in S::fields() {
        let Some(column) = schema
            .columns
            .iter()
            .find(|column| column.name == field.name)
        else {
            continue;
        };
        let property = field_schema(column.column_type, column.nullable);
        if !field.write_only {
            response_properties.insert(field.name.to_string(), property.clone());
            if !column.nullable {
                response_required.push(field.name.to_string());
            }
        }
        if !field.read_only {
            create_properties.insert(field.name.to_string(), property);
            if !column.nullable && column.default.is_none() && !column.auto_now_add {
                create_required.push(field.name.to_string());
            }
        }
    }

    let response_schema = object_schema(response_properties, response_required);
    let create_schema = object_schema(create_properties, create_required);
    let patch_schema = object_schema(
        schema
            .columns
            .iter()
            .filter_map(|column| {
                S::fields()
                    .iter()
                    .find(|field| field.name == column.name && !field.read_only)
                    .map(|field| {
                        (
                            field.name.to_string(),
                            field_schema(column.column_type, column.nullable),
                        )
                    })
            })
            .collect(),
        Vec::new(),
    );

    let collection = format!("/{}/", path.trim_matches('/'));
    let detail = format!("{collection}{{id}}/");
    let mut paths = Map::new();
    paths.insert(
        collection,
        json!({
            "get": { "operationId": format!("list{model_name}"), "responses": { "200": response_ref(&format!("{model_name}List")) } },
            "post": {
                "operationId": format!("create{model_name}"),
                "requestBody": { "required": true, "content": { "application/json": { "schema": schema_ref(&format!("{model_name}Create")) } } },
                "responses": { "201": response_ref(model_name) }
            }
        }),
    );
    paths.insert(
        detail,
        json!({
            "get": { "operationId": format!("retrieve{model_name}"), "responses": { "200": response_ref(model_name), "404": { "description": "Not found" } } },
            "patch": {
                "operationId": format!("patch{model_name}"),
                "requestBody": { "required": true, "content": { "application/json": { "schema": schema_ref(&format!("{model_name}Patch")) } } },
                "responses": { "200": response_ref(model_name), "404": { "description": "Not found" } }
            },
            "delete": { "operationId": format!("delete{model_name}"), "responses": { "204": { "description": "No content" }, "404": { "description": "Not found" } } }
        }),
    );

    let mut schemas = Map::new();
    schemas.insert(model_name.to_string(), response_schema);
    schemas.insert(format!("{model_name}Create"), create_schema);
    schemas.insert(format!("{model_name}Patch"), patch_schema);
    schemas.insert(
        format!("{model_name}List"),
        json!({ "type": "object", "required": ["count", "results"], "properties": {
            "count": { "type": "integer" },
            "results": { "type": "array", "items": schema_ref(model_name) }
        }}),
    );

    json!({
        "openapi": "3.0.3",
        "info": { "title": options.title, "version": options.version },
        "paths": paths,
        "components": { "schemas": schemas }
    })
}

fn field_schema(column_type: ColumnType, nullable: bool) -> Value {
    let mut schema = match column_type {
        ColumnType::Integer => json!({ "type": "integer", "format": "int64" }),
        ColumnType::Text => json!({ "type": "string" }),
        ColumnType::Boolean => json!({ "type": "boolean" }),
        ColumnType::DateTime => json!({ "type": "string", "format": "date-time" }),
    };
    if nullable {
        schema["nullable"] = json!(true);
    }
    schema
}

fn object_schema(properties: Map<String, Value>, required: Vec<String>) -> Value {
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn schema_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn response_ref(name: &str) -> Value {
    json!({ "description": "OK", "content": { "application/json": { "schema": schema_ref(name) } } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(che_orm2::Model)]
    #[orm(table = "api_tasks")]
    struct ApiTask {
        #[orm(primary_key)]
        id: i64,
        title: String,
        secret: String,
    }

    #[derive(che_orm2::ModelSerializer)]
    #[serializer(model = ApiTask)]
    #[allow(dead_code)]
    struct ApiTaskSerializer {
        #[serializer(read_only)]
        id: i64,
        title: String,
        #[serializer(write_only)]
        secret: String,
    }

    #[test]
    fn openapi_uses_serializer_visibility() {
        let document =
            openapi_json_for::<ApiTask, ApiTaskSerializer>("/tasks", OpenApiOptions::default());
        assert!(document["paths"]["/tasks/"]["post"].is_object());
        assert!(document["components"]["schemas"]["ApiTask"]["properties"]["title"].is_object());
        assert!(document["components"]["schemas"]["ApiTask"]["properties"]["secret"].is_null());
        assert!(
            document["components"]["schemas"]["ApiTaskCreate"]["properties"]["secret"].is_object()
        );
    }
}
