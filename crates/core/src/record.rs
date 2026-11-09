//! Recording a call on this Mac: everyone's voice mixed into one file, and
//! optionally your screen.
//!
//! A thread of its own takes everyone's frames from an [`audio_tap::Listener`]
//! and mixes them on its own clock, one 10 ms frame per 10 ms. Each voice has
//! a little slack ([`PRIME`]) so frames arriving a few milliseconds late
//! still land in time; a voice that falls silent (muted, left) adds silence.
//! Screen frames from a capture of their own ([`ScreenInput`]) go in as they
//! come, shifted by that slack so picture and sound line up. The writer
//! (`record_mac`) encodes AAC (and H.264) in fragments, so a crash loses only
//! the last few seconds.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context as _;

use crate::audio_tap::{FRAME, Listener, SAMPLE_RATE, Speaker};
#[cfg(target_os = "macos")]
pub(crate) use crate::record_mac::VideoFormat;

/// Silence put before a voice that starts (or starts again), to absorb
/// jitter: frames arrive through queues, a few milliseconds apart.
const PRIME: usize = FRAME * 6;
/// More buffered than this and a voice has drifted ahead of the mix clock
/// (its device runs a little fast): its oldest audio goes.
const MAX_BACKLOG: usize = FRAME * 40;
/// The mix is written in chunks this long.
const CHUNK: usize = FRAME * 10;
/// The loudest a mixed sample may be; louder frames are turned down.
const CEILING: f32 = 0.98;

/// What was saved.
#[derive(Clone, Debug)]
pub struct Saved {
    pub path: PathBuf,
    pub duration: Duration,
    pub bytes: u64,
}

/// Your screen in a recording: its frames as they're captured, and the
/// capture, which stops with the recording.
#[cfg(target_os = "macos")]
pub(crate) struct ScreenInput {
    pub frames: std::sync::mpsc::Receiver<ScreenFrame>,
    pub format: VideoFormat,
    pub capture: Box<dyn Send>,
}

/// One captured frame and when it came.
#[cfg(target_os = "macos")]
pub(crate) struct ScreenFrame(
    objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
    Instant,
);

// SAFETY: CVPixelBuffers are reference-counted CF objects, fine to move
// between threads; the recorder only reads them.
#[cfg(target_os = "macos")]
unsafe impl Send for ScreenFrame {}

/// Hands captured frames to the recorder. A recorder that falls behind
/// misses frames rather than holding up the capture.
#[cfg(target_os = "macos")]
pub(crate) struct FrameQueue(pub std::sync::mpsc::SyncSender<ScreenFrame>);

