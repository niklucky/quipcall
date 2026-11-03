//! Screen sharing on macOS with ScreenCaptureKit.
//!
//! - [`list_sources`]: screens and windows, each with a small thumbnail, for
//!   our own share picker.
//! - [`Capture`]: frames of one screen or window arrive as full-range NV12
//!   IOSurface `CVPixelBuffer`s at the preset's frame rate and go to WebRTC
//!   as native buffers: VideoToolbox encodes them without a CPU copy, and
//!   ScreenCaptureKit does the scaling on the GPU.
//!
//! Needs the Screen Recording permission (see [`crate::permissions`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use livekit::webrtc::prelude::{BoxVideoBuffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_frame::native::NativeBuffer;
use livekit::webrtc::video_source::native::NativeVideoSource;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{CVPixelBuffer, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCRunningApplication, SCScreenshotManager, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamDelegate, SCStreamOutput, SCStreamOutputType,
};
use parking_lot::Mutex;

use crate::screen::{ScreenSharePreset, SourceId};

/// A preview image, drawable as is (`gpui::surface`).
#[derive(Clone)]
pub struct Thumbnail(pub CFRetained<CVPixelBuffer>);

// SAFETY: CVPixelBuffers are reference-counted CF objects, fine to move
// between threads; nobody writes to a thumbnail after capture.
unsafe impl Send for Thumbnail {}
unsafe impl Sync for Thumbnail {}

#[derive(Clone)]
pub struct ShareSource {
    pub id: SourceId,
    /// "Screen 1", or the window's title.
    pub title: String,
    /// The app that owns the window (windows only).
    pub app: Option<String>,
    pub thumbnail: Option<Thumbnail>,
}

/// Screen Recording permission is off (ScreenCaptureKit's "user declined").
#[derive(Debug)]
pub struct NoPermission;

impl std::fmt::Display for NoPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Screen Recording permission is off")
    }
}

impl std::error::Error for NoPermission {}

/// `SCStreamErrorUserDeclined`.
const USER_DECLINED: isize = -3801;

/// Moves an Objective-C object between threads. ScreenCaptureKit's content
/// objects are immutable snapshots.
struct Shared<T>(T);
// SAFETY: see above; used only for SCK snapshot objects and results.
unsafe impl<T> Send for Shared<T> {}

/// Apps whose on-screen "windows" are system chrome, not something to share.
const SYSTEM_APPS: &[&str] = &[
    "Window Server",
    "Dock",
    "Control Center",
    "Notification Center",
    "Wallpaper",
    "SystemUIServer",
];

fn describe(error: *mut NSError) -> String {
    // SAFETY: SCK passes a valid NSError or null.
    unsafe { error.as_ref() }
        .map(|e| e.localizedDescription().to_string())
        .unwrap_or_else(|| "unknown error".into())
}

async fn shareable_content() -> anyhow::Result<Retained<SCShareableContent>> {
    let (tx, rx) = async_channel::bounded(1);
    {
        let handler = RcBlock::new(
            move |content: *mut SCShareableContent, error: *mut NSError| {
                // SAFETY: SCK passes a valid object (or null) for the duration of the call.
                let result = match unsafe { Retained::retain(content) } {
                    Some(content) => Ok(Shared(content)),
                    // SAFETY: SCK passes a valid NSError or null.
                    None if unsafe { error.as_ref() }
                        .is_some_and(|e| e.code() == USER_DECLINED) =>
                    {
                        Err(None)
                    }
                    None => Err(Some(describe(error))),
                };
                let _ = tx.try_send(result);
            },
        );
        // SAFETY: the handler is 'static and only sends on a channel.
        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                true, true, &handler,
            )
        };
    }
    match rx.recv().await.context("screen capture didn't answer")? {
        Ok(Shared(content)) => Ok(content),
        Err(None) => Err(NoPermission.into()),
        Err(Some(e)) => anyhow::bail!("{e}"),
    }
}

