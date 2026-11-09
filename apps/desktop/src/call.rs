//! The call model, shared by every window of a call: the floating capsule,
//! screen-share windows and the regular window.
//!
//! Windows are views onto this model. Which windows exist follows from the
//! mode and the participants; `sync_windows` opens and closes them.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, DisplayId, Entity, EventEmitter, Pixels,
    Task, WindowHandle, point, px, size,
};
use quipcall_core::{
    Engine, Event, MediaDevice, ModelInfo, ScreenSharePreset, SourceId, TrackKey,
    TranscribeOptions, VideoKind, VideoQuality, VideoSubscription,
};

use crate::capsule::{self, Capsule};
use crate::fullscreen::{self, Fullscreen};
use crate::models::{self, Models};
use crate::regular::Regular;
use crate::screen_window::{self, ScreenWindow};
use crate::share_picker::{self, Purpose, SharePicker};
use crate::store::{self, CallEvent, Happened, RecordingFile, SaidLine};
use crate::theme::first_name;
use crate::transcript::{self, Said, Transcript, TranscriptWindow};
use crate::video_mac::SurfaceSink;
use crate::{Args, Shell};

/// Your own screen preview repaints at most this often: enough to see what
/// you share, without redrawing at 60 fps.
const PREVIEW_FPS: u32 = 15;
/// How long "Anna started recording" and similar notices stay.
const FLASH: Duration = Duration::from_secs(6);

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
    /// Everyone in one floating glass capsule; no call window.
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
    /// When they last joined, unmuted or spoke: the capsule shows whoever
    /// was active last.
    pub last_active: Option<Instant>,
}

impl Member {
    /// Speaking now, or active within `quiet`, and not muted.
    pub fn is_active(&self, quiet: Duration) -> bool {
        !self.mic_muted && (self.speaking || self.last_active.is_some_and(|t| t.elapsed() <= quiet))
    }
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
    pub events: Vec<CallEvent>,
    /// What you recorded in this call.
    pub recordings: Vec<RecordingFile>,
    /// What was said, if it was transcribed.
    pub transcript: Vec<SaidLine>,
}

/// Someone's Mac transcribes the call, yours or another's. One per call.
#[derive(Clone, Debug)]
pub struct Transcription {
    pub identity: String,
    pub name: String,
    pub model: Option<&'static ModelInfo>,
    pub mine: bool,
}

/// Getting ready to transcribe on this Mac.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Preparing {
    /// The model is downloading; transcription starts when it's here.
    Download(&'static ModelInfo),
    /// The model is loading.
    Loading,
}

/// Someone is recording the call. There's one recording per call, and only
/// whoever started it can stop it.
#[derive(Clone, Debug)]
pub struct Recording {
    /// The recorder's identity; yours when `file` is set.
    pub identity: String,
    pub name: String,
    pub since: Instant,
    /// Your own recording: where it goes.
    pub file: Option<PathBuf>,
    /// The recorder's screen is in it too.
    pub screen: bool,
    /// When it started, in seconds since you connected.
    at: u64,
}

impl Recording {
    pub fn is_mine(&self) -> bool {
        self.file.is_some()
    }
}

/// A line under the widget's row, or in the call window's title.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
    /// A recording to show in Finder.
    pub file: Option<PathBuf>,
}

/// A notice that goes away by itself.
#[derive(Clone, Debug)]
struct Flash {
    notice: Notice,
    until: Instant,
}

