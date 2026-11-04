//! The call model, shared by every window of a call: the floating circles, the
//! glass toolbar, screen-share windows and the regular window.
//!
//! Windows are views onto this model. Which windows exist follows from the
//! mode and the participants; `sync_windows` opens and closes them.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, DisplayId, Entity, EventEmitter, Pixels,
    Task, WindowBounds, WindowHandle, point, px, size,
};
use quipcall_core::{
    Engine, Event, MediaDevice, ScreenSharePreset, SourceId, TrackKey, VideoKind, VideoQuality,
    VideoSubscription,
};

use crate::Args;
use crate::bubble::{self, Bubble};
use crate::fullscreen::{self, Fullscreen};
use crate::regular::Regular;
use crate::screen_window::{self, ScreenWindow};
use crate::share_picker::{self, SharePicker};
use crate::toolbar::{self, Toolbar};
use crate::video_mac::SurfaceSink;

/// Your own screen preview repaints at most this often: enough to see what
/// you share, without redrawing at 60 fps.
const PREVIEW_FPS: u32 = 15;

#[derive(Clone, Debug)]
pub struct RoomRef {
    pub id: String,
    pub name: String,
    /// Quick call: not kept in the rooms list, only in history.
    pub one_time: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Joining,
    Connected,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Floating circles and the glass toolbar; no call window.
    Minimal,
    /// One window with tiles.
    Regular,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Camera {
    Off,
    Starting,
    On,
}

/// A video stream shown somewhere in the UI. Each frame repaints the entity
/// that owns it, and with it only the windows that show that entity.
pub struct Video {
    pub sink: Arc<SurfaceSink>,
    key: Option<TrackKey>,
    quality: Option<VideoQuality>,
    _subscription: Option<VideoSubscription>,
    _repaint: Task<()>,
}

impl Video {
    /// `max_fps` caps repaints (your own previews); remote video repaints on
    /// every frame.
    fn new<T: 'static>(
        label: &str,
        key: Option<TrackKey>,
        max_fps: Option<u32>,
        cx: &mut Context<T>,
    ) -> (Self, Arc<SurfaceSink>) {
        let (tx, rx) = async_channel::bounded::<()>(1);
        let sink = SurfaceSink::new(label.to_owned().into(), tx);
        let sink = Arc::new(match max_fps {
            Some(fps) => sink.with_max_fps(fps),
            None => sink,
        });
        let repaint = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let video = Self {
            sink: sink.clone(),
            key,
            quality: None,
            _subscription: None,
            _repaint: repaint,
        };
        (video, sink)
    }
}

pub struct Member {
    pub identity: String,
    pub name: String,
    pub is_local: bool,
    pub video: Option<Video>,
    /// What you're sharing, while you share (your own member only; other
    /// people's shares are [`Screen`]s with windows of their own).
    pub screen: Option<Video>,
    pub speaking: bool,
    pub mic_muted: bool,
}

pub struct Screen {
    pub key: TrackKey,
    pub owner: String,
    pub video: Video,
}

#[derive(Clone, Debug)]
pub struct Ended {
    pub room: RoomRef,
    pub started: Option<SystemTime>,
    pub duration: Duration,
    pub people: Vec<String>,
}

#[derive(Default)]
struct Windows {
    bubbles: HashMap<String, WindowHandle<Bubble>>,
    screens: HashMap<TrackKey, WindowHandle<ScreenWindow>>,
    fullscreen: HashMap<TrackKey, WindowHandle<Fullscreen>>,
    toolbar: Option<WindowHandle<Toolbar>>,
    regular: Option<WindowHandle<Regular>>,
}

