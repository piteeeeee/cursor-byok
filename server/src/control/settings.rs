//! Implements settings management endpoints.
use crate::Result;
use axum::{extract::State, Json};

use crate::store::{
    DesktopSettings, PortSettings, ProxySettings, ProxySettingsInput, StatisticsStorage,
    TokenPricingSettings,
};

use super::ControlService;

pub async fn get_ports(State(service): State<ControlService>) -> Result<Json<PortSettings>> {
    Ok(Json(service.ports().await?))
}

pub async fn update_ports(
    State(service): State<ControlService>,
    Json(settings): Json<PortSettings>,
) -> Result<Json<PortSettings>> {
    Ok(Json(service.set_ports(settings).await?))
}

pub async fn get_storage(State(service): State<ControlService>) -> Result<Json<StatisticsStorage>> {
    Ok(Json(service.statistics_storage().await?))
}

pub async fn clear_storage(
    State(service): State<ControlService>,
) -> Result<Json<StatisticsStorage>> {
    Ok(Json(service.clear_statistics_storage().await?))
}

pub async fn get_proxy(State(service): State<ControlService>) -> Result<Json<ProxySettings>> {
    Ok(Json(service.proxy_settings().await?))
}

pub async fn update_proxy(
    State(service): State<ControlService>,
    Json(settings): Json<ProxySettingsInput>,
) -> Result<Json<ProxySettings>> {
    Ok(Json(service.set_proxy_settings(settings).await?))
}

pub async fn get_desktop(State(service): State<ControlService>) -> Result<Json<DesktopSettings>> {
    Ok(Json(service.desktop_settings().await?))
}

pub async fn update_desktop(
    State(service): State<ControlService>,
    Json(settings): Json<DesktopSettings>,
) -> Result<Json<DesktopSettings>> {
    service.set_desktop_settings(settings).await?;
    get_desktop(State(service)).await
}

pub async fn get_pricing_settings(
    State(service): State<ControlService>,
) -> Result<Json<TokenPricingSettings>> {
    Ok(Json(service.pricing_settings().await?))
}

pub async fn update_pricing_settings(
    State(service): State<ControlService>,
    Json(settings): Json<TokenPricingSettings>,
) -> Result<Json<TokenPricingSettings>> {
    Ok(Json(service.set_pricing_settings(settings).await?))
}
