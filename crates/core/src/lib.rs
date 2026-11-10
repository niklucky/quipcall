//! Quipcall call engine.
//!
//! Owns everything about being in a call: the LiveKit room, platform audio,
//! published tracks and remote video plumbing. It has no UI dependency — a UI
//! drives it through [`Engine`] methods and consumes [`Event`]s.
//!
//! LiveKit needs tokio; the engine owns a small runtime so the UI can use any
//! executor it likes.

pub mod audio_tap;
mod camera;
pub mod denoise;
pub mod models;
pub mod permissions;
#[cfg(target_os = "macos")]
pub mod pixel_buffer;
#[cfg(target_os = "macos")]
pub mod player;
pub mod record;
#[cfg(target_os = "macos")]
mod record_mac;
mod screen;
#[cfg(target_os = "macos")]
mod screen_mac;
mod transcribe;
pub mod trial;
pub mod video;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use futures_util::{FutureExt as _, StreamExt as _};
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::{AudioProcessingOptions, PlatformAudio};
use parking_lot::Mutex;
use quipcall_protocol::{ApiError, JoinInfo};

pub use audio_tap::{Listener, Speaker, VoiceFrame};
pub use livekit;
pub use livekit::track::VideoQuality;
pub use models::{ModelInfo, catalog};
pub use permissions::{Access, MediaDevice};
pub use quipcall_protocol as protocol;
pub use quipcall_protocol::TranscriptLine;
use quipcall_protocol::{TRANSCRIPT_TOPIC, TranscriptMessage};
pub use record::Saved;
pub use screen::{ContentMode, ScreenSharePreset, SourceId};
#[cfg(target_os = "macos")]
pub use screen_mac::{NoPermission, ShareSource, ShareSourceUpdate, Thumbnail, own_window};
pub use video::{FrameSink, VideoSubscription};

/// The participant attribute that says someone is recording the call:
/// "audio", or "screen" (audio and their screen). Everyone's app shows it.
const RECORDING: &str = "quipcall.recording";
/// The participant attribute that says someone's Mac transcribes the call:
/// "<model id>@<unix ms it started>". If two Macs ever do, the one that
/// started last carries on (a handover), and the other stops.
const TRANSCRIBING: &str = "quipcall.transcribing";
/// The participant attribute that says a Mac could transcribe the call: the
/// id of the model it has ready.
const CAN_TRANSCRIBE: &str = "quipcall.can_transcribe";

/// Stable identifier of a remote track inside the UI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackKey {
    pub participant: String,
    pub sid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoKind {
    Camera,
    Screen,
}

#[derive(Debug)]
pub enum Event {
    Connected {
        room: String,
        identity: String,
    },
    Disconnected {
        reason: String,
    },
    ParticipantJoined {
        identity: String,
        name: String,
    },
    ParticipantLeft {
        identity: String,
    },
    VideoTrackAdded {
        key: TrackKey,
        kind: VideoKind,
        track: RemoteVideoTrack,
    },
    VideoTrackRemoved {
        key: TrackKey,
    },
    ActiveSpeakers(Vec<String>),
    /// A remote participant's microphone was muted, unmuted, or removed.
    ParticipantMic {
        identity: String,
        muted: bool,
    },
    /// The OS is showing a permission prompt; the call waits for the answer.
    WaitingForPermission(MediaDevice),
    MicChanged {
        enabled: bool,
    },
    CameraChanged {
        enabled: bool,
    },
    ScreenShareChanged {
        active: bool,
        preset: Option<ScreenSharePreset>,
    },
    /// You're recording into `path`, with your screen if `screen`; everyone
    /// has been told.
    RecordingStarted {
        path: PathBuf,
        screen: bool,
    },
    /// Your recording stopped and its file is complete.
    RecordingSaved(Saved),
    /// Your recording couldn't start, or failed on the way.
    RecordingFailed(String),
    /// Someone else started or stopped recording the call (with their screen
    /// if `screen`).
    ParticipantRecording {
        identity: String,
        recording: bool,
        screen: bool,
    },
    /// Your Mac transcribes the call; everyone has been told.
    TranscriptionStarted,
    /// What someone said, or a draft of it while they talk (`done` false),
    /// as transcribed on `by`'s Mac (an identity; yours, or the
    /// transcriber's). A line replaces earlier ones with its id from the same
    /// transcriber.
    TranscriptLine {
        by: String,
        line: TranscriptLine,
    },
    /// Your transcription stopped, and every line is out. `by` asked for it,
    /// if it wasn't you; `handed_over`: their Mac carries on.
    TranscriptionStopped {
        by: Option<String>,
        handed_over: bool,
    },
    /// Your transcription couldn't start.
    TranscriptionFailed(String),
    /// Someone else's Mac started or stopped transcribing the call, with the
    /// model `model` (a catalog id).
    ParticipantTranscribing {
        identity: String,
        model: Option<String>,
    },
    /// Someone's Mac can transcribe the call, with `model`, or can't any more.
    ParticipantCanTranscribe {
        identity: String,
        model: Option<String>,
    },
    /// `from` asks your Mac to transcribe the call; `handover`: their Mac
    /// does now, and stops once yours has started.
    TranscriptAsked {
        from: String,
        handover: bool,
    },
    /// `from` answered your ask.
    TranscriptAnswered {
        from: String,
        accepted: bool,
    },
    Error(String),
}

/// What to transcribe with.
#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    pub model: &'static ModelInfo,
    /// Where the model and [`catalog::VAD`] are.
    pub dir: PathBuf,
    /// ISO 639-1, or `None` to let the model tell.
    pub language: Option<String>,
    /// Take over from whoever transcribes now (they asked): start even
    /// though someone does.
    pub handover: bool,
}

pub struct Engine {
    /// `None` only while dropping, when it moves to a thread of its own.
    rt: Option<tokio::runtime::Runtime>,
    state: Arc<Mutex<State>>,
    events: async_channel::Sender<Event>,
}

/// How long leaving may take once the engine is dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct State {
    room: Option<Arc<Room>>,
    /// Keeps the platform audio device module alive for the duration of a call.
    /// Without it remote audio has nowhere to play.
    audio: Option<PlatformAudio>,
    mic: Option<LocalAudioTrack>,
    camera: Option<camera::Camera>,
    /// Set while the camera is being opened, to ignore repeated clicks.
    camera_starting: bool,
    screen: Option<screen::ScreenShare>,
    /// Remote video publications, for per-tile quality control.
    videos: HashMap<TrackKey, RemoteTrackPublication>,
    /// Remote microphones, so someone listening (a recording) can hear them.
    mics: HashMap<TrackKey, RemoteAudioTrack>,
    /// Their decoded audio, while something listens.
    mic_streams: HashMap<TrackKey, tokio::task::JoinHandle<()>>,
    /// Your recording, while you record.
    recorder: Option<record::Recorder>,
    /// Your transcription, while your Mac transcribes.
    transcribing: Option<transcribe::Transcribing>,
    /// Set while the model loads, to ignore repeated clicks.
    transcribe_starting: bool,
    /// The call's final lines, from your Mac or whoever transcribed before
    /// (a handover keeps them), for whoever joins while yours transcribes.
    transcribed: Vec<TranscriptLine>,
    /// When your transcription started, unix ms (see [`TRANSCRIBING`]).
    transcribing_since: u64,
    /// The model your Mac could transcribe with, told to everyone.
    can_transcribe: Option<String>,
    /// `disconnect` saying goodbye to the server; see [`Engine::closed`].
    closing: Vec<Closing>,
}

