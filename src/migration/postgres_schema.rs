use super::*;
use crate::{PostgresDialect, SqlDialect};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresOperationPlan {
    pub statements: Vec<String>,
    /// Foreign keys whose referenced key is not yet available.
    pub deferred_foreign_keys: Vec<DeferredForeignKey>,
    pub resulting_state: ProjectState,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PostgresSchemaEditor;

/// A foreign key whose definition is resolved from the current schema state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredForeignKey {
    pub table: String,
    pub column: String,
}

impl DeferredForeignKey {
    fn matches(&self, table: &str, column: &str) -> bool {
        self.table == table && self.column == column
    }
    fn source<'a>(&self, state: &'a ProjectState) -> Option<&'a ColumnState> {
        state
            .tables()
            .iter()
            .find(|table| table.name == self.table)?
            .columns
            .iter()
            .find(|column| column.name == self.column)
    }
    pub fn statement(&self, state: &ProjectState) -> Result<String, MigrationError> {
        let column = self
            .source(state)
            .filter(|column| column.references.is_some())
            .ok_or_else(|| {
                MigrationError::InvalidState(format!(
                    "missing deferred foreign key {}.{}",
                    self.table, self.column
                ))
            })?;
        add_foreign_key(&self.table, column)
    }
    #[cfg(feature = "postgres")]
    pub(crate) fn exists(&self, state: &ProjectState) -> bool {
        self.source(state)
            .is_some_and(|column| column.references.is_some())
    }
    #[cfg(feature = "postgres")]
    pub(crate) fn ready(&self, state: &ProjectState) -> bool {
        self.source(state)
            .is_some_and(|column| foreign_ready(column, state))
    }
    #[cfg(feature = "postgres")]
    pub(crate) fn follow(&mut self, operation: &MigrationOperation) {
        match operation {
            MigrationOperation::RenameTable { old_name, new_name } if self.table == *old_name => {
                self.table = new_name.clone()
            }
            MigrationOperation::RenameColumn {
                table,
                old_name,
                new_name,
            } if self.matches(table, old_name) => self.column = new_name.clone(),
            MigrationOperation::RunSql {
                state_operations, ..
            } => {
                for operation in state_operations {
                    self.follow(operation);
                }
            }
            _ => {}
        }
    }
}

fn foreign_ready(column: &ColumnState, state: &ProjectState) -> bool {
    let Some((target, target_column)) = column
        .references
        .as_ref()
        .and_then(ForeignKeyState::target_parts)
    else {
        return true;
    };
    state
        .tables()
        .iter()
        .find(|table| table.name == target)
        .is_some_and(|table| reference_target_ready(table, target_column))
}

fn reference_target_ready(table: &TableState, column: &str) -> bool {
    table
        .columns
        .iter()
        .any(|candidate| candidate.name == column && (candidate.primary_key || candidate.unique))
        || table
            .unique_constraints
            .iter()
            .any(|columns| columns.len() == 1 && columns[0] == column)
}

