//! Speech to text for calls, on this Mac. Everyone's voice goes in, one
//! stream per person; lines of text come out, each with who said it.
//!
//! - **Listening** (one thread): each voice is cut to 16 kHz and scored by
//!   Silero VAD on the CPU every 32 ms, which splits it into utterances
//!   (`segment.rs`). Near-silent frames skip the VAD.
//! - **Transcribing** (another thread): utterances queue for the speech model
//!   (Parakeet v3 or Qwen3-ASR through transcribe.cpp, on the GPU when there
//!   is one). While someone talks, the utterance so far is transcribed about
//!   every second as a draft; the finished utterance replaces it. Finished
//!   utterances go first, and a newer draft replaces one still waiting, so a
//!   slow Mac skips drafts rather than fall behind.
//!
//! Loading a model takes a moment, and the first run of each new build
//! compiles Metal shaders (about 3 s), so [`Transcriber::start`] blocks.

pub mod catalog;
mod resample;
mod segment;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Once, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use parking_lot::{Condvar, Mutex};
use transcribe_cpp as tc;

pub use catalog::ModelInfo;

use crate::resample::Downsampler;
use crate::segment::{Cut, FRAME, Segmenter};

const RATE: f32 = 16_000.0;
/// Voice frames waiting to be listened to, across everyone: 10 s of one voice.
const QUEUE: usize = 1_000;
/// A voice that sends nothing for this long has stopped talking (muted, or
/// left the call).
const GONE: Duration = Duration::from_millis(800);
/// Quieter than -60 dBFS is not speech; the VAD needn't look.
const QUIET: f32 = 1e-3;

pub struct Options {
    /// The speech model's .gguf.
    pub model: PathBuf,
    /// Silero VAD's .gguf ([`catalog::VAD`]).
    pub vad: PathBuf,
    /// ISO 639-1, or `None` to let the model tell.
    pub language: Option<String>,
}

/// What someone said.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// The utterance. A later line with the same id replaces this one. Ids
    /// keep growing across transcribers, from the clock.
    pub id: u64,
    pub speaker: String,
    /// From the start of transcription.
    pub start: Duration,
    /// When they began, on the wall clock.
    pub at: SystemTime,
    pub end: Duration,
    pub text: String,
    /// Final. Until then, text may change. A final line with no text takes
    /// back a draft that turned out to be noise.
    pub done: bool,
}

pub struct Transcriber {
    input: Option<mpsc::SyncSender<(String, Vec<f32>, Instant)>>,
    threads: Vec<JoinHandle<()>>,
}

impl Transcriber {
    /// Loads the models and starts. `on_line` is called on a worker thread.
    pub fn start(options: Options, on_line: impl Fn(Line) + Send + 'static) -> Result<Self> {
        static LOG: Once = Once::new();
        LOG.call_once(tc::init_logging);

        let begun = Instant::now();
        let vad = tc::Model::load_with(
            &options.vad,
            &tc::ModelOptions {
                backend: tc::Backend::Cpu,
                ..Default::default()
            },
        )
        .with_context(|| format!("loading {}", options.vad.display()))?;
        let model = tc::Model::load(&options.model)
            .with_context(|| format!("loading {}", options.model.display()))?;
        let languages = model.capabilities()?.languages;
        let mut session = model.session()?;
        let run = tc::RunOptions {
            language: options
                .language
                .filter(|l| languages.iter().any(|m| m == l)),
            ..Default::default()
        };
        // Compiles the GPU code now rather than on the first words.
        pooled(|| session.run(&vec![0.0; 16_000], &run)).context("warming up")?;
        tracing::info!(
            "transcribing with {} on {} (ready in {:?}, language {:?})",
            model.arch(),
            model.backend(),
            begun.elapsed(),
            run.language
        );

        let queue = Arc::new(Queue::default());
        let (input, frames) = mpsc::sync_channel(QUEUE);
        let start = Instant::now();
        let wall = SystemTime::now();
        let listen = std::thread::Builder::new()
            .name("stt-listen".into())
            .spawn({
                let queue = queue.clone();
                move || listen(frames, vad, start, &queue)
            })?;
        let transcribe = std::thread::Builder::new()
            .name("stt-transcribe".into())
            .spawn(move || transcribe(session, run, wall, &queue, on_line))?;
        Ok(Self {
            input: Some(input),
            threads: vec![listen, transcribe],
        })
    }

    /// Some of one person's voice: mono, 48 kHz, -1..1. Never blocks; if
    /// listening falls 10 s behind, frames are dropped.
    pub fn feed(&self, speaker: &str, samples: Vec<f32>) {
        if let Some(input) = &self.input {
            let _ = input.try_send((speaker.to_owned(), samples, Instant::now()));
        }
    }

    /// Finishes what's being said, transcribes it, and stops.
    pub fn stop(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        self.input = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        self.finish();
    }
}

struct Voice {
    down: Downsampler,
    vad: tc::VadSession,
    seg: Segmenter,
    /// 16 kHz samples short of a VAD frame.
    pending: Vec<f32>,
    heard: Instant,
    /// 10 ms frames heard, for the log.
    frames: u64,
}