type Closing = futures_util::future::Shared<futures_util::future::BoxFuture<'static, ()>>;

impl Engine {
    pub fn new() -> anyhow::Result<(Self, async_channel::Receiver<Event>)> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("quipcall-engine")
            .enable_all()
            .build()?;
        let (events, rx) = async_channel::unbounded();
        Ok((
            Self {
                rt: Some(rt),
                state: Default::default(),
                events,
            },
            rx,
        ))
    }

    /// Fetch a token for `room` from a quipcall server.
    pub fn join(
        &self,
        server: &str,
        room: &str,
        identity: &str,
        name: &str,
    ) -> impl Future<Output = anyhow::Result<JoinInfo>> + Send + 'static {
        let url = format!("{}/api/join", server.trim_end_matches('/'));
        let query = [
            ("room", room.to_owned()),
            ("identity", identity.to_owned()),
            ("name", name.to_owned()),
        ];
        let handle = self.rt().handle().clone();
        async move {
            handle
                .spawn(async move {
                    let client = reqwest::Client::builder()
                        .timeout(Duration::from_secs(10))
                        .build()?;
                    // Flaky Wi-Fi or a VPN dropping the first connection
                    // shouldn't fail the call: retry transport errors only.
                    let mut attempt = 1;
                    let resp = loop {
                        match client.get(&url).query(&query).send().await {
                            Ok(resp) => break resp,
                            Err(e) if attempt < 3 && (e.is_connect() || e.is_timeout()) => {
                                tracing::warn!("join attempt {attempt} failed: {e}");
                                tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                                attempt += 1;
                            }
                            Err(e) => return Err(e.into()),
                        }
                    };
                    if !resp.status().is_success() {
                        let err: ApiError = resp.json().await.unwrap_or(ApiError {
                            error: "unknown server error".into(),
                        });
                        anyhow::bail!("server: {}", err.error);
                    }
                    Ok(resp.json::<JoinInfo>().await?)
                })
                .await?
        }
    }

    pub fn connect(&self, join: JoinInfo) {
        let state = self.state.clone();
        let events = self.events.clone();
        self.rt().spawn(async move {
            if let Err(e) = connect(state, events.clone(), join).await {
                let _ = events
                    .send(Event::Error(format!("connect failed: {e:#}")))
                    .await;
            }
        });
    }

    pub fn disconnect(&self) {
        // Everything of this call leaves the state now, the audio device too:
        // a call connected before the goodbye below finishes has its own.
        audio_tap::set_mic_live(false);
        let (room, audio, screen, camera, recorder, transcribing) = {
            let mut s = self.state.lock();
            s.mic = None;
            s.transcribed.clear();
            s.mics.clear();
            for (_, task) in s.mic_streams.drain() {
                task.abort();
            }
            // Forget goodbyes already said. `peek` would only see ones
            // something awaited, so poll them.
            s.closing.retain(|c| c.clone().now_or_never().is_none());
            (
                s.room.take(),
                s.audio.take(),
                s.screen.take(),
                s.camera.take(),
                s.recorder.take(),
                s.transcribing.take(),
            )
        };
        drop(screen);
        let events = self.events.clone();
        let closing = self.rt().spawn(async move {
            // What was being said when you left is still transcribed; the
            // call's history waits for it.
            if let Some(transcribing) = transcribing {
                let _ = tokio::task::spawn_blocking(move || transcribing.stop()).await;
                let _ = events
                    .send(Event::TranscriptionStopped {
                        by: None,
                        handed_over: false,
                    })
                    .await;
            }
            // Leaving finishes your recording: everything up to now is kept.
            if let Some(recorder) = recorder {
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = recorder.stop() {
                        tracing::warn!("recording: {e:#}");
                    }
                })
                .await;
            }
            // Say goodbye first: until the server hears it, everyone else
            // (and you, if you rejoin) sees you in the room for another 20 s.
            if let Some(room) = room {
                let _ = room.close().await;
            }
            // Stopping the camera blocks briefly; keep it off the UI thread.
            if let Some(camera) = camera {
                let _ = tokio::task::spawn_blocking(move || drop(camera)).await;
            }
            // Release the audio device only after the room is gone.
            drop(audio);
        });
        let closing = closing.map(|_| ()).boxed().shared();
        self.state.lock().closing.push(closing);
    }

    /// Resolves once every `disconnect` so far has finished: the server knows
    /// you left. Works from any executor, e.g. while the app quits.
    pub fn closed(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        let closing = self.state.lock().closing.clone();
        futures_util::future::join_all(closing).map(|_| ())
    }

    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().expect("engine runtime")
    }

    pub fn set_mic_enabled(&self, enabled: bool) {
        // LiveKit spawns tokio tasks internally; UI threads have no runtime.
        let _rt = self.rt().enter();
        tracing::info!(enabled, "set mic");
        let s = self.state.lock();
        if let Some(mic) = &s.mic {
            if enabled {
                mic.unmute()
            } else {
                mic.mute()
            }
            audio_tap::set_mic_live(enabled);
            let _ = self.events.try_send(Event::MicChanged { enabled });
        }
    }

    /// Open the camera and publish it. Frames also go to `preview` (self-view).
    pub fn start_camera(&self, preview: Option<Arc<dyn FrameSink>>) {
        {
            let mut s = self.state.lock();
            if s.camera.is_some() || s.camera_starting {
                return;
            }
            s.camera_starting = true;
        }
        let state = self.state.clone();
        let events = self.events.clone();
        self.rt().spawn(async move {
            let result = async {
                if !camera::synthetic() {
                    if permissions::status(MediaDevice::Camera) == Access::Undecided {
                        let _ = events
                            .send(Event::WaitingForPermission(MediaDevice::Camera))
                            .await;
                    }
                    anyhow::ensure!(
                        permissions::request(MediaDevice::Camera).await,
                        "camera access is off. Allow it in System Settings → Privacy & Security → Camera"
                    );
                }
                let room = state.lock().room.clone().context("not in a call")?;
                let camera = camera::Camera::start(&room, preview).await?;
                // The call may have ended while the camera was starting.
                let stale = {
                    let mut s = state.lock();
                    if s.room.as_ref().is_some_and(|r| Arc::ptr_eq(r, &room)) {
                        s.camera = Some(camera);
                        None
                    } else {
                        Some(camera)
                    }
                };
                if let Some(camera) = stale {
                    camera.stop(&room).await;
                    anyhow::bail!("call ended");
                }
                Ok(())
            }
            .await;
            state.lock().camera_starting = false;
            let event = match result {
                Ok(()) => Event::CameraChanged { enabled: true },
                Err(e) => Event::Error(format!("camera: {e:#}")),
            };
            let _ = events.send(event).await;
        });
    }

    pub fn stop_camera(&self) {
        let (room, camera) = {
            let mut s = self.state.lock();
            (s.room.clone(), s.camera.take())
        };
        let events = self.events.clone();
        self.rt().spawn(async move {
            if let Some(camera) = camera {
                match room {
                    Some(room) => camera.stop(&room).await,
                    None => drop(camera),
                }
            }
            let _ = events.send(Event::CameraChanged { enabled: false }).await;
        });
    }

    /// Lists share choices first, then delivers previews as they arrive. Native
    /// screen APIs may block even before calling their completion handlers, so
    /// keep the entire operation off both the UI and media executor threads.
    /// Dropping the receiver stops queuing and waiting for previews. In-flight
    /// native requests retain their shared budget until completion, including
    /// when another picker opens with its own stream.
    #[cfg(target_os = "macos")]
    pub fn screen_sources(
        &self,
        thumb_width: u32,
    ) -> async_channel::Receiver<anyhow::Result<ShareSourceUpdate>> {
        let (tx, rx) = async_channel::bounded(8);
        let runtime = self.rt().handle().clone();
        self.rt().spawn_blocking(move || {
            crate::autorelease_pool(|| {
                runtime.block_on(async {
                    tokio::select! {
                        biased;
                        _ = tx.closed() => {},
                        result = screen_mac::list_sources(thumb_width, &tx) => {
                            if let Err(error) = result {
                                let _ = tx.send(Err(error)).await;
                            }
                        }
                    }
                });
            });
        });
        rx
    }

    /// Share `source` (required on macOS, where our own picker chooses it).
    /// Captured frames also go to `preview`, so you can see what you share.
    pub fn start_screen_share(
        &self,
        source: Option<SourceId>,
        preset: ScreenSharePreset,
        preview: Option<Arc<dyn FrameSink>>,
    ) {
        let state = self.state.clone();
        let events = self.events.clone();
        let handle = self.rt().handle().clone();
        self.rt().spawn(async move {
            let result = async {
                let room = state.lock().room.clone().context("not in a call")?;
                let previous = state.lock().screen.take();
                if let Some(previous) = previous {
                    previous.unpublish(&room).await;
                }
                let on_fail = {
                    let (state, events) = (state.clone(), events.clone());
                    move || {
                        handle.spawn(stop_screen_share(state, events));
                    }
                };
                let share = screen::ScreenShare::start(
                    &room,
                    source,
                    preset,
                    preview,
                    events.clone(),
                    on_fail,
                )
                .await?;
                state.lock().screen = Some(share);
                anyhow::Ok(())
            }
            .await;
            let event = match result {
                Ok(()) => Event::ScreenShareChanged {
                    active: true,
                    preset: Some(preset),
                },
                Err(e) => Event::Error(format!("screen share: {e:#}")),
            };
            let _ = events.send(event).await;
        });
    }

    pub fn stop_screen_share(&self) {
        self.rt()
            .spawn(stop_screen_share(self.state.clone(), self.events.clone()));
    }

    /// Pick the simulcast layer for a remote video. The UI calls this from the
    /// tile's on-screen size; LiveKit's Rust SDK doesn't do adaptive sizing itself.
    pub fn set_video_quality(&self, key: &TrackKey, quality: VideoQuality) {
        let _rt = self.rt().enter();
        if let Some(publication) = self.state.lock().videos.get(key)
            && publication.simulcasted()
        {
            publication.set_video_quality(quality);
        }
    }

    /// Start pumping decoded frames of `track` into `sink`. Frames stop when the
    /// returned subscription is dropped.
    pub fn attach_video(
        &self,
        track: &RemoteVideoTrack,
        sink: Arc<dyn FrameSink>,
    ) -> VideoSubscription {
        video::attach(self.rt().handle(), track, sink)
    }

    /// Everyone's voice, 10 ms at a time, until the listener is dropped.
    pub fn listen(&self) -> Listener {
        listen(&self.state, self.rt().handle())
    }

    /// Record the call into `path`: everyone's voice, mixed (.m4a), and with
    /// `screen`, that screen or window too (.mp4). Everyone is told first (a
    /// participant attribute), so nobody is recorded without seeing it. One
    /// recording per call: refused while someone else records. With
    /// `each_voice`, every voice also goes into a file of its own, in a
    /// folder named after the recording.
    pub fn start_recording(
        &self,
        path: PathBuf,
        screen: Option<(SourceId, ScreenSharePreset)>,
        each_voice: bool,
    ) {
        let state = self.state.clone();
        let events = self.events.clone();
        let handle = self.rt().handle().clone();
        let with_screen = screen.is_some();
        self.rt().spawn(async move {
            let event =
                match start_recording(&state, &handle, &events, path, screen, each_voice).await {
                    Ok(path) => Event::RecordingStarted {
                        path,
                        screen: with_screen,
                    },
                    Err(e) => Event::RecordingFailed(format!("{e:#}")),
                };
            let _ = events.send(event).await;
        });
    }

    /// Finish your recording; [`Event::RecordingSaved`] follows.
    pub fn stop_recording(&self) {
        let (room, recorder) = {
            let mut s = self.state.lock();
            (s.room.clone(), s.recorder.take())
        };
        let Some(recorder) = recorder else { return };
        let events = self.events.clone();
        self.rt().spawn(async move {
            let saved = tokio::task::spawn_blocking(move || recorder.stop()).await;
            if let Some(room) = room
                && let Err(e) = set_attribute(&room, RECORDING, "").await
            {
                tracing::warn!("recording attribute: {e}");
            }
            let event = match saved {
                Ok(Ok(saved)) => Event::RecordingSaved(saved),
                Ok(Err(e)) => Event::RecordingFailed(format!("{e:#}")),
                Err(e) => Event::RecordingFailed(format!("{e}")),
            };
            let _ = events.send(event).await;
        });
    }

    /// Transcribe the call on this Mac: everyone's voice, each on its own.
    /// Loading the model takes a few seconds. Everyone is told before any
    /// voice is heard; one transcriber per call.
    pub fn start_transcribing(&self, options: TranscribeOptions) {
        let state = self.state.clone();
        let events = self.events.clone();
        let handle = self.rt().handle().clone();
        self.rt().spawn(async move {
            let started = start_transcribing(&state, &handle, &events, options).await;
            state.lock().transcribe_starting = false;
            let event = match &started {
                Ok(()) => Event::TranscriptionStarted,
                Err(e) => Event::TranscriptionFailed(format!("{e:#}")),
            };
            let _ = events.send(event).await;
            if started.is_ok() {
                settle_transcriber(&state, &events).await;
            }
        });
    }

    /// Stop transcribing; the last lines and [`Event::TranscriptionStopped`]
    /// follow.
    pub fn stop_transcribing(&self) {
        let state = self.state.clone();
        let events = self.events.clone();
        self.rt()
            .spawn(async move { stop_transcribing(&state, &events, None).await });
    }

    /// Ask `identity`'s Mac to stop transcribing the call: anyone may.
    pub fn ask_to_stop_transcript(&self, identity: &str) {
        self.send_transcript_to(identity, TranscriptMessage::Stop);
    }

    /// Ask `identity`'s Mac to transcribe the call; `handover` if yours does
    /// now. [`Event::TranscriptAnswered`] follows, unless they just start.
    pub fn ask_to_transcribe(&self, identity: &str, handover: bool) {
        self.send_transcript_to(identity, TranscriptMessage::Ask { handover });
    }

    /// Answer `identity`'s ask.
    pub fn answer_transcript_ask(&self, identity: &str, accepted: bool) {
        self.send_transcript_to(identity, TranscriptMessage::Answer { accepted });
    }

    /// Tell everyone your Mac could transcribe the call with `model` (it's
    /// downloaded), or that it can't. Kept for the next call too.
    pub fn set_can_transcribe(&self, model: Option<&'static ModelInfo>) {
        let value = model.map(|m| m.id.to_owned());
        let room = {
            let mut s = self.state.lock();
            if s.can_transcribe == value {
                return;
            }
            s.can_transcribe = value.clone();
            s.room.clone()
        };
        if let Some(room) = room {
            self.rt().spawn(async move {
                let value = value.as_deref().unwrap_or("");
                if let Err(e) = set_attribute(&room, CAN_TRANSCRIBE, value).await {
                    tracing::warn!("can-transcribe attribute: {e}");
                }
            });
        }
    }

    fn send_transcript_to(&self, identity: &str, message: TranscriptMessage) {
        let Some(room) = self.state.lock().room.clone() else {
            return;
        };
        let to = identity.to_owned();
        self.rt().spawn(async move {
            if let Err(e) = send_transcript(&room, &message, Some(to)).await {
                tracing::warn!("transcript message {message:?}: {e}");
            }
        });
    }
}