fn name(table: &str, kind: &str, columns: &[String]) -> String {
    crate::schema::schema_object_name(
        table,
        kind,
        &columns.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}
fn col_name(table: &str, kind: &str, column: &str) -> String {
    crate::schema::schema_object_name(table, kind, &[column])
}
fn columns_sql(columns: &[String]) -> String {
    columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ")
}
fn type_sql(ty: ColumnType) -> String {
    let mut sql = String::new();
    PostgresDialect::column_type(ty, &mut sql);
    sql
}
fn default_sql(column: &ColumnState) -> Option<String> {
    column
        .default
        .clone()
        .or_else(|| (column.auto_now || column.auto_now_add).then(|| "CURRENT_TIMESTAMP".into()))
}
fn choices_sql(column: &ColumnState) -> String {
    format!(
        "{} IN ({})",
        quote_identifier(&column.name),
        column
            .choices
            .as_ref()
            .unwrap()
            .iter()
            .map(|value| format!("'{}'", value.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
fn foreign_sql(column: &ColumnState) -> Result<String, MigrationError> {
    let reference = column.references.as_ref().unwrap();
    let (table, col) = reference
        .target_parts()
        .ok_or_else(|| MigrationError::InvalidState("invalid foreign key".into()))?;
    let mut sql = format!(
        "REFERENCES {}({})",
        quote_identifier(table),
        quote_identifier(col)
    );
    if let Some(action) = &reference.on_delete {
        if !matches!(
            action.to_ascii_lowercase().as_str(),
            "cascade" | "restrict" | "set null" | "set default" | "no action"
        ) {
            return Err(MigrationError::InvalidState(
                "invalid ON DELETE action".into(),
            ));
        }
        sql.push_str(&format!(" ON DELETE {}", action.to_ascii_uppercase()));
    }
    Ok(sql)
}
fn add_foreign_key(table: &str, column: &ColumnState) -> Result<String, MigrationError> {
    Ok(format!(
        "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) {}",
        quote_identifier(table),
        quote_identifier(&col_name(table, "fk", &column.name)),
        quote_identifier(&column.name),
        foreign_sql(column)?
    ))
}
fn column_sql(table: &str, column: &ColumnState, foreign: bool) -> Result<String, MigrationError> {
    let mut sql = format!(
        "{} {}",
        quote_identifier(&column.name),
        type_sql(column.column_type)
    );
    if column.primary_key && column.column_type == ColumnType::Integer {
        sql.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    }
    if column.primary_key {
        sql.push_str(&format!(
            " CONSTRAINT {} PRIMARY KEY",
            quote_identifier(&col_name(table, "pk", &column.name))
        ));
    }
    if !column.nullable && (!column.primary_key || column.column_type == ColumnType::Uuid) {
        sql.push_str(" NOT NULL");
    }
    if column.unique {
        sql.push_str(&format!(
            " CONSTRAINT {} UNIQUE",
            quote_identifier(&col_name(table, "field_unique", &column.name))
        ));
    }
    if let Some(default) = default_sql(column) {
        sql.push_str(&format!(" DEFAULT {default}"));
    }
    if let Some(check) = &column.check {
        sql.push_str(&format!(
            " CONSTRAINT {} CHECK ({check})",
            quote_identifier(&col_name(table, "check", &column.name))
        ));
    }
    if column.choices.is_some() {
        sql.push_str(&format!(
            " CONSTRAINT {} CHECK ({})",
            quote_identifier(&col_name(table, "choices", &column.name)),
            choices_sql(column)
        ));
    }
    if foreign && column.references.is_some() {
        sql.push_str(&format!(
            " CONSTRAINT {} {}",
            quote_identifier(&col_name(table, "fk", &column.name)),
            foreign_sql(column)?
        ));
    }
    Ok(sql)
}
fn index_sql(table: &str, columns: &[String], position: usize) -> String {
    format!(
        "CREATE INDEX {} ON {} ({})",
        quote_identifier(&PostgresDialect::index_name(table, position)),
        quote_identifier(table),
        columns_sql(columns)
    )
}
fn table<'a>(state: &'a ProjectState, name: &str) -> Result<&'a TableState, MigrationError> {
    state
        .tables()
        .iter()
        .find(|table| table.name == name)
        .ok_or_else(|| MigrationError::InvalidState(format!("missing table {name}")))
}
fn constraints(table: &TableState, pending_columns: &BTreeSet<usize>) -> Vec<String> {
    let mut names = Vec::new();
    for (index, column) in table.columns.iter().enumerate() {
        for (kind, present) in [
            ("pk", column.primary_key),
            ("field_unique", column.unique),
            ("check", column.check.is_some()),
            ("choices", column.choices.is_some()),
            (
                "fk",
                column.references.is_some() && !pending_columns.contains(&index),
            ),
        ] {
            if present {
                names.push(col_name(&table.name, kind, &column.name));
            }
        }
    }
    names.extend(
        table
            .unique_constraints
            .iter()
            .map(|columns| name(&table.name, "unique", columns)),
    );
    names
}

impl PostgresSchemaEditor {
    pub fn plan(
        operation: &MigrationOperation,
        before: &ProjectState,
    ) -> Result<PostgresOperationPlan, MigrationError> {
        Self::plan_with_deferred(operation, before, &[])
    }

    pub(crate) fn plan_with_deferred(
        operation: &MigrationOperation,
        before: &ProjectState,
        pending: &[DeferredForeignKey],
    ) -> Result<PostgresOperationPlan, MigrationError> {
        let mut after = before.clone();
        operation.state_forwards(&mut after)?;
        let mut statements = Vec::new();
        let mut deferred = Vec::new();
        match operation {
            MigrationOperation::CreateModel { model } => {
                create(&model.table, before, &mut statements, &mut deferred)?
            }
            MigrationOperation::CreateTable { table } => {
                create(table, before, &mut statements, &mut deferred)?
            }
            MigrationOperation::DropTable { name } => {
                statements.push(format!("DROP TABLE {}", quote_identifier(name)))
            }
            MigrationOperation::RenameTable { old_name, new_name } => {
                statements.push(format!(
                    "ALTER TABLE {} RENAME TO {}",
                    quote_identifier(old_name),
                    quote_identifier(new_name)
                ));
                rename_constraints(
                    table(before, old_name)?,
                    table(&after, new_name)?,
                    pending,
                    &mut statements,
                );
                for position in 0..table(before, old_name)?.indexes.len() {
                    statements.push(format!(
                        "ALTER INDEX {} RENAME TO {}",
                        quote_identifier(&PostgresDialect::index_name(old_name, position)),
                        quote_identifier(&PostgresDialect::index_name(new_name, position))
                    ));
                }
            }
            MigrationOperation::RenameColumn {
                table: table_name,
                old_name,
                new_name,
            } => {
                statements.push(format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {}",
                    quote_identifier(table_name),
                    quote_identifier(old_name),
                    quote_identifier(new_name)
                ));
                rename_constraints(
                    table(before, table_name)?,
                    table(&after, table_name)?,
                    pending,
                    &mut statements,
                );
            }
            MigrationOperation::AddColumn { table, column } => {
                let foreign = foreign_ready(column, &after);
                statements.push(format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    quote_identifier(table),
                    column_sql(table, column, foreign)?
                ));
                if !foreign {
                    deferred.push(DeferredForeignKey {
                        table: table.clone(),
                        column: column.name.clone(),
                    });
                }
            }
            MigrationOperation::RemoveColumn {
                table: table_name,
                column,
            } => {
                statements.push(format!(
                    "ALTER TABLE {} DROP COLUMN {}",
                    quote_identifier(table_name),
                    quote_identifier(column)
                ));
                renumber_indexes(
                    table(before, table_name)?,
                    table(&after, table_name)?,
                    &mut statements,
                );
            }
            MigrationOperation::AlterColumn {
                table,
                old_column,
                new_column,
            } => {
                let absent = pending
                    .iter()
                    .any(|key| key.matches(table, &old_column.name));
                let replacing = absent
                    || old_column.references != new_column.references
                    || old_column.column_type != new_column.column_type;
                let foreign = !replacing || foreign_ready(new_column, &after);
                let mut old = old_column.clone();
                let mut new = new_column.clone();
                if absent {
                    old.references = None;
                }
                if !foreign {
                    new.references = None;
                    deferred.push(DeferredForeignKey {
                        table: table.clone(),
                        column: new_column.name.clone(),
                    });
                }
                alter(table, &old, &new, &mut statements)?;
            }
            MigrationOperation::AddIndex {
                table: table_name,
                columns,
            } => statements.push(index_sql(
                table_name,
                columns,
                table(before, table_name)?.indexes.len(),
            )),
            MigrationOperation::RemoveIndex {
                table: table_name,
                columns,
            } => {
                let position = table(before, table_name)?
                    .indexes
                    .iter()
                    .position(|item| item == columns)
                    .ok_or_else(|| MigrationError::InvalidState("missing index".into()))?;
                statements.push(format!(
                    "DROP INDEX {}",
                    quote_identifier(&PostgresDialect::index_name(table_name, position))
                ));
                renumber_indexes(
                    table(before, table_name)?,
                    table(&after, table_name)?,
                    &mut statements,
                );
            }
            MigrationOperation::AddUniqueConstraint { table, columns } => statements.push(format!(
                "ALTER TABLE {} ADD CONSTRAINT {} UNIQUE ({})",
                quote_identifier(table),
                quote_identifier(&name(table, "unique", columns)),
                columns_sql(columns)
            )),
            MigrationOperation::RemoveUniqueConstraint { table, columns } => {
                statements.push(format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    quote_identifier(table),
                    quote_identifier(&name(table, "unique", columns))
                ))
            }
            MigrationOperation::BackfillColumn {
                table,
                column,
                value,
            } => statements.push(format!(
                "UPDATE {} SET {} = {value} WHERE {} IS NULL",
                quote_identifier(table),
                quote_identifier(column),
                quote_identifier(column)
            )),
            MigrationOperation::CreateSchemaObject { object } => {
                statements.push(object.sql.clone())
            }
            MigrationOperation::DropSchemaObject { kind, name } => {
                let keyword = match kind {
                    SchemaObjectKind::Table => "TABLE",
                    SchemaObjectKind::View => "VIEW",
                    SchemaObjectKind::Trigger => "TRIGGER",
                };
                let mut sql = format!("DROP {keyword} {}", quote_identifier(name));
                if *kind == SchemaObjectKind::Trigger {
                    let object = before
                        .objects()
                        .iter()
                        .find(|o| o.kind == *kind && o.name == *name)
                        .ok_or_else(|| MigrationError::InvalidState("missing trigger".into()))?;
                    let words = object.sql.split_whitespace().collect::<Vec<_>>();
                    let target = words
                        .windows(2)
                        .find(|w| w[0].eq_ignore_ascii_case("ON"))
                        .map(|w| w[1].trim_matches('"'))
                        .ok_or_else(|| {
                            MigrationError::InvalidState(
                                "trigger target requires explicit RunSql".into(),
                            )
                        })?;
                    crate::schema::validate_identifier(target).map_err(|error| {
                        MigrationError::InvalidState(format!("invalid trigger target: {error:?}"))
                    })?;
                    sql.push_str(&format!(" ON {}", quote_identifier(target)));
                }
                statements.push(sql);
            }
            MigrationOperation::RunSql { forward, .. } => statements.push(forward.clone()),
        }
        Ok(PostgresOperationPlan {
            statements,
            deferred_foreign_keys: deferred,
            resulting_state: after,
        })
    }
}
fn create(
    table: &TableState,
    before: &ProjectState,
    sql: &mut Vec<String>,
    deferred: &mut Vec<DeferredForeignKey>,
) -> Result<(), MigrationError> {
    let mut definitions = Vec::new();
    for column in &table.columns {
        let foreign = column.references.as_ref().is_none_or(|reference| {
            reference
                .target_parts()
                .is_some_and(|(target, target_column)| {
                    if target == table.name {
                        reference_target_ready(table, target_column)
                    } else {
                        foreign_ready(column, before)
                    }
                })
        });
        definitions.push(column_sql(&table.name, column, foreign)?);
        if !foreign {
            deferred.push(DeferredForeignKey {
                table: table.name.clone(),
                column: column.name.clone(),
            });
        }
    }
    for columns in &table.unique_constraints {
        definitions.push(format!(
            "CONSTRAINT {} UNIQUE ({})",
            quote_identifier(&name(&table.name, "unique", columns)),
            columns_sql(columns)
        ));
    }
    sql.push(format!(
        "CREATE TABLE {} ({})",
        quote_identifier(&table.name),
        definitions.join(", ")
    ));
    sql.extend(
        table
            .indexes
            .iter()
            .enumerate()
            .map(|(i, columns)| index_sql(&table.name, columns, i)),
    );
    Ok(())
}
fn rename_constraints(
    before: &TableState,
    after: &TableState,
    pending: &[DeferredForeignKey],
    sql: &mut Vec<String>,
) {
    let pending_columns = before
        .columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| {
            pending
                .iter()
                .any(|key| key.matches(&before.name, &column.name))
                .then_some(index)
        })
        .collect::<BTreeSet<_>>();
    for (old, new) in constraints(before, &pending_columns)
        .into_iter()
        .zip(constraints(after, &pending_columns))
    {
        if old != new {
            sql.push(format!(
                "ALTER TABLE {} RENAME CONSTRAINT {} TO {}",
                quote_identifier(&after.name),
                quote_identifier(&old),
                quote_identifier(&new)
            ));
        }
    }
}
fn renumber_indexes(before: &TableState, after: &TableState, sql: &mut Vec<String>) {
    for (new, columns) in after.indexes.iter().enumerate() {
        if let Some(old) = before.indexes.iter().position(|item| item == columns)
            && old != new
        {
            sql.push(format!(
                "ALTER INDEX {} RENAME TO {}",
                quote_identifier(&PostgresDialect::index_name(&before.name, old)),
                quote_identifier(&PostgresDialect::index_name(&after.name, new))
            ));
        }
    }
}
fn alter(
    table: &str,
    old: &ColumnState,
    new: &ColumnState,
    sql: &mut Vec<String>,
) -> Result<(), MigrationError> {
    let prefix = format!("ALTER TABLE {}", quote_identifier(table));
    let column = quote_identifier(&new.name);
    let drop = |kind: &str| {
        format!(
            "{prefix} DROP CONSTRAINT {}",
            quote_identifier(&col_name(table, kind, &old.name))
        )
    };
    let old_identity = old.primary_key && old.column_type == ColumnType::Integer;
    let new_identity = new.primary_key && new.column_type == ColumnType::Integer;
    let type_changed = old.column_type != new.column_type;
    let replace_check = type_changed || old.check != new.check;
    let replace_choices = type_changed || old.choices != new.choices;
    let replace_foreign = type_changed || old.references != new.references;
    let old_default = default_sql(old);
    let new_default = default_sql(new);
    let replace_default =
        type_changed || old_default != new_default || (!old_identity && new_identity);

    // Drop constraints that may depend on the old type before changing it.
    if replace_foreign && old.references.is_some() {
        sql.push(drop("fk"));
    }
    if replace_check && old.check.is_some() {
        sql.push(drop("check"));
    }
    if replace_choices && old.choices.is_some() {
        sql.push(drop("choices"));
    }
    if old.unique && !new.unique {
        sql.push(drop("field_unique"));
    }
    if old_identity && !new_identity {
        sql.push(format!("{prefix} ALTER COLUMN {column} DROP IDENTITY"));
    }
    if old.primary_key && !new.primary_key {
        sql.push(drop("pk"));
    }
    if replace_default && old_default.is_some() {
        sql.push(format!("{prefix} ALTER COLUMN {column} DROP DEFAULT"));
    }
    if type_changed {
        sql.push(format!(
            "{prefix} ALTER COLUMN {column} TYPE {}",
            type_sql(new.column_type)
        ));
    }
    if old.nullable != new.nullable {
        sql.push(format!(
            "{prefix} ALTER COLUMN {column} {} NOT NULL",
            if new.nullable { "DROP" } else { "SET" }
        ));
    }
    if !old_identity && new_identity {
        sql.push(format!(
            "{prefix} ALTER COLUMN {column} ADD GENERATED BY DEFAULT AS IDENTITY"
        ));
    }
    if !old.primary_key && new.primary_key {
        sql.push(format!(
            "{prefix} ADD CONSTRAINT {} PRIMARY KEY ({column})",
            quote_identifier(&col_name(table, "pk", &new.name))
        ));
    }
    if replace_default && let Some(value) = new_default {
        sql.push(format!(
            "{prefix} ALTER COLUMN {column} SET DEFAULT {value}"
        ));
    }
    if !old.unique && new.unique {
        sql.push(format!(
            "{prefix} ADD CONSTRAINT {} UNIQUE ({column})",
            quote_identifier(&col_name(table, "field_unique", &new.name))
        ));
    }
    if replace_check && let Some(check) = &new.check {
        sql.push(format!(
            "{prefix} ADD CONSTRAINT {} CHECK ({check})",
            quote_identifier(&col_name(table, "check", &new.name))
        ));
    }
    if replace_choices && new.choices.is_some() {
        sql.push(format!(
            "{prefix} ADD CONSTRAINT {} CHECK ({})",
            quote_identifier(&col_name(table, "choices", &new.name)),
            choices_sql(new)
        ));
    }
    if replace_foreign && new.references.is_some() {
        sql.push(add_foreign_key(table, new)?);
    }
    Ok(())
}