pub struct Call {
    pub room: RoomRef,
    pub web_url: String,
    engine: Engine,
    pub status: Status,
    pub me: Entity<Member>,
    pub others: Vec<Entity<Member>>,
    pub screens: Vec<Entity<Screen>>,
    /// Screen shares shown fullscreen, each on the display it was on.
    fullscreen: HashMap<TrackKey, Option<DisplayId>>,
    /// Where a share's floating window was before it went fullscreen.
    screen_bounds: HashMap<TrackKey, Bounds<Pixels>>,
    pub mic_on: bool,
    pub camera: Camera,
    pub sharing: Option<ScreenSharePreset>,
    /// Preset to preselect next time you share.
    share_preset: ScreenSharePreset,
    picker: Option<WindowHandle<SharePicker>>,
    /// When the call link was last copied (for the "copied" tick).
    pub copied_at: Option<Instant>,
    pub mode: Mode,
    pub waiting_for: Option<MediaDevice>,
    pub error: Option<String>,
    /// Turn the camera on once connected (`--camera`).
    camera_on_connect: bool,
    connected_at: Option<(SystemTime, Instant)>,
    people: BTreeSet<String>,
    windows: Windows,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<Ended> for Call {}

impl Call {
    /// Join `room` and show the call in minimal mode.
    pub fn start(room: RoomRef, args: &Args, cx: &mut App) -> Entity<Call> {
        let args = args.clone();
        let call = cx.new(|cx| Call::new(room, &args, cx));
        schedule_sync(&call, cx);
        call
    }

    fn new(room: RoomRef, args: &Args, cx: &mut Context<Self>) -> Self {
        let (engine, events) = Engine::new().expect("start call engine");
        let events_task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |call, cx| call.on_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let identity = unique_identity(&args.name);
        let me = cx.new(|_| Member {
            identity: identity.clone(),
            name: args.name.clone(),
            is_local: true,
            video: None,
            screen: None,
            speaking: false,
            mic_muted: true,
        });

        let join = engine.join(&args.server, &room.id, &identity, &args.name);
        let join_task = cx.spawn(async move |this, cx| {
            let result = join.await;
            let _ = this.update(cx, |call, cx| {
                match result {
                    Ok(info) => call.engine.connect(info),
                    Err(e) => call.fail(format!("{e:#}"), cx),
                }
                cx.notify();
            });
        });

        Self {
            room,
            web_url: args.web.clone(),
            engine,
            status: Status::Joining,
            me,
            others: Vec::new(),
            screens: Vec::new(),
            fullscreen: HashMap::new(),
            screen_bounds: HashMap::new(),
            mic_on: false,
            camera: Camera::Off,
            sharing: None,
            share_preset: ScreenSharePreset::PRESETS[1],
            picker: None,
            copied_at: None,
            mode: if args.regular {
                Mode::Regular
            } else {
                Mode::Minimal
            },
            waiting_for: None,
            error: None,
            camera_on_connect: args.camera,
            connected_at: None,
            people: BTreeSet::new(),
            windows: Windows::default(),
            _tasks: vec![events_task, join_task],
        }
    }

    pub fn link(&self) -> String {
        format!(
            "{}/?room={}",
            self.web_url.trim_end_matches('/'),
            self.room.id
        )
    }

    /// Text for the toolbar's notice line, if there's something to say.
    pub fn notice(&self) -> Option<(String, bool)> {
        if let Some(device) = self.waiting_for {
            return Some((
                format!("Allow {} access in the macOS prompt", device.label()),
                false,
            ));
        }
        if self.status == Status::Joining {
            return Some(("Joining…".into(), false));
        }
        self.error.clone().map(|e| (e, true))
    }

    pub fn members(&self) -> impl Iterator<Item = &Entity<Member>> {
        std::iter::once(&self.me).chain(self.others.iter())
    }

    // ---- actions (called from any window) ---------------------------------

    pub fn toggle_mic(&mut self, cx: &mut Context<Self>) {
        self.engine.set_mic_enabled(!self.mic_on);
        cx.notify();
    }

    pub fn toggle_camera(&mut self, cx: &mut Context<Self>) {
        match self.camera {
            Camera::Off => {
                let (video, sink) = self
                    .me
                    .update(cx, |_, cx| Video::new("self-view", None, None, cx));
                self.engine.start_camera(Some(sink));
                self.me.update(cx, |me, cx| {
                    me.video = Some(video);
                    cx.notify();
                });
                self.camera = Camera::Starting;
            }
            Camera::On => {
                self.engine.stop_camera();
                self.me.update(cx, |me, cx| {
                    me.video = None;
                    cx.notify();
                });
                self.camera = Camera::Off;
            }
            Camera::Starting => {}
        }
        cx.notify();
    }

