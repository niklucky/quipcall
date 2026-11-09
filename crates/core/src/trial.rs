//! "Try it with your voice" in Settings: your microphone, outside a call,
//! through the speech model you picked, as live as in a call.
//!
//! Test instances (`QUIPCALL_NO_MIC`) never open a microphone: they read a
//! 48 kHz float WAV from `QUIPCALL_FAKE_MIC` instead, in real time.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use quipcall_stt::{Line, Options, Transcriber};

use crate::models::{self, ModelInfo, catalog};
use crate::permissions::{self, MediaDevice};

/// The longest a try runs.
const MAX: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum TrialEvent {
    /// The model has loaded and the microphone is on: speak.
    Listening,
    /// What you said, or a draft of it.
    Line(Line),
    /// Over: stopped, or [`MAX`] reached.
    Done,
    Failed(String),
}

/// A try in progress. Dropping it stops it.
pub struct Trial {
    pub events: async_channel::Receiver<TrialEvent>,
    stop: Arc<AtomicBool>,
}

impl Trial {
    /// Stop listening; the last words still come, then [`TrialEvent::Done`].
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Trial {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn start(model: &'static ModelInfo, dir: PathBuf, language: Option<String>) -> Trial {
    let (tx, events) = async_channel::unbounded();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    std::thread::Builder::new()
        .name("voice-trial".into())
        .spawn(move || {
            let last = match run(model, dir, language, stopping, &tx) {
                Ok(()) => TrialEvent::Done,
                Err(e) => TrialEvent::Failed(format!("{e:#}")),
            };
            let _ = tx.try_send(last);
        })
        .expect("voice trial thread");
    Trial { events, stop }
}

fn run(
    model: &'static ModelInfo,
    dir: PathBuf,
    language: Option<String>,
    stop: Arc<AtomicBool>,
    tx: &async_channel::Sender<TrialEvent>,
) -> anyhow::Result<()> {
    let fake = std::env::var_os("QUIPCALL_NO_MIC").is_some();
    if !fake {
        let rt = tokio::runtime::Builder::new_current_thread().build()?;
        anyhow::ensure!(
            rt.block_on(permissions::request(MediaDevice::Microphone)),
            "microphone access is off. Allow it in System Settings → Privacy & Security → Microphone"
        );
    }
    let lines = tx.clone();
    let transcriber = Transcriber::start(
        Options {
            model: models::path(model, &dir),
            vad: models::path(&catalog::VAD, &dir),
            language,
        },
        move |line| {
            let _ = lines.try_send(TrialEvent::Line(line));
        },
    )?;
    let (frames, from_mic) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);
    let source: Box<dyn Send> = if fake {
        Box::new(fake_mic(frames, stop.clone())?)
    } else {
        mic::open(frames)?
    };
    let _ = tx.try_send(TrialEvent::Listening);
    let begun = Instant::now();
    while !stop.load(Ordering::Relaxed) && begun.elapsed() < MAX {
        match from_mic.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) => transcriber.feed("you", samples),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    drop(source);
    transcriber.stop();
    Ok(())
}

/// A WAV played as if spoken, 10 ms at a time.
fn fake_mic(
    frames: std::sync::mpsc::SyncSender<Vec<f32>>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let path =
        std::env::var_os("QUIPCALL_FAKE_MIC").context("QUIPCALL_FAKE_MIC: a WAV to speak")?;
    let wav = std::fs::read(&path)?;
    let at = wav
        .windows(4)
        .position(|w| w == b"data")
        .context("not a WAV")?
        + 8;
    let (samples, _) = wav[at..].as_chunks::<4>();
    let samples: Vec<f32> = samples.iter().map(|b| f32::from_le_bytes(*b)).collect();
    Ok(std::thread::spawn(move || {
        for frame in samples.chunks(480) {
            if stop.load(Ordering::Relaxed) || frames.send(frame.to_vec()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }))
}

#[cfg(target_os = "macos")]
mod mic {
    use std::sync::mpsc::SyncSender;

    use anyhow::Context as _;
    use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread, DefinedClass, define_class, msg_send};
    use objc2_av_foundation::{
        AVCaptureAudioDataOutput, AVCaptureAudioDataOutputSampleBufferDelegate,
        AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput,
        AVCaptureSession, AVMediaTypeAudio,
    };
    use objc2_core_media::CMSampleBuffer;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; no Drop.
        #[unsafe(super(NSObject))]
        #[name = "QuipcallTrialMicDelegate"]
        #[ivars = SyncSender<Vec<f32>>]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for Delegate {
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            fn did_output(
                &self,
                _output: &AVCaptureOutput,
                sample_buffer: &CMSampleBuffer,
                _connection: &AVCaptureConnection,
            ) {
                // Float32 mono at 48 kHz, as asked for below.
                // SAFETY: a valid sample buffer from AVFoundation.
                let Some(block) = (unsafe { sample_buffer.data_buffer() }) else {
                    return;
                };
                let len = unsafe { block.data_length() };
                let mut bytes = vec![0u8; len];
                let ok = unsafe {
                    block.copy_data_bytes(0, len, std::ptr::NonNull::new_unchecked(bytes.as_mut_ptr().cast()))
                };
                if ok != 0 {
                    return;
                }
                let (samples, _) = bytes.as_chunks::<4>();
                let samples = samples.iter().map(|b| f32::from_ne_bytes(*b)).collect();
                // Falling behind drops audio rather than block capture.
                let _ = self.ivars().try_send(samples);
            }
        }
    );

