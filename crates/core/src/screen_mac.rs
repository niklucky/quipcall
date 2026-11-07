//! Screen sharing on macOS with ScreenCaptureKit.
//!
//! - [`list_sources`]: screens and windows, followed by small thumbnails, for
//!   our own share picker.
//! - [`Capture`]: frames of one screen or window arrive as full-range NV12
//!   IOSurface `CVPixelBuffer`s at the preset's frame rate and go to WebRTC
//!   as native buffers: VideoToolbox encodes them without a CPU copy, and
//!   ScreenCaptureKit does the scaling on the GPU.
//!
//! Needs the Screen Recording permission (see [`crate::permissions`]).
//!
//! `QUIPCALL_FAKE_SCREEN=1` (or `=WIDTHxHEIGHT`, default 1920x1200) shares a
//! moving test pattern instead, for headless tests: nothing on screen is
//! captured and no permission is needed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use futures_util::{StreamExt, stream};
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
use tokio::sync::Semaphore;

use crate::FrameSink;
use crate::pixel_buffer::{PixelBufferPool, paint_test_pattern};
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

/// Choices are usable before any screenshot has finished. Preview failures
/// leave the corresponding choice's icon in place.
pub enum ShareSourceUpdate {
    Sources(Vec<ShareSource>),
    Thumbnail(SourceId, Thumbnail),
}

const CONTENT_TIMEOUT: Duration = Duration::from_secs(10);
const THUMBNAIL_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_THUMBNAILS: usize = 4;
// Shared by every picker and engine. A timed-out or dismissed picker cannot
// reclaim capacity from a native request that is still running.
static THUMBNAIL_BUDGET: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_THUMBNAILS)));

type PreviewCallback<P> = Box<dyn FnOnce(Option<P>) + Send>;

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
/// objects are immutable snapshots; filters are fully configured before moving.
struct Shared<T>(T);
// SAFETY: see above; used only for SCK snapshots and immutable content filters.
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

/// Every capture path starts here. Thumbnails (SCScreenshotManager) and
/// filter geometry (contentRect, pointPixelScale) need macOS 14.
async fn shareable_content() -> anyhow::Result<Retained<SCShareableContent>> {
    if !objc2::available!(macos = 14.0) {
        anyhow::bail!("screen sharing needs macOS 14 or later");
    }
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

fn thumbnail(filter: &SCContentFilter, max_width: u32, complete: PreviewCallback<Thumbnail>) {
    let complete = Mutex::new(Some(complete));
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
            // Release the native-operation permit on callback completion even
            // if ScreenCaptureKit keeps the block alive afterwards.
            if let Some(complete) = complete.lock().take() {
                complete(thumb);
            }
        });
        SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
            filter,
            &config,
            Some(&handler),
        );
    }
}

fn own_app(content: &SCShareableContent) -> Option<Retained<SCRunningApplication>> {
    let pid = std::process::id() as i32;
    // SAFETY: reading an immutable snapshot.
    unsafe { content.applications() }
        .iter()
        .find(|app| unsafe { app.processID() } == pid)
}

/// Run on the engine's blocking pool, with its timer runtime entered.
pub(crate) async fn list_sources(
    thumb_width: u32,
    updates: &async_channel::Sender<anyhow::Result<ShareSourceUpdate>>,
) -> anyhow::Result<()> {
    if !crate::permissions::screen_recording_allowed() {
        return Err(NoPermission.into());
    }
    let content = tokio::time::timeout(CONTENT_TIMEOUT, shareable_content())
        .await
        .context("macOS took too long to list screens and windows; try again")??;
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
            thumbs.push((SourceId::Display(display.displayID()), Shared(filter)));
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
            thumbs.push((SourceId::Window(window.windowID()), Shared(filter)));
            sources.push(ShareSource {
                id: SourceId::Window(window.windowID()),
                title,
                app: Some(app_name),
                thumbnail: None,
            });
        }
    }

    tracing::debug!(count = sources.len(), "screen share choices ready");
    if updates
        .send(Ok(ShareSourceUpdate::Sources(sources)))
        .await
        .is_err()
    {
        return Ok(());
    }
    // Bound the native screenshot work, and deliver in completion order: one
    // unresponsive window must not hold up every other preview.
    let previews = preview_stream(
        thumbs,
        THUMBNAIL_TIMEOUT,
        THUMBNAIL_BUDGET.clone(),
        move |Shared(filter), complete| thumbnail(&filter, thumb_width, complete),
    );
    futures_util::pin_mut!(previews);
    while let Some((id, thumbnail)) = previews.next().await {
        if let Some(thumbnail) = thumbnail
            && updates
                .send(Ok(ShareSourceUpdate::Thumbnail(id, thumbnail)))
                .await
                .is_err()
        {
            break;
        }
    }
    Ok(())
}

