//! Neural noise suppression for the microphone (DeepFilterNet 3).
//!
//! WebRTC cancels echo first, then hands every 10 ms frame to [`Denoiser`] on
//! its capture thread, through a hook in our patched `webrtc-sys` (see
//! `vendor/README.md`). WebRTC's adaptive gain control (AGC2) runs after the
//! model, so it follows the cleaned voice: before it, a bark or a rustle
//! turned the voice down for seconds after the noise was gone. While the
//! model runs, WebRTC's own noise suppression and older gain control (AGC1,
//! before the hook) are off: two suppressors in a row damage speech.
//!
//! The model mixes back some of the noise ([`Strength::CALLS`]): removing all
//! of it also took quiet parts of the voice with it. It may remove more in
//! pauses, where no voice can be lost.
//!
//! `QUIPCALL_DENOISE=webrtc` uses WebRTC's suppressor instead, `=off` neither.
//! `QUIPCALL_DENOISE_LIMIT=<dB>[/<dB in pauses>]` (or `none`) overrides how
//! much the model may remove, for listening tests.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;
use parking_lot::Mutex;

/// The model's sample rate and hop: exactly one WebRTC frame (10 ms at 48 kHz).
pub const SAMPLE_RATE: i32 = 48_000;
pub const FRAME: usize = 480;

/// A Mac that needs longer than this per 10 ms frame keeps WebRTC's suppressor.
const MAX_FRAME_COST: Duration = Duration::from_millis(3);
/// A gap in the microphone's frames this long means it was muted or a new
/// call started: the model restarts. Frames normally come every 10 ms, at
/// worst a few at once.
const RESTART_AFTER: Duration = Duration::from_millis(100);

/// What cleans the microphone during a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// DeepFilterNet 3.
    Neural,
    /// WebRTC's built-in suppressor: steady noise only.
    WebRtc,
    Off,
}

/// How hard the model works.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strength {
    /// The most the model may lower any sound, in dB (`None`: no limit). The
    /// rest of the noise is mixed back in.
    pub limit_db: Option<f32>,
    /// A deeper limit once the model has heard no voice for a moment
    /// (`None`: keep `limit_db`). In a pause no voice masks the noise, and
    /// none can be lost.
    pub pause_limit_db: Option<f32>,
    /// Frames whose local SNR the model rates below this (dB) count as noise
    /// only and are lowered fully.
    pub noise_only_below_db: f32,
    /// Gain into the model, undone after it (dB). The model takes a voice
    /// below about -40 to -45 dBFS for noise, which a laptop microphone at
    /// arm's length reaches; louder voices lose little from the gain.
    pub input_gain_db: f32,
}

impl Strength {
    /// DeepFilterNet's own defaults: remove all the noise it can.
    pub const MODEL_DEFAULT: Strength = Strength {
        limit_db: None,
        pause_limit_db: None,
        noise_only_below_db: -10.0,
        input_gain_db: 0.0,
    };

    /// What calls use: noise drops by at most 12 dB while you talk and by up
    /// to 30 dB in pauses; the voice dips a little more than with WebRTC's
    /// own suppressor. `dev/denoise-eval.py` compares the options.
    pub const CALLS: Strength = Strength {
        limit_db: Some(12.0),
        pause_limit_db: Some(30.0),
        noise_only_below_db: -10.0,
        input_gain_db: 12.0,
    };

    /// [`Self::CALLS`], or the limits in `QUIPCALL_DENOISE_LIMIT`.
    fn for_calls() -> Strength {
        let Ok(value) = std::env::var("QUIPCALL_DENOISE_LIMIT") else {
            return Self::CALLS;
        };
        match parse_limits(&value) {
            Some((limit_db, pause_limit_db)) => Strength {
                limit_db,
                pause_limit_db,
                ..Self::CALLS
            },
            None => {
                tracing::warn!(value, "QUIPCALL_DENOISE_LIMIT: not none, <dB> or <dB>/<dB>");
                Self::CALLS
            }
        }
    }
}

/// `none`, `<dB>` or `<dB>/<dB in pauses>`, each finite and not negative.
fn parse_limits(value: &str) -> Option<(Option<f32>, Option<f32>)> {
    if value == "none" {
        return Some((None, None));
    }
    let db = |s: &str| {
        s.parse::<f32>()
            .ok()
            .filter(|db| db.is_finite() && *db >= 0.0)
    };
    match value.split_once('/') {
        None => Some((Some(db(value)?), None)),
        Some((limit, pause)) => Some((Some(db(limit)?), Some(db(pause)?))),
    }
}

