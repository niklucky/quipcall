//! Memory soak: two voices talking for `MINUTES` of audio, fed ten times
//! faster than real time. Watch the process with `dev/measure.sh`.
//!
//!   QUIPCALL_STT_MODELS=<dir> cargo run --release -p quipcall-stt --example soak -- <wav48k-f32> [MINUTES]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use quipcall_stt::{Options, Transcriber, catalog};

fn main() {
    let mut args = std::env::args().skip(1);
    let wav = std::fs::read(args.next().expect("a 48 kHz float WAV")).unwrap();
    let minutes: f32 = args.next().map_or(30.0, |m| m.parse().unwrap());
    let at = wav.windows(4).position(|w| w == b"data").unwrap() + 8;
    let (samples, _) = wav[at..].as_chunks::<4>();
    let speech: Vec<f32> = samples.iter().map(|b| f32::from_le_bytes(*b)).collect();

    let dir = PathBuf::from(std::env::var("QUIPCALL_STT_MODELS").unwrap());
    let lines = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = lines.clone();
    let t = Transcriber::start(
        Options {
            model: dir.join(catalog::PARAKEET_V3.file),
            vad: dir.join(catalog::VAD.file),
            language: None,
        },
        move |l| {
            if l.done {
                counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    let frames = (minutes * 60.0 * 100.0) as usize;
    let silence = vec![0.0f32; 480];
    let begun = Instant::now();
    for i in 0..frames {
        // Alice talks while Boris is quiet, then the other way round.
        let at = i * 480 % (speech.len() * 2);
        let (a, b) = if at < speech.len() {
            (
                speech[at..(at + 480).min(speech.len())].to_vec(),
                silence.clone(),
            )
        } else {
            let at = at - speech.len();
            (
                silence.clone(),
                speech[at..(at + 480).min(speech.len())].to_vec(),
            )
        };
        t.feed("alice", a);
        t.feed("boris", b);
        // 10 ms of audio a millisecond, smoothly: bursts would overflow
        // the listening queue.
        std::thread::sleep(Duration::from_millis(1));
        if i % 6000 == 0 {
            eprintln!(
                "{:.0} min of audio in {:.0?}, {} lines",
                i as f32 / 6000.0,
                begun.elapsed(),
                lines.load(std::sync::atomic::Ordering::Relaxed)
            );
        }
    }
    t.stop();
}