fn preview_stream<T: Send + 'static, P: Send + 'static>(
    jobs: Vec<(SourceId, T)>,
    timeout: Duration,
    budget: Arc<Semaphore>,
    capture: impl Fn(T, PreviewCallback<P>) + Send + Sync + 'static,
) -> impl futures_util::Stream<Item = (SourceId, Option<P>)> {
    let capture = Arc::new(capture);
    stream::iter(jobs)
        .map(move |(id, job)| {
            let budget = budget.clone();
            let capture = capture.clone();
            async move {
                // The deadline includes capacity, worker scheduling, native
                // submission (which can block), and callback delivery.
                let preview = tokio::time::timeout(timeout, async move {
                    let permit = Arc::new(budget.acquire_owned().await.ok()?);
                    let (tx, rx) = async_channel::bounded(1);
                    tokio::task::spawn_blocking(move || {
                        // Keep capacity until BOTH the submission returns and
                        // its native callback completes or is discarded.
                        let _worker_permit = permit.clone();
                        if tx.is_closed() {
                            return;
                        }
                        crate::autorelease_pool(|| {
                            capture(
                                job,
                                Box::new(move |preview| {
                                    let _ = tx.try_send(preview);
                                    drop(permit);
                                }),
                            );
                        });
                    });
                    rx.recv().await.ok().flatten()
                })
                .await
                .ok()
                .flatten();
                (id, preview)
            }
        })
        .buffer_unordered(MAX_THUMBNAILS)
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
    /// Your own preview of the share (gets every frame, same buffers).
    preview: Option<Arc<dyn FrameSink>>,
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
        send(
            &self.source,
            self.preview.as_deref(),
            self.started,
            pixel_buffer,
        );
    }
}

fn send(
    source: &NativeVideoSource,
    preview: Option<&dyn FrameSink>,
    started: Instant,
    pixel_buffer: CFRetained<CVPixelBuffer>,
) {
    // SAFETY: we pass an owned (+1) reference; WebRTC's wrapper retains it and
    // releases ours.
    let native = unsafe {
        NativeBuffer::from_cv_pixel_buffer(CFRetained::into_raw(pixel_buffer).as_ptr().cast())
    };
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us: started.elapsed().as_micros() as i64,
        frame_metadata: None,
        buffer: Box::new(native) as BoxVideoBuffer,
    };
    source.capture_frame(&frame);
    if let Some(preview) = preview {
        preview.on_frame(&frame);
    }
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

/// Starts capturing `id`; frames go to `video` and to `preview`. Dropping the
/// result stops capturing.
pub(crate) async fn start(
    id: SourceId,
    preset: ScreenSharePreset,
    video: NativeVideoSource,
    preview: Option<Arc<dyn FrameSink>>,
    on_stop: impl FnOnce() + Send + 'static,
) -> anyhow::Result<Box<dyn Send>> {
    if let Some((width, height)) = synthetic_size() {
        let _ = (id, on_stop);
        return Ok(Box::new(Synthetic::start(
            width, height, preset, video, preview,
        )?));
    }
    let capture = {
        let content = shareable_content().await?;
        setup(&content, id, preset, video, preview, Box::new(on_stop))?
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
    Ok(Box::new(capture))
}

/// Builds the stream for `id` (not started yet).
fn setup(
    content: &SCShareableContent,
    id: SourceId,
    preset: ScreenSharePreset,
    video: NativeVideoSource,
    preview: Option<Arc<dyn FrameSink>>,
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
        preview: preview.clone(),
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
    spawn_keepalive(video, preview, started, last, keepalive_stop.clone());
    Ok(Capture {
        stream,
        _output: output,
        _delegate: delegate,
        _queue: queue,
        keepalive_stop,
    })
}

/// Re-sends the last frame once a second while the screen is still. The
/// preview gets it too: it may have skipped the newest frame to save repaints.
fn spawn_keepalive(
    video: NativeVideoSource,
    preview: Option<Arc<dyn FrameSink>>,
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
                    crate::autorelease_pool(|| {
                        send(&video, preview.as_deref(), started, pixel_buffer)
                    });
                }
            }
        });
}

