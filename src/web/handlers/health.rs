//! Health check and connection status endpoints.

use crate::immich_api::ImmichClient;
use crate::web::state::AppState;
use axum::{extract::State, response::Json};
use serde::Serialize;

/// Build version response.
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub git_commit: Option<&'static str>,
    pub git_branch: Option<&'static str>,
}

/// Health check endpoint.
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "OK",
        version: env!("CARGO_PKG_VERSION"),
        git_commit: option_env!("GIT_COMMIT"),
        git_branch: option_env!("GIT_BRANCH"),
    })
}

/// Connection status response.
#[derive(Serialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub version: Option<String>,
    pub error: Option<String>,
}

/// Check connection to Immich.
pub async fn check_connection(State(state): State<AppState>) -> Json<ConnectionStatus> {
    let config = state.config.read().await;
    let client = match ImmichClient::new(&config.api) {
        Ok(c) => c,
        Err(e) => {
            return Json(ConnectionStatus {
                connected: false,
                version: None,
                error: Some(e.to_string()),
            });
        }
    };

    match client.validate_connection().await {
        Ok(info) => Json(ConnectionStatus {
            connected: true,
            version: Some(info.version),
            error: None,
        }),
        Err(e) => Json(ConnectionStatus {
            connected: false,
            version: None,
            error: Some(e.to_string()),
        }),
    }
}
