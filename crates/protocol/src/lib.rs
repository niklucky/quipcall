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

/// The LiveKit data topic for transcripts, between apps in a call.
pub const TRANSCRIPT_TOPIC: &str = "quipcall.transcript";

/// A message on [`TRANSCRIPT_TOPIC`]. The Mac that transcribes a call sends
/// each line to everyone as it comes (drafts too), and the lines so far to
/// whoever joins late. Anyone can ask it to stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptMessage {
    Lines {
        lines: Vec<TranscriptLine>,
    },
    /// Stop transcribing this call, for everyone.
    Stop,
}

/// What someone said in a call, or a draft of it while they talk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptLine {
    /// The utterance, per transcriber. A later line with the same id
    /// replaces this one; a final one with no text takes a draft back.
    pub id: u64,
    /// The speaker's identity.
    pub speaker: String,
    /// When they began, unix milliseconds.
    pub at_ms: u64,
    pub text: String,
    /// Final; a draft until then.
    pub done: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_messages_on_the_wire() {
        let message = TranscriptMessage::Lines {
            lines: vec![TranscriptLine {
                id: 7,
                speaker: "anna-1f2e".into(),
                at_ms: 1_762_700_000_000,
                text: "Привет".into(),
                done: true,
            }],
        };
        let json = serde_json::to_string(&message).unwrap();
        assert!(
            json.starts_with(r#"{"type":"lines","lines":[{"id":7,"#),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<TranscriptMessage>(&json).unwrap(),
            message
        );
        assert_eq!(
            serde_json::from_str::<TranscriptMessage>(r#"{"type":"stop"}"#).unwrap(),
            TranscriptMessage::Stop
        );
    }
}
