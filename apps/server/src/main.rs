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
use quipcall_protocol::{ApiError, JoinInfo, JoinRequest, is_valid_room_id};
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

    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/join", get(join));

    let app = Router::new()
        .nest("/api", api)
        .layer(TraceLayer::new_for_http())
        // The Vite dev server runs on another origin. Tightened in Stage 3.
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Mint a LiveKit token for a room. No auth yet: rooms are open to anyone
/// with the link, like a meeting link (accounts arrive in Stage 3).
async fn join(
    State(state): State<AppState>,
    Query(req): Query<JoinRequest>,
) -> Result<Json<JoinInfo>, AppError> {
    if !is_valid_room_id(&req.room) {
        return Err(AppError::bad_request(
            "room must be 1–64 letters, digits, '-', '_' or '.'",
        ));
    }
    if !(1..=128).contains(&req.identity.len()) || req.identity.chars().any(char::is_control) {
        return Err(AppError::bad_request("identity must be 1–128 characters"));
    }
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&req.identity);
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();

    let cfg = &state.config;
    let token = AccessToken::with_api_key(&cfg.livekit_api_key, &cfg.livekit_api_secret)
        .with_identity(&req.identity)
        .with_name(&name)
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