async fn stop_transcribing(
    state: &Arc<Mutex<State>>,
    events: &async_channel::Sender<Event>,
    by: Option<String>,
) {
    stop_transcribing_for(state, events, by, false).await
}

async fn stop_transcribing_for(
    state: &Arc<Mutex<State>>,
    events: &async_channel::Sender<Event>,
    by: Option<String>,
    handed_over: bool,
) {
    let (room, transcribing) = {
        let mut s = state.lock();
        (s.room.clone(), s.transcribing.take())
    };
    let Some(transcribing) = transcribing else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || transcribing.stop()).await;
    if let Some(room) = &room
        && let Err(e) = set_attribute(room, TRANSCRIBING, "").await
    {
        tracing::warn!("transcribing attribute: {e}");
    }
    // They may have stopped while your last words went out: then nobody
    // carries on.
    let handed_over = handed_over
        && room.as_ref().zip(by.as_deref()).is_some_and(|(room, by)| {
            room.remote_participants()
                .get(&ParticipantIdentity(by.to_owned()))
                .is_some_and(transcribes)
        });
    let _ = events
        .send(Event::TranscriptionStopped { by, handed_over })
        .await;
}

/// Someone else claimed the transcript while yours was starting (two people
/// pressing Transcribe at once): their claim came before yours was in place,
/// so the room's events let it by. Settle it the way both Macs agree on.
async fn settle_transcriber(state: &Arc<Mutex<State>>, events: &async_channel::Sender<Event>) {
    let (room, since) = {
        let s = state.lock();
        match (&s.room, &s.transcribing) {
            (Some(room), Some(_)) => (room.clone(), s.transcribing_since),
            _ => return,
        }
    };
    let you = room.local_participant().identity().to_string();
    if let Some((theirs, identity)) = newest_claim(&room)
        && yields_to((since, &you), (theirs, &identity))
    {
        tracing::info!(%identity, "their Mac started at the same time and carries on");
        stop_transcribing_for(state, events, Some(identity), true).await;
    }
}

