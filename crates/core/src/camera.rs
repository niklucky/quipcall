//! Camera capture and publishing.
//!
//! macOS: AVFoundation delivers full-range NV12 `CVPixelBuffer`s, which go to
//! WebRTC as native buffers (VideoToolbox encodes them without a CPU copy) and,
//! unchanged, to the self-view sink (GPUI draws them directly).

use std::sync::Arc;

use anyhow::Context as _;
use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::*;
use livekit::webrtc::prelude::{RtcVideoSource, VideoResolution};
use livekit::webrtc::video_source::native::NativeVideoSource;

use crate::FrameSink;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

pub(crate) struct Camera {
    _capture: platform::Capture,
    sid: TrackSid,
}

impl Camera {
    pub(crate) async fn start(
        room: &Room,
        preview: Option<Arc<dyn FrameSink>>,
    ) -> anyhow::Result<Self> {
        let source = NativeVideoSource::new(
            VideoResolution {
                width: WIDTH,
                height: HEIGHT,
            },
            false,
        );
        let track =
            LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(source.clone()));

        // `startRunning` blocks until the device is up (a few hundred ms).
        let capture =
            tokio::task::spawn_blocking(move || platform::Capture::start(source, preview))
                .await
                .context("camera thread")??;

        let options = TrackPublishOptions {
            source: TrackSource::Camera,
            // VideoToolbox takes the CVPixelBuffers as they are; simulcast
            // gives 720p / 360p / 180p layers for the per-tile layer picker.
            video_codec: VideoCodec::H264,
            simulcast: true,
            video_encoding: Some(VideoEncoding {
                max_bitrate: 1_700_000,
                max_framerate: 30.0,
            }),
            ..Default::default()
        };
        let publication = room
            .local_participant()
            .publish_track(LocalTrack::Video(track), options)
            .await
            .context("publish camera track")?;
        Ok(Self {
            _capture: capture,
            sid: publication.sid(),
        })
    }

    /// Stops the device and removes the track from the room.
    pub(crate) async fn stop(self, room: &Room) {
        let Self { _capture, sid } = self;
        // Stopping AVFoundation blocks until capture has wound down.
        let _ = tokio::task::spawn_blocking(move || drop(_capture)).await;
        if let Err(e) = room.local_participant().unpublish_track(&sid).await {
            tracing::warn!("unpublish camera track: {e}");
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(crate) struct Capture;

    impl Capture {
        pub(crate) fn start(
            _source: NativeVideoSource,
            _preview: Option<Arc<dyn FrameSink>>,
        ) -> anyhow::Result<Self> {
            anyhow::bail!("camera capture is not implemented on this platform yet")
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
    use livekit::webrtc::prelude::{BoxVideoBuffer, VideoFrame, VideoRotation};
    use livekit::webrtc::video_frame::native::NativeBuffer;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread, DefinedClass, define_class, msg_send};
    use objc2_av_foundation::{
        AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput,
        AVCaptureSession, AVCaptureSessionPreset1280x720, AVCaptureVideoDataOutput,
        AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
    };
    use objc2_core_foundation::{CFRetained, CFString};
    use objc2_core_media::CMSampleBuffer;
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
        kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    };
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    use super::*;

    /// Called on the capture queue for every frame.
    struct Handler {
        source: NativeVideoSource,
        preview: Option<Arc<dyn FrameSink>>,
        started: Instant,
        frames: AtomicU64,
    }

    impl Handler {
        fn on_pixel_buffer(&self, pixel_buffer: CFRetained<CVPixelBuffer>) {
            if self.frames.fetch_add(1, Ordering::Relaxed) == 0 {
                tracing::info!(
                    width = CVPixelBufferGetWidth(&pixel_buffer),
                    height = CVPixelBufferGetHeight(&pixel_buffer),
                    "first camera frame"
                );
            }
            // SAFETY: we pass an owned (+1) reference; WebRTC's wrapper retains
            // it and releases ours.
            let native = unsafe {
                NativeBuffer::from_cv_pixel_buffer(
                    CFRetained::into_raw(pixel_buffer).as_ptr().cast(),
                )
            };
            let frame = VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us: self.started.elapsed().as_micros() as i64,
                frame_metadata: None,
                buffer: Box::new(native) as BoxVideoBuffer,
            };
            self.source.capture_frame(&frame);
            if let Some(preview) = &self.preview {
                preview.on_frame(&frame);
            }
        }
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; we don't implement Drop.
        #[unsafe(super(NSObject))]
        #[name = "QuipcallCameraDelegate"]
        #[ivars = Handler]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Delegate {
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            fn did_output(
                &self,
                _output: &AVCaptureOutput,
                sample_buffer: &CMSampleBuffer,
                _connection: &AVCaptureConnection,
            ) {
                // SAFETY: a valid sample buffer from AVFoundation.
                if let Some(pixel_buffer) = unsafe { sample_buffer.image_buffer() } {
                    self.ivars().on_pixel_buffer(pixel_buffer);
                }
            }
        }
    );

    impl Delegate {
        fn new(handler: Handler) -> Retained<Self> {
            let this = Self::alloc().set_ivars(handler);
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }
    }

    pub(crate) struct Capture {
        session: Retained<AVCaptureSession>,
        _delegate: Retained<Delegate>,
        _queue: DispatchRetained<DispatchQueue>,
    }

    // SAFETY: AVCaptureSession is documented as thread-safe for start/stop; we
    // only call `stopRunning` (on drop) besides setup.
    unsafe impl Send for Capture {}

    impl Capture {
        pub(crate) fn start(
            source: NativeVideoSource,
            preview: Option<Arc<dyn FrameSink>>,
        ) -> anyhow::Result<Self> {
            // SAFETY: plain AVFoundation setup calls with valid arguments. The
            // delegate and queue are kept alive by `Capture`.
            unsafe {
                let media = AVMediaTypeVideo.context("AVMediaTypeVideo")?;
                let device = AVCaptureDevice::defaultDeviceWithMediaType(media)
                    .context("no camera found")?;
                tracing::info!(camera = %device.localizedName(), "opening camera");
                let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                    .map_err(|e| anyhow::anyhow!("open camera: {}", e.localizedDescription()))?;

                let output = AVCaptureVideoDataOutput::new();
                // Full-range NV12: what GPUI's surface wants and what WebRTC's
                // own macOS capturer uses.
                let key: &CFString = kCVPixelBufferPixelFormatTypeKey;
                let key: &NSString = &*(key as *const CFString as *const NSString);
                let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
                let settings = NSDictionary::<NSString, AnyObject>::from_slices(
                    &[key],
                    &[format.as_ref() as &AnyObject],
                );
                output.setVideoSettings(Some(&settings));
                output.setAlwaysDiscardsLateVideoFrames(true);

                let queue = DispatchQueue::new("quipcall.camera", DispatchQueueAttr::SERIAL);
                let delegate = Delegate::new(Handler {
                    source,
                    preview,
                    started: Instant::now(),
                    frames: AtomicU64::new(0),
                });
                output.setSampleBufferDelegate_queue(
                    Some(ProtocolObject::from_ref(&*delegate)),
                    Some(&queue),
                );

                let session = AVCaptureSession::new();
                session.beginConfiguration();
                if session.canSetSessionPreset(AVCaptureSessionPreset1280x720) {
                    session.setSessionPreset(AVCaptureSessionPreset1280x720);
                }
                anyhow::ensure!(session.canAddInput(&input), "camera input rejected");
                session.addInput(&input);
                anyhow::ensure!(session.canAddOutput(&output), "camera output rejected");
                session.addOutput(&output);
                session.commitConfiguration();
                session.startRunning();
                anyhow::ensure!(session.isRunning(), "camera did not start");

                Ok(Self {
                    session,
                    _delegate: delegate,
                    _queue: queue,
                })
            }
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            // SAFETY: valid session. Blocks until capture has stopped, after
            // which the delegate gets no more callbacks.
            unsafe { self.session.stopRunning() };
        }
    }
}
