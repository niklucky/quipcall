//! Cached server rooms and local call history.
//!
//! `~/Library/Application Support/Quipcall/local.json`, or `$QUIPCALL_DATA_DIR/local.json`
//! (tests and parallel dev copies).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::call::Ended;

const HISTORY_LIMIT: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    Direct,
    Public,
    #[default]
    Private,
}
impl RoomKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => "1-to-1 call",
            Self::Public => "Public room",
            Self::Private => "Private room",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Direct => "You and the first person to join. The same guest can return.",
            Self::Public => "Anyone with the room link can join.",
            Self::Private => "Only invited members and guests can join.",
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SavedRoom {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: RoomKind,
    #[serde(default)]
    pub owner: String,
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
    /// Who joined, left, shared and recorded, in order. Empty for calls
    /// recorded before this existed.
    #[serde(default)]
    pub events: Vec<CallEvent>,
    /// What you recorded in the call, on this Mac.
    #[serde(default)]
    pub recordings: Vec<RecordingFile>,
    /// What was said, if the call was transcribed.
    #[serde(default)]
    pub transcript: Vec<SaidLine>,
}

/// A line of a call's transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaidLine {
    /// When they began, in milliseconds since the call connected (before
    /// it, if you joined while it was being transcribed).
    pub at_ms: i64,
    pub who: String,
    /// You said it.
    #[serde(default)]
    pub mine: bool,
    pub text: String,
}

/// Recording, as set in Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSettings {
    /// Record offers "Audio and screen" first.
    #[serde(default)]
    pub screen_first: bool,
    /// Each voice in a file of its own too, next to the mix.
    #[serde(default)]
    pub each_voice: bool,
    /// Where recordings go; `None` for Movies › Quipcall.
    #[serde(default)]
    pub folder: Option<PathBuf>,
}

/// Transcription, as set in Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionSettings {
    /// ISO 639-1 of the language spoken; `None` to let the model tell.
    #[serde(default)]
    pub language: Option<String>,
    /// A catalog id; `None` for the one that fits the language.
    #[serde(default)]
    pub model: Option<String>,
    /// Don't open the transcript window when a transcript starts.
    #[serde(default)]
    pub keep_window_closed: bool,
    /// Start transcribing when someone in the call asks, without a prompt.
    #[serde(default)]
    pub accept_asks: bool,
}

impl TranscriptionSettings {
    /// The speech model to use.
    pub fn model(&self) -> &'static quipcall_core::ModelInfo {
        self.model
            .as_deref()
            .and_then(quipcall_core::catalog::model)
            .unwrap_or_else(|| quipcall_core::catalog::for_language(self.language.as_deref()))
    }
}

/// A recording on this Mac.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingFile {
    pub path: PathBuf,
    /// When it started, in seconds since the call connected.
    pub at: u64,
    pub duration_secs: u64,
    /// Your screen is in it too (an .mp4), not only the sound.
    #[serde(default)]
    pub screen: bool,
    /// When it started, in milliseconds since the call connected: what
    /// lines its player plays from. Recordings from before this have `at`.
    #[serde(default)]
    pub at_ms: Option<i64>,
    /// Each voice on its own, in this folder.
    #[serde(default)]
    pub voices: Option<PathBuf>,
}

impl RecordingFile {
    pub fn start_ms(&self) -> i64 {
        self.at_ms.unwrap_or(self.at as i64 * 1000)
    }

    /// It holds the moment `at_ms` of its call.
    pub fn covers(&self, at_ms: i64) -> bool {
        let start = self.start_ms();
        (start..start + self.duration_secs as i64 * 1000 + 1000).contains(&at_ms)
    }
}