/// A transcript message to everyone, or to one person.
async fn send_transcript(
    room: &Room,
    message: &TranscriptMessage,
    to: Option<String>,
) -> anyhow::Result<()> {
    room.local_participant()
        .publish_data(DataPacket {
            payload: serde_json::to_vec(message)?,
            topic: Some(TRANSCRIPT_TOPIC.to_owned()),
            reliable: true,
            destination_identities: to.into_iter().map(ParticipantIdentity).collect(),
        })
        .await?;
    Ok(())
}

/// The lines so far, to someone who just joined, a few at a time: data
/// packets should stay under 15 KB.
async fn send_history(room: &Room, lines: Vec<TranscriptLine>, to: String) {
    for chunk in lines.chunks(40) {
        let message = TranscriptMessage::Lines {
            lines: chunk.to_vec(),
        };
        if let Err(e) = send_transcript(room, &message, Some(to.clone())).await {
            tracing::warn!("transcript history to {to}: {e}");
            return;
        }
    }
}

/// A transcript message from someone in the call.
async fn on_transcript(
    state: &Arc<Mutex<State>>,
    events: &async_channel::Sender<Event>,
    from: &RemoteParticipant,
    payload: &[u8],
) {
    let by = from.identity().to_string();
    match serde_json::from_slice::<TranscriptMessage>(payload) {
        Ok(TranscriptMessage::Lines { lines }) => {
            {
                let mut s = state.lock();
                for line in &lines {
                    remember(&mut s.transcribed, line);
                }
            }
            for line in lines {
                let _ = events
                    .send(Event::TranscriptLine {
                        by: by.clone(),
                        line,
                    })
                    .await;
            }
        }
        Ok(TranscriptMessage::Stop) => {
            tracing::info!(%by, "asked to stop the transcript");
            // The last words take a moment; room events go on meanwhile.
            let (state, events) = (state.clone(), events.clone());
            tokio::spawn(async move { stop_transcribing(&state, &events, Some(by)).await });
        }
        Ok(TranscriptMessage::Ask { handover }) => {
            tracing::info!(%by, handover, "asked to transcribe");
            let _ = events
                .send(Event::TranscriptAsked { from: by, handover })
                .await;
        }
        Ok(TranscriptMessage::Answer { accepted }) => {
            let _ = events
                .send(Event::TranscriptAnswered { from: by, accepted })
                .await;
        }
        Err(e) => tracing::warn!(%by, "transcript message: {e}"),
    }
}