#[cfg(target_os = "macos")]
impl crate::screen_mac::PixelSink for FrameQueue {
    fn on_pixels(
        &self,
        pixel_buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
    ) {
        let _ = self.0.try_send(ScreenFrame(pixel_buffer, Instant::now()));
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct ScreenInput;

pub struct Recorder {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<anyhow::Result<Saved>>>,
}

impl Recorder {
    /// Start writing everything `listener` hears to `path`: an .m4a, or with
    /// `screen`, an .mp4. Fails right away if the file can't be created.
    pub(crate) fn start(
        listener: Listener,
        path: PathBuf,
        screen: Option<ScreenInput>,
    ) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("quipcall-record".into())
            .spawn({
                let (path, stop) = (path.clone(), stop.clone());
                move || {
                    // The writer stays on this thread (AVFoundation objects
                    // aren't Send).
                    let writer = match Writer::create(&path, video_format(&screen)) {
                        Ok(writer) => {
                            let _ = ready_tx.send(Ok(()));
                            writer
                        }
                        Err(e) => {
                            let message = format!("{e:#}");
                            let _ = ready_tx.send(Err(e));
                            anyhow::bail!(message);
                        }
                    };
                    run(listener, screen, writer, &stop, path)
                }
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!("the recording thread stopped"),
        }
        tracing::info!(path = %path.display(), "recording");
        Ok(Self {
            path,
            stop,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Finish the file. Blocks for as long as the encoder needs (well under
    /// a second): call it off the UI thread.
    pub fn stop(mut self) -> anyhow::Result<Saved> {
        self.finish()
    }

    fn finish(&mut self) -> anyhow::Result<Saved> {
        self.stop.store(true, Ordering::Relaxed);
        let thread = self.thread.take().context("already stopped")?;
        let saved = thread
            .join()
            .map_err(|_| anyhow::anyhow!("the recording thread panicked"))??;
        tracing::info!(
            path = %saved.path.display(),
            secs = saved.duration.as_secs(),
            bytes = saved.bytes,
            "recording saved"
        );
        Ok(saved)
    }
}

impl Drop for Recorder {
    /// A recording is never left half-written: dropping one finishes it.
    fn drop(&mut self) {
        if self.thread.is_some()
            && let Err(e) = self.finish()
        {
            tracing::warn!("recording: {e:#}");
        }
    }
}

#[cfg(target_os = "macos")]
fn video_format(screen: &Option<ScreenInput>) -> Option<VideoFormat> {
    screen.as_ref().map(|s| s.format)
}

#[cfg(not(target_os = "macos"))]
fn video_format(_: &Option<ScreenInput>) -> Option<()> {
    None
}

/// Screen frames that came in, into the file. Sound reaches the file
/// [`PRIME`] late, so the picture moves by as much.
#[cfg(target_os = "macos")]
fn write_frames(
    screen: &Option<ScreenInput>,
    writer: &mut Writer,
    start: Instant,
) -> anyhow::Result<()> {
    let Some(screen) = screen else { return Ok(()) };
    let slack = Duration::from_micros(PRIME as u64 * 1_000_000 / SAMPLE_RATE as u64);
    while let Ok(ScreenFrame(pixels, at)) = screen.frames.try_recv() {
        writer.write_frame(&pixels, at.saturating_duration_since(start) + slack)?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn write_frames(_: &Option<ScreenInput>, _: &mut Writer, _: Instant) -> anyhow::Result<()> {
    Ok(())
}

fn run(
    listener: Listener,
    mut screen: Option<ScreenInput>,
    mut writer: Writer,
    stop: &AtomicBool,
    path: PathBuf,
) -> anyhow::Result<Saved> {
    let mut mixer = Mixer::default();
    let mut chunk: Vec<f32> = Vec::with_capacity(CHUNK);
    let start = Instant::now();
    let mut mixed: u64 = 0;
    loop {
        let stopping = stop.load(Ordering::Relaxed);
        while let Ok(frame) = listener.frames.try_recv() {
            mixer.push(frame.speaker, &frame.samples);
        }
        write_frames(&screen, &mut writer, start)?;
        // One frame for every 10 ms since the start.
        let due = start.elapsed().as_micros() as u64 / 10_000;
        while mixed < due {
            chunk.extend_from_slice(&mixer.pop());
            mixed += 1;
            if chunk.len() >= CHUNK {
                writer.write(&chunk)?;
                chunk.clear();
            }
        }
        if stopping {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Stop the capture, then keep what it had already sent.
    #[cfg(target_os = "macos")]
    if let Some(screen) = &mut screen {
        drop(std::mem::replace(&mut screen.capture, Box::new(())));
    }
    write_frames(&screen, &mut writer, start)?;
    drop(screen);
    // What's still buffered was heard before you pressed Stop.
    while mixer.buffered() > 0 {
        chunk.extend_from_slice(&mixer.pop());
        mixed += 1;
    }
    if !chunk.is_empty() {
        writer.write(&chunk)?;
    }
    drop(listener);
    let bytes = writer.finish()?;
    Ok(Saved {
        path,
        duration: Duration::from_micros(mixed * 10_000),
        bytes,
    })
}

/// Everyone's voice, buffered until the next mixed frame.
#[derive(Default)]
struct Mixer {
    voices: HashMap<Speaker, VecDeque<f32>>,
    /// Applied to the mix; below 1 while a frame would clip.
    gain: Option<f32>,
}

impl Mixer {
    fn push(&mut self, speaker: Speaker, samples: &[f32]) {
        let voice = self.voices.entry(speaker).or_default();
        if voice.is_empty() {
            voice.extend(std::iter::repeat_n(0.0, PRIME));
        }
        voice.extend(samples);
        if voice.len() > MAX_BACKLOG {
            let extra = voice.len() - PRIME;
            voice.drain(..extra);
        }
    }

    /// The most anyone has buffered, in samples.
    fn buffered(&self) -> usize {
        self.voices.values().map(VecDeque::len).max().unwrap_or(0)
    }

    /// The next 10 ms of everyone together. Voices that have nothing left
    /// count as silence.
    fn pop(&mut self) -> [f32; FRAME] {
        let mut out = [0.0f32; FRAME];
        for voice in self.voices.values_mut() {
            for (o, s) in out.iter_mut().zip(voice.drain(..FRAME.min(voice.len()))) {
                *o += s;
            }
        }
        self.voices.retain(|_, v| !v.is_empty());
        self.limit(&mut out);
        out
    }

    /// Two people talking at once can add up past full scale: turn the frame
    /// down at once, then let the gain come back up by about 1 dB per 10 ms.
    fn limit(&mut self, frame: &mut [f32; FRAME]) {
        let peak = frame.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        let before = self.gain.unwrap_or(1.0);
        let target = if peak * before > CEILING {
            CEILING / peak
        } else {
            (before * 1.12).min(1.0)
        };
        self.gain = (target < 1.0).then_some(target);
        if before == 1.0 && target == 1.0 {
            return;
        }
        // Ramp across the frame so the change makes no click.
        let step = (target - before) / FRAME as f32;
        for (i, s) in frame.iter_mut().enumerate() {
            let g = if target < before {
                target
            } else {
                before + step * i as f32
            };
            *s = (*s * g).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(target_os = "macos")]
use crate::record_mac::MediaWriter as Writer;

#[cfg(not(target_os = "macos"))]
struct Writer;
#[cfg(not(target_os = "macos"))]
impl Writer {
    fn create(_: &Path, _: Option<()>) -> anyhow::Result<Self> {
        anyhow::bail!("recording needs macOS for now")
    }
    fn write(&mut self, _: &[f32]) -> anyhow::Result<()> {
        Ok(())
    }
    fn finish(self) -> anyhow::Result<u64> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(level: f32) -> Vec<f32> {
        vec![level; FRAME]
    }

    #[test]
    fn voices_add_up_after_a_little_slack() {
        let mut mixer = Mixer::default();
        mixer.push(Speaker::Local, &tone(0.2));
        mixer.push(Speaker::Remote("anna".into()), &tone(0.3));
        // The slack comes out first, as silence.
        for _ in 0..PRIME / FRAME {
            assert!(mixer.pop().iter().all(|&s| s == 0.0));
        }
        let frame = mixer.pop();
        assert!(frame.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        // Nobody has anything left: silence, and the voices are forgotten.
        assert!(mixer.pop().iter().all(|&s| s == 0.0));
        assert!(mixer.voices.is_empty());
    }

    #[test]
    fn a_voice_running_fast_is_trimmed() {
        let mut mixer = Mixer::default();
        for _ in 0..100 {
            mixer.push(Speaker::Local, &tone(0.1));
        }
        assert!(mixer.buffered() <= MAX_BACKLOG);
    }

    #[test]
    fn loud_overlaps_are_turned_down() {
        let mut mixer = Mixer::default();
        for who in ["a", "b", "c"] {
            for _ in 0..(PRIME / FRAME + 1) {
                mixer.push(Speaker::Remote(who.into()), &tone(0.6));
            }
        }
        for _ in 0..PRIME / FRAME {
            mixer.pop();
        }
        let frame = mixer.pop();
        assert!(frame.iter().all(|s| s.abs() <= CEILING + 1e-6));
        assert!(frame[0] > 0.9, "turned down, not muted");
    }
}