/// Something that happened in a call, `at` seconds after it connected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEvent {
    pub at: u64,
    pub who: String,
    pub what: Happened,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Happened {
    Joined,
    Left,
    StartedSharing,
    StoppedSharing,
    StartedRecording,
    StoppedRecording,
    StartedTranscribing,
    StoppedTranscribing,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    /// The name others see. Unset until the first launch asks for it.
    #[serde(default)]
    pub name: Option<String>,
    pub rooms: Vec<SavedRoom>,
    /// Identity whose server room list is cached here; local history is separate.
    #[serde(default)]
    pub rooms_principal: Option<String>,
    /// Local saved links and aliases, partitioned by account independently of
    /// the server cache. Keeping a link never grants access or renames it remotely.
    #[serde(default)]
    pub bookmarks: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// Newest first.
    pub history: Vec<HistoryItem>,
    #[serde(default)]
    pub transcription: TranscriptionSettings,
    #[serde(default)]
    pub recording: RecordingSettings,
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

    /// Drop the old identity's room metadata before fetching the new account.
    /// An offline/failed refresh must never leave the previous account's cache.
    pub fn bind_rooms(&mut self, principal: Option<&str>) {
        if self.rooms_principal.as_deref() != principal {
            self.rooms.clear();
            self.rooms_principal = principal.map(str::to_owned);
        }
    }

    pub fn is_bookmarked(&self, id: &str) -> bool {
        self.rooms_principal
            .as_ref()
            .and_then(|p| self.bookmarks.get(p))
            .is_some_and(|b| b.contains_key(id))
    }
    fn alias(&self, id: &str) -> Option<&String> {
        self.rooms_principal
            .as_ref()
            .and_then(|p| self.bookmarks.get(p))
            .and_then(|b| b.get(id))
    }
    pub fn refresh_rooms(&mut self, rooms: Vec<SavedRoom>) {
        self.rooms.clear();
        for room in rooms.into_iter().rev() {
            self.cache_room(room);
        }
        // An unavailable saved link can still be kept locally; admission is
        // checked by the server when it is opened, never inferred from this list.
        let bookmarks = self
            .rooms_principal
            .as_ref()
            .and_then(|p| self.bookmarks.get(p))
            .cloned()
            .unwrap_or_default();
        for (id, name) in bookmarks {
            if !self.rooms.iter().any(|r| r.id == id) {
                self.rooms.push(SavedRoom {
                    id,
                    name,
                    ..Default::default()
                });
            }
        }
    }
    pub fn cache_room(&mut self, mut room: SavedRoom) {
        if room.kind == RoomKind::Direct && !self.is_bookmarked(&room.id) {
            return;
        }
        if let Some(name) = self.alias(&room.id) {
            room.name.clone_from(name);
        }
        if let Some(existing) = self.rooms.iter_mut().find(|r| r.id == room.id) {
            *existing = room;
        } else {
            self.rooms.insert(0, room);
        }
    }
    pub fn rename_bookmark(&mut self, id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(alias) = self
            .rooms_principal
            .as_ref()
            .and_then(|p| self.bookmarks.get_mut(p))
            .and_then(|b| b.get_mut(id))
        {
            *alias = name.to_owned();
            self.rename_room(id, name);
            for item in self.history.iter_mut().filter(|h| h.room_id == id) {
                item.room_name = name.to_owned();
            }
        }
    }

    pub fn remove_room(&mut self, id: &str) {
        self.rooms.retain(|r| r.id != id);
        if let Some(bookmarks) = self
            .rooms_principal
            .as_ref()
            .and_then(|p| self.bookmarks.get_mut(p))
        {
            bookmarks.remove(id);
        }
    }

    pub fn rename_room(&mut self, id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(room) = self.rooms.iter_mut().find(|r| r.id == id) {
            room.name = name.to_owned();
        }
    }

    /// Keep a one-time room: it joins the rooms list under `name`.
    pub fn keep_room(&mut self, id: &str, name: &str) -> bool {
        let Some(principal) = &self.rooms_principal else {
            return false;
        };
        self.bookmarks
            .entry(principal.clone())
            .or_default()
            .insert(id.to_owned(), name.to_owned());
        if !self.rooms.iter().any(|r| r.id == id) {
            self.rooms.push(SavedRoom {
                id: id.to_owned(),
                name: name.to_owned(),
                ..Default::default()
            });
        }
        for item in self.history.iter_mut().filter(|h| h.room_id == id) {
            item.one_time = false;
            item.room_name = name.to_owned();
        }
        true
    }

    /// The calls in a room, newest first.
    pub fn calls_in<'a>(&'a self, room_id: &'a str) -> impl Iterator<Item = &'a HistoryItem> {
        self.history.iter().filter(move |h| h.room_id == room_id)
    }

    /// Everyone you've talked to in a room, most frequent first.
    pub fn people_in(&self, room_id: &str) -> Vec<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for item in self.calls_in(room_id) {
            for name in &item.people {
                match counts.iter_mut().find(|(n, _)| n == name) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((name.clone(), 1)),
                }
            }
        }
        // Stable: ties keep the order of the most recent call.
        counts.sort_by_key(|c| std::cmp::Reverse(c.1));
        counts.into_iter().map(|(name, _)| name).collect()
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
                events: ended.events.clone(),
                recordings: ended.recordings.clone(),
                transcript: ended.transcript.clone(),
            },
        );
        self.history.truncate(HISTORY_LIMIT);
    }
}