/// DeepFilterNet state for one microphone stream. Samples are in WebRTC's
/// 16-bit range, mono, [`FRAME`] at a time.
pub struct Denoiser {
    df: DfTract,
    input: Array2<f32>,
    output: Array2<f32>,
    /// Scale from WebRTC's 16-bit range into the model and back.
    scale_in: f32,
    scale_out: f32,
    strength: Strength,
    /// Frames since the model last heard a voice.
    quiet_frames: u32,
    /// The limit in force, between the voice's and the pause's (dB).
    limit_db: f32,
    /// Frames of output still to silence after [`Denoiser::restart`].
    stale_frames: usize,
    /// For [`HISS`].
    seed: u32,
}

/// DeepFilterNet skips frames quieter than this (mean square in the model,
/// -70 dBFS) and leaves its state as it was. After stretches of that (digital
/// silence: a hardware mute, a studio recording), it held a quiet voice down
/// for seconds. Such frames get a faint hiss instead ([`HISS`]), which keeps
/// the model running; it removes the hiss again. A real microphone's own
/// noise sits far above this, so calls otherwise never see the hiss: under a
/// voice, it made the model trim the voice.
const SILENT: f32 = 1e-7;
/// The hiss's peak, as uniform noise: -60 dBFS RMS in the model.
const HISS: f32 = 1.73e-3;

/// The model hears a voice when it rates a frame's speech-to-noise ratio
/// above this (dB). In noisy pauses it rarely does: 90% of frames sit below
/// -11 dB in `dev/denoise-eval.py`'s scenes.
const VOICE_LSNR_DB: f32 = -5.0;
/// A pause starts this long (in frames) after the model last heard a voice,
/// so word endings and short gaps keep the voice's limit.
const PAUSE_AFTER_FRAMES: u32 = 30;
/// How fast the limit deepens in a pause, dB per frame: the noise fades out
/// rather than cutting off. When the voice returns, so does its limit, at once.
const PAUSE_GLIDE_DB: f32 = 0.5;

// SAFETY: tract keeps intermediate tensors in `Rc`s, which makes `DfTract`
// `!Send`. Every one of those `Rc`s is created by and stays inside this one
// model state (the `Denoiser` is never cloned), so moving the whole state to
// another thread moves every reference with it, and the mutex around the
// live filter keeps it to one thread at a time.
unsafe impl Send for Denoiser {}

impl Denoiser {
    pub fn new() -> anyhow::Result<Self> {
        Self::with(DfParams::default(), &RuntimeParams::default_with_ch(1))
    }

    pub fn with(model: DfParams, params: &RuntimeParams) -> anyhow::Result<Self> {
        let df = DfTract::new(model, params).context("load DeepFilterNet")?;
        anyhow::ensure!(
            df.sr == SAMPLE_RATE as usize && df.hop_size == FRAME,
            "model runs at {} Hz with {} sample hops",
            df.sr,
            df.hop_size
        );
        Ok(Self {
            df,
            input: Array2::zeros((1, FRAME)),
            output: Array2::zeros((1, FRAME)),
            scale_in: 1.0 / 32768.0,
            scale_out: 32768.0,
            strength: Strength::MODEL_DEFAULT,
            quiet_frames: 0,
            limit_db: 0.0,
            stale_frames: 0,
            seed: 0x2545_f491,
        })
    }

    /// The model's delay in samples: the window overlap plus its lookahead.
    pub fn delay(&self) -> usize {
        self.df.fft_size - self.df.hop_size + self.df.lookahead * self.df.hop_size
    }

    /// Change how hard the model works, from the next frame on.
    pub fn set_strength(&mut self, strength: Strength) {
        self.strength = strength;
        self.quiet_frames = 0;
        self.set_limit(strength.limit_db);
        self.df.min_db_thresh = strength.noise_only_below_db;
        let gain_db = strength.input_gain_db;
        let gain = 10f32.powf(if gain_db.is_finite() { gain_db } else { 0.0 } / 20.0);
        self.scale_in = gain / 32768.0;
        self.scale_out = 32768.0 / gain;
    }

    /// Start over after a gap in the audio (unmuting, a new call). The model
    /// still holds the end of the last stretch, so the output is silenced
    /// until it can't reach it: its deep filter reads `df_order` past
    /// spectra, each two frames long with the window overlap (6 frames, 60 ms
    /// here). Its other state settles within a second. (DeepFilterNet's own
    /// `init` would leave its buffers misaligned.)
    pub fn restart(&mut self) {
        let reach = self.df.df_order.max(self.df.lookahead);
        self.stale_frames = reach + self.df.fft_size.div_ceil(self.df.hop_size) - 1;
        self.quiet_frames = 0;
        self.set_limit(self.strength.limit_db);
    }