    /// Stop sharing, or open the picker to start.
    pub fn toggle_share(&mut self, cx: &mut Context<Self>) {
        if self.sharing.is_some() {
            self.engine.stop_screen_share();
            return;
        }
        // Already open: bring it to the front.
        if let Some(picker) = self.picker
            && picker
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        let call = cx.entity();
        let preset = self.share_preset;
        cx.defer(move |cx| {
            let picker = share_picker::open(call.clone(), preset, cx);
            call.update(cx, |c, _| c.picker = picker);
        });
    }

    pub fn share(&mut self, source: SourceId, preset: ScreenSharePreset, cx: &mut Context<Self>) {
        // Captured frames go straight to your circle, which turns into a
        // preview of the share: the same IOSurfaces, no copy.
        self.me.update(cx, |me, cx| {
            let (video, sink) = Video::new("self-screen", None, Some(PREVIEW_FPS), cx);
            self.engine
                .start_screen_share(Some(source), preset, Some(sink));
            me.screen = Some(video);
            cx.notify();
        });
        self.share_preset = preset;
        cx.notify();
    }

    pub fn is_fullscreen(&self, key: &TrackKey) -> bool {
        self.fullscreen.contains_key(key)
    }

    /// Show a screen share fullscreen on `display`. `floating` is where its
    /// floating window is, to put it back there afterwards.
    pub fn open_fullscreen(
        &mut self,
        key: TrackKey,
        display: Option<DisplayId>,
        floating: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if self.fullscreen.contains_key(&key) {
            return;
        }
        if let Some(bounds) = floating {
            self.screen_bounds.insert(key.clone(), bounds);
        }
        self.fullscreen.insert(key, display);
        cx.notify();
        schedule_sync(&cx.entity(), cx);
    }

    pub fn close_fullscreen(&mut self, key: &TrackKey, cx: &mut Context<Self>) {
        if self.fullscreen.remove(key).is_some() {
            cx.notify();
            schedule_sync(&cx.entity(), cx);
        }
    }

    pub fn copy_link(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.link()));
        self.copied_at = Some(Instant::now());
        cx.notify();
        // Clear the tick after a moment.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1600))
                .await;
            let _ = this.update(cx, |call, cx| {
                if call
                    .copied_at
                    .is_some_and(|t| t.elapsed() >= Duration::from_millis(1500))
                {
                    call.copied_at = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
            schedule_sync(&cx.entity(), cx);
        }
    }

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    /// Resolves once the server knows you left (see [`Call::leave`]).
    pub fn closed(&self) -> impl Future<Output = ()> + use<> {
        self.engine.closed()
    }

    pub fn leave(&mut self, cx: &mut Context<Self>) {
        if self.status == Status::Ended {
            return;
        }
        self.engine.disconnect();
        self.status = Status::Ended;
        if let Some(picker) = self.picker.take() {
            close(&picker, cx);
        }
        let (started, duration) = match self.connected_at {
            Some((wall, mono)) => (Some(wall), mono.elapsed()),
            None => (None, Duration::ZERO),
        };
        cx.emit(Ended {
            room: self.room.clone(),
            started,
            duration,
            people: self.people.iter().cloned().collect(),
        });
        cx.notify();
        schedule_sync(&cx.entity(), cx);
    }

    /// Ask for the simulcast layer that fits a view `pixel_height` device
    /// pixels tall, so small circles don't decode 720p.
    pub fn fit_video(&mut self, member: &Entity<Member>, pixel_height: f32, cx: &mut App) {
        let quality = match pixel_height {
            h if h >= 540.0 => VideoQuality::High,
            h if h >= 270.0 => VideoQuality::Medium,
            _ => VideoQuality::Low,
        };
        let engine = &self.engine;
        member.update(cx, |m, _| {
            if let Some(video) = &mut m.video
                && let Some(key) = &video.key
                && video.quality != Some(quality)
            {
                engine.set_video_quality(key, quality);
                video.quality = Some(quality);
            }
        });
    }

    // ---- engine events ----------------------------------------------------

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        tracing::warn!("{message}");
        self.error = Some(message);
        cx.notify();
    }

