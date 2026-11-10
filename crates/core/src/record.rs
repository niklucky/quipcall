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
//!
//! Optionally each voice goes into a file of its own as well ([`Voices`]),
//! in a folder next to the mix. A voice's file starts with silence up to
//! when it was first heard, so every file lines up with the mix.

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

/// Names a voice for its own file.
pub(crate) type NameOf = Arc<dyn Fn(&Speaker) -> String + Send + Sync>;

/// Each voice in a file of its own, in `dir`.
pub(crate) struct Voices {
    pub(crate) dir: PathBuf,
    pub(crate) name_of: NameOf,
}

/// What was saved.
#[derive(Clone, Debug)]
pub struct Saved {
    /// Each voice on its own, if asked for.
    pub voices: Option<PathBuf>,
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
    /// `on_failed` runs (on the recording thread) if writing fails later:
    /// [`Recorder::stop`] then returns the error, and the file keeps what
    /// was written up to its last fragment.
    pub(crate) fn start(
        listener: Listener,
        path: PathBuf,
        screen: Option<ScreenInput>,
        voices: Option<Voices>,
        on_failed: impl FnOnce() + Send + 'static,
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
                    let result = run(listener, screen, writer, voices, &stop, path);
                    if let Err(e) = &result
                        && !stop.load(Ordering::Relaxed)
                    {
                        tracing::warn!("recording failed: {e:#}");
                        on_failed();
                    }
                    result
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

    /// Take nothing more in, now: the call is over. [`Recorder::stop`]
    /// then finishes the file.
    pub(crate) fn end(&self) {
        self.stop.store(true, Ordering::Relaxed);
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
    // After a stall several frames wait. Only the newest goes in: the
    // others are stale, and a burst of big frames can break the encoder
    // (AVAssetWriter fails with -16341 at 1920x1200).
    let mut newest = None;
    while let Ok(frame) = screen.frames.try_recv() {
        newest = Some(frame);
    }
    if let Some(ScreenFrame(pixels, at)) = newest {
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
    voices: Option<Voices>,
    stop: &AtomicBool,
    path: PathBuf,
) -> anyhow::Result<Saved> {
    let mut mixer = Mixer::default();
    let mut chunk: Vec<f32> = Vec::with_capacity(CHUNK);
    let start = Instant::now();
    let mut mixed: u64 = 0;
    let voices_dir = voices.as_ref().map(|v| v.dir.clone());
    let mut files = voices.map(VoiceThread::start).transpose()?;
    let mut each: HashMap<Speaker, [f32; FRAME]> = HashMap::new();
    loop {
        let stopping = stop.load(Ordering::Relaxed);
        while let Ok(frame) = listener.frames.try_recv() {
            mixer.push(frame.speaker, &frame.samples);
        }
        write_frames(&screen, &mut writer, start)?;
        // One frame for every 10 ms since the start.
        let due = start.elapsed().as_micros() as u64 / 10_000;
        while mixed < due {
            let out = files.as_ref().map(|_| &mut each);
            chunk.extend_from_slice(&mixer.pop(out));
            if let Some(files) = &mut files {
                files.add(&mut each, mixed);
            }
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
        let out = files.as_ref().map(|_| &mut each);
        chunk.extend_from_slice(&mixer.pop(out));
        if let Some(files) = &mut files {
            files.add(&mut each, mixed);
        }
        mixed += 1;
    }
    if !chunk.is_empty() {
        writer.write(&chunk)?;
    }
    drop(listener);
    // The mix first: the voices' files may still be catching up.
    let bytes = writer.finish();
    if let Some(files) = files {
        files.finish();
    }
    let bytes = bytes?;
    Ok(Saved {
        voices: voices_dir.filter(|d| d.exists()),
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
    /// count as silence. With `each`, every voice's own 10 ms too.
    fn pop(&mut self, mut each: Option<&mut HashMap<Speaker, [f32; FRAME]>>) -> [f32; FRAME] {
        let mut out = [0.0f32; FRAME];
        for (speaker, voice) in self.voices.iter_mut() {
            let mut own = [0.0f32; FRAME];
            for ((o, mine), s) in out
                .iter_mut()
                .zip(own.iter_mut())
                .zip(voice.drain(..FRAME.min(voice.len())))
            {
                *o += s;
                *mine = s;
            }
            if let Some(each) = each.as_deref_mut() {
                each.insert(speaker.clone(), own);
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

/// Mixed frames the voices' files may fall behind by: 10 s. A voice first
/// heard late starts with silence, about 3 s to encode per hour in; frames
/// that would wait longer are dropped for the voices' files (silence in
/// their place) rather than pile up in memory or hold up saving the mix.
const VOICES_QUEUE: usize = 1000;

/// A second of silence.
static SILENCE: [f32; SAMPLE_RATE as usize] = [0.0; SAMPLE_RATE as usize];

/// [`VoiceFiles`] on a thread of their own: a voice first heard an hour in
/// starts with an hour of silence, and encoding that mustn't hold up the mix.
struct VoiceThread {
    frames: std::sync::mpsc::SyncSender<(HashMap<Speaker, [f32; FRAME]>, u64)>,
    /// Frames the files missed since it was last said.
    dropped: u64,
    /// The last frame, if the files missed it: they're made as long.
    missed_last: Option<u64>,
    thread: JoinHandle<()>,
}

impl VoiceThread {
    fn start(voices: Voices) -> anyhow::Result<Self> {
        let (frames, from_mix) = std::sync::mpsc::sync_channel(VOICES_QUEUE);
        let thread = std::thread::Builder::new()
            .name("quipcall-voices".into())
            .spawn(move || {
                // The writers stay on this thread (they aren't Send).
                let mut files = VoiceFiles::new(voices);
                while let Ok((mut each, at)) = from_mix.recv() {
                    files.add(&mut each, at);
                }
                files.finish();
            })?;
        Ok(Self {
            frames,
            dropped: 0,
            missed_last: None,
            thread,
        })
    }

    /// Frame `at` of the mix, for every voice; `each` is emptied. Never
    /// waits: if the files are [`VOICES_QUEUE`] behind, they miss it.
    fn add(&mut self, each: &mut HashMap<Speaker, [f32; FRAME]>, at: u64) {
        if self.frames.try_send((std::mem::take(each), at)).is_err() {
            self.dropped += 1;
            self.missed_last = Some(at);
        } else {
            self.missed_last = None;
            if self.dropped > 0 {
                self.say_dropped();
            }
        }
    }

    fn say_dropped(&mut self) {
        tracing::warn!(
            frames = self.dropped,
            "the voices' files fell behind the mix: silence in their place"
        );
        self.dropped = 0;
    }

    /// Once everything sent is written.
    fn finish(mut self) {
        if self.dropped > 0 {
            self.say_dropped();
        }
        // Every file as long as the mix, even if it missed the end.
        if let Some(at) = self.missed_last {
            let _ = self.frames.send((HashMap::new(), at));
        }
        drop(self.frames);
        if self.thread.join().is_err() {
            tracing::warn!("the voices' files: their thread panicked");
        }
    }
}

/// A file per voice, made when the voice is first heard.
struct VoiceFiles {
    voices: Voices,
    files: HashMap<Speaker, VoiceFile>,
    /// Voices whose file couldn't be made, or broke: not tried again.
    failed: std::collections::HashSet<Speaker>,
    /// Names given, lowercased: Mac volumes usually ignore case.
    names: std::collections::HashSet<String>,
}

impl VoiceFiles {
    fn new(voices: Voices) -> Self {
        Self {
            voices,
            files: HashMap::new(),
            failed: Default::default(),
            names: Default::default(),
        }
    }

    /// Frame `at` (of the mix) for every voice: theirs from `each`, which
    /// is emptied, and silence for whoever was quiet.
    fn add(&mut self, each: &mut HashMap<Speaker, [f32; FRAME]>, at: u64) {
        let new: Vec<Speaker> = each
            .keys()
            .filter(|s| !self.files.contains_key(*s) && !self.failed.contains(*s))
            .cloned()
            .collect();
        for speaker in new {
            match self.create(&speaker) {
                Some(file) => {
                    self.files.insert(speaker, file);
                }
                None => {
                    self.failed.insert(speaker);
                }
            }
        }
        let mut broken = Vec::new();
        for (speaker, file) in &mut self.files {
            if let Err(e) = file.add(at, each.get(speaker)) {
                tracing::warn!("a voice's file: {e:#}");
                broken.push(speaker.clone());
            }
        }
        for speaker in broken {
            if let Some(file) = self.files.remove(&speaker) {
                let _ = file.finish();
            }
            self.failed.insert(speaker);
        }
        each.clear();
    }

    /// A file named after the voice, under a name nothing else in the folder
    /// has, whatever the case ("Anna" and "anna" are one file on most Macs).
    fn create(&mut self, speaker: &Speaker) -> Option<VoiceFile> {
        let name: String = (self.voices.name_of)(speaker)
            .chars()
            .map(|c| {
                if matches!(c, '/' | ':' | '\\') || c.is_control() {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        if let Err(e) = std::fs::create_dir_all(&self.voices.dir) {
            tracing::warn!("the voices' folder: {e}");
            return None;
        }
        // The disk has the last word: it knows how its names compare.
        let (name, path) = (1..)
            .map(|n| {
                let name = if n == 1 {
                    name.clone()
                } else {
                    format!("{name} {n}")
                };
                let path = self.voices.dir.join(format!("{name}.m4a"));
                (name, path)
            })
            .find(|(name, path)| !self.names.contains(&name.to_lowercase()) && !path.exists())
            .expect("unbounded");
        self.names.insert(name.to_lowercase());
        match Writer::create(&path, None) {
            Ok(writer) => Some(VoiceFile {
                writer,
                chunk: Vec::with_capacity(CHUNK),
                next: 0,
            }),
            Err(e) => {
                tracing::warn!("a voice's file for {name}: {e:#}");
                None
            }
        }
    }

    fn finish(self) {
        for (_, file) in self.files {
            if let Err(e) = file.finish() {
                tracing::warn!("a voice's file: {e:#}");
            }
        }
    }
}

/// One voice's file. It lines up with the mix: silence wherever the voice
/// was quiet, or wasn't heard yet.
struct VoiceFile {
    writer: Writer,
    chunk: Vec<f32>,
    /// The frame of the mix that comes next.
    next: u64,
}

impl VoiceFile {
    /// Frame `at` of the mix: theirs, or silence. Frames missed before it
    /// (the voice is new, or the files fell behind) are silence too.
    fn add(&mut self, at: u64, frame: Option<&[f32; FRAME]>) -> anyhow::Result<()> {
        if at < self.next {
            return Ok(());
        }
        if at > self.next {
            self.silence(at - self.next)?;
        }
        self.chunk
            .extend_from_slice(frame.map_or(&SILENCE[..FRAME], |f| &f[..]));
        self.next = at + 1;
        if self.chunk.len() >= CHUNK {
            self.writer.write(&self.chunk)?;
            self.chunk.clear();
        }
        Ok(())
    }

    /// `frames` of silence, a second at a time: four times quicker to
    /// encode than in chunks.
    fn silence(&mut self, frames: u64) -> anyhow::Result<()> {
        if !self.chunk.is_empty() {
            self.writer.write(&self.chunk)?;
            self.chunk.clear();
        }
        let mut left = frames as usize * FRAME;
        while left > 0 {
            let n = left.min(SILENCE.len());
            self.writer.write(&SILENCE[..n])?;
            left -= n;
        }
        self.next += frames;
        Ok(())
    }

    fn finish(mut self) -> anyhow::Result<()> {
        if !self.chunk.is_empty() {
            self.writer.write(&self.chunk)?;
        }
        self.writer.finish().map(|_| ())
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
    fn each_voice_comes_out_on_its_own_too() {
        let mut mixer = Mixer::default();
        let anna = Speaker::Remote("anna".into());
        for _ in 0..=PRIME / FRAME {
            mixer.push(Speaker::Local, &tone(0.25));
            mixer.push(anna.clone(), &tone(0.5));
        }
        let mut each = HashMap::new();
        for _ in 0..PRIME / FRAME {
            mixer.pop(Some(&mut each));
        }
        let mix = mixer.pop(Some(&mut each));
        assert!((mix[0] - 0.75).abs() < 1e-6);
        assert!((each[&Speaker::Local][0] - 0.25).abs() < 1e-6);
        assert!((each[&anna][0] - 0.5).abs() < 1e-6);
    }

    /// How long a file is, by `afinfo`.
    #[cfg(target_os = "macos")]
    fn seconds(path: &Path) -> f32 {
        let info = std::process::Command::new("afinfo")
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&info.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("estimated duration: "))
            .and_then(|l| l.split_whitespace().next()?.parse().ok())
            .unwrap_or_else(|| panic!("afinfo reads {}", path.display()))
    }

    fn voices_in(
        dir: &Path,
        name_of: impl Fn(&Speaker) -> String + Send + Sync + 'static,
    ) -> Voices {
        let _ = std::fs::remove_dir_all(dir);
        Voices {
            dir: dir.to_owned(),
            name_of: Arc::new(name_of),
        }
    }

    /// Someone first heard 30 s in: their file starts with 30 s of silence,
    /// written off the mixer's thread, so handing their frames over is
    /// instant however long the silence. More frames than the files may fall
    /// behind by still never wait; any they miss are silence, in place.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_late_voice_lines_up_without_holding_up_the_mix() {
        let dir = std::env::temp_dir().join(format!("quipcall-voices-{}", std::process::id()));
        let anna = Speaker::Remote("anna".into());
        let mut thread = VoiceThread::start(voices_in(&dir, |_| "Anna".to_owned())).unwrap();
        let begun = Instant::now();
        let late = 3_000; // frames: 30 s
        let count = VOICES_QUEUE as u64 + 100;
        for at in late..late + count {
            let mut each = HashMap::from([(anna.clone(), [0.25f32; FRAME])]);
            thread.add(&mut each, at);
            assert!(each.is_empty());
        }
        assert!(
            begun.elapsed() < Duration::from_millis(100),
            "{:?}",
            begun.elapsed()
        );
        thread.finish();
        let secs = seconds(&dir.join("Anna.m4a"));
        let expected = (late + count) as f32 / 100.0;
        assert!((secs - expected).abs() < 0.1, "{secs} s, not {expected}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Anna" and "anna" would be one file on a Mac volume: each gets one.
    #[cfg(target_os = "macos")]
    #[test]
    fn voices_whose_names_differ_only_in_case_get_a_file_each() {
        let dir = std::env::temp_dir().join(format!("quipcall-names-{}", std::process::id()));
        let mut files = VoiceFiles::new(voices_in(&dir, |speaker| match speaker {
            Speaker::Local => "Anna".to_owned(),
            Speaker::Remote(_) => "anna".to_owned(),
        }));
        for at in 0..100 {
            let mut each = HashMap::from([
                (Speaker::Local, [0.25f32; FRAME]),
                (Speaker::Remote("anna-2".into()), [0.5f32; FRAME]),
            ]);
            files.add(&mut each, at);
        }
        assert!(files.failed.is_empty());
        files.finish();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        for name in &names {
            assert!((seconds(&dir.join(name)) - 1.0).abs() < 0.1, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The call is over: the recording stops taking anything in at once,
    /// however long finishing (or the transcript's last words) then takes.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_recording_ends_with_the_call_not_when_it_is_finished() {
        let dir = std::env::temp_dir().join(format!("quipcall-end-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hub = Arc::new(crate::audio_tap::Hub::default());
        let path = dir.join("call.m4a");
        let recorder = Recorder::start(hub.listen(|| {}), path, None, None, || {}).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        recorder.end();
        // The transcript's last words, say.
        std::thread::sleep(Duration::from_millis(1000));
        let saved = recorder.stop().unwrap();
        assert!(
            saved.duration < Duration::from_millis(800),
            "{:?}",
            saved.duration
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn voices_add_up_after_a_little_slack() {
        let mut mixer = Mixer::default();
        mixer.push(Speaker::Local, &tone(0.2));
        mixer.push(Speaker::Remote("anna".into()), &tone(0.3));
        // The slack comes out first, as silence.
        for _ in 0..PRIME / FRAME {
            assert!(mixer.pop(None).iter().all(|&s| s == 0.0));
        }
        let frame = mixer.pop(None);
        assert!(frame.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        // Nobody has anything left: silence, and the voices are forgotten.
        assert!(mixer.pop(None).iter().all(|&s| s == 0.0));
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
            mixer.pop(None);
        }
        let frame = mixer.pop(None);
        assert!(frame.iter().all(|s| s.abs() <= CEILING + 1e-6));
        assert!(frame[0] > 0.9, "turned down, not muted");
    }
}
