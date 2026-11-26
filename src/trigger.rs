//! Declarative SQLite triggers for writes on registered models.
use crate::migration::quote_identifier as quote;
use crate::{
    ColumnRef, MigrationError, Model, ModelField, ProjectState, SchemaObjectKind, SchemaObjectState,
};
use std::marker::PhantomData;

/// A typed field reference inside a trigger condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerField {
    column: ColumnRef,
    row: Row,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    New,
    Old,
    Source,
}

impl TriggerField {
    /// The incoming value of a field on the trigger's model.
    pub fn new_row<M, T>(field: &ModelField<M, T>) -> Self {
        Self {
            column: field.column(),
            row: Row::New,
        }
    }

    /// The previous value of a field on the trigger's model.
    pub fn old_row<M, T>(field: &ModelField<M, T>) -> Self {
        Self {
            column: field.column(),
            row: Row::Old,
        }
    }

    /// A field of the local model in an existence query.
    pub fn source<M, T>(field: &ModelField<M, T>) -> Self {
        Self {
            column: field.column(),
            row: Row::Source,
        }
    }

    pub fn eq(self, other: Self) -> TriggerCondition {
        TriggerCondition(Predicate::Compare(self, "=", other))
    }

    pub fn ne(self, other: Self) -> TriggerCondition {
        TriggerCondition(Predicate::Compare(self, "<>", other))
    }

    pub fn is_not_null(self) -> TriggerCondition {
        TriggerCondition(Predicate::NotNull(self))
    }
}

