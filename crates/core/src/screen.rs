//! Screen sharing.
//!
//! Spike implementation: libwebrtc's `DesktopCapturer` (ScreenCaptureKit + the
//! system picker on macOS, PipeWire on Linux, WGC on Windows). We drive the
//! capture clock ourselves, which is what gives us a selectable frame rate.
//!
//! Known cost: frames arrive as CPU BGRA and are converted to I420 here. The
//! planned direct-ScreenCaptureKit path hands `CVPixelBuffer`s to VideoToolbox
//! without touching the CPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use livekit::options::{DegradationPreference, TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::*;
use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions, DesktopFrame,
};
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::{
    I420Buffer, RtcVideoSource, VideoFrame, VideoResolution, VideoRotation,
};
use livekit::webrtc::video_source::native::NativeVideoSource;

use crate::Event;

/// What the shared content looks like; drives codec and degradation choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentMode {
    /// Documents, code, slides: keep resolution sharp, drop frames under pressure.
    Text,
    /// Video, animations, scrolling demos: keep frame rate, drop resolution.
    Motion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSharePreset {
    pub fps: u32,
    pub mode: ContentMode,
}

impl ScreenSharePreset {
    pub const PRESETS: [ScreenSharePreset; 4] = [
        ScreenSharePreset {
            fps: 5,
            mode: ContentMode::Text,
        },
        ScreenSharePreset {
            fps: 15,
            mode: ContentMode::Text,
        },
        ScreenSharePreset {
            fps: 30,
            mode: ContentMode::Motion,
        },
        ScreenSharePreset {
            fps: 60,
            mode: ContentMode::Motion,
        },
    ];

    pub fn label(&self) -> String {
        let mode = match self.mode {
            ContentMode::Text => "text",
            ContentMode::Motion => "motion",
        };
        format!("{} fps · {mode}", self.fps)
    }

    /// Longest edge we encode. Retina captures are 3000+ px wide; encoding
    /// that is wasted CPU and bandwidth for a call.
    fn max_width(&self) -> u32 {
        match self.mode {
            ContentMode::Text => 2560,
            ContentMode::Motion => 1920,
        }
    }

    fn encoding(&self) -> VideoEncoding {
        let max_bitrate = match (self.mode, self.fps) {
            (ContentMode::Text, f) if f <= 5 => 1_200_000,
            (ContentMode::Text, _) => 2_500_000,
            (ContentMode::Motion, f) if f <= 30 => 4_000_000,
            (ContentMode::Motion, _) => 6_000_000,
        };
        VideoEncoding {
            max_bitrate,
            max_framerate: self.fps as f64,
        }
    }

    fn publish_options(&self) -> TrackPublishOptions {
        let (codec, degradation) = match self.mode {
            // VP9 has screen-content tools; software encode is fine at low fps.
            ContentMode::Text => (VideoCodec::VP9, DegradationPreference::MaintainResolution),
            // H.264 goes to VideoToolbox: cheap at 60 fps.
            ContentMode::Motion => (VideoCodec::H264, DegradationPreference::MaintainFramerate),
        };
        TrackPublishOptions {
            source: TrackSource::Screenshare,
            video_codec: codec,
            video_encoding: Some(self.encoding()),
            simulcast: false,
            degradation_preference: Some(degradation),
            ..Default::default()
        }
    }
}

pub(crate) struct ScreenShare {
    stop: Arc<AtomicBool>,
    sid: TrackSid,
}

impl ScreenShare {
    /// `on_fail` runs (on the capture thread) if capturing stops on its own:
    /// picker cancelled, permission denied, window closed.
    pub(crate) async fn start(
        room: &Room,
        preset: ScreenSharePreset,
        events: async_channel::Sender<Event>,
        on_fail: impl FnOnce() + Send + 'static,
    ) -> anyhow::Result<Self> {
        let source = NativeVideoSource::new(
            VideoResolution {
                width: 1920,
                height: 1080,
            },
            true,
        );
        let track =
            LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(source.clone()));
        let stop = Arc::new(AtomicBool::new(false));