    struct Capture {
        session: Retained<AVCaptureSession>,
        _delegate: Retained<Delegate>,
        _queue: DispatchRetained<DispatchQueue>,
    }

    // SAFETY: as the camera's: only `stopRunning` after setup.
    unsafe impl Send for Capture {}

    impl Drop for Capture {
        fn drop(&mut self) {
            // SAFETY: valid session; no callbacks once it has stopped.
            unsafe { self.session.stopRunning() };
        }
    }

    pub(super) fn open(frames: SyncSender<Vec<f32>>) -> anyhow::Result<Box<dyn Send>> {
        // SAFETY: plain AVFoundation setup with valid arguments; the
        // delegate and queue live as long as the capture.
        unsafe {
            let media = AVMediaTypeAudio.context("AVMediaTypeAudio")?;
            let device = AVCaptureDevice::defaultDeviceWithMediaType(media)
                .context("no microphone found")?;
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| anyhow::anyhow!("open microphone: {}", e.localizedDescription()))?;
            let output = AVCaptureAudioDataOutput::new();
            let number = |n: u32| NSNumber::new_u32(n);
            let keys = [
                NSString::from_str("AVFormatIDKey"),
                NSString::from_str("AVSampleRateKey"),
                NSString::from_str("AVNumberOfChannelsKey"),
                NSString::from_str("AVLinearPCMBitDepthKey"),
                NSString::from_str("AVLinearPCMIsFloatKey"),
                NSString::from_str("AVLinearPCMIsNonInterleaved"),
                NSString::from_str("AVLinearPCMIsBigEndianKey"),
            ];
            let values = [
                number(u32::from_be_bytes(*b"lpcm")),
                number(48_000),
                number(1),
                number(32),
                NSNumber::new_bool(true),
                NSNumber::new_bool(false),
                NSNumber::new_bool(false),
            ];
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let value_refs: Vec<&AnyObject> = values.iter().map(|v| v.as_ref()).collect();
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&key_refs, &value_refs);
            output.setAudioSettings(Some(&settings));

            let queue = DispatchQueue::new("quipcall.trial-mic", DispatchQueueAttr::SERIAL);
            let delegate: Retained<Delegate> = {
                let this = Delegate::alloc().set_ivars(frames);
                msg_send![super(this), init]
            };
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
            let session = AVCaptureSession::new();
            session.beginConfiguration();
            anyhow::ensure!(session.canAddInput(&input), "microphone input rejected");
            session.addInput(&input);
            anyhow::ensure!(session.canAddOutput(&output), "microphone output rejected");
            session.addOutput(&output);
            session.commitConfiguration();
            session.startRunning();
            anyhow::ensure!(session.isRunning(), "the microphone did not start");
            Ok(Box::new(Capture {
                session,
                _delegate: delegate,
                _queue: queue,
            }))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod mic {
    pub(super) fn open(_: std::sync::mpsc::SyncSender<Vec<f32>>) -> anyhow::Result<Box<dyn Send>> {
        anyhow::bail!("trying your voice needs macOS for now")
    }
}
