use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum RestError {
    #[error(transparent)]
    Orm(#[from] che_orm2::OrmError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("resource not found")]
    NotFound,
}

pub type RestResult<T> = Result<T, RestError>;

impl IntoResponse for RestError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Json(_) | Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Orm(che_orm2::OrmError::QueryBuild(che_orm2::QueryBuildError::EmptyUpdate)) => {
                StatusCode::BAD_REQUEST
            }
            Self::Orm(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({"detail": self.to_string()}))).into_response()
    }
}
