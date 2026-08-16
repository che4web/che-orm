use std::marker::PhantomData;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use che_orm2::{Model, ModelSerializer, ModelWriteSerializer};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use crate::{
    RestError, RestResult, RestState,
    openapi::{OpenApiOptions, openapi_json_for},
};

pub trait ViewSet: Clone + Send + Sync + 'static {
    type Model: Model + Send + 'static;
    type Serializer: ModelSerializer<Model = Self::Model, Input = Self::Model>
        + ModelWriteSerializer<Model = Self::Model>
        + Send
        + 'static;

    fn path(&self) -> &'static str;
}

pub struct CrudViewSet<M, S> {
    path: &'static str,
    _marker: PhantomData<fn() -> (M, S)>,
}

impl<M, S> Clone for CrudViewSet<M, S> {
    fn clone(&self) -> Self {
        Self {
            path: self.path,
            _marker: PhantomData,
        }
    }
}

impl<M, S> CrudViewSet<M, S> {
    pub const fn new(path: &'static str) -> Self {
        Self {
            path,
            _marker: PhantomData,
        }
    }
}

impl<M, S> ViewSet for CrudViewSet<M, S>
where
    M: Model + Send + 'static,
    S: ModelSerializer<Model = M, Input = M>
        + ModelWriteSerializer<Model = M>
        + Send
        + Sync
        + 'static,
{
    type Model = M;
    type Serializer = S;

    fn path(&self) -> &'static str {
        self.path
    }
}

pub fn router<V>(state: RestState, viewset: V) -> Router
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let path = viewset.path().trim_end_matches('/');
    Router::new()
        .route(&format!("{path}/"), get(list::<V>).post(create::<V>))
        .route(
            &format!("{path}/{{id}}/"),
            get(retrieve::<V>).patch(patch::<V>).delete(destroy::<V>),
        )
        .with_state((state, viewset))
}

/// Adds the generated CRUD router and a JSON OpenAPI endpoint.
pub fn router_with_openapi<V>(state: RestState, viewset: V) -> Router
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let path = viewset.path().trim_end_matches('/');
    Router::new()
        .route(&format!("{path}/"), get(list::<V>).post(create::<V>))
        .route(
            &format!("{path}/{{id}}/"),
            get(retrieve::<V>).patch(patch::<V>).delete(destroy::<V>),
        )
        .route("/openapi.json", get(openapi_handler::<V>))
        .with_state((state, viewset))
}

async fn openapi_handler<V: ViewSet>(
    State((_state, viewset)): State<(RestState, V)>,
) -> impl IntoResponse {
    Json(openapi_json_for::<V::Model>(
        viewset.path(),
        OpenApiOptions::default(),
    ))
}

#[derive(Debug, serde::Deserialize)]
struct ListParams {
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list<V>(
    State((state, _viewset)): State<(RestState, V)>,
    Query(params): Query<ListParams>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let all = state.database().all::<V::Model>().await?;
    let count = all.len();
    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(20);
    let rows = all
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|model| {
            serde_json::to_value(V::Serializer::from_input(model)).map_err(RestError::from)
        })
        .collect::<RestResult<Vec<_>>>()?;
    Ok(Json(json!({"count": count, "results": rows})))
}

async fn retrieve<V>(
    State((state, _viewset)): State<(RestState, V)>,
    Path(id): Path<i64>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let model = state
        .database()
        .get::<V::Model>(id)
        .await?
        .ok_or(RestError::NotFound)?;
    Ok(Json(serde_json::to_value(V::Serializer::from_input(
        model,
    ))?))
}

async fn destroy<V>(
    State((state, _viewset)): State<(RestState, V)>,
    Path(id): Path<i64>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
{
    if !state.database().delete::<V::Model>(id).await? {
        return Err(RestError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn create<V>(
    State((state, _viewset)): State<(RestState, V)>,
    Json(input): Json<<V::Serializer as ModelWriteSerializer>::CreateInput>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let model = V::Serializer::create(state.database(), input).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(V::Serializer::from_input(model))?),
    ))
}

async fn patch<V>(
    State((state, _viewset)): State<(RestState, V)>,
    Path(id): Path<i64>,
    Json(input): Json<<V::Serializer as ModelWriteSerializer>::PatchInput>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let model = V::Serializer::patch(state.database(), id, input)
        .await?
        .ok_or(RestError::NotFound)?;
    Ok(Json(serde_json::to_value(V::Serializer::from_input(
        model,
    ))?))
}

#[allow(dead_code)]
fn decode<T: DeserializeOwned>(body: &[u8]) -> RestResult<T> {
    Ok(serde_json::from_slice(body)?)
}
