mod auth;
mod config;
mod rooms;
#[cfg(test)]
mod tests;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use config::Config;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{sync::Arc, time::Duration};
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    db: SqlitePool,
    http: reqwest::Client,
    mail_ready: Arc<tokio::sync::Notify>,
}
impl AppState {
    async fn new(config: Config) -> anyhow::Result<Self> {
        let opts: SqliteConnectOptions = config.database_url.parse()?;
        let path = opts.get_filename();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        // Keep the alpha pool small; claims use explicit write transactions and
        // checked conditional updates independently of the pool size.
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                opts.create_if_missing(true)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(5)),
            )
            .await?;
        sqlx::migrate!().run(&db).await?;
        Ok(Self {
            config: Arc::new(config),
            mail_ready: Arc::new(tokio::sync::Notify::new()),
            db,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?,
        })
    }
}
fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/auth/me", get(auth::me).patch(auth::profile))
        .route("/api/auth/guest", post(auth::guest))
        .route("/api/auth/request", post(auth::request_link))
        .route("/api/auth/verify", post(auth::verify_link))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/invites", post(auth::invite_account))
        .route("/api/auth/desktop/start", post(auth::desktop_start))
        .route("/api/auth/desktop/approve", post(auth::desktop_approve))
        .route("/api/auth/desktop/poll", post(auth::desktop_poll))
        .route("/api/rooms", get(rooms::list).post(rooms::create))
        .route(
            "/api/rooms/{id}",
            get(rooms::detail).patch(rooms::rename).delete(rooms::close),
        )
        .route("/api/rooms/{id}/invites", post(rooms::invite))
        .route("/api/rooms/{id}/keep", post(rooms::keep))
        .route(
            "/api/rooms/{id}/invites/{hash}",
            delete(rooms::revoke_invite),
        )
        .route("/api/rooms/{id}/members/{principal}", delete(rooms::remove))
        .route("/api/invites/redeem", post(rooms::redeem))
        .route("/api/entry", post(rooms::entry))
        .route("/api/join", post(rooms::join))
        .route("/api/media/check", get(rooms::media_check))
        .layer(DefaultBodyLimit::max(8192))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}
async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) {
        let h = req.headers();
        if h.get("x-quipcall").and_then(|v| v.to_str().ok()) != Some("1")
            || h.get(header::ORIGIN)
                .is_some_and(|v| v.to_str().ok() != Some(&s.config.public_url))
        {
            return AppError::forbidden("Untrusted request origin").into_response();
        }
    }
    if req.uri().path().starts_with("/api/auth/") && req.method() != axum::http::Method::GET {
        // nginx replaces X-Real-IP; the API listener is loopback-only in deploy.
        let ip = req
            .headers()
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("local");
        let (bucket, limit) = if req.uri().path() == "/api/auth/desktop/poll" {
            ("desktop-poll-ip", 1800)
        } else {
            ("auth-ip", 120)
        };
        if let Err(e) = auth::rate(&s, &format!("{bucket}:{}", auth::hash(ip)), limit, 60).await {
            return e.into_response();
        }
    }
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    res.headers_mut()
        .insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    res
}
/// Independent loops prevent slow LiveKit timeouts from delaying auth purges.
fn start_cleanup(state: AppState) -> [tokio::task::JoinHandle<()>; 2] {
    let auth_state = state.clone();
    let auth = tokio::spawn(async move {
        loop {
            if let Err(e) = auth::cleanup(&auth_state).await {
                tracing::warn!("auth cleanup: {}", e.message);
            }
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
    });
    let rooms = tokio::spawn(async move {
        loop {
            if let Err(e) = rooms::cleanup_quick_calls(&state).await {
                tracing::warn!("quick call cleanup: {}", e.message);
            }
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
    });
    [auth, rooms]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "quipcall_server=info".into()),
        )
        .init();
    let state = AppState::new(Config::from_env()?).await?;
    tokio::spawn(auth::mail_worker(state.clone()));
    let _cleanup = start_cleanup(state.clone());
    let listener = tokio::net::TcpListener::bind(&state.config.bind).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, router(state)).await?;
    Ok(())
}
#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}
type Result<T> = std::result::Result<T, AppError>;
impl AppError {
    fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            message: msg.into(),
        }
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, msg)
    }
    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Sign in to continue")
    }
    fn missing() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Not found or no access")
    }
}
impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        tracing::error!(error = %e.into(), "request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong; please retry",
        )
    }
}
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(quipcall_protocol::ApiError {
                error: self.message,
            }),
        )
            .into_response()
    }
}