/// Even pixel dimensions no wider than `max_width`, for content of `width` x
/// `height` points at `scale` pixels per point.
fn fit(width: f64, height: f64, scale: f64, max_width: u32) -> (usize, usize) {
    let (w, h) = (width * scale, height * scale);
    let k = (max_width as f64 / w).min(1.0);
    let even = |v: f64| ((v * k) as usize).max(2) & !1;
    (even(w), even(h))
}

fn thumbnail(
    filter: &SCContentFilter,
    max_width: u32,
) -> async_channel::Receiver<Option<Thumbnail>> {
    let (tx, rx) = async_channel::bounded(1);
    // SAFETY: plain configuration of fresh objects; the handler is 'static.
    unsafe {
        let rect = filter.contentRect();
        let (w, h) = fit(
            rect.size.width,
            rect.size.height,
            filter.pointPixelScale() as f64,
            max_width,
        );
        let config = SCStreamConfiguration::new();
        config.setWidth(w);
        config.setHeight(h);
        config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        config.setShowsCursor(false);
        let handler = RcBlock::new(move |sample: *mut CMSampleBuffer, _error: *mut NSError| {
            let thumb = sample
                .as_ref()
                .and_then(|s| s.image_buffer())
                .map(Thumbnail);
            let _ = tx.try_send(thumb);
        });
        SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
            filter,
            &config,
            Some(&handler),
        );
    }
    rx
}

fn own_app(content: &SCShareableContent) -> Option<Retained<SCRunningApplication>> {
    let pid = std::process::id() as i32;
    // SAFETY: reading an immutable snapshot.
    unsafe { content.applications() }
        .iter()
        .find(|app| unsafe { app.processID() } == pid)
}

/// Screens first, then windows (front to back), each with a thumbnail up to
/// `thumb_width` pixels wide.
pub async fn list_sources(thumb_width: u32) -> anyhow::Result<Vec<ShareSource>> {
    let content = shareable_content().await?;
    let pid = std::process::id() as i32;
    let mut sources = Vec::new();
    let mut thumbs = Vec::new();

    // SAFETY: reading immutable snapshots and creating filters from them.
    unsafe {
        let displays = content.displays();
        let many = displays.count() > 1;
        // Previews match what gets shared: our own windows left out.
        let ours: Vec<_> = own_app(&content).into_iter().collect();
        let ours = NSArray::from_retained_slice(&ours);
        for (ix, display) in displays.iter().enumerate() {
            let filter = SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &ours,
                &NSArray::new(),
            );
            thumbs.push(thumbnail(&filter, thumb_width));
            sources.push(ShareSource {
                id: SourceId::Display(display.displayID()),
                title: if many {
                    format!("Screen {}", ix + 1)
                } else {
                    "Entire screen".into()
                },
                app: None,
                thumbnail: None,
            });
        }

        for window in content.windows().iter() {
            let frame = window.frame();
            if window.windowLayer() != 0
                || !window.isOnScreen()
                || frame.size.width < 120.0
                || frame.size.height < 80.0
            {
                continue;
            }
            let Some(app) = window.owningApplication() else {
                continue;
            };
            let app_name = app.applicationName().to_string();
            if app.processID() == pid || SYSTEM_APPS.contains(&app_name.as_str()) {
                continue;
            }
            let title = window
                .title()
                .map(|t| t.to_string())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| app_name.clone());
            let filter = SCContentFilter::initWithDesktopIndependentWindow(
                SCContentFilter::alloc(),
                &window,
            );
            thumbs.push(thumbnail(&filter, thumb_width));
            sources.push(ShareSource {
                id: SourceId::Window(window.windowID()),
                title,
                app: Some(app_name),
                thumbnail: None,
            });
        }
    }

    // The screenshots were all requested above and run in parallel.
    for (source, thumb) in sources.iter_mut().zip(thumbs) {
        source.thumbnail = thumb.recv().await.ok().flatten();
    }
    Ok(sources)
}

