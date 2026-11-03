//! Wire types shared by the server, desktop app and (via generated TS) the web client.

use serde::{Deserialize, Serialize};

/// Query for the dev-only token endpoint (`GET /api/dev/token`).
/// Replaced by authenticated call tokens in Stage 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevTokenRequest {
    pub room: String,
    pub identity: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Everything a client needs to join a LiveKit room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinInfo {
    /// LiveKit WebSocket URL as reachable from the client.
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