    fn drop_preview(&mut self, cx: &mut Context<Self>) {
        self.me.update(cx, |me, cx| {
            if me.screen.take().is_some() {
                cx.notify();
            }
        });
    }

    fn member(&self, identity: &str, cx: &App) -> Option<Entity<Member>> {
        self.others
            .iter()
            .find(|m| m.read(cx).identity == identity)
            .cloned()
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        let mut windows_changed = false;
        match event {
            Event::Connected { .. } => {
                self.status = Status::Connected;
                self.waiting_for = None;
                self.connected_at = Some((SystemTime::now(), Instant::now()));
                if std::mem::take(&mut self.camera_on_connect) {
                    self.toggle_camera(cx);
                }
            }
            Event::Disconnected { reason } => {
                if self.status != Status::Ended {
                    self.fail(format!("disconnected: {reason}"), cx);
                }
            }
            Event::WaitingForPermission(device) => self.waiting_for = Some(device),
            Event::ParticipantJoined { identity, name } => {
                let name = if name.trim().is_empty() {
                    identity.clone()
                } else {
                    name
                };
                self.people.insert(name.clone());
                if self.member(&identity, cx).is_none() {
                    let member = cx.new(|_| Member {
                        identity,
                        name,
                        is_local: false,
                        video: None,
                        screen: None,
                        speaking: false,
                        mic_muted: false,
                    });
                    self.others.push(member);
                    windows_changed = true;
                }
            }
            Event::ParticipantLeft { identity } => {
                let before = self.others.len() + self.screens.len();
                self.others.retain(|m| m.read(cx).identity != identity);
                self.screens
                    .retain(|s| s.read(cx).key.participant != identity);
                windows_changed = before != self.others.len() + self.screens.len();
            }
            Event::VideoTrackAdded { key, kind, track } => match kind {
                VideoKind::Camera => {
                    if let Some(member) = self.member(&key.participant, cx) {
                        let engine = &self.engine;
                        member.update(cx, |m, cx| {
                            let (mut video, sink) =
                                Video::new(&format!("{}/camera", m.identity), Some(key), None, cx);
                            video._subscription = Some(engine.attach_video(&track, sink));
                            m.video = Some(video);
                            cx.notify();
                        });
                    }
                }
                VideoKind::Screen => {
                    let owner = self
                        .member(&key.participant, cx)
                        .map(|m| m.read(cx).name.clone())
                        .unwrap_or_else(|| key.participant.clone());
                    let engine = &self.engine;
                    let screen = cx.new(|cx| {
                        let (mut video, sink) = Video::new(
                            &format!("{}/screen", key.participant),
                            Some(key.clone()),
                            None,
                            cx,
                        );
                        video._subscription = Some(engine.attach_video(&track, sink));
                        Screen { key, owner, video }
                    });
                    self.screens.push(screen);
                    windows_changed = true;
                }
            },
            Event::VideoTrackRemoved { key } => {
                let before = self.screens.len();
                self.screens.retain(|s| s.read(cx).key != key);
                windows_changed = before != self.screens.len();
                if let Some(member) = self.member(&key.participant, cx) {
                    member.update(cx, |m, cx| {
                        if m.video.as_ref().and_then(|v| v.key.as_ref()) == Some(&key) {
                            m.video = None;
                            cx.notify();
                        }
                    });
                }
            }
            Event::ActiveSpeakers(speakers) => {
                for member in self.members().cloned().collect::<Vec<_>>() {
                    member.update(cx, |m, cx| {
                        let speaking = speakers.contains(&m.identity);
                        if m.speaking != speaking {
                            m.speaking = speaking;
                            cx.notify();
                        }
                    });
                }
            }
            Event::ParticipantMic { identity, muted } => {
                if let Some(member) = self.member(&identity, cx) {
                    member.update(cx, |m, cx| {
                        m.mic_muted = muted;
                        cx.notify();
                    });
                }
            }
            Event::MicChanged { enabled } => {
                self.mic_on = enabled;
                self.me.update(cx, |me, cx| {
                    me.mic_muted = !enabled;
                    cx.notify();
                });
            }
            Event::CameraChanged { enabled } => {
                self.waiting_for = None;
                if enabled {
                    self.camera = Camera::On;
                } else {
                    self.camera = Camera::Off;
                    self.me.update(cx, |me, cx| {
                        me.video = None;
                        cx.notify();
                    });
                }
            }
            Event::ScreenShareChanged { active, preset } => {
                self.sharing = if active { preset } else { None };
                if !active {
                    self.drop_preview(cx);
                }
            }
            Event::Error(message) => {
                self.waiting_for = None;
                // A failed start leaves nothing shared (a previous share is
                // stopped before the new one starts).
                if message.starts_with("screen share") {
                    self.sharing = None;
                    self.drop_preview(cx);
                }
                if self.camera == Camera::Starting && message.starts_with("camera") {
                    self.camera = Camera::Off;
                    self.me.update(cx, |me, cx| {
                        me.video = None;
                        cx.notify();
                    });
                }
                self.fail(message, cx);
            }
        }
        cx.notify();
        if windows_changed {
            schedule_sync(&cx.entity(), cx);
        }
    }
}