/// Dev and tests: the largest window of this process, to exercise screen
/// sharing without capturing anything else on the screen.
#[doc(hidden)]
pub async fn own_window() -> anyhow::Result<Option<SourceId>> {
    let content = shareable_content().await?;
    let pid = std::process::id() as i32;
    // SAFETY: reading an immutable snapshot.
    let largest = unsafe {
        content
            .windows()
            .iter()
            .filter(|w| w.owningApplication().is_some_and(|a| a.processID() == pid))
            .max_by_key(|w| (w.frame().size.width * w.frame().size.height) as u64)
            .map(|w| SourceId::Window(w.windowID()))
    };
    Ok(largest)
}

// ---- capture ---------------------------------------------------------------

/// The most recent frame, so a static screen can be re-sent: encoders only
/// emit a keyframe for someone who just joined when they get a new frame, and
/// ScreenCaptureKit sends nothing while the screen doesn't change.
type LastFrame = Mutex<Option<(Thumbnail, Instant)>>;

struct Handler {
    source: NativeVideoSource,
    started: Instant,
    frames: AtomicU64,
    last: Arc<LastFrame>,
}

impl Handler {
    fn push(&self, pixel_buffer: CFRetained<CVPixelBuffer>) {
        *self.last.lock() = Some((Thumbnail(pixel_buffer.clone()), Instant::now()));
        if self.frames.fetch_add(1, Ordering::Relaxed) == 0 {
            tracing::info!(
                width = objc2_core_video::CVPixelBufferGetWidth(&pixel_buffer),
                height = objc2_core_video::CVPixelBufferGetHeight(&pixel_buffer),
                "first screen frame"
            );
        }
        send(&self.source, self.started, pixel_buffer);
    }
}

fn send(source: &NativeVideoSource, started: Instant, pixel_buffer: CFRetained<CVPixelBuffer>) {
    // SAFETY: we pass an owned (+1) reference; WebRTC's wrapper retains it and
    // releases ours.
    let native = unsafe {
        NativeBuffer::from_cv_pixel_buffer(CFRetained::into_raw(pixel_buffer).as_ptr().cast())
    };
    source.capture_frame(&VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us: started.elapsed().as_micros() as i64,
        frame_metadata: None,
        buffer: Box::new(native) as BoxVideoBuffer,
    });
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; we don't implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "QuipcallScreenOutput"]
    #[ivars = Handler]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // Idle and blank frames carry no image; skip them.
            // SAFETY: a valid sample buffer from SCK.
            if let Some(pixel_buffer) = unsafe { sample.image_buffer() } {
                crate::autorelease_pool(|| self.ivars().push(pixel_buffer));
            }
        }
    }
);

type OnStop = Mutex<Option<Box<dyn FnOnce() + Send>>>;

define_class!(
    // SAFETY: as above.
    #[unsafe(super(NSObject))]
    #[name = "QuipcallScreenStreamDelegate"]
    #[ivars = OnStop]
    struct StopDelegate;

    unsafe impl NSObjectProtocol for StopDelegate {}

    unsafe impl SCStreamDelegate for StopDelegate {
        /// The window closed, the display went away, or the user stopped
        /// sharing from the menu bar.
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            tracing::info!("screen capture stopped: {}", error.localizedDescription());
            if let Some(on_stop) = self.ivars().lock().take() {
                on_stop();
            }
        }
    }
);

/// A running capture. Dropping it stops the stream.
pub(crate) struct Capture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _delegate: Retained<StopDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
    keepalive_stop: Arc<AtomicBool>,
}

// SAFETY: SCStream is thread-safe for start/stop, which is all we call after
// setup; the other fields only keep objects alive.
unsafe impl Send for Capture {}

impl Drop for Capture {
    fn drop(&mut self) {
        self.keepalive_stop.store(true, Ordering::Relaxed);
        // SAFETY: valid stream; no completion handler needed.
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}

pub(crate) async fn start(
    id: SourceId,
    preset: ScreenSharePreset,
    video: NativeVideoSource,
    on_stop: impl FnOnce() + Send + 'static,
) -> anyhow::Result<Capture> {
    let capture = {
        let content = shareable_content().await?;
        setup(&content, id, preset, video, Box::new(on_stop))?
    };

    let (tx, rx) = async_channel::bounded(1);
    {
        let handler = RcBlock::new(move |error: *mut NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                Err(describe(error))
            };
            let _ = tx.try_send(result);
        });
        // SAFETY: valid stream; the handler only sends on a channel.
        unsafe {
            capture
                .stream
                .startCaptureWithCompletionHandler(Some(&handler))
        };
    }
    if let Err(e) = rx.recv().await.context("screen capture didn't start")? {
        anyhow::bail!("{e}");
    }
    Ok(capture)
}