        spawn_capture_thread(source, preset, stop.clone(), events, on_fail)?;

        let publication = room
            .local_participant()
            .publish_track(LocalTrack::Video(track), preset.publish_options())
            .await;
        match publication {
            Ok(publication) => Ok(Self {
                stop,
                sid: publication.sid(),
            }),
            Err(e) => {
                stop.store(true, Ordering::Relaxed);
                Err(e).context("publish screen track")
            }
        }
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub(crate) async fn unpublish(self, room: &Room) {
        self.stop();
        if let Err(e) = room.local_participant().unpublish_track(&self.sid).await {
            tracing::warn!("unpublish screen track: {e}");
        }
    }
}

fn spawn_capture_thread(
    source: NativeVideoSource,
    preset: ScreenSharePreset,
    stop: Arc<AtomicBool>,
    events: async_channel::Sender<Event>,
    on_fail: impl FnOnce() + Send + 'static,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("screen-capture".into())
        .spawn(move || {
            #[cfg(target_os = "macos")]
            let kind = DesktopCaptureSourceType::Generic; // system picker: screen or window
            #[cfg(not(target_os = "macos"))]
            let kind = DesktopCaptureSourceType::Screen;

            let mut options = DesktopCapturerOptions::new(kind);
            options.set_include_cursor(true);
            let Some(mut capturer) = DesktopCapturer::new(options) else {
                let _ = events.send_blocking(Event::Error("screen capture unavailable".into()));
                on_fail();
                return;
            };

            let failed = Arc::new(AtomicBool::new(false));
            let mut frames = FramePump {
                source,
                max_width: preset.max_width(),
                started: Instant::now(),
                sent: 0,
            };
            capturer.start_capture(None, {
                let failed = failed.clone();
                move |result| match result {
                    Ok(frame) => frames.push(&frame),
                    Err(CaptureError::Temporary) => {} // e.g. picker still open
                    Err(CaptureError::Permanent) => failed.store(true, Ordering::Relaxed),
                }
            });

            let interval = Duration::from_secs_f64(1.0 / preset.fps.max(1) as f64);
            let mut next = Instant::now();
            while !stop.load(Ordering::Relaxed) && !failed.load(Ordering::Relaxed) {
                capturer.capture_frame();
                next += interval;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now; // fell behind; don't try to catch up with a burst
                }
            }

            if failed.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                tracing::info!("screen capture ended (picker cancelled or source closed)");
                on_fail();
            }
        })?;
    Ok(())
}

struct FramePump {
    source: NativeVideoSource,
    max_width: u32,
    started: Instant,
    sent: u64,
}

impl FramePump {
    fn push(&mut self, frame: &DesktopFrame) {
        let (width, height) = (frame.width() as u32, frame.height() as u32);
        if width < 2 || height < 2 {
            return;
        }
        // A fresh buffer per frame: the encoder keeps a reference to it.
        let mut i420 = I420Buffer::new(width, height);
        let (stride_y, stride_u, stride_v) = i420.strides();
        let (y, u, v) = i420.data_mut();
        // macOS delivers BGRA in memory, which libyuv calls ARGB.
        yuv_helper::argb_to_i420(
            frame.data(),
            frame.stride(),
            y,
            stride_y,
            u,
            stride_u,
            v,
            stride_v,
            width as i32,
            height as i32,
        );

        if width > self.max_width {
            let new_w = self.max_width & !1;
            let new_h = ((height as u64 * new_w as u64 / width as u64) as u32) & !1;
            i420 = i420.scale(new_w as i32, new_h as i32);
        }

        self.source.capture_frame(&VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: self.started.elapsed().as_micros() as i64,
            frame_metadata: None,
            buffer: i420,
        });
        self.sent += 1;
        if self.sent == 1 {
            tracing::info!(width, height, "first screen frame captured");
        }
    }
}