// ---- synthetic source (tests) ---------------------------------------------

/// The size of the synthetic screen, if `QUIPCALL_FAKE_SCREEN` asks for one.
fn synthetic_size() -> Option<(u32, u32)> {
    let value = std::env::var("QUIPCALL_FAKE_SCREEN").ok()?;
    let size = value
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        .filter(|&(w, h)| (16..=8192).contains(&w) && (16..=8192).contains(&h));
    Some(size.map_or((1920, 1200), |(w, h)| (w & !1, h & !1)))
}

/// A moving test pattern at the preset's frame rate, sent down the same path
/// as captured frames (preview and keepalive included).
struct Synthetic {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Synthetic {
    fn start(
        width: u32,
        height: u32,
        preset: ScreenSharePreset,
        video: NativeVideoSource,
        preview: Option<Arc<dyn FrameSink>>,
    ) -> anyhow::Result<Self> {
        tracing::info!(
            width,
            height,
            fps = preset.fps,
            "sharing the synthetic screen (QUIPCALL_FAKE_SCREEN)"
        );
        let pool = PixelBufferPool::new(width, height).context("pixel buffer pool")?;
        let started = Instant::now();
        let last: Arc<LastFrame> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        spawn_keepalive(
            video.clone(),
            preview.clone(),
            started,
            last.clone(),
            stop.clone(),
        );
        let handler = Handler {
            source: video,
            preview,
            started,
            frames: AtomicU64::new(0),
            last,
        };
        let thread = std::thread::Builder::new()
            .name("fake-screen".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    let interval = Duration::from_secs(1) / preset.fps.max(1);
                    let mut next = Instant::now();
                    let mut n = 0;
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(pixel_buffer) = pool.take() {
                            paint_test_pattern(&pixel_buffer, n, [160, 100]);
                            crate::autorelease_pool(|| handler.push(pixel_buffer));
                        }
                        n += 1;
                        next += interval;
                        std::thread::sleep(next.saturating_duration_since(Instant::now()));
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Synthetic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held_previews(
        count: u32,
        timeout: Duration,
        budget: Arc<Semaphore>,
    ) -> (
        impl futures_util::Stream<Item = (SourceId, Option<u32>)>,
        async_channel::Receiver<PreviewCallback<u32>>,
    ) {
        let (tx, callbacks) = async_channel::unbounded();
        let previews = preview_stream(
            (0..count).map(|id| (SourceId::Window(id), id)).collect(),
            timeout,
            budget,
            move |_, complete| {
                let _ = tx.try_send(complete);
            },
        );
        (previews, callbacks)
    }

    #[tokio::test]
    async fn native_submission_stall_does_not_block_previews_or_deadlines() {
        let budget = Arc::new(Semaphore::new(MAX_THUMBNAILS));
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let (started_tx, started_rx) = async_channel::bounded(1);
        let previews = preview_stream(
            vec![
                (SourceId::Window(1), Some(blocked)),
                (SourceId::Window(2), None),
            ],
            Duration::from_millis(100),
            budget.clone(),
            move |blocked, complete| {
                if let Some(blocked) = blocked {
                    started_tx.try_send(()).unwrap();
                    // Safety net for a regression that blocks the test executor.
                    let _ = blocked.recv_timeout(Duration::from_secs(2));
                }
                complete(Some(42));
            },
        );
        let started = Instant::now();
        let results = tokio::time::timeout(Duration::from_secs(1), previews.collect::<Vec<_>>())
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        started_rx.try_recv().unwrap();
        assert_eq!(
            results,
            vec![(SourceId::Window(2), Some(42)), (SourceId::Window(1), None)]
        );
        assert_eq!(budget.available_permits(), MAX_THUMBNAILS - 1);
        release.send(()).unwrap();
        // Wait for the native worker's eventual return, without using real capture.
        let _all = tokio::time::timeout(
            Duration::from_secs(1),
            budget.acquire_many(MAX_THUMBNAILS as u32),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn callback_before_submission_returns_keeps_worker_capacity() {
        let budget = Arc::new(Semaphore::new(1));
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let mut previews = Box::pin(preview_stream(
            vec![(SourceId::Window(1), blocked)],
            Duration::from_secs(1),
            budget.clone(),
            |blocked, complete| {
                complete(Some(42));
                let _ = blocked.recv_timeout(Duration::from_secs(2));
            },
        ));
        assert_eq!(previews.next().await, Some((SourceId::Window(1), Some(42))));
        drop(previews);
        assert_eq!(budget.available_permits(), 0);
        release.send(()).unwrap();
        let _all = tokio::time::timeout(Duration::from_secs(1), budget.acquire())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn timeouts_keep_native_capture_budget_across_picker_sessions() {
        let budget = Arc::new(Semaphore::new(MAX_THUMBNAILS));
        let (previews, callbacks) = held_previews(12, Duration::from_millis(50), budget.clone());
        let results = previews.collect::<Vec<_>>().await;
        assert_eq!(results.len(), 12);
        assert!(results.iter().all(|(_, preview)| preview.is_none()));
        // Native callbacks still exist after the receivers timed out.
        assert_eq!(callbacks.len(), MAX_THUMBNAILS);
        assert_eq!(budget.available_permits(), 0);

        let (reopened, new_callbacks) = held_previews(8, Duration::from_millis(20), budget.clone());
        assert!(
            reopened
                .collect::<Vec<_>>()
                .await
                .iter()
                .all(|(_, preview)| preview.is_none())
        );
        assert!(new_callbacks.is_empty());
        assert_eq!(budget.available_permits(), 0);

        callbacks.recv().await.unwrap()(Some(1));
        let mut next = Box::pin(preview_stream(
            vec![(SourceId::Window(99), ())],
            Duration::from_secs(1),
            budget.clone(),
            |_, complete| complete(Some(99)),
        ));
        assert_eq!(next.next().await, Some((SourceId::Window(99), Some(99))));
        drop(next);
        drop(callbacks);
        let _all = tokio::time::timeout(
            Duration::from_secs(1),
            budget.acquire_many(MAX_THUMBNAILS as u32),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn closing_picker_keeps_inflight_budget_and_discards_queued_jobs() {
        let budget = Arc::new(Semaphore::new(MAX_THUMBNAILS));
        let (previews, callbacks) = held_previews(20, Duration::from_secs(10), budget.clone());
        let loading = tokio::spawn(previews.collect::<Vec<_>>());
        let mut active = Vec::new();
        for _ in 0..MAX_THUMBNAILS {
            active.push(
                tokio::time::timeout(Duration::from_secs(1), callbacks.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        loading.abort();
        assert!(loading.await.unwrap_err().is_cancelled());
        assert_eq!(budget.available_permits(), 0);
        let (reopened, new_callbacks) = held_previews(8, Duration::from_millis(20), budget.clone());
        assert!(
            reopened
                .collect::<Vec<_>>()
                .await
                .iter()
                .all(|(_, preview)| preview.is_none())
        );
        assert!(new_callbacks.is_empty());
        assert!(callbacks.is_empty());
        drop(active);
        let _all = tokio::time::timeout(
            Duration::from_secs(1),
            budget.acquire_many(MAX_THUMBNAILS as u32),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn failed_capture_releases_capacity_for_next_preview() {
        let budget = Arc::new(Semaphore::new(1));
        let previews = preview_stream(
            vec![(SourceId::Window(1), false), (SourceId::Window(2), true)],
            Duration::from_secs(1),
            budget.clone(),
            |success, complete| complete(success.then_some(42)),
        );
        let results = previews.collect::<Vec<_>>().await;
        assert!(results.contains(&(SourceId::Window(1), None)));
        assert!(results.contains(&(SourceId::Window(2), Some(42))));
        let _all = tokio::time::timeout(Duration::from_secs(1), budget.acquire())
            .await
            .unwrap()
            .unwrap();
    }
}