    /// `None`, or a value that isn't finite: no limit.
    fn set_limit(&mut self, limit_db: Option<f32>) {
        let limit_db = limit_db.filter(|db| db.is_finite()).map(|db| db.max(0.0));
        self.limit_db = limit_db.unwrap_or(f32::INFINITY);
        self.df.atten_lim = limit_db.map(|db| 10f32.powf(-db / 20.0));
    }

    /// Deepen the limit in pauses, from the next frame on.
    fn follow_pauses(&mut self, lsnr: f32) {
        let (Some(voice), Some(pause)) = (self.strength.limit_db, self.strength.pause_limit_db)
        else {
            return;
        };
        self.quiet_frames = if lsnr > VOICE_LSNR_DB {
            0
        } else {
            self.quiet_frames.saturating_add(1)
        };
        let limit = if self.quiet_frames > PAUSE_AFTER_FRAMES {
            (self.limit_db + PAUSE_GLIDE_DB).min(pause)
        } else {
            voice
        };
        if limit != self.limit_db {
            self.set_limit(Some(limit));
        }
    }

    /// Clean one frame in place. Returns the model's estimate of the frame's
    /// speech-to-noise ratio (dB).
    pub fn process(&mut self, samples: &mut [f32]) -> anyhow::Result<f32> {
        anyhow::ensure!(samples.len() == FRAME, "frame of {} samples", samples.len());
        let mut power = 0.0;
        for (dst, src) in self.input.iter_mut().zip(samples.iter()) {
            *dst = src * self.scale_in;
            power += *dst * *dst;
        }
        if power / (FRAME as f32) < SILENT {
            for dst in self.input.iter_mut() {
                self.seed ^= self.seed << 13;
                self.seed ^= self.seed >> 17;
                self.seed ^= self.seed << 5;
                *dst += (self.seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * HISS;
            }
        }
        let lsnr = self.df.process(self.input.view(), self.output.view_mut())?;
        for (dst, src) in samples.iter_mut().zip(self.output.iter()) {
            *dst = (src * self.scale_out).clamp(-32768.0, 32767.0);
        }
        if self.stale_frames > 0 {
            self.stale_frames -= 1;
            samples.fill(0.0);
        }
        self.follow_pauses(lsnr);
        Ok(lsnr)
    }
}

/// The filter the capture hook uses. Once a call loads it, it stays for the
/// rest of the run: macOS keeps the memory the load touched even after the
/// model is freed (~12 MB measured), and the next call starts sooner.
static FILTER: Mutex<Option<Live>> = Mutex::new(None);
/// Set when the model can't run here, so later calls don't retry.
static UNAVAILABLE: AtomicBool = AtomicBool::new(false);

struct Live {
    denoiser: Denoiser,
    /// The WebRTC audio processor this filter serves; a second one passes
    /// through rather than mixing two streams into one model state.
    stream: usize,
    /// When the last frame came; a longer gap restarts the model.
    last_frame: Option<Instant>,
    /// The model failed; frames pass through until `prepare` frees it.
    failed: bool,
    warned: bool,
    frames: u64,
    levels: Levels,
    busy: Duration,
    slowest: Duration,
}

/// Choose what cleans the microphone, loading the model on first use (about
/// 200 ms). Blocking: run it off the UI and audio threads, and before the
/// microphone track is created, which fixes WebRTC's own suppression.
pub fn prepare() -> Mode {
    // The audio thread only try-locks, so holding this while loading just
    // lets frames through unfiltered.
    let mut filter = FILTER.lock();
    // A model that failed during a call is freed here, off the audio thread,
    // once the lock is released. Checked under the lock: it may have failed
    // a moment ago.
    let failed = filter.take_if(|live| live.failed);
    let mode = match std::env::var("QUIPCALL_DENOISE").as_deref() {
        Ok("webrtc") => Mode::WebRtc,
        Ok("off") => Mode::Off,
        _ if failed.is_some() || UNAVAILABLE.load(Ordering::Relaxed) => Mode::WebRtc,
        _ => {
            if filter.is_none() {
                match load() {
                    Ok(live) => *filter = Some(live),
                    Err(e) => {
                        tracing::warn!("noise suppression: {e:#}; using WebRTC's");
                        UNAVAILABLE.store(true, Ordering::Relaxed);
                    }
                }
            }
            match filter.as_mut() {
                Some(live) => {
                    // A new call may bring a new audio processor, and the
                    // model still holds the end of the last call.
                    live.stream = 0;
                    live.last_frame = None;
                    let strength = Strength::for_calls();
                    live.denoiser.set_strength(strength);
                    tracing::info!(?strength, "noise suppression strength");
                    Mode::Neural
                }
                None => Mode::WebRtc,
            }
        }
    };
    drop(filter);
    drop(failed);
    // SAFETY: plain setters in our webrtc-sys patch; the callbacks are
    // `extern "C"` functions that live for the whole program.
    unsafe {
        let neural = mode == Mode::Neural;
        quipcall_set_device_noise_suppression(mode == Mode::WebRtc);
        quipcall_set_device_gain_control(!neural);
        quipcall_set_capture_gain_control(neural);
        quipcall_set_capture_processor(neural.then_some(process), Some(release));
    }
    tracing::info!(?mode, "noise suppression");
    mode
}

fn load() -> anyhow::Result<Live> {
    let start = Instant::now();
    let mut denoiser = Denoiser::new()?;
    let loaded = start.elapsed();
    // Time the model on quiet noise (it skips silence) to catch Macs too slow
    // for it. A second instance for this would cost another ~10 MB; the model
    // forgets these 300 ms within about a second of the call.
    let mut seed = 0x2545_f491_u32;
    let mut frame = [0f32; FRAME];
    let runs = 30;
    let start = Instant::now();
    for _ in 0..runs {
        for s in &mut frame {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *s = (seed as f32 / u32::MAX as f32 - 0.5) * 2000.0;
        }
        denoiser.process(&mut frame)?;
    }
    let per_frame = start.elapsed() / runs;
    tracing::info!(?loaded, ?per_frame, "DeepFilterNet loaded");
    anyhow::ensure!(
        per_frame <= MAX_FRAME_COST,
        "this Mac needs {per_frame:?} per 10 ms frame"
    );
    Ok(Live {
        denoiser,
        stream: 0,
        last_frame: None,
        failed: false,
        warned: false,
        frames: 0,
        busy: Duration::ZERO,
        slowest: Duration::ZERO,
        levels: Levels::default(),
    })
}

/// How many 10 ms frames came in at each level, 0 to -99 dBFS in 1 dB steps.
struct Levels([u32; 100]);

impl Default for Levels {
    fn default() -> Self {
        Levels([0; 100])
    }
}

impl Levels {
    fn add(&mut self, frame: &[f32]) {
        let power = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        let dbfs = 10.0 * (power / (32768.0 * 32768.0) + 1e-10).log10();
        self.0[(-dbfs).clamp(0.0, 99.0) as usize] += 1;
    }

