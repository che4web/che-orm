use crate::{
    Error, FieldSchema, FieldType, ForeignKeySchema, IndexSchema, Model, ModelSchema, Result,
    Schema,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub changes: Vec<SchemaChange>,
    pub schema: Schema,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaChange {
    CreateTable(ModelSchema),
    DropTable {
        table: String,
    },
    AddColumn {
        table: String,
        field: FieldSchema,
    },
    DropColumn {
        table: String,
        column: String,
    },
    AlterColumn {
        table: String,
        old: FieldSchema,
        new: FieldSchema,
    },
    CreateIndex {
        table: String,
        index: IndexSchema,
    },
    DropIndex {
        table: String,
        name: String,
    },
}

pub fn create_table_sql<M: Model>() -> String {
    create_table_model_sql(&ModelSchema::from_model::<M>())
}

pub fn diff_schemas(old: &Schema, new: &Schema) -> Migration {
    let mut changes = Vec::new();

    for new_model in &new.models {
        let Some(old_model) = old
            .models
            .iter()
            .find(|model| model.table == new_model.table)
        else {
            changes.push(SchemaChange::CreateTable(new_model.clone()));
            continue;
        };

        for new_field in &new_model.fields {
            match old_model
                .fields
                .iter()
                .find(|field| field.name == new_field.name)
            {
                Some(old_field) if old_field != new_field => {
                    changes.push(SchemaChange::AlterColumn {
                        table: new_model.table.clone(),
                        old: old_field.clone(),
                        new: new_field.clone(),
                    });
                }
                None => changes.push(SchemaChange::AddColumn {
                    table: new_model.table.clone(),
                    field: new_field.clone(),
                }),
                _ => {}
            }
        }

        for old_field in &old_model.fields {
            if !new_model
                .fields
                .iter()
                .any(|field| field.name == old_field.name)
            {
                changes.push(SchemaChange::DropColumn {
                    table: old_model.table.clone(),
                    column: old_field.name.clone(),
                });
            }
        }

        for new_index in &new_model.indexes {
            if !old_model.indexes.iter().any(|index| index == new_index) {
                changes.push(SchemaChange::CreateIndex {
                    table: new_model.table.clone(),
                    index: new_index.clone(),
                });
            }
        }
        for old_index in &old_model.indexes {
            if !new_model.indexes.iter().any(|index| index == old_index) {
                changes.push(SchemaChange::DropIndex {
                    table: old_model.table.clone(),
                    name: old_index.name.clone(),
                });
            }
        }
    }

    for old_model in &old.models {
        if !new
            .models
            .iter()
            .any(|model| model.table == old_model.table)
        {
            changes.push(SchemaChange::DropTable {
                table: old_model.table.clone(),
            });
        }
    }

    Migration {
        changes,
        schema: new.clone(),
    }
}

pub fn validate_migration(migration: &Migration) -> Result<()> {
    for change in &migration.changes {
        match change {
            SchemaChange::AddColumn { table, field }
                if requires_existing_value(field) && field.default.is_none() =>
            {
                return Err(Error::UnsafeMigration(format!(
                    "adding required column '{}.{}' needs a default",
                    table, field.name
                )));
            }
            SchemaChange::AlterColumn { table, old, new }
                if old.nullable && requires_existing_value(new) && new.default.is_none() =>
            {
                return Err(Error::UnsafeMigration(format!(
                    "making column '{}.{}' required needs a default",
                    table, new.name
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn requires_existing_value(field: &FieldSchema) -> bool {
    !field.nullable && !field.primary_key && !field.auto && !field.auto_now && !field.auto_now_add
}

pub fn sqlite_migration_sql(migration: &Migration) -> String {
    let mut statements = Vec::new();
    let mut rebuilt_tables = Vec::new();

    for change in &migration.changes {
        if let SchemaChange::DropColumn { table, .. } | SchemaChange::AlterColumn { table, .. } =
            change
            && !rebuilt_tables.iter().any(|rebuilt| *rebuilt == table)
        {
            rebuilt_tables.push(table);
        }
    }

    for change in &migration.changes {
        match change {
            SchemaChange::DropColumn { table, .. } | SchemaChange::AlterColumn { table, .. } => {
                if let Some(model) = migration
                    .schema
                    .models
                    .iter()
                    .find(|model| model.table == *table)
                    && !statements.iter().any(|statement: &String| {
                        statement.contains(&format!("__che_orm_new_{}", model.table))
                    })
                {
                    statements.push(rebuild_table_sql(model, &migration.changes));
                }
            }
            SchemaChange::AddColumn { table, .. }
                if rebuilt_tables.iter().any(|rebuilt| *rebuilt == table) => {}
            SchemaChange::CreateIndex { table, .. }
                if rebuilt_tables.iter().any(|rebuilt| *rebuilt == table) => {}
            SchemaChange::DropIndex { table, .. }
                if rebuilt_tables.iter().any(|rebuilt| *rebuilt == table) => {}
            _ => statements.push(sqlite_change_sql(change)),
        }
    }

    statements.join("\n\n")
}

fn rebuild_table_sql(model: &ModelSchema, changes: &[SchemaChange]) -> String {
    let temporary_table = format!("__che_orm_new_{}", model.table);
    let columns = model
        .fields
        .iter()
        .map(column_schema_sql)
        .collect::<Vec<_>>()
        .join(",\n    ");
    let copied_columns = model
        .fields
        .iter()
        .filter(|field| {
            !changes.iter().any(|change| {
                matches!(
                    change,
                    SchemaChange::AddColumn { table, field: added }
                        if table == &model.table && added.name == field.name
                )
            })
        })
        .map(|field| quote_identifier(&field.name))
        .collect::<Vec<_>>()
        .join(", ");

    let indexes = model
        .indexes
        .iter()
        .map(|index| index_sql(&model.table, index))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "PRAGMA defer_foreign_keys = ON;\nCREATE TABLE {temporary} (\n    {columns}\n);\nINSERT INTO {temporary} ({copied}) SELECT {copied} FROM {table};\nDROP TABLE {table};\nALTER TABLE {temporary} RENAME TO {table};{indexes}",
        temporary = quote_identifier(&temporary_table),
        columns = columns,
        copied = copied_columns,
        table = quote_identifier(&model.table),
        indexes = if indexes.is_empty() {
            String::new()
        } else {
            format!("\n{indexes}")
        },
    )
}

fn create_table_model_sql(model: &ModelSchema) -> String {
    let columns = model
        .fields
        .iter()
        .map(column_schema_sql)
        .collect::<Vec<_>>()
        .join(",\n    ");

    let indexes = model
        .indexes
        .iter()
        .map(|index| index_sql(&model.table, index))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n    {}\n);{}",
        model.table,
        columns,
        if indexes.is_empty() {
            String::new()
        } else {
            format!("\n{indexes}")
        }
    )
}

fn sqlite_change_sql(change: &SchemaChange) -> String {
    match change {
        SchemaChange::CreateTable(model) => create_table_model_sql(model),
        SchemaChange::DropTable { table } => format!("DROP TABLE IF EXISTS {table};"),
        SchemaChange::AddColumn { table, field } => {
            format!(
                "ALTER TABLE {table} ADD COLUMN {};",
                column_schema_sql(field)
            )
        }
        SchemaChange::CreateIndex { table, index } => index_sql(table, index),
        SchemaChange::DropIndex { name, .. } => {
            format!("DROP INDEX IF EXISTS {};", quote_identifier(name))
        }
        SchemaChange::DropColumn { .. } | SchemaChange::AlterColumn { .. } => {
            unreachable!("drop columns are handled by table rebuild")
        }
    }
}

fn index_sql(table: &str, index: &IndexSchema) -> String {
    let unique = if index.unique { "UNIQUE " } else { "" };
    let columns = index
        .columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CREATE {unique}INDEX IF NOT EXISTS {} ON {} ({columns});",
        quote_identifier(&index.name),
        quote_identifier(table),
    )
}

fn column_schema_sql(field: &FieldSchema) -> String {
    column_parts(
        &field.name,
        field.ty,
        field.primary_key,
        field.nullable,
        field.auto,
        field.unique,
        field.default.as_deref(),
        field.auto_now_add,
        field.auto_now,
        field.foreign_key.as_ref(),
        field.choices.as_deref(),
        field.max_length,
    )
}

fn column_parts(
    name: &str,
    ty: FieldType,
    primary_key: bool,
    nullable: bool,
    auto: bool,
    unique: bool,
    default: Option<&str>,
    auto_now_add: bool,
    auto_now: bool,
    foreign_key: Option<&ForeignKeySchema>,
    choices: Option<&[String]>,
    max_length: Option<u32>,
) -> String {
    let mut parts = vec![name.to_string()];

    if primary_key && auto {
        parts.push("INTEGER PRIMARY KEY AUTOINCREMENT".to_string());
    } else {
        parts.push(sql_type(ty).to_string());
        if primary_key {
            parts.push("PRIMARY KEY".to_string());
        }
    }

    if !nullable && !primary_key {
        parts.push("NOT NULL".to_string());
    }
    if unique {
        parts.push("UNIQUE".to_string());
    }
    if let Some(default) = default {
        parts.push(format!("DEFAULT {default}"));
    } else if auto_now_add || auto_now {
        parts.push("DEFAULT CURRENT_TIMESTAMP".to_string());
    }
    if let Some(foreign_key) = foreign_key {
        parts.push(format!(
            "REFERENCES {}({})",
            foreign_key.table, foreign_key.column
        ));
    }
    if let Some(choices) = choices {
        let values = choices
            .iter()
            .map(|choice| format!("'{}'", choice.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("CHECK ({name} IN ({values}))"));
    }
    if let Some(max_length) = max_length {
        parts.push(format!("CHECK (length({name}) <= {max_length})"));
    }

    parts.join(" ")
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sql_type(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Integer => "INTEGER",
        FieldType::Text => "TEXT",
        FieldType::Boolean => "BOOLEAN",
        FieldType::Real => "REAL",
        FieldType::DateTime => "TEXT",
        FieldType::Json | FieldType::Choice => "TEXT",
        FieldType::FilePath => "TEXT",
    }
}
