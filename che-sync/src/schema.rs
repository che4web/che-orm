use crate::Registry;
use che_orm::{MigrationError, MigrationOperation, ProjectState, SyncRegistration};

/// Compatibility name for the ORM database model role.
pub use che_orm::ModelRole as SchemaRole;

impl Registry {
    /// Freeze these operations in application-owned compiled migrations.
    /// ORM role registrations include them automatically in ProjectState.
    pub fn migration_operations(&self, role: SchemaRole) -> Vec<MigrationOperation> {
        self.require_role(role)
            .expect("sync registry role mismatch");
        let registrations = self
            .models
            .iter()
            .map(|d| SyncRegistration {
                role,
                name: d.name,
                version: d.version,
                schema: d.schema.clone(),
                fields: d.fields.clone(),
                protected_columns: vec![],
                revision: d.revision,
                updated_at: d.updated_at,
                deleted_at: d.deleted_at,
                scope: d.scope,
            })
            .collect::<Vec<_>>();
        che_orm::sync_migration_operations_for_role(role, &registrations)
            .expect("validated registry")
    }

    pub fn extend_migration_state(
        &self,
        state: &mut ProjectState,
        role: SchemaRole,
    ) -> Result<(), MigrationError> {
        self.require_role(role)
            .map_err(|error| MigrationError::InvalidState(error.to_string()))?;
        for operation in self.migration_operations(role) {
            if let MigrationOperation::CreateModel { model } = &operation {
                if let Some(existing) = state
                    .models()
                    .iter()
                    .find(|existing| existing.key == model.key)
                {
                    if existing != model {
                        return Err(MigrationError::InvalidState(format!(
                            "sync schema mismatch for {:?}",
                            model.key
                        )));
                    }
                    continue;
                }
            }
            operation.state_forwards(state)?;
        }
        Ok(())
    }
}
