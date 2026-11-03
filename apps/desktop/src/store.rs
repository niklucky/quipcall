//! Rooms and call history, kept on this Mac until the backend exists.
//!
//! `~/Library/Application Support/Quipcall/local.json`, or `$QUIPCALL_DATA_DIR/local.json`
//! (tests and parallel dev copies).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::call::Ended;

const HISTORY_LIMIT: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedRoom {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryItem {
    pub room_id: String,
    pub room_name: String,
    pub one_time: bool,
    /// Unix seconds; 0 if the call never connected.
    pub started: u64,
    pub duration_secs: u64,
    pub people: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    pub rooms: Vec<SavedRoom>,
    /// Newest first.
    pub history: Vec<HistoryItem>,
}

impl Store {
    fn path() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("QUIPCALL_DATA_DIR") {
            return Some(PathBuf::from(dir).join("local.json"));
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/Quipcall/local.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let result = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
            std::fs::rename(tmp, &path)?;
            anyhow::Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("saving {}: {e:#}", path.display());
        }
    }

    pub fn add_room(&mut self, name: &str) -> SavedRoom {
        let room = SavedRoom {
            id: format!("{}-{}", slug(name), random_suffix()),
            name: name.to_owned(),
        };
        self.rooms.push(room.clone());
        room
    }

    pub fn remove_room(&mut self, id: &str) {
        self.rooms.retain(|r| r.id != id);
    }

    /// A name for a new room that isn't taken yet: "Room 1", "Room 2", …
    pub fn next_room_name(&self) -> String {
        (1..)
            .map(|n| format!("Room {n}"))
            .find(|name| !self.rooms.iter().any(|r| &r.name == name))
            .expect("unbounded range")
    }

    pub fn record(&mut self, ended: &Ended) {
        let started = ended
            .started
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        self.history.insert(
            0,
            HistoryItem {
                room_id: ended.room.id.clone(),
                room_name: ended.room.name.clone(),
                one_time: ended.room.one_time,
                started,
                duration_secs: ended.duration.as_secs(),
                people: ended.people.clone(),
            },
        );
        self.history.truncate(HISTORY_LIMIT);
    }
}

pub fn quick_room_id() -> String {
    format!("quick-{}{}", random_suffix(), random_suffix())
}

fn slug(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "room".into()
    } else {
        slug.split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }
}

fn random_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mixed = (nanos as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ std::process::id() as u64;
    format!("{:04x}", (mixed >> 32) & 0xffff)
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(unix_secs: u64) -> String {
    if unix_secs == 0 {
        return "didn't connect".into();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(unix_secs);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..172800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86400),
    }
}

pub fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
    }
}
