//! Quipcall call engine.
//!
//! Owns everything about being in a call: the LiveKit room, platform audio,
//! published tracks and remote video plumbing. It has no UI dependency — a UI
//! drives it through [`Engine`] methods and consumes [`Event`]s.
//!
//! LiveKit needs tokio; the engine owns a small runtime so the UI can use any
//! executor it likes.

mod camera;
pub mod permissions;
mod screen;
pub mod video;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::{AudioProcessingOptions, PlatformAudio};
use parking_lot::Mutex;
use quipcall_protocol::{ApiError, JoinInfo};

pub use livekit;
pub use livekit::track::VideoQuality;
pub use permissions::{Access, MediaDevice};
pub use quipcall_protocol as protocol;
pub use screen::{ContentMode, ScreenSharePreset};
pub use video::{FrameSink, VideoSubscription};

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
    Error(String),
}

pub struct Engine {
    rt: tokio::runtime::Runtime,
    state: Arc<Mutex<State>>,
    events: async_channel::Sender<Event>,
}

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
}

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
                rt,
                state: Default::default(),
                events,
            },
            rx,
        ))
    }

    /// Fetch a join token from the dev endpoint of a quipcall server.
    pub fn dev_join(
        &self,
        server: &str,
        room: &str,
        identity: &str,
        name: &str,
    ) -> impl Future<Output = anyhow::Result<JoinInfo>> + Send + 'static {
        let url = format!("{}/api/dev/token", server.trim_end_matches('/'));
        let query = [
            ("room", room.to_owned()),
            ("identity", identity.to_owned()),
            ("name", name.to_owned()),
        ];
        let handle = self.rt.handle().clone();
        async move {
            handle
                .spawn(async move {
                    let resp = reqwest::Client::new()
                        .get(&url)
                        .query(&query)
                        .send()
                        .await?;
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
        self.rt.spawn(async move {
            if let Err(e) = connect(state, events.clone(), join).await {
                let _ = events
                    .send(Event::Error(format!("connect failed: {e:#}")))
                    .await;
            }
        });
    }

    pub fn disconnect(&self) {
        let (room, screen, camera) = {
            let mut s = self.state.lock();
            s.mic = None;
            (s.room.take(), s.screen.take(), s.camera.take())
        };
        if let Some(screen) = screen {
            screen.stop();
        }
        let state = self.state.clone();
        self.rt.spawn(async move {
            // Stopping the camera blocks briefly; keep it off the UI thread.
            if let Some(camera) = camera {
                let _ = tokio::task::spawn_blocking(move || drop(camera)).await;
            }
            if let Some(room) = room {
                let _ = room.close().await;
            }
            // Release the audio device only after the room is gone.
            state.lock().audio = None;
        });
    }

    pub fn set_mic_enabled(&self, enabled: bool) {
        // LiveKit spawns tokio tasks internally; UI threads have no runtime.
        let _rt = self.rt.enter();
        let s = self.state.lock();
        if let Some(mic) = &s.mic {
            if enabled {
                mic.unmute()
            } else {
                mic.mute()
            }
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
        self.rt.spawn(async move {
            let result = async {
                if permissions::status(MediaDevice::Camera) == Access::Undecided {
                    let _ = events
                        .send(Event::WaitingForPermission(MediaDevice::Camera))
                        .await;
                }
                anyhow::ensure!(
                    permissions::request(MediaDevice::Camera).await,
                    "camera access is off. Allow it in System Settings → Privacy & Security → Camera"
                );
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
        self.rt.spawn(async move {
            if let Some(camera) = camera {
                match room {
                    Some(room) => camera.stop(&room).await,
                    None => drop(camera),
                }
            }
            let _ = events.send(Event::CameraChanged { enabled: false }).await;
        });
    }

    pub fn start_screen_share(&self, preset: ScreenSharePreset) {
        let state = self.state.clone();
        let events = self.events.clone();
        let handle = self.rt.handle().clone();
        self.rt.spawn(async move {
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
                let share =
                    screen::ScreenShare::start(&room, preset, events.clone(), on_fail).await?;
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
        self.rt
            .spawn(stop_screen_share(self.state.clone(), self.events.clone()));
    }

    /// Pick the simulcast layer for a remote video. The UI calls this from the
    /// tile's on-screen size; LiveKit's Rust SDK doesn't do adaptive sizing itself.
    pub fn set_video_quality(&self, key: &TrackKey, quality: VideoQuality) {
        let _rt = self.rt.enter();
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
        video::attach(self.rt.handle(), track, sink)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.disconnect();
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
    tracing::info!(room = %room.name(), "connected");

    {
        let mut s = state.lock();
        s.room = Some(room.clone());
        s.audio = audio.clone();
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
    }

    // Publishing opens the mic and can take a moment; don't hold up room events.
    if let Some(audio) = audio.filter(|_| mic_allowed) {
        let (room, state, events) = (room.clone(), state.clone(), events.clone());
        tokio::spawn(async move {
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
                    let _ = events.send(Event::MicChanged { enabled: true }).await;
                }
                Err(e) => {
                    let _ = events.send(Event::Error(format!("microphone: {e}"))).await;
                }
            }
        });
    }

    while let Some(event) = room_events.recv().await {
        track_publications(&state, &event);
        if let Some(event) = translate(event) {
            let disconnected = matches!(event, Event::Disconnected { .. });
            if events.send(event).await.is_err() || disconnected {
                break;
            }
        }
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
        RoomEvent::Disconnected { .. } => state.lock().videos.clear(),
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
        RoomEvent::ActiveSpeakersChanged { speakers } => {
            Event::ActiveSpeakers(speakers.iter().map(|p| p.identity().to_string()).collect())
        }
        RoomEvent::Disconnected { reason } => Event::Disconnected {
            reason: format!("{reason:?}"),
        },
        _ => return None,
    })
}