    /// The level that the loudest `share` of frames reach.
    fn loudest(&self, share: f32) -> i32 {
        let total: u32 = self.0.iter().sum();
        let mut seen = 0;
        for (db, &n) in self.0.iter().enumerate() {
            seen += n;
            if seen as f32 >= share * total as f32 {
                return -(db as i32);
            }
        }
        -99
    }
}

type ProcessFn = unsafe extern "C" fn(*const c_void, *const *mut f32, usize, usize, i32);
type ReleaseFn = unsafe extern "C" fn(*const c_void);

unsafe extern "C" {
    fn quipcall_set_capture_processor(process: Option<ProcessFn>, release: Option<ReleaseFn>);
    fn quipcall_set_capture_gain_control(enabled: bool);
    fn quipcall_set_device_noise_suppression(enabled: bool);
    fn quipcall_set_device_gain_control(enabled: bool);
}

/// Called by WebRTC on its capture thread with each processed 10 ms frame.
unsafe extern "C" fn process(
    stream: *const c_void,
    channels: *const *mut f32,
    num_channels: usize,
    num_frames: usize,
    sample_rate_hz: i32,
) {
    if channels.is_null() || num_channels == 0 {
        return;
    }
    // Never wait on the audio thread: skip the frame if a call is starting.
    let Some(mut filter) = FILTER.try_lock() else {
        return;
    };
    let Some(live) = filter.as_mut().filter(|live| !live.failed) else {
        return;
    };
    let stream = stream as usize;
    if live.stream == 0 {
        live.stream = stream;
    }
    if live.stream != stream || sample_rate_hz != SAMPLE_RATE || num_frames != FRAME {
        if !live.warned {
            live.warned = true;
            tracing::warn!(
                sample_rate_hz,
                num_frames,
                "noise suppression skipped: unexpected audio stream"
            );
        }
        return;
    }
    // SAFETY: WebRTC passes `num_channels` valid pointers to `num_frames`
    // floats each, owned by its audio buffer for the duration of this call.
    let channels = unsafe { std::slice::from_raw_parts(channels, num_channels) };
    let first = unsafe { std::slice::from_raw_parts_mut(channels[0], num_frames) };
    live.levels.add(first);
    let start = Instant::now();
    // Frames stop while muted (WebRTC skips this hook) and between calls.
    if live
        .last_frame
        .is_none_or(|last| start.duration_since(last) > RESTART_AFTER)
    {
        live.denoiser.restart();
    }
    live.last_frame = Some(start);
    let failure = match catch_unwind(AssertUnwindSafe(|| live.denoiser.process(first))) {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(format!("{e:#}")),
        Err(_) => Some("the model panicked".into()),
    };
    if let Some(failure) = failure {
        // Unfiltered for the rest of this call (its track has WebRTC's
        // suppressor off); later calls use WebRTC's. Freeing the model here
        // could take long enough to drop audio, so `prepare` does it.
        tracing::error!("noise suppression stopped: {failure}");
        UNAVAILABLE.store(true, Ordering::Relaxed);
        live.failed = true;
        return;
    }
    for &other in &channels[1..] {
        // SAFETY: as above; WebRTC's channels never alias.
        unsafe { std::slice::from_raw_parts_mut(other, num_frames) }.copy_from_slice(first);
    }
    let took = start.elapsed();
    live.frames += 1;
    live.busy += took;
    live.slowest = live.slowest.max(took);
    // Once per minute of unmuted audio.
    if live.frames % 6000 == 0 {
        tracing::info!(
            average = ?live.busy / 6000,
            slowest = ?live.slowest,
            "noise suppression, time per 10 ms frame"
        );
        // The model takes quiet voices for noise (below about -45 dBFS before
        // its input gain); this shows where a real microphone sits.
        tracing::info!(
            loudest_10pct_dbfs = live.levels.loudest(0.1),
            median_dbfs = live.levels.loudest(0.5),
            "microphone level into noise suppression"
        );
        live.busy = Duration::ZERO;
        live.slowest = Duration::ZERO;
        live.levels = Levels::default();
    }
}

/// Called when WebRTC destroys the audio processor behind `stream`.
unsafe extern "C" fn release(stream: *const c_void) {
    if let Some(live) = FILTER.lock().as_mut()
        && live.stream == stream as usize
    {
        live.stream = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{Denoiser, FRAME, Strength, parse_limits};

    fn rms(frame: &[f32]) -> f32 {
        (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
    }

    #[test]
    fn restart_drops_the_previous_audio() {
        let mut denoiser = Denoiser::new().unwrap();
        denoiser.set_strength(Strength::CALLS);
        let mut seed = 1u32;
        let mut frame = [0f32; FRAME];
        // A voice-like buzz in noise, so both of the model's stages run.
        for n in 0..300 {
            for (i, s) in frame.iter_mut().enumerate() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let t = (n * FRAME + i) as f32 / 48_000.0;
                let voice: f32 = (1..20)
                    .map(|h| (std::f32::consts::TAU * 150.0 * h as f32 * t).sin() / h as f32)
                    .sum();
                *s = voice * 3000.0 + (seed as f32 / u32::MAX as f32 - 0.5) * 3000.0;
            }
            denoiser.process(&mut frame).unwrap();
        }
        denoiser.restart();
        // Silence goes in; nothing above the hiss's remains (about 2) may
        // come out. Without the restart, frames 1-3 carried 450-1170.
        for n in 0..20 {
            frame.fill(0.0);
            denoiser.process(&mut frame).unwrap();
            assert!(rms(&frame) < 20.0, "frame {n}: {}", rms(&frame));
        }
    }

    #[test]
    fn limits_from_the_environment() {
        assert_eq!(parse_limits("none"), Some((None, None)));
        assert_eq!(parse_limits("18"), Some((Some(18.0), None)));
        assert_eq!(parse_limits("15/30"), Some((Some(15.0), Some(30.0))));
        for bad in [
            "", "-3", "NaN", "inf", "12/", "/30", "12/x", "12/30/40", "12/-1",
        ] {
            assert_eq!(parse_limits(bad), None, "{bad:?}");
        }
    }
}
