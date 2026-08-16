use std::{collections::HashMap, marker::PhantomData};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use che_orm2::{DatabaseQuery, Model, ModelSerializer, ModelWriteSerializer};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use crate::{
    RestError, RestResult, RestState,
    filters::{FilterError, FilterSet, FilterSetSpec},
    openapi::{OpenApiOptions, openapi_json_for},
    permissions::{AuthenticatedUser, Permission, ViewAction},
};

pub trait ViewSet: Clone + Send + Sync + 'static {
    type Model: Model + Send + 'static;
    type Serializer: ModelSerializer<Model = Self::Model, Input = Self::Model>
        + ModelWriteSerializer<Model = Self::Model>
        + Send
        + 'static;
    type FilterSet: FilterSetSpec<Model = Self::Model> + Default;
    type Permission: Permission<Self::Model> + Default;

    fn path(&self) -> &'static str;

    fn filterset(&self) -> Self::FilterSet {
        Default::default()
    }

    fn list_query<'db>(
        &self,
        query: DatabaseQuery<'db, Self::Model>,
    ) -> DatabaseQuery<'db, Self::Model> {
        query
    }

    fn retrieve_query<'db>(
        &self,
        query: DatabaseQuery<'db, Self::Model>,
    ) -> DatabaseQuery<'db, Self::Model> {
        query
    }

    fn create_input(
        &self,
        input: <Self::Serializer as ModelWriteSerializer>::CreateInput,
    ) -> <Self::Serializer as ModelWriteSerializer>::CreateInput {
        input
    }

    fn patch_input(
        &self,
        input: <Self::Serializer as ModelWriteSerializer>::PatchInput,
    ) -> <Self::Serializer as ModelWriteSerializer>::PatchInput {
        input
    }
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
    type FilterSet = FilterSet<M>;
    type Permission = crate::permissions::AllowAny;

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
    Json(openapi_json_for::<V::Model, V::Serializer>(
        viewset.path(),
        OpenApiOptions::default(),
    ))
}

async fn list<V>(
    State((state, viewset)): State<(RestState, V)>,
    user: Option<Extension<AuthenticatedUser>>,
    Query(params): Query<HashMap<String, String>>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let filterset = viewset.filterset();
    let permission = V::Permission::default();
    let user = user.as_ref().map(|extension| &extension.0);
    permission.check(&state, user, ViewAction::List)?;
    let base_query = viewset.list_query(state.database().query::<V::Model>());
    let count_query = filterset.apply(base_query, &params)?;
    let count = state
        .database()
        .count_query(count_query.into_select_query())
        .await?;
    let offset = parse_page_param(&params, "offset")?.unwrap_or(0);
    let limit = parse_page_param(&params, "limit")?.unwrap_or(20);
    let query = filterset.apply(
        viewset.list_query(state.database().query::<V::Model>()),
        &params,
    )?;
    let rows = query
        .limit(limit)
        .offset(offset)
        .all()
        .await?
        .into_iter()
        .map(|model| {
            serde_json::to_value(V::Serializer::from_input(model)).map_err(RestError::from)
        })
        .collect::<RestResult<Vec<_>>>()?;
    Ok(Json(json!({"count": count, "results": rows})))
}

fn parse_page_param(
    params: &HashMap<String, String>,
    name: &str,
) -> Result<Option<u64>, FilterError> {
    params
        .get(name)
        .map(|value| {
            value.parse().map_err(|_| FilterError::InvalidValue {
                field: name.to_owned(),
                expected: "integer",
            })
        })
        .transpose()
}

async fn retrieve<V>(
    State((state, viewset)): State<(RestState, V)>,
    user: Option<Extension<AuthenticatedUser>>,
    Path(id): Path<i64>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let permission = V::Permission::default();
    let user = user.as_ref().map(|extension| &extension.0);
    permission.check(&state, user, ViewAction::Retrieve)?;
    let model = viewset
        .retrieve_query(state.database().query::<V::Model>())
        .filter(V::Model::primary_key().eq(id))
        .first()
        .await?
        .ok_or(RestError::NotFound)?;
    permission.check_object(&state, user, ViewAction::Retrieve, &model)?;
    Ok(Json(serde_json::to_value(V::Serializer::from_input(
        model,
    ))?))
}

async fn destroy<V>(
    State((state, viewset)): State<(RestState, V)>,
    user: Option<Extension<AuthenticatedUser>>,
    Path(id): Path<i64>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
{
    let permission = V::Permission::default();
    let user = user.as_ref().map(|extension| &extension.0);
    permission.check(&state, user, ViewAction::Delete)?;
    let model = viewset
        .retrieve_query(state.database().query::<V::Model>())
        .filter(V::Model::primary_key().eq(id))
        .first()
        .await?
        .ok_or(RestError::NotFound)?;
    permission.check_object(&state, user, ViewAction::Delete, &model)?;
    state.database().delete::<V::Model>(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create<V>(
    State((state, viewset)): State<(RestState, V)>,
    user: Option<Extension<AuthenticatedUser>>,
    Json(input): Json<<V::Serializer as ModelWriteSerializer>::CreateInput>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let permission = V::Permission::default();
    let user = user.as_ref().map(|extension| &extension.0);
    permission.check(&state, user, ViewAction::Create)?;
    let model = V::Serializer::create(state.database(), viewset.create_input(input)).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(V::Serializer::from_input(model))?),
    ))
}

async fn patch<V>(
    State((state, viewset)): State<(RestState, V)>,
    user: Option<Extension<AuthenticatedUser>>,
    Path(id): Path<i64>,
    Json(input): Json<<V::Serializer as ModelWriteSerializer>::PatchInput>,
) -> RestResult<impl IntoResponse>
where
    V: ViewSet,
    V::Serializer: Serialize,
{
    let permission = V::Permission::default();
    let user = user.as_ref().map(|extension| &extension.0);
    permission.check(&state, user, ViewAction::Patch)?;
    let current = viewset
        .retrieve_query(state.database().query::<V::Model>())
        .filter(V::Model::primary_key().eq(id))
        .first()
        .await?
        .ok_or(RestError::NotFound)?;
    permission.check_object(&state, user, ViewAction::Patch, &current)?;
    let model = V::Serializer::patch(state.database(), id, viewset.patch_input(input))
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
