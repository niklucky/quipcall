//! Two people speak (macOS `say`), and the transcript says who said what.
//!
//! Needs the models: `QUIPCALL_STT_MODELS=<dir> cargo test -p quipcall-stt
//! -- --ignored`, with the files named as in the catalog.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quipcall_stt::{Line, Options, Transcriber, catalog};

/// `say` into 48 kHz float samples.
fn say(voice: &str, text: &str, dir: &Path) -> Vec<f32> {
    let path = dir.join(format!("{voice}.wav"));
    let ok = Command::new("say")
        .args(["-v", voice, "-o"])
        .arg(&path)
        .args(["--file-format=WAVE", "--data-format=LEF32@48000", text])
        .status()
        .expect("say")
        .success();
    assert!(ok, "say -v {voice}");
    let wav = std::fs::read(&path).unwrap();
    // The samples follow the "data" chunk's header.
    let at = wav
        .windows(4)
        .position(|w| w == b"data")
        .expect("data chunk")
        + 8;
    let (samples, _) = wav[at..].as_chunks::<4>();
    samples.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

#[test]
#[ignore = "needs models: QUIPCALL_STT_MODELS=<dir>"]
fn two_voices_in_order() {
    let dir = PathBuf::from(std::env::var("QUIPCALL_STT_MODELS").expect("QUIPCALL_STT_MODELS"));
    let tmp = std::env::temp_dir().join(format!("qc-stt-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let english = say(
        "Samantha",
        "The quick brown fox jumps over the lazy dog. Then it goes home to sleep.",
        &tmp,
    );
    let russian = say("Milena", "Привет! Давай созвонимся завтра утром.", &tmp);

    let lines: Arc<Mutex<Vec<Line>>> = Arc::default();
    let transcriber = Transcriber::start(
        Options {
            model: dir.join(catalog::PARAKEET_V3.file),
            vad: dir.join(catalog::VAD.file),
            language: None,
        },
        {
            let lines = lines.clone();
            move |line| lines.lock().unwrap().push(line)
        },
    )
    .expect("start");

    // Alice speaks, then Boris, each with silence around, 10 ms at a time
    // and faster than real time. Both voices send all along, as in a call.
    let silence = vec![0.0f32; 48_000];
    let alice: Vec<f32> = [
        &silence[..],
        &english,
        &silence,
        &vec![0.0; russian.len()],
        &silence,
    ]
    .concat();
    let boris: Vec<f32> = [
        &silence[..],
        &vec![0.0; english.len()],
        &silence,
        &russian,
        &silence,
    ]
    .concat();
    for (a, b) in alice.chunks(480).zip(boris.chunks(480)) {
        transcriber.feed("alice", a.to_vec());
        transcriber.feed("boris", b.to_vec());
        std::thread::sleep(Duration::from_micros(500));
    }
    transcriber.stop();
    let _ = std::fs::remove_dir_all(&tmp);

    let lines = lines.lock().unwrap();
    for l in lines.iter() {
        eprintln!(
            "{:>6.2}–{:<6.2} {} {} {}",
            l.start.as_secs_f32(),
            l.end.as_secs_f32(),
            l.speaker,
            if l.done { "✓" } else { "…" },
            l.text
        );
    }
    let said = |who: &str| -> String {
        lines
            .iter()
            .filter(|l| l.done && l.speaker == who)
            .map(|l| l.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let alice = said("alice");
    let boris = said("boris");
    for word in ["quick", "brown", "fox", "lazy", "dog", "sleep"] {
        assert!(alice.contains(word), "alice: {alice}");
    }
    for word in ["привет", "завтра", "утром"] {
        assert!(boris.contains(word), "boris: {boris}");
    }
    // Every draft was followed by its final line.
    for draft in lines.iter().filter(|l| !l.done) {
        assert!(
            lines.iter().any(|l| l.done && l.id == draft.id),
            "{draft:?}"
        );
    }
}

#[test]
#[ignore = "needs models: QUIPCALL_STT_MODELS=<dir>"]
fn a_voice_back_from_mute_starts_afresh() {
    let dir = PathBuf::from(std::env::var("QUIPCALL_STT_MODELS").expect("QUIPCALL_STT_MODELS"));
    let tmp = std::env::temp_dir().join(format!("qc-stt-mute-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let first = say("Samantha", "Let's look at the numbers first.", &tmp);
    let second = say("Samantha", "Sorry, I was muted. Can you hear me now?", &tmp);
    let _ = std::fs::remove_dir_all(&tmp);

    let lines: Arc<Mutex<Vec<Line>>> = Arc::default();
    let transcriber = Transcriber::start(
        Options {
            model: dir.join(catalog::PARAKEET_V3.file),
            vad: dir.join(catalog::VAD.file),
            language: Some("en".into()),
        },
        {
            let lines = lines.clone();
            move |line| lines.lock().unwrap().push(line)
        },
    )
    .expect("start");
    let feed = |samples: &[f32]| {
        for chunk in samples.chunks(480) {
            transcriber.feed("alice", chunk.to_vec());
            std::thread::sleep(Duration::from_micros(500));
        }
    };
    // She talks, goes quiet, then mutes: nothing comes for a while.
    feed(&[&first[..], &vec![0.0; 48_000]].concat());
    std::thread::sleep(Duration::from_millis(1500));
    // Unmuted, she talks straight away.
    feed(&[&second[..], &vec![0.0; 48_000]].concat());
    transcriber.stop();

    let lines = lines.lock().unwrap();
    let done: Vec<&Line> = lines.iter().filter(|l| l.done).collect();
    assert_eq!(done.len(), 2, "{done:?}");
    // Begun after the mute, not back when she went quiet.
    // (Fed faster than real time: the first took ~0.1 s, the mute 1.5 s.)
    let gap = done[1].start.saturating_sub(done[0].start);
    assert!(gap >= Duration::from_millis(1400), "{gap:?}: {done:?}");
    assert!(done[1].text.to_lowercase().contains("muted"), "{done:?}");
}
