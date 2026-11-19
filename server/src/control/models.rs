//! Implements the read-only model catalog endpoint.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::{model::ModelConfig, Result};

use super::{ControlService, ModelConnectivityResult};

pub async fn list(State(service): State<ControlService>) -> Json<Vec<ModelConfig>> {
    Json(service.models().to_vec())
}

pub async fn test(
    State(service): State<ControlService>,
    Path((model_hash, test_id)): Path<(String, String)>,
) -> Result<Json<ModelConnectivityResult>> {
    Ok(Json(service.test_model(&model_hash, &test_id).await?))
}

pub async fn cancel(
    State(service): State<ControlService>,
    Path((_model_hash, test_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    service.cancel_model_test(&test_id);
    Ok(StatusCode::NO_CONTENT)
}