async fn start_transcribing(
    state: &Arc<Mutex<State>>,
    handle: &tokio::runtime::Handle,
    events: &async_channel::Sender<Event>,
    options: TranscribeOptions,
) -> anyhow::Result<()> {
    let room = state.lock().room.clone().context("not in a call")?;
    {
        let mut s = state.lock();
        anyhow::ensure!(
            s.transcribing.is_none() && !s.transcribe_starting,
            "you're already transcribing"
        );
        s.transcribe_starting = true;
    }
    if !options.handover
        && let Some(p) = room.remote_participants().values().find(|p| transcribes(p))
    {
        anyhow::bail!("{} is already transcribing this call", p.name());
    }
    let model = options.model;
    let stt = quipcall_stt::Options {
        model: models::path(model, &options.dir),
        vad: models::path(&catalog::VAD, &options.dir),
        language: options.language,
    };
    let you = room.local_participant().identity().to_string();
    // Each line shows here and goes to everyone, in order.
    let (outgoing, mut to_send) = tokio::sync::mpsc::unbounded_channel::<TranscriptLine>();
    let lines = events.clone();
    let by = you.clone();
    let on_line = move |line: quipcall_stt::Line| {
        let line = TranscriptLine {
            id: line.id,
            speaker: line.speaker,
            at_ms: line
                .at
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            text: line.text,
            done: line.done,
        };
        let _ = outgoing.send(line.clone());
        let _ = lines.try_send(Event::TranscriptLine {
            by: by.clone(),
            line,
        });
    };
    {
        let (room, state) = (room.clone(), state.clone());
        handle.spawn(async move {
            while let Some(line) = to_send.recv().await {
                remember(&mut state.lock().transcribed, &line);
                let message = TranscriptMessage::Lines { lines: vec![line] };
                if let Err(e) = send_transcript(&room, &message, None).await {
                    tracing::warn!("sending a transcript line: {e}");
                }
            }
        });
    }
    let transcriber =
        tokio::task::spawn_blocking(move || quipcall_stt::Transcriber::start(stt, on_line))
            .await??;
    // Everyone sees it before a single word is heard. Later than every
    // claim you know of, so taking over wins even if your clock is behind
    // theirs.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let since = now.max(newest_claim(&room).map_or(0, |(since, _)| since + 1));
    state.lock().transcribing_since = since;
    if let Err(e) = set_attribute(&room, TRANSCRIBING, &format!("{}@{since}", model.id)).await {
        let _ = tokio::task::spawn_blocking(move || transcriber.stop()).await;
        return Err(e.context("couldn't let the others know, so nothing is transcribed"));
    }
    let transcribing = transcribe::Transcribing::start(listen(state, handle), transcriber, you);
    // The call may have ended while the model loaded.
    let stale = {
        let mut s = state.lock();
        if s.room.as_ref().is_some_and(|r| Arc::ptr_eq(r, &room)) {
            s.transcribing = Some(transcribing);
            None
        } else {
            Some(transcribing)
        }
    };
    if let Some(transcribing) = stale {
        let _ = tokio::task::spawn_blocking(move || transcribing.stop()).await;
        anyhow::bail!("call ended");
    }
    Ok(())
}

/// A listener; remote microphones start decoding for it.
fn listen(state: &Arc<Mutex<State>>, handle: &tokio::runtime::Handle) -> Listener {
    let weak = Arc::downgrade(state);
    let listener = audio_tap::listen(move || {
        if let Some(state) = weak.upgrade() {
            let mut s = state.lock();
            // Someone may have started listening again meanwhile.
            if !audio_tap::listening() {
                for (_, task) in s.mic_streams.drain() {
                    task.abort();
                }
            }
        }
    });
    let mut s = state.lock();
    let s = &mut *s;
    for (key, track) in &s.mics {
        if !s.mic_streams.contains_key(key) {
            let task = handle.spawn(pump_mic(key.participant.clone(), track.clone()));
            s.mic_streams.insert(key.clone(), task);
        }
    }
    listener
}

/// One remote microphone, decoded at 48 kHz mono, to everyone listening.
async fn pump_mic(identity: String, track: RemoteAudioTrack) {
    use livekit::webrtc::audio_stream::native::{NativeAudioStream, NativeAudioStreamOptions};
    // Two seconds of queue: the decoder never waits for us.
    let mut stream = NativeAudioStream::with_options(
        track.rtc_track(),
        audio_tap::SAMPLE_RATE as i32,
        1,
        NativeAudioStreamOptions {
            queue_size_frames: Some(200),
        },
    );
    let speaker = Speaker::Remote(identity);
    while let Some(frame) = stream.next().await {
        let samples = frame.data.iter().map(|&s| s as f32 / 32768.0).collect();
        audio_tap::send(
            VoiceFrame {
                speaker: speaker.clone(),
                samples,
            },
            true,
        );
    }
}

async fn start_recording(
    state: &Arc<Mutex<State>>,
    handle: &tokio::runtime::Handle,
    events: &async_channel::Sender<Event>,
    path: PathBuf,
    screen: Option<(SourceId, ScreenSharePreset)>,
    each_voice: bool,
) -> anyhow::Result<PathBuf> {
    let room = state.lock().room.clone().context("not in a call")?;
    anyhow::ensure!(state.lock().recorder.is_none(), "you're already recording");
    // "Design review 2026-11-10 10.02 voices/Anna Petrova.m4a".
    let voices = each_voice.then(|| {
        let stem = path.file_stem().map_or_else(
            || "Recording".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        let weak = Arc::downgrade(&room);
        let name_of: record::NameOf = Arc::new(move |speaker: &Speaker| {
            let Some(room) = weak.upgrade() else {
                return "Someone".to_owned();
            };
            match speaker {
                Speaker::Local => room.local_participant().name(),
                Speaker::Remote(identity) => room
                    .remote_participants()
                    .get(&ParticipantIdentity(identity.clone()))
                    .map(|p| p.name())
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| identity.clone()),
            }
        });
        record::Voices {
            dir: path.with_file_name(format!("{stem} voices")),
            name_of,
        }
    });
    if let Some(p) = room
        .remote_participants()
        .values()
        .find(|p| p.attributes().get(RECORDING).is_some_and(|v| !v.is_empty()))
    {
        anyhow::bail!("{} is already recording this call", p.name());
    }
    // Everyone sees it before a single frame is kept.
    let kind = if screen.is_some() { "screen" } else { "audio" };
    set_attribute(&room, RECORDING, kind)
        .await
        .context("couldn't let the others know, so nothing is recorded")?;
    let screen = match capture_for_recording(screen).await {
        Ok(screen) => screen,
        Err(e) => {
            let _ = set_attribute(&room, RECORDING, "").await;
            return Err(e);
        }
    };
    let listener = listen(state, handle);
    // Writing can fail later (the encoder gave up): say so at once, and
    // stop telling everyone that you record.
    let on_failed = {
        let (state, events, handle) = (Arc::downgrade(state), events.clone(), handle.clone());
        move || {
            handle.spawn(async move {
                let Some(state) = state.upgrade() else { return };
                let (room, recorder) = {
                    let mut s = state.lock();
                    (s.room.clone(), s.recorder.take())
                };
                let Some(recorder) = recorder else { return };
                let result = tokio::task::spawn_blocking(move || recorder.stop()).await;
                if let Some(room) = room {
                    let _ = set_attribute(&room, RECORDING, "").await;
                }
                let message = match result {
                    Ok(Err(e)) => format!("{e:#}"),
                    _ => "the recording stopped".into(),
                };
                let _ = events.send(Event::RecordingFailed(message)).await;
            });
        }
    };
    let started = tokio::task::spawn_blocking(move || {
        record::Recorder::start(listener, path, screen, voices, on_failed)
    })
    .await;
    let recorder = match started {
        Ok(Ok(recorder)) => recorder,
        Ok(Err(e)) => {
            let _ = set_attribute(&room, RECORDING, "").await;
            return Err(e);
        }
        Err(e) => {
            let _ = set_attribute(&room, RECORDING, "").await;
            return Err(e.into());
        }
    };
    let path = recorder.path().to_owned();
    // The call may have ended while the file was being created.
    let stale = {
        let mut s = state.lock();
        if s.room.as_ref().is_some_and(|r| Arc::ptr_eq(r, &room)) {
            s.recorder = Some(recorder);
            None
        } else {
            Some(recorder)
        }
    };
    if let Some(recorder) = stale {
        let _ = tokio::task::spawn_blocking(move || recorder.stop()).await;
        anyhow::bail!("call ended");
    }
    Ok(path)
}