/// Where recordings go: the folder chosen in Settings, else Movies ›
/// Quipcall (macOS doesn't ask for access to Movies), or
/// `$QUIPCALL_DATA_DIR/Recordings` for tests.
pub fn recordings_dir(chosen: Option<&std::path::Path>) -> PathBuf {
    if let Some(dir) = std::env::var_os("QUIPCALL_DATA_DIR") {
        return PathBuf::from(dir).join("Recordings");
    }
    if let Some(dir) = chosen {
        return dir.to_owned();
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
    PathBuf::from(home).join("Movies/Quipcall")
}

/// Where speech models are downloaded to: Application Support › Quipcall ›
/// Models, `$QUIPCALL_DATA_DIR/Models`, or `$QUIPCALL_MODELS_DIR` (tests
/// that share one download).
pub fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("QUIPCALL_MODELS_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("QUIPCALL_DATA_DIR") {
        return PathBuf::from(dir).join("Models");
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/Quipcall/Models")
}

/// "2026-11-09 11.14" for unix seconds, local time: part of a file name.
pub fn file_stamp(unix_secs: u64) -> String {
    let (days, hour, minute) = local_parts(unix_secs);
    let (year, month, day) = civil_from_days(days);
    format!("{year}-{month:02}-{day:02} {hour:02}.{minute:02}")
}

/// "840 KB", "11 MB", "1.2 GB".
pub fn file_size(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes.div_ceil(1000)),
        1_000_000..1_000_000_000 => format!("{} MB", bytes / 1_000_000),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

pub fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
    }
}

/// Local wall-clock time of day, "10:02", for unix seconds.
pub fn clock(unix_secs: u64) -> String {
    let (_, hour, minute) = local_parts(unix_secs);
    format!("{hour:02}:{minute:02}")
}

/// The day heading for unix seconds: "Today", "Yesterday", "Wednesday"
/// within a week, then "Nov 2".
pub fn day(unix_secs: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days_then, ..) = local_parts(unix_secs);
    let (days_now, ..) = local_parts(now);
    match days_now - days_then {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..7 => WEEKDAYS[(days_then + 4).rem_euclid(7) as usize].into(),
        _ => {
            let (_, month, day) = civil_from_days(days_then);
            format!("{} {day}", MONTHS[month as usize - 1])
        }
    }
}