fn listen(
    frames: mpsc::Receiver<(String, Vec<f32>, Instant)>,
    vad: tc::Model,
    start: Instant,
    queue: &Queue,
) {
    let mut voices: HashMap<String, Voice> = HashMap::new();
    // Ids go on from the clock, so a transcript stopped and started again
    // (or another app's run) never reuses one: a line replaces its namesake.
    let mut next = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut next_id = || {
        next += 1;
        next
    };
    let mut checked = Instant::now();
    let since = |at: Instant| at.saturating_duration_since(start);
    let push = |speaker: &str, cut: Cut| {
        queue.push(Job {
            utterance: cut.utterance,
            speaker: speaker.into(),
            start: since(cut.at),
            audio: cut.audio,
            done: cut.done,
        });
    };

    loop {
        let frame = frames.recv_timeout(GONE / 4);
        let disconnected = matches!(frame, Err(mpsc::RecvTimeoutError::Disconnected));
        if let Ok((speaker, samples, at)) = frame {
            let voice = match voices.get_mut(&speaker) {
                Some(v) => v,
                None => match vad.vad_session() {
                    Ok(session) => voices.entry(speaker.clone()).or_insert(Voice {
                        down: Downsampler::new(),
                        vad: session,
                        seg: Segmenter::new(),
                        pending: Vec::with_capacity(FRAME * 2),
                        heard: at,
                        frames: 0,
                    }),
                    Err(e) => {
                        tracing::warn!("no VAD for a voice: {e}");
                        continue;
                    }
                },
            };
            voice.heard = at;
            voice.frames += 1;
            if voice.frames % 500 == 1 {
                let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                tracing::debug!(speaker, frames = voice.frames, peak, "voice");
            }
            voice.down.push(&samples, &mut voice.pending);
            while voice.pending.len() >= FRAME {
                let frame: Vec<f32> = voice.pending.drain(..FRAME).collect();
                let quiet =
                    frame.iter().map(|s| s * s).sum::<f32>() / (FRAME as f32) < QUIET * QUIET;
                let p = if quiet {
                    0.0
                } else {
                    match voice.vad.stream_feed(&frame) {
                        Ok(r) => r.probs.last().copied().unwrap_or(0.0),
                        Err(e) => {
                            tracing::warn!("VAD: {e}");
                            0.0
                        }
                    }
                };
                let heard = at - Duration::from_secs_f32(voice.pending.len() as f32 / RATE);
                if let Some(cut) = voice.seg.push(&frame, p, heard, &mut next_id) {
                    tracing::debug!(
                        speaker,
                        utterance = cut.utterance,
                        secs = cut.audio.len() as f32 / RATE,
                        done = cut.done,
                        "utterance"
                    );
                    push(&speaker, cut);
                }
            }
        }

        if disconnected || checked.elapsed() >= GONE / 4 {
            checked = Instant::now();
            voices.retain(|speaker, voice| {
                if !disconnected && voice.heard.elapsed() < GONE {
                    return true;
                }
                if let Some(cut) = voice.seg.finish() {
                    push(speaker, cut);
                }
                // Heard again later (unmuted, say), a voice starts afresh:
                // nothing from before (the preroll, with its old time; the
                // filter; the VAD's state) leaks into what they say next.
                false
            });
        }
        if disconnected {
            queue.close();
            return;
        }
    }
}

struct Job {
    utterance: u64,
    speaker: Arc<str>,
    start: Duration,
    audio: Vec<f32>,
    done: bool,
}

/// Utterances waiting for the speech model.
#[derive(Default)]
struct Queue {
    state: Mutex<(VecDeque<Job>, bool)>,
    ready: Condvar,
}

impl Queue {
    fn push(&self, job: Job) {
        let mut state = self.state.lock();
        // A draft or the final text makes older drafts pointless.
        state.0.retain(|j| j.done || j.utterance != job.utterance);
        state.0.push_back(job);
        self.ready.notify_one();
    }

    /// The next job: finished utterances first. `None` once closed and done;
    /// drafts are skipped after closing.
    fn pop(&self) -> Option<Job> {
        let mut state = self.state.lock();
        loop {
            let (jobs, closed) = &mut *state;
            if *closed {
                jobs.retain(|j| j.done);
            }
            if let Some(i) = jobs.iter().position(|j| j.done) {
                return jobs.remove(i);
            }
            if let Some(job) = jobs.pop_front() {
                return Some(job);
            }
            if *closed {
                return None;
            }
            self.ready.wait(&mut state);
        }
    }

    fn close(&self) {
        self.state.lock().1 = true;
        self.ready.notify_all();
    }
}

fn transcribe(
    mut session: tc::Session,
    run: tc::RunOptions,
    wall: SystemTime,
    queue: &Queue,
    on_line: impl Fn(Line),
) {
    // Utterances with a draft on screen.
    let mut shown = HashSet::new();
    while let Some(job) = queue.pop() {
        let text = if job.audio.is_empty() {
            String::new()
        } else {
            match pooled(|| session.run(&job.audio, &run)) {
                Ok(t) => t.text.trim().to_owned(),
                Err(e) => {
                    tracing::warn!("transcribing {:.1} s: {e}", job.audio.len() as f32 / RATE);
                    String::new()
                }
            }
        };
        if job.done {
            if !shown.remove(&job.utterance) && text.is_empty() {
                continue;
            }
        } else if text.is_empty() {
            continue;
        } else {
            shown.insert(job.utterance);
        }
        on_line(Line {
            id: job.utterance,
            speaker: job.speaker.to_string(),
            start: job.start,
            at: wall + job.start,
            end: job.start + Duration::from_secs_f32(job.audio.len() as f32 / RATE),
            text,
            done: job.done,
        });
    }
}

/// Runs `f` in an autorelease pool: Metal hands out autoreleased objects,
/// and our threads have no pool that ever drains.
fn pooled<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "macos")]
    {
        objc2::rc::autoreleasepool(|_| f())
    }
    #[cfg(not(target_os = "macos"))]
    {
        f()
    }
}