#[derive(Default)]
struct Windows {
    capsule: Option<WindowHandle<Capsule>>,
    screens: HashMap<TrackKey, WindowHandle<ScreenWindow>>,
    fullscreen: HashMap<TrackKey, WindowHandle<Fullscreen>>,
    regular: Option<WindowHandle<Regular>>,
    transcript: Option<WindowHandle<TranscriptWindow>>,
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
    /// Whoever records the call, you or someone else.
    pub recording: Option<Recording>,
    /// Asked to record; the engine hasn't answered yet.
    record_starting: bool,
    /// Your recordings in this call, saved or still going.
    recordings: Vec<RecordingFile>,
    /// Whose Mac transcribes the call, if anyone's.
    pub transcription: Option<Transcription>,
    pub preparing: Option<Preparing>,
    /// What was said, and what happened, in order.
    pub transcript: Entity<Transcript>,
    /// The transcript window is open (minimal mode).
    pub transcript_open: bool,
    flash: Option<Flash>,
    pub mode: Mode,
    pub waiting_for: Option<MediaDevice>,
    pub error: Option<String>,
    /// Turn the camera on once connected (`--camera`).
    camera_on_connect: bool,
    connected_at: Option<(SystemTime, Instant)>,
    people: BTreeSet<String>,
    /// Who joined, left, shared and recorded, for the call's timeline.
    pub events: Vec<CallEvent>,
    windows: Windows,
    _tasks: Vec<Task<()>>,
    _models: gpui::Subscription,
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
        let transcript = cx.new(|_| Transcript::new(args.name.clone()));
        // A download finishing starts the transcription waiting for it.
        let models = Models::global(cx);
        let models_task = cx.observe(&models, |call, models, cx| {
            if let Some(Preparing::Download(model)) = call.preparing {
                match models.read(cx).state(model) {
                    models::State::Ready => call.start_engine(model, cx),
                    models::State::Failed(e) => {
                        call.preparing = None;
                        call.fail(format!("downloading {}: {e}", model.name), cx);
                    }
                    models::State::Missing => call.preparing = None,
                    models::State::Downloading { .. } => {}
                }
                cx.notify();
            }
        });
        let me = cx.new(|_| Member {
            identity: identity.clone(),
            name: args.name.clone(),
            is_local: true,
            video: None,
            screen: None,
            speaking: false,
            mic_muted: true,
            last_active: None,
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
            recording: None,
            record_starting: false,
            recordings: Vec::new(),
            transcription: None,
            preparing: None,
            transcript,
            transcript_open: false,
            flash: None,
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
            events: Vec::new(),
            windows: Windows::default(),
            _tasks: vec![events_task, join_task],
            _models: models_task,
        }
    }

    pub fn link(&self) -> String {
        link(&self.web_url, &self.room.id)
    }

    /// A notice for the capsule or the call window's title, if there's
    /// something to say.
    pub fn notice(&self) -> Option<Notice> {
        let plain = |text: String, error| Notice {
            text,
            error,
            file: None,
        };
        if let Some(device) = self.waiting_for {
            return Some(plain(
                format!("Allow {} access in the macOS prompt", device.label()),
                false,
            ));
        }
        if self.status == Status::Joining {
            return Some(plain("Joining…".into(), false));
        }
        if let Some(error) = &self.error {
            return Some(plain(error.clone(), true));
        }
        self.flash
            .as_ref()
            .filter(|f| f.until > Instant::now())
            .map(|f| f.notice.clone())
    }