/// "Wed" for a weekday name, shorter forms of [`day`] for tight rows.
pub fn day_short(unix_secs: u64) -> String {
    let long = day(unix_secs);
    match long.as_str() {
        "Today" | "Yesterday" => long,
        _ if WEEKDAYS.contains(&long.as_str()) => long[..3].to_owned(),
        _ => long,
    }
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 in local time, hour, minute.
fn local_parts(unix_secs: u64) -> (i64, u32, u32) {
    let local = unix_secs as i64 + utc_offset_secs();
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    (days, (secs / 3600) as u32, (secs % 3600 / 60) as u32)
}

/// This Mac's current offset from UTC, in seconds.
fn utc_offset_secs() -> i64 {
    #[cfg(target_os = "macos")]
    {
        objc2_foundation::NSTimeZone::localTimeZone().secondsFromGMT() as i64
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// (year, month 1–12, day 1–31) for days since 1970-01-01 (Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::{SavedRoom, Store, civil_from_days, file_size};

    #[test]
    fn kept_link_and_local_alias_survive_refresh_and_account_switches() {
        let mut store = Store::default();
        store.bind_rooms(Some("me"));
        assert!(store.keep_room("friend-room", "Friends"));
        store.rename_bookmark("friend-room", "Friday catch-up");
        store.refresh_rooms(vec![SavedRoom {
            id: "friend-room".into(),
            name: "Their shared name".into(),
            owner: "friend".into(),
            ..Default::default()
        }]);
        assert_eq!(store.rooms[0].name, "Friday catch-up");
        assert_eq!(store.rooms[0].owner, "friend");
        let saved = serde_json::to_vec(&store).unwrap();
        let mut restored: Store = serde_json::from_slice(&saved).unwrap();
        restored.refresh_rooms(vec![]);
        assert_eq!(restored.rooms[0].name, "Friday catch-up");
        restored.bind_rooms(Some("other-account"));
        restored.refresh_rooms(vec![]);
        assert!(restored.rooms.is_empty());
        restored.bind_rooms(Some("me"));
        restored.refresh_rooms(vec![]);
        assert_eq!(restored.rooms[0].name, "Friday catch-up");
    }

    #[test]
    fn quick_calls_are_not_saved_until_kept_even_after_refresh() {
        let mut store = Store::default();
        store.bind_rooms(Some("owner"));
        let direct = SavedRoom {
            id: "quick".into(),
            name: "Quick call".into(),
            kind: super::RoomKind::Direct,
            owner: "owner".into(),
        };
        store.cache_room(direct.clone());
        assert!(store.rooms.is_empty());
        store.refresh_rooms(vec![direct.clone()]);
        assert!(store.rooms.is_empty());
        assert!(store.keep_room("quick", "Catch-up"));
        store.refresh_rooms(vec![direct]);
        assert_eq!(store.rooms.len(), 1);
        assert_eq!(store.rooms[0].name, "Catch-up");
        assert_eq!(store.rooms[0].kind, super::RoomKind::Direct);
    }

    #[test]
    fn switching_accounts_clears_and_persists_the_room_cache_before_refresh() {
        let mut store = Store::default();
        store.bind_rooms(Some("account-a"));
        store.cache_room(SavedRoom {
            id: "private-a".into(),
            name: "Private project A".into(),
            owner: "account-a".into(),
            ..Default::default()
        });
        store.bind_rooms(Some("account-a"));
        assert_eq!(store.rooms.len(), 1, "same-account refresh keeps its cache");
        store.bind_rooms(Some("account-b"));
        // No replacement list is installed, as when /rooms fails or is offline.
        assert!(store.rooms.is_empty());
        let saved = serde_json::to_vec(&store).unwrap();
        let reloaded: Store = serde_json::from_slice(&saved).unwrap();
        assert_eq!(reloaded.rooms_principal.as_deref(), Some("account-b"));
        assert!(reloaded.rooms.is_empty());
        assert!(
            !String::from_utf8(saved)
                .unwrap()
                .contains("Private project A")
        );
    }

    #[test]
    fn legacy_unscoped_room_cache_is_not_reused_for_a_new_identity() {
        let mut store = Store::default();
        store.cache_room(SavedRoom {
            id: "old-room".into(),
            name: "Old project".into(),
            ..Default::default()
        });
        store.bind_rooms(Some("account-b"));
        assert!(store.rooms.is_empty());
    }

    #[test]
    fn sizes() {
        assert_eq!(file_size(840_000), "840 KB");
        assert_eq!(file_size(11_400_000), "11 MB");
        assert_eq!(file_size(1_240_000_000), "1.2 GB");
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-11-07
        assert_eq!(civil_from_days(20_764), (2026, 11, 7));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
