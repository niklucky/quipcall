mod config;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use livekit_api::access_token::{AccessToken, VideoGrants};
use quipcall_protocol::{ApiError, DevTokenRequest, JoinInfo};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

use crate::config::Config;

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "quipcall_server=debug,tower_http=info".into()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let state = AppState {
        config: config.clone(),
    };

    let mut api = Router::new().route("/health", get(|| async { "ok" }));
    if config.dev_mode {
        tracing::warn!("dev mode: /api/dev/token is enabled — never run this in production");
        api = api.route("/dev/token", get(dev_token));
    }

    let app = Router::new()
        .nest("/api", api)
        .layer(TraceLayer::new_for_http())
        // Dev: the Vite server runs on a different origin. Tightened in Stage 3.
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Dev-only: mint a LiveKit token for any room/identity. No auth.
async fn dev_token(
    State(state): State<AppState>,
    Query(req): Query<DevTokenRequest>,
) -> Result<Json<JoinInfo>, AppError> {
    if req.room.is_empty() || req.identity.is_empty() {
        return Err(AppError::bad_request("room and identity are required"));
    }
    let cfg = &state.config;
    let token = AccessToken::with_api_key(&cfg.livekit_api_key, &cfg.livekit_api_secret)
        .with_identity(&req.identity)
        .with_name(req.name.as_deref().unwrap_or(&req.identity))
        .with_grants(VideoGrants {
            room_join: true,
            room: req.room.clone(),
            ..Default::default()
        })
        .to_jwt()
        .map_err(|e| AppError::internal(format!("token: {e}")))?;

    Ok(Json(JoinInfo {
        url: cfg.livekit_public_url.clone(),
        token,
    }))
}

struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!("{}", self.message);
        }
        (
            self.status,
            Json(ApiError {
                error: self.message,
            }),
        )
            .into_response()
    }
}