/// Open and close windows to match the call's mode and participants. Runs
/// deferred, outside any entity update, because new windows read the model
/// while they're built.
fn schedule_sync(call: &Entity<Call>, cx: &mut App) {
    let call = call.clone();
    cx.defer(move |cx| sync_windows(&call, cx));
}

fn sync_windows(call: &Entity<Call>, cx: &mut App) {
    // Forget fullscreen and placement of shares that have ended.
    call.update(cx, |c, cx| {
        let live: Vec<TrackKey> = c.screens.iter().map(|s| s.read(cx).key.clone()).collect();
        c.fullscreen.retain(|key, _| live.contains(key));
        c.screen_bounds.retain(|key, _| live.contains(key));
    });
    let (minimal, regular, members, screens, fullscreen, screen_bounds, mut windows) = {
        let c = call.read(cx);
        let live = c.status != Status::Ended;
        let minimal = live && c.mode == Mode::Minimal;
        let regular = live && c.mode == Mode::Regular;
        let members: Vec<Entity<Member>> = if minimal {
            c.members().cloned().collect()
        } else {
            Vec::new()
        };
        // A share shown fullscreen has no floating window meanwhile.
        let screens: Vec<Entity<Screen>> = if minimal {
            c.screens
                .iter()
                .filter(|s| !c.is_fullscreen(&s.read(cx).key))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        // Fullscreen works from both modes.
        let fullscreen: Vec<(Entity<Screen>, Option<DisplayId>)> = if live {
            c.screens
                .iter()
                .filter_map(|s| Some((s.clone(), *c.fullscreen.get(&s.read(cx).key)?)))
                .collect()
        } else {
            Vec::new()
        };
        (
            minimal,
            regular,
            members,
            screens,
            fullscreen,
            c.screen_bounds.clone(),
            Windows::default(),
        )
    };
    call.update(cx, |c, _| std::mem::swap(&mut windows, &mut c.windows));

    let area = visible_area(cx);

    // Circles.
    let wanted: Vec<String> = members
        .iter()
        .map(|m| m.read(cx).identity.clone())
        .collect();
    windows.bubbles.retain(|identity, handle| {
        let keep = wanted.contains(identity);
        if !keep {
            close(handle, cx);
        }
        keep
    });
    for member in &members {
        let identity = member.read(cx).identity.clone();
        if windows.bubbles.contains_key(&identity) {
            continue;
        }
        let slot = windows.bubbles.len();
        let bounds = bubble_slot(area, slot, bubble::SIZE);
        if let Some(handle) = bubble::open(call.clone(), member.clone(), bounds, cx) {
            windows.bubbles.insert(identity, handle);
        }
    }

    // Screen shares.
    let wanted: Vec<TrackKey> = screens.iter().map(|s| s.read(cx).key.clone()).collect();
    windows.screens.retain(|key, handle| {
        let keep = wanted.contains(key);
        if !keep {
            close(handle, cx);
        }
        keep
    });
    for screen in &screens {
        let key = screen.read(cx).key.clone();
        if windows.screens.contains_key(&key) {
            continue;
        }
        let offset = px(32.0) * windows.screens.len() as f32;
        let bounds = screen_bounds.get(&key).copied().unwrap_or_else(|| {
            Bounds::new(
                point(
                    area.origin.x + px(48.0) + offset,
                    area.origin.y + px(48.0) + offset,
                ),
                size(
                    px(screen_window::WIDTH),
                    px(screen_window::WIDTH * 9.0 / 16.0),
                ),
            )
        });
        if let Some(handle) = screen_window::open(call.clone(), screen.clone(), bounds, cx) {
            windows.screens.insert(key, handle);
        }
    }

    // Fullscreen shares.
    let wanted: Vec<TrackKey> = fullscreen
        .iter()
        .map(|(s, _)| s.read(cx).key.clone())
        .collect();
    windows.fullscreen.retain(|key, handle| {
        let keep = wanted.contains(key);
        if !keep {
            close(handle, cx);
        }
        keep
    });
    for (screen, display) in &fullscreen {
        let key = screen.read(cx).key.clone();
        if windows.fullscreen.contains_key(&key) {
            continue;
        }
        if let Some(handle) = fullscreen::open(call.clone(), screen.clone(), *display, cx) {
            windows.fullscreen.insert(key, handle);
        }
    }

    // Toolbar.
    match (&windows.toolbar, minimal) {
        (None, true) => {
            let s = toolbar::window_size(call.read(cx));
            let bounds = Bounds::new(
                point(
                    area.origin.x + (area.size.width - px(s.width)) / 2.0,
                    area.origin.y + area.size.height - px(s.height) - px(28.0),
                ),
                size(px(s.width), px(s.height)),
            );
            windows.toolbar = toolbar::open(call.clone(), bounds, cx);
        }
        (Some(handle), false) => {
            close(handle, cx);
            windows.toolbar = None;
        }
        _ => {}
    }

    // Regular window.
    match (&windows.regular, regular) {
        (None, true) => windows.regular = crate::regular::open(call.clone(), cx),
        (Some(handle), false) => {
            close(handle, cx);
            windows.regular = None;
        }
        _ => {}
    }

    tracing::debug!(
        "call windows: {} circles, {} screen shares ({} fullscreen), toolbar {}, tiles {}",
        windows.bubbles.len(),
        windows.screens.len(),
        windows.fullscreen.len(),
        windows.toolbar.is_some(),
        windows.regular.is_some(),
    );
    call.update(cx, |c, _| c.windows = windows);
}

/// Closes on the next run-loop turn: the window may be the one handling the
/// click that got us here (the toolbar's expand button, say).
fn close<V: 'static>(handle: &WindowHandle<V>, cx: &mut App) {
    let handle = *handle;
    cx.spawn(async move |cx| {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
}

/// The usable part of the main screen (no menu bar or Dock).
fn visible_area(cx: &App) -> Bounds<Pixels> {
    cx.primary_display()
        .map(|d| d.visible_bounds())
        .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))))
}

/// Where the `slot`-th circle goes: a column up the right edge, then the next
/// column to the left. Slot 0 (you) sits in the bottom-right corner.
fn bubble_slot(area: Bounds<Pixels>, slot: usize, diameter: f32) -> WindowBounds {
    let margin = 28.0;
    let cell = bubble::SIZE + 16.0;
    let usable = (area.size.height / px(1.0) - 2.0 * margin).max(cell);
    let per_column = ((usable / cell) as usize).max(1);
    let (col, row) = (slot / per_column, slot % per_column);
    let right = area.origin.x / px(1.0) + area.size.width / px(1.0) - margin;
    let bottom = area.origin.y / px(1.0) + area.size.height / px(1.0) - margin;
    // Centre each circle in its cell so mixed sizes line up.
    let cx = right - cell * col as f32 - cell / 2.0;
    let cy = bottom - cell * row as f32 - cell / 2.0;
    WindowBounds::Windowed(Bounds::new(
        point(px(cx - diameter / 2.0), px(cy - diameter / 2.0)),
        size(px(diameter), px(diameter)),
    ))
}

/// LiveKit identities must be unique per room; this lets you run several
/// copies of the app side by side.
fn unique_identity(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{slug}-{:04x}", nanos & 0xffff)
}