/// A boolean expression evaluated by the database on each affected row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerCondition(Predicate);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Predicate {
    Compare(TriggerField, &'static str, TriggerField),
    NotNull(TriggerField),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
    NotExists(&'static str, Box<Predicate>),
}

impl TriggerCondition {
    pub fn and(self, other: Self) -> Self {
        Self(Predicate::And(Box::new(self.0), Box::new(other.0)))
    }

    pub fn or(self, other: Self) -> Self {
        Self(Predicate::Or(Box::new(self.0), Box::new(other.0)))
    }

    pub fn not_exists<M: Model>(condition: Self) -> Self {
        Self(Predicate::NotExists(M::table_name(), Box::new(condition.0)))
    }
}

/// A database guard attached to a registered model. Each selected event
/// is lowered to a row trigger with a stable name derived from the guard name.
pub struct SqliteGuard<M> {
    definition: GuardDefinition,
    model: PhantomData<fn() -> M>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuardDefinition {
    pub(crate) table: &'static str,
    pub(crate) model_name: &'static str,
    name: String,
    condition: TriggerCondition,
    message: String,
    insert: bool,
    update: Vec<ColumnRef>,
}

impl<M: Model> SqliteGuard<M> {
    pub fn reject_when(
        name: impl Into<String>,
        condition: TriggerCondition,
        message: impl Into<String>,
    ) -> Self {
        Self {
            definition: GuardDefinition {
                table: M::table_name(),
                model_name: M::model_name(),
                name: name.into(),
                condition,
                message: message.into(),
                insert: false,
                update: Vec::new(),
            },
            model: PhantomData,
        }
    }

    pub fn before_insert(mut self) -> Self {
        self.definition.insert = true;
        self
    }

    /// Adds a model field to the UPDATE OF event.
    pub fn before_update_of<T>(mut self, field: &ModelField<M, T>) -> Self {
        let column = field.column();
        if !self.definition.update.contains(&column) {
            self.definition.update.push(column);
        }
        self
    }

    pub(crate) fn into_definition(mut self) -> GuardDefinition {
        self.definition.update.sort_by_key(|column| column.name);
        self.definition
    }
}

impl GuardDefinition {
    pub(crate) fn compile(
        &self,
        app: &str,
        state: &ProjectState,
    ) -> Result<Vec<SchemaObjectState>, MigrationError> {
        let error = |message: &str| {
            MigrationError::InvalidState(format!("guard {app}.{}: {message}", self.name))
        };
        crate::schema::validate_identifier(&self.name).map_err(|_| error("invalid guard name"))?;
        if self.message.contains('\0') {
            return Err(error("message contains a NUL byte"));
        }
        if !self.insert && self.update.is_empty() {
            return Err(error("select INSERT or at least one UPDATE field"));
        }
        let mut events = Vec::new();
        if self.insert {
            events.push(("insert", "INSERT".to_owned()));
        }
        if !self.update.is_empty() {
            let columns = self
                .update
                .iter()
                .map(|field| {
                    if field.table != self.table {
                        return Err(error("UPDATE OF field belongs to another model"));
                    }
                    registered_column(state, *field)?;
                    Ok(quote(field.name))
                })
                .collect::<Result<Vec<_>, _>>()?;
            events.push(("update", format!("UPDATE OF {}", columns.join(", "))));
        }
        events.into_iter().map(|(suffix, event)| {
            let name = format!("{}_{}", self.name, suffix);
            let condition = self.render(state, &self.condition.0, suffix == "update", None)?;
            let table = quote(self.table);
            let message = self.message.replace('\'', "''");
            let sql = format!("CREATE TRIGGER {} BEFORE {event} ON {table} WHEN {condition} BEGIN SELECT RAISE(ABORT, '{message}'); END", quote(&name));
            Ok(SchemaObjectState { app: app.into(), kind: SchemaObjectKind::Trigger, name, sql })
        }).collect()
    }

    fn render(
        &self,
        state: &ProjectState,
        predicate: &Predicate,
        update: bool,
        source: Option<&str>,
    ) -> Result<String, MigrationError> {
        let field = |field: TriggerField| -> Result<_, MigrationError> {
            let prefix = match field.row {
                Row::New if field.column.table == self.table => "NEW",
                Row::Old if update && field.column.table == self.table => "OLD",
                Row::Source if source == Some(field.column.table) => "guard_source",
                _ => {
                    return Err(MigrationError::InvalidState(format!(
                        "guard {} uses {}.{} outside its row scope",
                        self.name, field.column.table, field.column.name
                    )));
                }
            };
            registered_column(state, field.column)?;
            Ok(format!("{prefix}.{}", quote(field.column.name)))
        };
        let render = |predicate| self.render(state, predicate, update, source);
        Ok(match predicate {
            Predicate::Compare(left, op, right) => {
                if registered_column(state, left.column)?.column_type
                    != registered_column(state, right.column)?.column_type
                {
                    return Err(MigrationError::InvalidState(format!(
                        "guard {} compares fields of different types",
                        self.name
                    )));
                }
                format!("({} {op} {})", field(*left)?, field(*right)?)
            }
            Predicate::NotNull(value) => format!("({} IS NOT NULL)", field(*value)?),
            Predicate::And(left, right) => format!("({} AND {})", render(left)?, render(right)?),
            Predicate::Or(left, right) => format!("({} OR {})", render(left)?, render(right)?),
            Predicate::NotExists(table, condition) => {
                if !state
                    .tables()
                    .iter()
                    .any(|candidate| candidate.name == *table)
                {
                    return Err(MigrationError::InvalidState(format!(
                        "guard {} references unknown table {table}",
                        self.name
                    )));
                }
                let condition = self.render(state, condition, update, Some(table))?;
                format!(
                    "NOT EXISTS (SELECT 1 FROM {} AS guard_source WHERE {condition})",
                    quote(table)
                )
            }
        })
    }
}

pub(crate) fn registered_column(
    state: &ProjectState,
    field: ColumnRef,
) -> Result<&crate::ColumnState, MigrationError> {
    state
        .tables()
        .iter()
        .find(|table| table.name == field.table)
        .and_then(|table| {
            table
                .columns
                .iter()
                .find(|column| column.name == field.name)
        })
        .ok_or_else(|| {
            MigrationError::InvalidState(format!(
                "SQLite trigger references unknown field {}.{}",
                field.table, field.name
            ))
        })
}
