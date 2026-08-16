use che_orm2::Model;

use crate::{RestError, RestState};

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthenticatedUser {
    pub id: i64,
    pub username: String,
    pub is_staff: bool,
    pub is_admin: bool,
    pub is_superuser: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewAction {
    List,
    Retrieve,
    Create,
    Patch,
    Delete,
}

pub trait Permission<M: Model>: Clone + Send + Sync + 'static {
    fn check(
        &self,
        _state: &RestState,
        _user: Option<&AuthenticatedUser>,
        _action: ViewAction,
    ) -> Result<(), RestError> {
        Ok(())
    }

    fn check_object(
        &self,
        _state: &RestState,
        _user: Option<&AuthenticatedUser>,
        _action: ViewAction,
        _model: &M,
    ) -> Result<(), RestError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAny;

impl<M: Model> Permission<M> for AllowAny {}
