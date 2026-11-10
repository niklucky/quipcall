//! Everyone's voice in a call, 10 ms at a time, for recording and
//! transcription.
//!
//! - Your microphone as the call sends it: after echo cancellation, noise
//!   suppression and gain, before encoding. A hook in our webrtc-sys patch
//!   (`quipcall_set_capture_tap`) copies each frame on WebRTC's capture
//!   thread. Nothing comes while you're muted.
//! - Each remote microphone as it's decoded, through LiveKit's audio sinks
//!   (one per track, see `Engine::listen`).
//!
//! Each call has its own [`Hub`]: a recording still being finished after you
//! left hears nothing of the next call. Your microphone is one stream for the
//! whole app (LiveKit has one audio device), so it goes to the call it's live
//! in. Nothing is copied until something listens, and copying stops when the
//! last [`Listener`] is dropped.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// One call's voices, to whoever listens in that call.
#[derive(Default)]
pub(crate) struct Hub {
    listeners: Mutex<Vec<(u64, async_channel::Sender<VoiceFrame>)>>,
}

/// Everyone's voice in one call until dropped. Frames come in the order
/// they're heard, people interleaved.
pub struct Listener {
    pub frames: async_channel::Receiver<VoiceFrame>,
    hub: Arc<Hub>,
    id: u64,
    on_last: Option<Box<dyn FnOnce() + Send>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let mut listeners = self.hub.listeners.lock();
        listeners.retain(|(id, _)| *id != self.id);
        let last = listeners.is_empty();
        drop(listeners);
        unhook();
        if let Some(on_last) = self.on_last.take().filter(|_| last) {
            on_last();
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Listeners in every call: the hook is on while there are any. Switched
/// under this lock, so a listener arriving meanwhile keeps it on.
static LISTENERS: Mutex<usize> = Mutex::new(0);
/// The call your microphone is published and unmuted in. WebRTC already
/// skips the hook while muted; this makes sure a muted voice is never
/// recorded, and that your voice goes to that call only.
static MIC: Mutex<Option<Arc<Hub>>> = Mutex::new(None);

impl Hub {
    /// A new listener. `on_last` runs when it's this call's last one to go
    /// (to stop decoding remote audio); it should check [`Hub::listening`],
    /// since another may have arrived meanwhile.
    pub(crate) fn listen(self: &Arc<Self>, on_last: impl FnOnce() + Send + 'static) -> Listener {
        let (tx, frames) = async_channel::bounded(QUEUE);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().push((id, tx));
        let mut count = LISTENERS.lock();
        *count += 1;
        set_hook(true);
        drop(count);
        Listener {
            frames,
            hub: self.clone(),
            id,
            on_last: Some(Box::new(on_last)),
        }
    }

    pub(crate) fn listening(&self) -> bool {
        !self.listeners.lock().is_empty()
    }

    /// To every listener in this call; a listener that has fallen behind
    /// misses it. `wait` false on audio threads: a frame is skipped rather
    /// than block.
    pub(crate) fn send(&self, frame: VoiceFrame, wait: bool) {
        let listeners = if wait {
            self.listeners.lock()
        } else {
            match self.listeners.try_lock() {
                Some(listeners) => listeners,
                None => return,
            }
        };
        if let Some(((_, last), rest)) = listeners.split_last() {
            for (_, tx) in rest {
                let _ = tx.try_send(frame.clone());
            }
            let _ = last.try_send(frame);
        }
    }
}

fn unhook() {
    let mut count = LISTENERS.lock();
    *count = count.saturating_sub(1);
    if *count == 0 {
        set_hook(false);
    }
}

/// Whether frames from your microphone go out to `hub`'s call. Turning it
/// off for a call that no longer has it changes nothing.
pub(crate) fn set_mic_live(hub: &Arc<Hub>, live: bool) {
    let mut mic = MIC.lock();
    if live {
        *mic = Some(hub.clone());
    } else if mic.as_ref().is_some_and(|m| Arc::ptr_eq(m, hub)) {
        *mic = None;
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
    if samples.is_null() || num_frames == 0 {
        return;
    }
    // Never wait on the capture thread.
    let Some(Some(hub)) = MIC.try_lock().map(|mic| mic.clone()) else {
        return;
    };
    if !hub.listeners.try_lock().is_some_and(|l| !l.is_empty()) {
        return;
    }
    // SAFETY: WebRTC passes `num_frames` valid floats, owned by its audio
    // buffer for the duration of this call.
    let input = unsafe { std::slice::from_raw_parts(samples, num_frames) };
    let samples = to_frame(input, sample_rate_hz);
    hub.send(
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
pub(crate) mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    /// Tests that touch your microphone (one for the whole app) can't race.
    pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

    pub(crate) fn speak() {
        let input = [1000.0f32; FRAME];
        unsafe { capture_tap(input.as_ptr(), input.len(), 48_000) };
    }

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
        let hub = Arc::new(Hub::default());
        let last = Arc::new(AtomicBool::new(false));
        let listener = hub.listen({
            let last = last.clone();
            move || last.store(true, Ordering::Relaxed)
        });

        set_mic_live(&hub, false);
        speak();
        assert!(listener.frames.try_recv().is_err(), "muted audio went out");

        set_mic_live(&hub, true);
        speak();
        let frame = listener.frames.try_recv().expect("a frame");
        assert_eq!(frame.speaker, Speaker::Local);
        assert_eq!(frame.samples.len(), FRAME);
        set_mic_live(&hub, false);

        assert!(hub.listening());
        drop(listener);
        assert!(!hub.listening());
        assert!(last.load(Ordering::Relaxed), "the last listener's goodbye");
    }

    /// You left a call whose recording is still being finished, and joined
    /// another: nothing of the new call reaches the old recording.
    #[test]
    fn a_call_hears_only_its_own_voices() {
        let _serial = SERIAL.lock();
        let (old, new) = (Arc::new(Hub::default()), Arc::new(Hub::default()));
        let finishing = old.listen(|| {});
        assert!(!new.listening(), "the new call's voices would be decoded");
        set_mic_live(&old, true);
        set_mic_live(&old, false);
        set_mic_live(&new, true);
        let listener = new.listen(|| {});
        speak();
        let boris = Speaker::Remote("boris".into());
        new.send(
            VoiceFrame {
                speaker: boris.clone(),
                samples: vec![0.1; FRAME],
            },
            true,
        );
        // The old call letting go of the microphone late changes nothing.
        set_mic_live(&old, false);
        speak();
        assert!(finishing.frames.try_recv().is_err());
        let heard: Vec<Speaker> = std::iter::from_fn(|| listener.frames.try_recv().ok())
            .map(|f| f.speaker)
            .collect();
        assert_eq!(heard, [Speaker::Local, boris, Speaker::Local]);
        set_mic_live(&new, false);
    }
}