/// Tell everyone (or stop telling them) that you record or transcribe.
/// LiveKit deletes an attribute set to "".
async fn set_attribute(room: &Room, key: &str, value: &str) -> anyhow::Result<()> {
    room.local_participant()
        .set_attributes(HashMap::from([(key.to_owned(), value.to_owned())]))
        .await?;
    Ok(())
}

/// Whether `participant`'s Mac transcribes the call.
fn transcribes(participant: &RemoteParticipant) -> bool {
    participant
        .attributes()
        .get(TRANSCRIBING)
        .is_some_and(|v| !v.is_empty())
}

/// A [`TRANSCRIBING`] value: the model's id, and when it started.
fn parse_transcribing(value: &str) -> (&str, u64) {
    match value.split_once('@') {
        Some((model, since)) => (model, since.parse().unwrap_or(0)),
        None => (value, 0),
    }
}

/// The newest [`TRANSCRIBING`] claim among the others: when, and whose.
fn newest_claim(room: &Room) -> Option<(u64, String)> {
    room.remote_participants()
        .values()
        .filter_map(|p| {
            let attributes = p.attributes();
            let value = attributes.get(TRANSCRIBING).filter(|v| !v.is_empty())?;
            Some((parse_transcribing(value).1, p.identity().to_string()))
        })
        .max()
}

/// A final line, kept for whoever joins later: once, however it came.
fn remember(lines: &mut Vec<TranscriptLine>, line: &TranscriptLine) {
    let same =
        |l: &&TranscriptLine| l.id == line.id && l.speaker == line.speaker && l.at_ms == line.at_ms;
    if line.done && !line.text.is_empty() && !lines.iter().rev().any(|l| same(&l)) {
        lines.push(line.clone());
    }
}

/// Someone else started transcribing while your Mac does. The one who
/// started last carries on: a handover, or two people pressing Transcribe at
/// once (both Macs agree on who stops).
fn yields_to(mine: (u64, &str), theirs: (u64, &str)) -> bool {
    theirs > mine
}

/// What a participant's attributes say they do, for everyone's UI. `changed`
/// limits it to those attributes (an update); `None` reports all (they
/// joined).
fn attribute_events(
    identity: &str,
    attributes: &HashMap<String, String>,
    changed: Option<&HashMap<String, String>>,
) -> Vec<Event> {
    let value = |key: &str| -> Option<&str> {
        match changed {
            Some(changed) => changed
                .contains_key(key)
                .then(|| changed.get(key).map_or("", String::as_str)),
            None => attributes
                .get(key)
                .map(String::as_str)
                .filter(|v| !v.is_empty()),
        }
    };
    let mut events = Vec::new();
    if let Some(v) = value(RECORDING) {
        events.push(Event::ParticipantRecording {
            identity: identity.to_owned(),
            recording: !v.is_empty(),
            screen: v == "screen",
        });
    }
    if let Some(v) = value(TRANSCRIBING) {
        events.push(Event::ParticipantTranscribing {
            identity: identity.to_owned(),
            model: (!v.is_empty()).then(|| parse_transcribing(v).0.to_owned()),
        });
    }
    if let Some(v) = value(CAN_TRANSCRIBE) {
        events.push(Event::ParticipantCanTranscribe {
            identity: identity.to_owned(),
            model: (!v.is_empty()).then(|| v.to_owned()),
        });
    }
    events
}

/// Your screen or a window for a recording, captured on its own.
#[cfg(target_os = "macos")]
async fn capture_for_recording(
    screen: Option<(SourceId, ScreenSharePreset)>,
) -> anyhow::Result<Option<record::ScreenInput>> {
    let Some((source, preset)) = screen else {
        return Ok(None);
    };
    let (tx, frames) = std::sync::mpsc::sync_channel(8);
    let sink = Arc::new(record::FrameQueue(tx));
    let on_stop = move || tracing::info!(?source, "the recorded screen or window went away");
    let (capture, (width, height)) = screen_mac::record(source, preset, sink, on_stop)
        .await
        .context("screen capture")?;
    Ok(Some(record::ScreenInput {
        frames,
        format: record::VideoFormat {
            width,
            height,
            fps: preset.fps,
            bitrate: preset.recording_bitrate(),
        },
        capture,
    }))
}

#[cfg(not(target_os = "macos"))]
async fn capture_for_recording(
    screen: Option<(SourceId, ScreenSharePreset)>,
) -> anyhow::Result<Option<record::ScreenInput>> {
    anyhow::ensure!(screen.is_none(), "recording the screen needs macOS for now");
    Ok(None)
}

