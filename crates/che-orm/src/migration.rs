use crate::{
    Error, FieldSchema, FieldType, ForeignKeyAction, ForeignKeySchema, IndexSchema, Model,
    ModelSchema, Result, Schema,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SQLITE_FK_REBUILD_DIRECTIVE: &str = "-- che-orm: sqlite-fk-rebuild";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub changes: Vec<SchemaChange>,
    pub old_schema: Schema,
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

pub fn sqlite_schema_sql(schema: &Schema) -> String {
    schema
        .validate()
        .expect("sqlite_schema_sql requires a valid schema");
    schema
        .models
        .iter()
        .map(create_table_model_sql)
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn postgres_schema_sql(schema: &Schema) -> String {
    schema
        .validate()
        .expect("postgres_schema_sql requires a valid schema");
    schema
        .models
        .iter()
        .map(|model| {
            let columns = model
                .fields
                .iter()
                .map(postgres_column_schema_sql)
                .collect::<Vec<_>>()
                .join(",\n    ");
            let indexes = model
                .indexes
                .iter()
                .map(|index| index_sql(&model.table, index))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "CREATE TABLE {} (\n    {}\n);{}",
                quote_identifier(&model.table),
                columns,
                if indexes.is_empty() {
                    String::new()
                } else {
                    format!("\n{indexes}")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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

        for old_index in &old_model.indexes {
            if !new_model.indexes.iter().any(|index| index == old_index) {
                changes.push(SchemaChange::DropIndex {
                    table: old_model.table.clone(),
                    name: old_index.name.clone(),
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
        old_schema: old.clone(),
        schema: new.clone(),
    }
}

pub fn validate_migration(migration: &Migration) -> Result<()> {
    migration.schema.validate()?;
    migration.old_schema.validate()?;
    validate_foreign_keys(&migration.schema)?;
    for change in &migration.changes {
        match change {
            SchemaChange::AddColumn { table, field }
                if requires_existing_value(field) && lacks_usable_default(field) =>
            {
                return Err(Error::UnsafeMigration(format!(
                    "adding required column '{}.{}' needs a default",
                    table, field.name
                )));
            }
            SchemaChange::AlterColumn { table, old, new }
                if old.nullable && requires_existing_value(new) && lacks_usable_default(new) =>
            {
                return Err(Error::UnsafeMigration(format!(
                    "making column '{}.{}' required needs a default",
                    table, new.name
                )));
            }
            SchemaChange::AlterColumn { table, old, new } if old.ty != new.ty => {
                return Err(Error::UnsafeMigration(format!(
                    "changing column '{}.{}' from {:?} to {:?} requires an explicit data conversion",
                    table, new.name, old.ty, new.ty
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(feature = "migration-native")]
pub(crate) fn validate_sqlx_migration(migration: &Migration) -> Result<()> {
    validate_migration(migration)?;
    let rebuilt_tables = rebuild_tables(&ordered_changes(migration));
    for table in rebuilt_tables {
        let inbound = migration.old_schema.models.iter().any(|model| {
            model.table != table
                && model.fields.iter().any(|field| {
                    field
                        .foreign_key
                        .as_ref()
                        .is_some_and(|foreign_key| foreign_key.table == table)
                })
        });
        if inbound {
            return Err(Error::UnsafeMigration(format!(
                "rebuilding table '{table}' with inbound foreign keys requires a manual migration"
            )));
        }
    }
    Ok(())
}

fn validate_foreign_keys(schema: &Schema) -> Result<()> {
    for model in &schema.models {
        for field in &model.fields {
            let Some(foreign_key) = &field.foreign_key else {
                continue;
            };
            let target_model = schema
                .models
                .iter()
                .find(|candidate| candidate.table == foreign_key.table)
                .ok_or_else(|| {
                    Error::UnsafeMigration(format!(
                        "foreign key '{}.{}' references missing table '{}'",
                        model.table, field.name, foreign_key.table
                    ))
                })?;
            let target_field = target_model
                .fields
                .iter()
                .find(|candidate| candidate.name == "id")
                .ok_or_else(|| {
                    Error::UnsafeMigration(format!(
                        "foreign key '{}.{}' references table '{}' without id primary key",
                        model.table, field.name, foreign_key.table
                    ))
                })?;
            if !target_field.primary_key || target_field.ty != FieldType::Integer {
                return Err(Error::UnsafeMigration(format!(
                    "foreign key '{}.{}' target '{}.id' must be an i64 primary key",
                    model.table, field.name, foreign_key.table
                )));
            }
            if foreign_key.on_delete == ForeignKeyAction::SetNull && !field.nullable {
                return Err(Error::UnsafeMigration(format!(
                    "foreign key '{}.{}' uses SET NULL but is not nullable",
                    model.table, field.name
                )));
            }
            if foreign_key.on_delete == ForeignKeyAction::SetDefault
                && field.default.is_none()
                && !field.auto_now
                && !field.auto_now_add
            {
                return Err(Error::UnsafeMigration(format!(
                    "foreign key '{}.{}' uses SET DEFAULT without a default",
                    model.table, field.name
                )));
            }
            if field.ty != FieldType::Integer {
                return Err(Error::UnsafeMigration(format!(
                    "foreign key '{}.{}' must be an INTEGER id reference to '{}'",
                    model.table, field.name, foreign_key.table
                )));
            }
        }
    }
    Ok(())
}

fn requires_existing_value(field: &FieldSchema) -> bool {
    !field.nullable && !field.primary_key && !field.auto && !field.auto_now && !field.auto_now_add
}

fn lacks_usable_default(field: &FieldSchema) -> bool {
    field
        .default
        .as_deref()
        .is_none_or(|default| default == "NULL")
}

pub fn sqlite_migration_sql(migration: &Migration) -> String {
    let changes = ordered_changes(migration);
    let mut statements = preflight_directives(migration);
    let rebuilt_tables = rebuild_tables(&changes);

    for change in &changes {
        if is_destructive(change) {
            statements.push("-- che-orm: destructive".to_string());
        }
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
                    statements.push(rebuild_table_sql(model, &changes));
                }
            }
            SchemaChange::AddColumn { table, field }
                if field.foreign_key.is_some() && field.default.is_some() =>
            {
                if let Some(model) = migration
                    .schema
                    .models
                    .iter()
                    .find(|model| model.table == *table)
                    && !statements.iter().any(|statement: &String| {
                        statement.contains(&format!("__che_orm_new_{}", model.table))
                    })
                {
                    statements.push(rebuild_table_sql(model, &changes));
                }
            }
            SchemaChange::AddColumn { table, .. }
                if rebuilt_tables.iter().any(|rebuilt| rebuilt == table) => {}
            SchemaChange::CreateIndex { table, .. }
                if rebuilt_tables.iter().any(|rebuilt| rebuilt == table) => {}
            SchemaChange::DropIndex { table, .. }
                if rebuilt_tables.iter().any(|rebuilt| rebuilt == table) => {}
            _ => statements.push(sqlite_change_sql(change)),
        }
    }

    let sql = statements.join("\n\n");
    if requires_fk_safe_rebuild(migration, &rebuilt_tables) {
        format!("{SQLITE_FK_REBUILD_DIRECTIVE}\n{sql}")
    } else {
        sql
    }
}

fn preflight_directives(migration: &Migration) -> Vec<String> {
    let mut directives = Vec::new();
    for change in &migration.changes {
        match change {
            SchemaChange::AlterColumn { table, old, new } => {
                if !old.unique && new.unique {
                    directives.push(preflight_unique(table, &new.name));
                }
                if new.choices != old.choices {
                    if let Some(values) = &new.choices {
                        directives.push(preflight_choices(table, &new.name, values));
                    }
                }
                let max_length_tightened = match (old.max_length, new.max_length) {
                    (None, Some(_)) => true,
                    (Some(old), Some(new)) => new < old,
                    _ => false,
                };
                if max_length_tightened {
                    if let Some(max_length) = new.max_length {
                        directives.push(preflight_max_length(table, &new.name, max_length));
                    }
                }
                if old.foreign_key != new.foreign_key {
                    if let Some(foreign_key) = &new.foreign_key {
                        directives.push(preflight_foreign_key(
                            table,
                            &new.name,
                            &foreign_key.table,
                        ));
                    }
                }
            }
            SchemaChange::CreateIndex { table, index } if index.unique => {
                directives.push(preflight_unique_columns(table, &index.columns));
            }
            _ => {}
        }
    }
    directives
}

fn preflight_unique(table: &str, column: &str) -> String {
    preflight_json(json!({
        "kind": "unique",
        "table": table,
        "columns": [column],
    }))
}

fn preflight_unique_columns(table: &str, columns: &[String]) -> String {
    preflight_json(json!({
        "kind": "unique",
        "table": table,
        "columns": columns,
    }))
}

fn preflight_foreign_key(table: &str, column: &str, target_table: &str) -> String {
    preflight_json(json!({
        "kind": "foreign_key",
        "table": table,
        "column": column,
        "target_table": target_table,
    }))
}

fn preflight_choices(table: &str, column: &str, values: &[String]) -> String {
    preflight_json(json!({
        "kind": "choices",
        "table": table,
        "column": column,
        "values": values,
    }))
}

fn preflight_max_length(table: &str, column: &str, max_length: u32) -> String {
    preflight_json(json!({
        "kind": "max_length",
        "table": table,
        "column": column,
        "max_length": max_length,
    }))
}

fn preflight_json(rule: serde_json::Value) -> String {
    format!("-- che-orm: preflight {}", rule)
}

fn is_destructive(change: &SchemaChange) -> bool {
    matches!(
        change,
        SchemaChange::DropTable { .. }
            | SchemaChange::DropColumn { .. }
            | SchemaChange::DropIndex { .. }
            | SchemaChange::AlterColumn { .. }
    )
}

fn rebuild_tables(changes: &[&SchemaChange]) -> Vec<String> {
    let mut tables = Vec::new();
    for change in changes {
        let table = match *change {
            SchemaChange::DropColumn { table, .. } | SchemaChange::AlterColumn { table, .. } => {
                Some(table.clone())
            }
            SchemaChange::AddColumn { table, field }
                if field.foreign_key.is_some() && field.default.is_some() =>
            {
                Some(table.clone())
            }
            _ => None,
        };
        if let Some(table) = table
            && !tables.contains(&table)
        {
            tables.push(table);
        }
    }
    tables
}

fn requires_fk_safe_rebuild(migration: &Migration, rebuilt_tables: &[String]) -> bool {
    migration
        .old_schema
        .models
        .iter()
        .chain(migration.schema.models.iter())
        .any(|model| {
            model.fields.iter().any(|field| {
                field
                    .foreign_key
                    .as_ref()
                    .is_some_and(|foreign_key| rebuilt_tables.contains(&foreign_key.table))
            })
        })
}

fn ordered_changes<'a>(migration: &'a Migration) -> Vec<&'a SchemaChange> {
    let mut dependencies = BTreeMap::<String, BTreeSet<String>>::new();
    for model in migration
        .old_schema
        .models
        .iter()
        .chain(migration.schema.models.iter())
    {
        let parents = model
            .fields
            .iter()
            .filter_map(|field| {
                field
                    .foreign_key
                    .as_ref()
                    .map(|foreign_key| foreign_key.table.clone())
            })
            .filter(|parent| parent != &model.table)
            .collect::<BTreeSet<_>>();
        dependencies
            .entry(model.table.clone())
            .or_default()
            .extend(parents);
    }

    let mut order: Vec<String> = Vec::new();
    while order.len() < dependencies.len() {
        let next = dependencies
            .iter()
            .filter(|(table, parents)| {
                !order.contains(table) && parents.iter().all(|parent| order.contains(parent))
            })
            .map(|(table, _)| table.clone())
            .next();
        let Some(table) = next else {
            // Keep cyclic dependency output deterministic; SQLite permits cyclic FK declarations.
            let remaining = dependencies
                .keys()
                .filter(|table| !order.contains(table))
                .cloned()
                .collect::<Vec<_>>();
            order.extend(remaining);
            break;
        };
        order.push(table);
    }

    let rank = |table: &str| {
        order
            .iter()
            .position(|candidate| candidate == table)
            .unwrap_or(usize::MAX)
    };
    let mut indexed = migration.changes.iter().enumerate().collect::<Vec<_>>();
    indexed.sort_by_key(|(position, change)| {
        let table = match change {
            SchemaChange::CreateTable(model) => model.table.as_str(),
            SchemaChange::DropTable { table }
            | SchemaChange::AddColumn { table, .. }
            | SchemaChange::DropColumn { table, .. }
            | SchemaChange::AlterColumn { table, .. }
            | SchemaChange::CreateIndex { table, .. }
            | SchemaChange::DropIndex { table, .. } => table.as_str(),
        };
        let reverse = matches!(
            change,
            SchemaChange::DropTable { .. }
                | SchemaChange::DropColumn { .. }
                | SchemaChange::AlterColumn { .. }
        );
        (
            if reverse {
                usize::MAX - rank(table)
            } else {
                rank(table)
            },
            *position,
        )
    });
    indexed.into_iter().map(|(_, change)| change).collect()
}

fn rebuild_table_sql(model: &ModelSchema, changes: &[&SchemaChange]) -> String {
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
        "CREATE TABLE {temporary} (\n    {columns}\n);\nINSERT INTO {temporary} ({copied}) SELECT {copied} FROM {table};\nDROP TABLE {table};\nALTER TABLE {temporary} RENAME TO {table};{indexes}",
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
        quote_identifier(&model.table),
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
        SchemaChange::DropTable { table } => {
            format!("DROP TABLE IF EXISTS {};", quote_identifier(table))
        }
        SchemaChange::AddColumn { table, field } => {
            format!(
                "ALTER TABLE {} ADD COLUMN {};",
                quote_identifier(table),
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

fn postgres_column_schema_sql(field: &FieldSchema) -> String {
    let mut parts = vec![quote_identifier(&field.name)];
    if field.primary_key && field.auto {
        parts.push("BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY".to_string());
    } else {
        parts.push(postgres_type(field.ty).to_string());
        if field.primary_key {
            parts.push("PRIMARY KEY".to_string());
        }
    }
    if !field.nullable && !field.primary_key {
        parts.push("NOT NULL".to_string());
    }
    if field.unique {
        parts.push("UNIQUE".to_string());
    }
    if let Some(default) = &field.default {
        parts.push(format!("DEFAULT {default}"));
    } else if field.auto_now || field.auto_now_add {
        parts.push("DEFAULT CURRENT_TIMESTAMP".to_string());
    }
    if let Some(foreign_key) = &field.foreign_key {
        parts.push(format!(
            "REFERENCES {}(id)",
            quote_identifier(&foreign_key.table)
        ));
        if foreign_key.on_delete != ForeignKeyAction::NoAction {
            parts.push(format!("ON DELETE {}", action_sql(foreign_key.on_delete)));
        }
    }
    if let Some(choices) = &field.choices {
        let values = choices
            .iter()
            .map(|choice| format!("'{}'", choice.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "CHECK ({} IN ({}))",
            quote_identifier(&field.name),
            values
        ));
    }
    if let Some(max_length) = field.max_length {
        parts.push(format!(
            "CHECK (length({}) <= {})",
            quote_identifier(&field.name),
            max_length
        ));
    }
    parts.join(" ")
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
    let quoted_name = quote_identifier(name);
    let mut parts = vec![quoted_name.clone()];

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
            "REFERENCES {}(id)",
            quote_identifier(&foreign_key.table)
        ));
        if foreign_key.on_delete != ForeignKeyAction::NoAction {
            parts.push(format!("ON DELETE {}", action_sql(foreign_key.on_delete)));
        }
    }
    if let Some(choices) = choices {
        let values = choices
            .iter()
            .map(|choice| format!("'{}'", choice.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("CHECK ({quoted_name} IN ({values}))"));
    }
    if let Some(max_length) = max_length {
        parts.push(format!("CHECK (length({quoted_name}) <= {max_length})"));
    }

    parts.join(" ")
}

fn action_sql(action: ForeignKeyAction) -> &'static str {
    match action {
        ForeignKeyAction::NoAction => "NO ACTION",
        ForeignKeyAction::Restrict => "RESTRICT",
        ForeignKeyAction::Cascade => "CASCADE",
        ForeignKeyAction::SetNull => "SET NULL",
        ForeignKeyAction::SetDefault => "SET DEFAULT",
    }
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

fn postgres_type(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Integer => "BIGINT",
        FieldType::Text => "TEXT",
        FieldType::Boolean => "BOOLEAN",
        FieldType::Real => "DOUBLE PRECISION",
        FieldType::DateTime => "TIMESTAMP",
        FieldType::Json => "JSONB",
        FieldType::Choice => "TEXT",
        FieldType::FilePath => "TEXT",
    }
}