/// Builds the stream for `id` (not started yet).
fn setup(
    content: &SCShareableContent,
    id: SourceId,
    preset: ScreenSharePreset,
    video: NativeVideoSource,
    on_stop: Box<dyn FnOnce() + Send>,
) -> anyhow::Result<Capture> {
    // SAFETY: reading immutable snapshots, then configuring fresh objects.
    let filter = unsafe {
        match id {
            SourceId::Display(display_id) => {
                let display = content
                    .displays()
                    .iter()
                    .find(|d| d.displayID() == display_id)
                    .context("that screen is gone")?;
                // Keep our own floating circles and toolbar out of the share.
                let ours: Vec<_> = own_app(content).into_iter().collect();
                SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &NSArray::from_retained_slice(&ours),
                    &NSArray::new(),
                )
            }
            SourceId::Window(window_id) => {
                let window = content
                    .windows()
                    .iter()
                    .find(|w| w.windowID() == window_id)
                    .context("that window is gone")?;
                SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
            }
        }
    };

    // SAFETY: configuring a fresh object.
    let config = unsafe {
        let config = SCStreamConfiguration::new();
        let rect = filter.contentRect();
        let (w, h) = fit(
            rect.size.width,
            rect.size.height,
            filter.pointPixelScale() as f64,
            preset.max_width(),
        );
        tracing::info!(?id, w, h, fps = preset.fps, "starting screen capture");
        config.setWidth(w);
        config.setHeight(h);
        config.setMinimumFrameInterval(CMTime::new(1, preset.fps.max(1) as i32));
        config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        config.setShowsCursor(true);
        config.setQueueDepth(5);
        config
    };

    let last: Arc<LastFrame> = Arc::default();
    let started = Instant::now();
    let output = Output::alloc().set_ivars(Handler {
        source: video.clone(),
        started,
        frames: AtomicU64::new(0),
        last: last.clone(),
    });
    // SAFETY: NSObject's designated initializer.
    let output: Retained<Output> = unsafe { msg_send![super(output), init] };
    let delegate = StopDelegate::alloc().set_ivars(Mutex::new(Some(on_stop)));
    // SAFETY: as above.
    let delegate: Retained<StopDelegate> = unsafe { msg_send![super(delegate), init] };

    let queue = DispatchQueue::new("quipcall.screen", DispatchQueueAttr::SERIAL);
    // SAFETY: valid filter, configuration and delegate; the output and queue
    // are kept alive by `Capture`.
    let stream = unsafe {
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            Some(ProtocolObject::from_ref(&*delegate)),
        );
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
            .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
        stream
    };

    let keepalive_stop = Arc::new(AtomicBool::new(false));
    spawn_keepalive(video, started, last, keepalive_stop.clone());
    Ok(Capture {
        stream,
        _output: output,
        _delegate: delegate,
        _queue: queue,
        keepalive_stop,
    })
}

/// Re-sends the last frame once a second while the screen is still.
fn spawn_keepalive(
    video: NativeVideoSource,
    started: Instant,
    last: Arc<LastFrame>,
    stop: Arc<AtomicBool>,
) {
    let _ = std::thread::Builder::new()
        .name("screen-keepalive".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                let stale = {
                    let mut last = last.lock();
                    match last.as_mut() {
                        Some((frame, at)) if at.elapsed() >= Duration::from_secs(1) => {
                            *at = Instant::now();
                            Some(frame.0.clone())
                        }
                        _ => None,
                    }
                };
                if let Some(pixel_buffer) = stale {
                    crate::autorelease_pool(|| send(&video, started, pixel_buffer));
                }
            }
        });
}
