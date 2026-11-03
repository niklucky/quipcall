//! Wire types shared by the server, desktop app and (via generated TS) the web client.

use serde::{Deserialize, Serialize};

/// Query for `GET /api/join`. Rooms are open: anyone with a room's link can
/// join it. Authenticated call tokens replace this in Stage 3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
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

/// Room IDs are 1–64 ASCII letters, digits, `-`, `_` or `.`: safe in URLs,
/// file names and logs.
pub fn is_valid_room_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}