    /// Show `notice` for a few seconds.
    fn flash(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.flash = Some(Flash {
            notice,
            until: Instant::now() + FLASH,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            let _ = this.update(cx, |call, cx| {
                if call
                    .flash
                    .as_ref()
                    .is_some_and(|f| f.until <= Instant::now())
                {
                    call.flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn dismiss_notice(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.flash = None;
        cx.notify();
    }

    pub fn members(&self) -> impl Iterator<Item = &Entity<Member>> {
        std::iter::once(&self.me).chain(self.others.iter())
    }

    /// How long you've been connected.
    pub fn elapsed(&self) -> Option<Duration> {
        self.connected_at.map(|(_, at)| at.elapsed())
    }

    /// When you connected, on the wall clock.
    pub fn connected_since(&self) -> Option<SystemTime> {
        self.connected_at.map(|(wall, _)| wall)
    }

    fn log(&mut self, who: String, what: Happened, cx: &mut Context<Self>) {
        let at = self.elapsed().map_or(0, |e| e.as_secs());
        let event = CallEvent { at, who, what };
        self.events.push(event.clone());
        self.transcript.update(cx, |t, cx| t.event(event, cx));
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

    /// Load picker choices and previews on the engine's worker pool.
    pub fn screen_sources(
        &self,
        thumb_width: u32,
    ) -> async_channel::Receiver<anyhow::Result<quipcall_core::ShareSourceUpdate>> {
        self.engine.screen_sources(thumb_width)
    }

    /// Stop sharing, or open the picker to start.
    pub fn toggle_share(&mut self, cx: &mut Context<Self>) {
        if self.sharing.is_some() {
            self.engine.stop_screen_share();
            return;
        }
        self.open_picker(Purpose::Share, cx);
    }

    /// The share picker, to share or to record: brought to the front if it's
    /// open already.
    fn open_picker(&mut self, purpose: Purpose, cx: &mut Context<Self>) {
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
            let picker = share_picker::open(call.clone(), preset, purpose, cx);
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

    /// Who is sharing their screen (identities), besides you.
    pub fn sharers(&self, cx: &App) -> Vec<String> {
        self.screens
            .iter()
            .map(|s| s.read(cx).key.participant.clone())
            .collect()
    }

    /// Someone's floating share window, to bring it to the front.
    pub fn share_window_of(&self, identity: &str) -> Option<WindowHandle<ScreenWindow>> {
        self.windows
            .screens
            .iter()
            .find(|(key, _)| key.participant == identity)
            .map(|(_, handle)| *handle)
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

    /// Start recording the call on this Mac, or stop your recording. Does
    /// nothing while someone else records: there's one recording per call,
    /// and only they can stop it.
    pub fn toggle_record(&mut self, cx: &mut Context<Self>) {
        match &self.recording {
            Some(r) if r.is_mine() => self.stop_recording(cx),
            _ => self.record_audio(cx),
        }
    }

    /// Nobody records yet, so you could.
    pub fn can_record(&self) -> bool {
        self.recording.is_none() && !self.record_starting && self.status == Status::Connected
    }

    /// Record everyone's voice on this Mac.
    pub fn record_audio(&mut self, cx: &mut Context<Self>) {
        self.start_recording(None, cx);
    }

    /// Pick a screen or window in the share picker, then record it with
    /// everyone's voice.
    pub fn choose_screen_to_record(&mut self, cx: &mut Context<Self>) {
        if self.can_record() {
            self.open_picker(Purpose::Record, cx);
        }
    }

    pub fn record_screen(
        &mut self,
        source: SourceId,
        preset: ScreenSharePreset,
        cx: &mut Context<Self>,
    ) {
        self.share_preset = preset;
        self.start_recording(Some((source, preset)), cx);
    }

    fn start_recording(
        &mut self,
        screen: Option<(SourceId, ScreenSharePreset)>,
        cx: &mut Context<Self>,
    ) {
        if !self.can_record() {
            return;
        }
        let ext = if screen.is_some() { "mp4" } else { "m4a" };
        self.engine
            .start_recording(recording_path(&self.room.name, ext), screen);
        self.record_starting = true;
        cx.notify();
    }

    /// Stop your recording; only you can.
    pub fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording.as_ref().is_some_and(Recording::is_mine) {
            self.engine.stop_recording();
            cx.notify();
        }
    }

    /// Nobody transcribes yet, so your Mac could.
    pub fn can_transcribe(&self) -> bool {
        self.transcription.is_none() && self.preparing.is_none() && self.status == Status::Connected
    }

    /// Transcribe the call on this Mac (downloading the model first if it
    /// isn't here), stop the transcript, or stop waiting for the download.
    pub fn toggle_transcribe(&mut self, cx: &mut Context<Self>) {
        match (&self.transcription, self.preparing) {
            (Some(_), _) => self.stop_transcribing(cx),
            (None, Some(Preparing::Download(model))) => {
                self.preparing = None;
                Models::global(cx).update(cx, |m, cx| m.cancel(model, cx));
                cx.notify();
            }
            (None, Some(Preparing::Loading)) => {}
            (None, None) => self.start_transcribing(cx),
        }
    }

    pub fn start_transcribing(&mut self, cx: &mut Context<Self>) {
        if !self.can_transcribe() {
            return;
        }
        let model = Shell::global(cx).read(cx).store.transcription.model();
        let models = Models::global(cx);
        if models.read(cx).is_ready(model) {
            self.start_engine(model, cx);
        } else {
            self.preparing = Some(Preparing::Download(model));
            models.update(cx, |m, cx| m.fetch(model, cx));
        }
        cx.notify();
    }

    fn start_engine(&mut self, model: &'static ModelInfo, cx: &mut Context<Self>) {
        let language = Shell::global(cx)
            .read(cx)
            .store
            .transcription
            .language
            .clone();
        self.engine.start_transcribing(TranscribeOptions {
            model,
            dir: store::models_dir(),
            language,
        });
        self.preparing = Some(Preparing::Loading);
        cx.notify();
    }

    /// Stop the transcript for everyone: anyone can, whoever's Mac does it.
    pub fn stop_transcribing(&mut self, cx: &mut Context<Self>) {
        match &self.transcription {
            Some(t) if t.mine => self.engine.stop_transcribing(),
            Some(t) => self.engine.ask_to_stop_transcript(&t.identity),
            None => return,
        }
        cx.notify();
    }

    /// Open or close the transcript window (minimal mode).
    pub fn set_transcript_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.transcript_open != open {
            self.transcript_open = open;
            cx.notify();
            schedule_sync(&cx.entity(), cx);
        }
    }

    /// "On your Mac · Parakeet v3", "Anna's Mac · Parakeet v3", "Stopped"…
    pub fn transcript_status(&self, _cx: &App) -> String {
        let model =
            |m: Option<&'static ModelInfo>| m.map_or(String::new(), |m| format!(" · {}", m.name));
        match (&self.transcription, self.preparing) {
            (Some(t), _) if t.mine => format!("On your Mac{}", model(t.model)),
            (Some(t), _) => format!("On {}'s Mac{}", first_name(&t.name), model(t.model)),
            (None, Some(Preparing::Download(m))) => format!("Downloading {}…", m.name),
            (None, Some(Preparing::Loading)) => "Starting…".into(),
            (None, None) => "Stopped".into(),
        }
    }

    /// Resolves once the server knows you left (see [`Call::leave`]).
    pub fn closed(&self) -> impl Future<Output = ()> + use<> {
        self.engine.closed()
    }

    pub fn leave(&mut self, cx: &mut Context<Self>) {
        if self.status == Status::Ended {
            return;
        }
        // Leaving finishes your recording too (the engine closes the file).
        if let Some(recording) = self.recording.take().filter(Recording::is_mine) {
            self.keep_recording(&recording, recording.since.elapsed(), cx);
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
            events: self.events.clone(),
            recordings: self.recordings.clone(),
            transcript: self.transcript.read(cx).saved(),
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

    /// A transcript started: show it, unless Settings says not to.
    fn open_transcript_by_itself(&mut self, cx: &mut Context<Self>) {
        let keep_closed = Shell::global(cx)
            .read(cx)
            .store
            .transcription
            .keep_window_closed;
        if !keep_closed {
            self.set_transcript_open(true, cx);
        }
    }

    /// Remember your recording for the call's history.
    fn keep_recording(
        &mut self,
        recording: &Recording,
        duration: Duration,
        cx: &mut Context<Self>,
    ) {
        if let Some(file) = &recording.file {
            self.recordings.push(RecordingFile {
                path: file.clone(),
                at: recording.at,
                duration_secs: duration.as_secs(),
                screen: recording.screen,
            });
            let me = self.me.read(cx).name.clone();
            self.log(me, Happened::StoppedRecording, cx);
        }
    }

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
                let now = SystemTime::now();
                self.connected_at = Some((now, Instant::now()));
                self.transcript.update(cx, |t, _| t.connected(now));
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
                    self.log(name.clone(), Happened::Joined, cx);
                    let member = cx.new(|_| Member {
                        identity,
                        name,
                        is_local: false,
                        video: None,
                        screen: None,
                        speaking: false,
                        mic_muted: false,
                        last_active: Some(Instant::now()),
                    });
                    self.others.push(member);
                    windows_changed = true;
                }
            }
            Event::ParticipantLeft { identity } => {
                if let Some(member) = self.member(&identity, cx) {
                    let name = member.read(cx).name.clone();
                    self.log(name, Happened::Left, cx);
                }
                // Their recording stops with them; it stays on their Mac.
                if let Some(r) = self.recording.take_if(|r| r.identity == identity) {
                    let first = first_name(&r.name).to_owned();
                    self.log(r.name, Happened::StoppedRecording, cx);
                    self.flash(
                        Notice {
                            text: format!("{first} left; their recording stopped"),
                            error: false,
                            file: None,
                        },
                        cx,
                    );
                }
                if let Some(t) = self.transcription.take_if(|t| t.identity == identity) {
                    self.log(t.name.clone(), Happened::StoppedTranscribing, cx);
                    if self.flash.is_none() {
                        self.flash(
                            Notice {
                                text: format!(
                                    "{} left; the transcript stopped",
                                    first_name(&t.name)
                                ),
                                error: false,
                                file: None,
                            },
                            cx,
                        );
                    }
                }
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
                    self.log(owner.clone(), Happened::StartedSharing, cx);
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
                if let Some(screen) = self.screens.iter().find(|s| s.read(cx).key == key) {
                    let owner = screen.read(cx).owner.clone();
                    self.log(owner, Happened::StoppedSharing, cx);
                }
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
                        if speaking {
                            m.last_active = Some(Instant::now());
                        }
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
                        if m.mic_muted && !muted {
                            m.last_active = Some(Instant::now());
                        }
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
                if active != self.sharing.is_some() {
                    let me = self.me.read(cx).name.clone();
                    self.log(
                        me,
                        if active {
                            Happened::StartedSharing
                        } else {
                            Happened::StoppedSharing
                        },
                        cx,
                    );
                }
                self.sharing = if active { preset } else { None };
                if !active {
                    self.drop_preview(cx);
                }
            }
            Event::RecordingStarted { path, screen } => {
                self.record_starting = false;
                let (identity, name) = {
                    let me = self.me.read(cx);
                    (me.identity.clone(), me.name.clone())
                };
                self.recording = Some(Recording {
                    identity,
                    name: name.clone(),
                    since: Instant::now(),
                    file: Some(path),
                    screen,
                    at: self.elapsed().map_or(0, |e| e.as_secs()),
                });
                self.log(name, Happened::StartedRecording, cx);
            }
            Event::RecordingSaved(saved) => {
                if let Some(recording) = self.recording.take_if(|r| r.is_mine()) {
                    self.keep_recording(&recording, saved.duration, cx);
                }
                // Short: the widget is narrow. Length and size are in Home.
                tracing::info!(
                    duration = %crate::capsule::clock(saved.duration),
                    size = %store::file_size(saved.bytes),
                    "recording saved"
                );
                self.flash(
                    Notice {
                        text: "Recording saved".into(),
                        error: false,
                        file: Some(saved.path),
                    },
                    cx,
                );
            }
            Event::RecordingFailed(message) => {
                self.record_starting = false;
                // A recording that failed on the way keeps what it wrote.
                if let Some(recording) = self.recording.take_if(|r| r.is_mine()) {
                    let duration = recording.since.elapsed();
                    self.keep_recording(&recording, duration, cx);
                }
                self.fail(format!("recording: {message}"), cx);
            }
            Event::ParticipantRecording {
                identity,
                recording,
                screen,
            } => {
                let name = self
                    .member(&identity, cx)
                    .map(|m| m.read(cx).name.clone())
                    .unwrap_or_else(|| identity.clone());
                if recording {
                    // Two people pressing Record at once both record; you keep
                    // seeing your own.
                    if self.recording.as_ref().is_none_or(|r| !r.is_mine()) {
                        self.recording = Some(Recording {
                            identity,
                            name: name.clone(),
                            since: Instant::now(),
                            file: None,
                            screen,
                            at: self.elapsed().map_or(0, |e| e.as_secs()),
                        });
                        self.log(name.clone(), Happened::StartedRecording, cx);
                        let what = if screen {
                            "the call and their screen"
                        } else {
                            "this call"
                        };
                        self.flash(
                            Notice {
                                text: format!("{} started recording {what}", first_name(&name)),
                                error: false,
                                file: None,
                            },
                            cx,
                        );
                    }
                } else if self.recording.take_if(|r| r.identity == identity).is_some() {
                    self.log(name, Happened::StoppedRecording, cx);
                }
            }
            Event::TranscriptionStarted => {
                self.preparing = None;
                let (identity, name) = {
                    let me = self.me.read(cx);
                    (me.identity.clone(), me.name.clone())
                };
                let model = Shell::global(cx).read(cx).store.transcription.model();
                self.transcription = Some(Transcription {
                    identity,
                    name: name.clone(),
                    model: Some(model),
                    mine: true,
                });
                self.log(name, Happened::StartedTranscribing, cx);
                self.open_transcript_by_itself(cx);
            }
            Event::TranscriptLine { by, line } => {
                // On the wall clock, relative to when you connected.
                let connected_ms = self
                    .connected_since()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                let me = self.me.read(cx);
                let (mine, name) = if line.speaker == me.identity {
                    (true, me.name.clone())
                } else {
                    let name = self
                        .member(&line.speaker, cx)
                        .map(|m| m.read(cx).name.clone())
                        .unwrap_or_else(|| line.speaker.clone());
                    (false, name)
                };
                let said = Said {
                    id: format!("{by}/{}", line.id),
                    identity: line.speaker,
                    name,
                    mine,
                    at_ms: line.at_ms as i64 - connected_ms,
                    text: line.text,
                    done: line.done,
                };
                self.transcript.update(cx, |t, cx| t.said(said, cx));
                // Lines don't change anything else on screen.
                return;
            }
            Event::TranscriptionStopped { by } => {
                if let Some(t) = self.transcription.take_if(|t| t.mine) {
                    // Someone else stopped it: say who.
                    let name = by
                        .as_deref()
                        .and_then(|id| self.member(id, cx))
                        .map(|m| m.read(cx).name.clone());
                    match name {
                        Some(name) => {
                            self.flash(
                                Notice {
                                    text: format!("{} stopped the transcript", first_name(&name)),
                                    error: false,
                                    file: None,
                                },
                                cx,
                            );
                            self.log(name, Happened::StoppedTranscribing, cx);
                        }
                        None => self.log(t.name, Happened::StoppedTranscribing, cx),
                    }
                }
            }
            Event::TranscriptionFailed(message) => {
                self.preparing = None;
                self.fail(format!("transcript: {message}"), cx);
            }
            Event::ParticipantTranscribing { identity, model } => {
                let name = self
                    .member(&identity, cx)
                    .map(|m| m.read(cx).name.clone())
                    .unwrap_or_else(|| identity.clone());
                match model {
                    Some(model) => {
                        if self.transcription.as_ref().is_none_or(|t| !t.mine) {
                            let first = first_name(&name).to_owned();
                            self.transcription = Some(Transcription {
                                identity,
                                name: name.clone(),
                                model: quipcall_core::catalog::model(&model),
                                mine: false,
                            });
                            self.log(name, Happened::StartedTranscribing, cx);
                            self.flash(
                                Notice {
                                    text: format!("{first}'s Mac is transcribing this call"),
                                    error: false,
                                    file: None,
                                },
                                cx,
                            );
                            self.open_transcript_by_itself(cx);
                        }
                    }
                    None => {
                        if self
                            .transcription
                            .take_if(|t| t.identity == identity)
                            .is_some()
                        {
                            self.log(name, Happened::StoppedTranscribing, cx);
                        }
                    }
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

/// A new file for a recording in `room`, e.g. "Design review 2026-11-09
/// 11.14.m4a" (`ext` "m4a" or "mp4"), in [`store::recordings_dir`].
fn recording_path(room: &str, ext: &str) -> PathBuf {
    let dir = store::recordings_dir();
    let room: String = room
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let base = format!("{} {}", room.trim(), store::file_stamp(now));
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{base}.{ext}")),
            n => dir.join(format!("{base} {n}.{ext}")),
        })
        .find(|p| !p.exists())
        .expect("unbounded range")
}

/// The call link for `room` on the web client at `web`.
pub fn link(web: &str, room: &str) -> String {
    format!("{}/?room={room}", web.trim_end_matches('/'))
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
    let (minimal, regular, transcript, screens, fullscreen, shares, screen_bounds, mut windows) = {
        let c = call.read(cx);
        let live = c.status != Status::Ended;
        let minimal = live && c.mode == Mode::Minimal;
        let regular = live && c.mode == Mode::Regular;
        // In the call window, the transcript is in the side panel.
        let transcript = minimal && c.transcript_open;
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
        // Shares still going, to tell a fullscreen you left from one that ended.
        let shares: Vec<TrackKey> = if live {
            c.screens.iter().map(|s| s.read(cx).key.clone()).collect()
        } else {
            Vec::new()
        };
        (
            minimal,
            regular,
            transcript,
            screens,
            fullscreen,
            shares,
            c.screen_bounds.clone(),
            Windows::default(),
        )
    };
    call.update(cx, |c, _| std::mem::swap(&mut windows, &mut c.windows));

    let area = visible_area(cx);

    // The capsule.
    match (&windows.capsule, minimal) {
        (None, true) => windows.capsule = capsule::open(call.clone(), cx),
        (Some(handle), false) => {
            close(handle, cx);
            windows.capsule = None;
        }
        _ => {}
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
        if !keep && shares.contains(key) {
            // You left it, from its own Space.
            close(handle, cx);
        } else if !keep {
            // The share or the call ended, maybe while you were on another
            // Space: it closes itself when it can.
            let handle = *handle;
            cx.spawn(async move |cx| {
                let _ = handle.update(cx, |view, window, cx| view.end(window, cx));
            })
            .detach();
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

    // Regular window.
    match (&windows.regular, regular) {
        (None, true) => windows.regular = crate::regular::open(call.clone(), cx),
        (Some(handle), false) => {
            close(handle, cx);
            windows.regular = None;
        }
        _ => {}
    }

    // The transcript window.
    match (&windows.transcript, transcript) {
        (None, true) => windows.transcript = transcript::open(call.clone(), cx),
        (Some(handle), false) => {
            close(handle, cx);
            windows.transcript = None;
        }
        _ => {}
    }

    tracing::debug!(
        "call windows: capsule {}, {} screen shares ({} fullscreen), call window {}",
        windows.capsule.is_some(),
        windows.screens.len(),
        windows.fullscreen.len(),
        windows.regular.is_some(),
    );
    call.update(cx, |c, _| c.windows = windows);
}

/// Closes on the next run-loop turn: the window may be the one handling the
/// click that got us here (the capsule's expand button, say).
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
