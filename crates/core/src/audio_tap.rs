//! Everyone's voice in a call, 10 ms at a time, for recording (and soon
//! transcription).
//!
//! - Your microphone as the call sends it: after echo cancellation, noise
//!   suppression and gain, before encoding. A hook in our webrtc-sys patch
//!   (`quipcall_set_capture_tap`) copies each frame on WebRTC's capture
//!   thread. Nothing comes while you're muted.
//! - Each remote microphone as it's decoded, through LiveKit's audio sinks
//!   (one per track, see `Engine::listen`).
//!
//! Nothing is copied until something listens, and copying stops when the last
//! [`Listener`] is dropped.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;

pub const SAMPLE_RATE: u32 = 48_000;
/// Samples in 10 ms.
pub const FRAME: usize = (SAMPLE_RATE / 100) as usize;
/// Frames a listener may fall behind before new ones are dropped: 2 s.
const QUEUE: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Speaker {
    /// You.
    Local,
    /// Someone else, by identity.
    Remote(String),
}

/// Some of one person's voice: mono, [`SAMPLE_RATE`], -1..1. Usually
/// [`FRAME`] samples.
#[derive(Clone, Debug)]
pub struct VoiceFrame {
    pub speaker: Speaker,
    pub samples: Vec<f32>,
}

/// Everyone's voice until dropped. Frames come in the order they're heard,
/// people interleaved.
pub struct Listener {
    pub frames: async_channel::Receiver<VoiceFrame>,
    id: u64,
    on_last: Option<Box<dyn FnOnce() + Send>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let mut hub = HUB.lock();
        hub.retain(|(id, _)| *id != self.id);
        // Switched under the lock, so a listener arriving meanwhile keeps it on.
        let last = hub.is_empty();
        if last {
            set_hook(false);
        }
        drop(hub);
        if let Some(on_last) = self.on_last.take().filter(|_| last) {
            on_last();
        }
    }
}

static HUB: Mutex<Vec<(u64, async_channel::Sender<VoiceFrame>)>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Your microphone is published and unmuted. WebRTC already skips the hook
/// while muted; this makes sure a muted voice is never recorded.
static MIC_LIVE: AtomicBool = AtomicBool::new(false);

/// A new listener. `on_last` runs when it's the last one to go (to stop
/// decoding remote audio); it should check [`listening`], since another may
/// have arrived meanwhile.
pub(crate) fn listen(on_last: impl FnOnce() + Send + 'static) -> Listener {
    let (tx, frames) = async_channel::bounded(QUEUE);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut hub = HUB.lock();
    hub.push((id, tx));
    set_hook(true);
    drop(hub);
    Listener {
        frames,
        id,
        on_last: Some(Box::new(on_last)),
    }
}

pub(crate) fn listening() -> bool {
    !HUB.lock().is_empty()
}

/// Whether frames from your microphone should go out.
pub(crate) fn set_mic_live(live: bool) {
    MIC_LIVE.store(live, Ordering::Relaxed);
}

/// To every listener; a listener that has fallen behind misses it. `wait`
/// false on audio threads: a frame is skipped rather than block.
pub(crate) fn send(frame: VoiceFrame, wait: bool) {
    let hub = if wait {
        HUB.lock()
    } else {
        match HUB.try_lock() {
            Some(hub) => hub,
            None => return,
        }
    };
    if let Some(((_, last), rest)) = hub.split_last() {
        for (_, tx) in rest {
            let _ = tx.try_send(frame.clone());
        }
        let _ = last.try_send(frame);
    }
}

type TapFn = unsafe extern "C" fn(*const f32, usize, i32);

unsafe extern "C" {
    fn quipcall_set_capture_tap(tap: Option<TapFn>);
}

fn set_hook(on: bool) {
    // SAFETY: a plain setter in our webrtc-sys patch; `capture_tap` is an
    // `extern "C"` function that lives for the whole program.
    unsafe { quipcall_set_capture_tap(on.then_some(capture_tap as TapFn)) };
}

/// Called by WebRTC on its capture thread with each finished 10 ms frame.
unsafe extern "C" fn capture_tap(samples: *const f32, num_frames: usize, sample_rate_hz: i32) {
    if samples.is_null() || num_frames == 0 || !MIC_LIVE.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: WebRTC passes `num_frames` valid floats, owned by its audio
    // buffer for the duration of this call.
    let input = unsafe { std::slice::from_raw_parts(samples, num_frames) };
    let samples = to_frame(input, sample_rate_hz);
    send(
        VoiceFrame {
            speaker: Speaker::Local,
            samples,
        },
        false,
    );
}

/// WebRTC's floats (16-bit range, at the processor's rate) as -1..1 at
/// [`SAMPLE_RATE`]. The processor runs at 48 kHz in calls (Opus); lower
/// rates are interpolated, which is plenty for a recording of speech.
fn to_frame(input: &[f32], sample_rate_hz: i32) -> Vec<f32> {
    let scale = 1.0 / 32768.0;
    if sample_rate_hz == SAMPLE_RATE as i32 || sample_rate_hz <= 0 {
        return input.iter().map(|s| s * scale).collect();
    }
    let out_len = input.len() * SAMPLE_RATE as usize / sample_rate_hz as usize;
    let step = sample_rate_hz as f32 / SAMPLE_RATE as f32;
    (0..out_len)
        .map(|i| {
            let pos = i as f32 * step;
            let at = pos as usize;
            let next = input.get(at + 1).unwrap_or(&input[at]);
            let frac = pos - at as f32;
            (input[at] * (1.0 - frac) + next * frac) * scale
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test touches the process-wide hub, so they can't race.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn rescales_and_resamples() {
        let frame = to_frame(&[16384.0; 480], 48_000);
        assert_eq!(frame.len(), FRAME);
        assert!((frame[0] - 0.5).abs() < 1e-6);
        let ramp: Vec<f32> = (0..160).map(|i| i as f32 * 100.0).collect();
        let up = to_frame(&ramp, 16_000);
        assert_eq!(up.len(), FRAME);
        // Halfway between the 2nd and 3rd input samples, a third of the way.
        assert!((up[4] - (100.0 + 100.0 / 3.0) / 32768.0).abs() < 1e-4);
    }

    #[test]
    fn microphone_frames_only_while_live() {
        let _serial = SERIAL.lock();
        let last = std::sync::Arc::new(AtomicBool::new(false));
        let listener = listen({
            let last = last.clone();
            move || last.store(true, Ordering::Relaxed)
        });
        let input = [1000.0f32; FRAME];
        let tap = |input: &[f32]| unsafe { capture_tap(input.as_ptr(), input.len(), 48_000) };

        set_mic_live(false);
        tap(&input);
        assert!(listener.frames.try_recv().is_err(), "muted audio went out");

        set_mic_live(true);
        tap(&input);
        let frame = listener.frames.try_recv().expect("a frame");
        assert_eq!(frame.speaker, Speaker::Local);
        assert_eq!(frame.samples.len(), FRAME);
        set_mic_live(false);

        assert!(listening());
        drop(listener);
        assert!(!listening());
        assert!(last.load(Ordering::Relaxed), "the last listener's goodbye");
    }
}