/// Runs `f` inside an Objective-C autorelease pool on Apple platforms.
///
/// libwebrtc's Objective-C frame wrappers are autoreleased. Our own threads
/// (tokio workers, the synthetic camera) have no pool that ever drains, so
/// without this every video frame, and the IOSurface behind it, leaks.
pub(crate) fn autorelease_pool<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "macos")]
    {
        objc2::rc::autoreleasepool(|_| f())
    }
    #[cfg(not(target_os = "macos"))]
    {
        f()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.disconnect();
        // Dropping the runtime would cancel the goodbye mid-way. Let it
        // finish on a thread of its own, then drop the runtime there.
        let closed = self.closed();
        if let Some(rt) = self.rt.take() {
            let spawned = std::thread::Builder::new()
                .name("quipcall-close".into())
                .spawn(move || {
                    let closed =
                        rt.block_on(async { tokio::time::timeout(CLOSE_TIMEOUT, closed).await });
                    if closed.is_err() {
                        tracing::warn!("leaving the room timed out");
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!("leave the room: {e}");
            }
        }
    }
}

async fn stop_screen_share(state: Arc<Mutex<State>>, events: async_channel::Sender<Event>) {
    let (room, share) = {
        let mut s = state.lock();
        (s.room.clone(), s.screen.take())
    };
    if let (Some(room), Some(share)) = (room, share) {
        share.unpublish(&room).await;
    }
    let _ = events
        .send(Event::ScreenShareChanged {
            active: false,
            preset: None,
        })
        .await;
}

async fn connect(
    state: Arc<Mutex<State>>,
    events: async_channel::Sender<Event>,
    join: JoinInfo,
) -> anyhow::Result<()> {
    // QUIPCALL_NO_MIC=1 skips the audio device entirely (headless tests, CI).
    let use_audio = std::env::var("QUIPCALL_NO_MIC").is_err();

    // Ask before anything opens the mic: an unanswered prompt would block
    // WebRTC's audio thread. Without access we still play remote audio.
    let mic_allowed = use_audio && {
        if permissions::status(MediaDevice::Microphone) == Access::Undecided {
            events
                .send(Event::WaitingForPermission(MediaDevice::Microphone))
                .await?;
        }
        permissions::request(MediaDevice::Microphone).await
    };
    if use_audio && !mic_allowed {
        events
            .send(Event::Error(
                "microphone access is off, so others can't hear you. Allow it in System \
                 Settings → Privacy & Security → Microphone, then rejoin"
                    .into(),
            ))
            .await?;
    }

    // Load the noise model while the room connects; the microphone track
    // must wait for it, since it fixes WebRTC's own suppression on creation.
    let denoise = mic_allowed.then(|| tokio::task::spawn_blocking(denoise::prepare));

    // The platform ADM has to exist before the peer connections are created.
    let audio = if use_audio {
        let audio = PlatformAudio::new().context("audio device")?;
        audio.configure_audio_processing(AudioProcessingOptions {
            echo_cancellation: true,
            noise_suppression: true,
            auto_gain_control: true,
            prefer_hardware_processing: true,
        })?;
        Some(audio)
    } else {
        tracing::warn!("QUIPCALL_NO_MIC set: no microphone, no speaker");
        None
    };

    let mut options = RoomOptions::default();
    options.auto_subscribe = true;
    // Adaptive stream in the Rust SDK only makes the server start at the lowest
    // layer; it never reports element sizes. We pick layers per tile instead.
    options.adaptive_stream = false;
    options.dynacast = true;
    let (room, mut room_events) = Room::connect(&join.url, &join.token, options).await?;
    let room = Arc::new(room);
    tracing::info!(
        room = %room.name(),
        identity = %room.local_participant().identity(),
        "connected"
    );

    let can_transcribe = {
        let mut s = state.lock();
        s.room = Some(room.clone());
        s.audio = audio.clone();
        s.can_transcribe.clone()
    };
    // Others can ask this Mac to transcribe.
    if let Some(model) = can_transcribe {
        let room = room.clone();
        tokio::spawn(async move {
            if let Err(e) = set_attribute(&room, CAN_TRANSCRIBE, &model).await {
                tracing::warn!("can-transcribe attribute: {e}");
            }
        });
    }
    let identity = room.local_participant().identity().to_string();
    events
        .send(Event::Connected {
            room: room.name(),
            identity,
        })
        .await?;
    for p in room.remote_participants().values() {
        events
            .send(Event::ParticipantJoined {
                identity: p.identity().to_string(),
                name: p.name(),
            })
            .await?;
        for event in attribute_events(&p.identity().to_string(), &p.attributes(), None) {
            events.send(event).await?;
        }
    }

    // Publishing opens the mic and can take a moment; don't hold up room events.
    if let Some(audio) = audio.filter(|_| mic_allowed) {
        let (room, state, events) = (room.clone(), state.clone(), events.clone());
        tokio::spawn(async move {
            if let Some(denoise) = denoise
                && let Err(e) = denoise.await
            {
                tracing::error!("noise suppression: {e}");
            }
            let mic = LocalAudioTrack::create_audio_track("microphone", audio.rtc_source());
            let options = TrackPublishOptions {
                source: TrackSource::Microphone,
                dtx: true,
                red: true,
                ..Default::default()
            };
            match room
                .local_participant()
                .publish_track(LocalTrack::Audio(mic.clone()), options)
                .await
            {
                Ok(_) => {
                    state.lock().mic = Some(mic);
                    audio_tap::set_mic_live(true);
                    let _ = events.send(Event::MicChanged { enabled: true }).await;
                }
                Err(e) => {
                    let _ = events.send(Event::Error(format!("microphone: {e}"))).await;
                }
            }
        });
    }

    while let Some(event) = room_events.recv().await {
        if let RoomEvent::TrackMuted {
            participant,
            publication,
        }
        | RoomEvent::TrackUnmuted {
            participant,
            publication,
        } = &event
        {
            tracing::debug!(
                participant = %participant.identity(),
                track = %publication.sid(),
                muted = publication.is_muted(),
                "track mute changed"
            );
        }
        track_publications(&state, &event);
        // Someone joining mid-recording, or starting or stopping.
        let attributes = match &event {
            RoomEvent::ParticipantConnected(p) => {
                attribute_events(&p.identity().to_string(), &p.attributes(), None)
            }
            RoomEvent::ParticipantAttributesChanged {
                participant: Participant::Remote(p),
                changed_attributes,
            } => attribute_events(
                &p.identity().to_string(),
                &p.attributes(),
                Some(changed_attributes),
            ),
            _ => Vec::new(),
        };
        match &event {
            RoomEvent::DataReceived {
                payload,
                topic: Some(topic),
                participant: Some(from),
                ..
            } if topic == TRANSCRIPT_TOPIC => {
                on_transcript(&state, &events, from, payload).await;
            }
            // Someone else started transcribing while you do: the newest
            // carries on.
            RoomEvent::ParticipantAttributesChanged {
                participant: Participant::Remote(p),
                changed_attributes,
            } if changed_attributes
                .get(TRANSCRIBING)
                .is_some_and(|v| !v.is_empty()) =>
            {
                let (_, since) = parse_transcribing(&changed_attributes[TRANSCRIBING]);
                let identity = p.identity().to_string();
                let you = room.local_participant().identity().to_string();
                let yields = {
                    let s = state.lock();
                    s.transcribing.is_some()
                        && yields_to((s.transcribing_since, &you), (since, &identity))
                };
                if yields {
                    tracing::info!(%identity, "their Mac takes over the transcript");
                    let (state, events) = (state.clone(), events.clone());
                    tokio::spawn(async move {
                        stop_transcribing_for(&state, &events, Some(identity), true).await
                    });
                }
            }
            // Someone joining while your Mac transcribes gets what was said.
            RoomEvent::ParticipantConnected(p) => {
                let history = {
                    let s = state.lock();
                    s.transcribing.as_ref().map(|_| s.transcribed.clone())
                };
                if let Some(lines) = history.filter(|l| !l.is_empty()) {
                    let to = p.identity().to_string();
                    tokio::spawn({
                        let room = room.clone();
                        async move { send_history(&room, lines, to).await }
                    });
                }
            }
            _ => {}
        }
        if let Some(event) = translate(event) {
            let disconnected = matches!(event, Event::Disconnected { .. });
            if events.send(event).await.is_err() || disconnected {
                break;
            }
        }
        for event in attributes {
            let _ = events.send(event).await;
        }
    }
    // The call ended without you leaving (the server let you go, or the
    // connection gave up): nobody sees you record any more, so stop, and
    // keep what was recorded, as leaving would.
    let (recorder, transcribing) = {
        let mut s = state.lock();
        if s.room.as_ref().is_some_and(|r| Arc::ptr_eq(r, &room)) {
            (s.recorder.take(), s.transcribing.take())
        } else {
            (None, None)
        }
    };
    if let Some(transcribing) = transcribing {
        let _ = tokio::task::spawn_blocking(move || transcribing.stop()).await;
        let _ = events
            .send(Event::TranscriptionStopped {
                by: None,
                handed_over: false,
            })
            .await;
    }
    if let Some(recorder) = recorder {
        let event = match tokio::task::spawn_blocking(move || recorder.stop()).await {
            Ok(Ok(saved)) => Event::RecordingSaved(saved),
            Ok(Err(e)) => Event::RecordingFailed(format!("{e:#}")),
            Err(e) => Event::RecordingFailed(format!("{e}")),
        };
        let _ = events.send(event).await;
    }
    Ok(())
}

fn track_publications(state: &Mutex<State>, event: &RoomEvent) {
    match event {
        RoomEvent::TrackSubscribed {
            track: RemoteTrack::Video(track),
            publication,
            participant,
        } => {
            let key = TrackKey {
                participant: participant.identity().to_string(),
                sid: track.sid().to_string(),
            };
            state.lock().videos.insert(key, publication.clone());
        }
        RoomEvent::TrackUnsubscribed {
            track: RemoteTrack::Video(track),
            participant,
            ..
        } => {
            let key = TrackKey {
                participant: participant.identity().to_string(),
                sid: track.sid().to_string(),
            };
            state.lock().videos.remove(&key);
        }
        RoomEvent::TrackSubscribed {
            track: RemoteTrack::Audio(track),
            publication,
            participant,
        } if publication.source() == TrackSource::Microphone => {
            let key = TrackKey {
                participant: participant.identity().to_string(),
                sid: track.sid().to_string(),
            };
            let mut s = state.lock();
            if audio_tap::listening() && !s.mic_streams.contains_key(&key) {
                let task = tokio::spawn(pump_mic(key.participant.clone(), track.clone()));
                s.mic_streams.insert(key.clone(), task);
            }
            s.mics.insert(key, track.clone());
        }
        RoomEvent::TrackUnsubscribed {
            track: RemoteTrack::Audio(track),
            participant,
            ..
        } => {
            let key = TrackKey {
                participant: participant.identity().to_string(),
                sid: track.sid().to_string(),
            };
            let mut s = state.lock();
            s.mics.remove(&key);
            if let Some(task) = s.mic_streams.remove(&key) {
                task.abort();
            }
        }
        RoomEvent::Disconnected { .. } => {
            let mut s = state.lock();
            s.videos.clear();
            s.mics.clear();
            for (_, task) in s.mic_streams.drain() {
                task.abort();
            }
        }
        _ => {}
    }
}

fn translate(event: RoomEvent) -> Option<Event> {
    Some(match event {
        RoomEvent::ParticipantConnected(p) => Event::ParticipantJoined {
            identity: p.identity().to_string(),
            name: p.name(),
        },
        RoomEvent::ParticipantDisconnected(p) => Event::ParticipantLeft {
            identity: p.identity().to_string(),
        },
        RoomEvent::TrackSubscribed {
            track: RemoteTrack::Video(track),
            publication,
            participant,
        } => {
            let kind = match publication.source() {
                TrackSource::Screenshare => VideoKind::Screen,
                _ => VideoKind::Camera,
            };
            let key = TrackKey {
                participant: participant.identity().to_string(),
                sid: track.sid().to_string(),
            };
            Event::VideoTrackAdded { key, kind, track }
        }
        RoomEvent::TrackUnsubscribed {
            track: RemoteTrack::Video(track),
            participant,
            ..
        } => {
            let key = TrackKey {
                participant: participant.identity().to_string(),
                sid: track.sid().to_string(),
            };
            Event::VideoTrackRemoved { key }
        }
        RoomEvent::TrackSubscribed {
            track: RemoteTrack::Audio(_),
            publication,
            participant,
        } if publication.source() == TrackSource::Microphone => Event::ParticipantMic {
            identity: participant.identity().to_string(),
            muted: publication.is_muted(),
        },
        RoomEvent::TrackMuted {
            participant: Participant::Remote(participant),
            publication,
        } if publication.source() == TrackSource::Microphone => Event::ParticipantMic {
            identity: participant.identity().to_string(),
            muted: true,
        },
        RoomEvent::TrackUnmuted {
            participant: Participant::Remote(participant),
            publication,
        } if publication.source() == TrackSource::Microphone => Event::ParticipantMic {
            identity: participant.identity().to_string(),
            muted: false,
        },
        RoomEvent::TrackUnpublished {
            publication,
            participant,
        } if publication.source() == TrackSource::Microphone => Event::ParticipantMic {
            identity: participant.identity().to_string(),
            muted: true,
        },
        RoomEvent::ActiveSpeakersChanged { speakers } => {
            Event::ActiveSpeakers(speakers.iter().map(|p| p.identity().to_string()).collect())
        }
        RoomEvent::Disconnected { reason } => Event::Disconnected {
            reason: format!("{reason:?}"),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_transcriber_carries_on() {
        assert_eq!(parse_transcribing("parakeet@1700"), ("parakeet", 1700));
        assert_eq!(parse_transcribing("parakeet"), ("parakeet", 0));
        // A handover: they started after you.
        assert!(yields_to((1000, "you"), (2000, "anna")));
        assert!(!yields_to((2000, "you"), (1000, "anna")));
        // At the same moment, both Macs agree on one.
        assert!(
            yields_to((1000, "anna"), (1000, "boris"))
                != yields_to((1000, "boris"), (1000, "anna"))
        );
    }

    /// The history someone joining gets: final lines, each once, however
    /// they came (yours, a transcriber's before a handover, its history).
    #[test]
    fn history_keeps_each_final_line_once() {
        let line = |id, text: &str, done| TranscriptLine {
            id,
            speaker: "anna".into(),
            at_ms: 1000 + id,
            text: text.into(),
            done,
        };
        let mut kept = Vec::new();
        remember(&mut kept, &line(1, "Hel", false));
        remember(&mut kept, &line(1, "Hello", true));
        remember(&mut kept, &line(1, "Hello", true));
        remember(&mut kept, &line(2, "", true));
        remember(&mut kept, &line(3, "Bye", true));
        let texts: Vec<&str> = kept.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["Hello", "Bye"]);
    }

    /// Goodbyes already said don't pile up, even if nothing awaited them.
    #[test]
    fn finished_goodbyes_are_forgotten() {
        let (engine, _events) = Engine::new().unwrap();
        engine.disconnect();
        // Wait for the goodbye task itself; awaiting `closed()` would hide
        // the bug this is about.
        let metrics = engine.rt().metrics();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while metrics.num_alive_tasks() > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "goodbye never finished"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        engine.disconnect();
        assert_eq!(engine.state.lock().closing.len(), 1);
    }
}
