#!/usr/bin/env python3
"""Compare noise suppressors offline. Opens no microphone or speaker.

    python3 dev/denoise-eval.py

Makes speech with macOS TTS, downloads real noise recordings from Wikimedia
Commons (checked by SHA-1), mixes them, and runs the filters in
crates/core/examples/denoise_eval.rs. Listening samples go to
target/denoise-eval/out/, metrics to target/denoise-eval/results.jsonl.
With numpy, scipy, pesq and pystoi installed, it also scores speech quality
(PESQ, wideband) and intelligibility (STOI) against the clean speech.

Recordings (Wikimedia Commons):
  bark     "Barking of a dog.ogg", Amada44, CC BY-SA 3.0
  wrapper  "Food wrapper crumpling.ogg", stilgar, public domain
  cars     "Cars passing over bridge.ogg", Dsw4, public domain
  typing   "Typing - Model M 1986.ogg", Raymangold22, CC0
  paper    "Rejection letter cut into pieces.ogg", hugh, public domain
"""

import hashlib
import json
from pathlib import Path
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
DIR = ROOT / "target/denoise-eval"
COMMONS = "https://upload.wikimedia.org/wikipedia/commons/"
NOISES = {
    "bark": ("a/a2/Barking_of_a_dog.ogg", "2dc95fc596ea2601b685a9dc569b2e82859cc6cc"),
    "wrapper": ("f/f0/Food_wrapper_crumpling.ogg", "786523e8c98fc2c2f75b2e30783110b8af446bf9"),
    "cars": ("9/96/Cars_passing_over_bridge.ogg", "ce5294fd9e53bba450d763382dbe9ecbc4d7eb73"),
    "typing": ("0/0a/Typing_-_Model_M_1986.ogg", "99004320c87964668f91cbb9e039c3513200289d"),
    "paper": ("a/a6/Rejection_letter_cut_into_pieces.ogg", "36a5580e6bd96ee4c5a42b7ec7b5a9f5c6f74d8c"),
}
SPEECH = (
    "Hi, can you hear me? I think the connection is fine now. "
    "[[slnc 700]] So, about the trip next week. We could leave on Friday morning, "
    "and stop for lunch somewhere on the way. [[slnc 900]] "
    "Hold on, the dog is going crazy again. Sorry about that. "
    "[[slnc 800]] Anyway, the hotel has free parking, and breakfast is included. "
    "[[slnc 600]] Let me know what you think, and I will book it tonight."
)


def to_pcm(src, dst):
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", "-i", str(src), "-ac", "1", "-ar", "48000",
         "-f", "s16le", str(dst)],
        check=True,
    )


def main():
    noise_dir = DIR / "noise"
    noise_dir.mkdir(parents=True, exist_ok=True)
    for name, (path, sha1) in NOISES.items():
        ogg = noise_dir / f"{name}.ogg"
        if not ogg.exists():
            req = urllib.request.Request(COMMONS + path, headers={"User-Agent": "quipcall-eval/0.1"})
            ogg.write_bytes(urllib.request.urlopen(req).read())
        if hashlib.sha1(ogg.read_bytes()).hexdigest() != sha1:
            ogg.unlink()
            raise SystemExit(f"{name}: checksum mismatch, deleted")
        to_pcm(ogg, noise_dir / f"{name}.pcm")

    subprocess.run(["say", "-v", "Samantha", "-o", str(DIR / "speech.aiff"), SPEECH], check=True)
    to_pcm(DIR / "speech.aiff", DIR / "speech.pcm")

    result = subprocess.run(
        ["cargo", "run", "--release", "-q", "-p", "quipcall-core", "--example", "denoise_eval",
         "--", str(DIR)],
        cwd=ROOT, check=True, capture_output=True, text=True,
    )
    lines = [json.loads(l) for l in result.stdout.splitlines() if l.startswith("{")]
    (DIR / "results.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines))

    model = lines[0]
    print(f"DeepFilterNet: load {model['load_ms']} ms, +{model['footprint_mb']} MB footprint")
    rows = lines[1:]
    scores = quality(rows)
    print(f"\n{'scenario':10} {'filter':20} {'pauses':>7} {'PESQ':>5} {'STOI':>5} {'delay':>6} "
          f"{'mean':>6} {'p99':>6} {'CPU':>6}")
    for r in rows:
        pesq, stoi = scores.get((r["scenario"], r["filter"]), (float("nan"), float("nan")))
        print(f"{r['scenario']:10} {r['filter']:20} {r['pause_reduction_db']:5.1f}dB {pesq:5.2f} "
              f"{stoi:5.3f} {r['delay_ms']:4.0f}ms {r['frame_mean_us']:4}us "
              f"{r['frame_p99_us']:4}us {r['cpu_pct']:5.2f}%")
    print(f"\nListening samples: {DIR / 'out'}")


def quality(rows):
    """PESQ and STOI of each output against the clean speech, if available."""
    try:
        import wave
        import numpy as np
        from pesq import pesq
        from pystoi import stoi
        from scipy.signal import resample_poly
    except ImportError:
        print("(pip install numpy scipy pesq pystoi for PESQ and STOI)")
        return {}

    def read(path):
        with wave.open(str(path)) as w:
            pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16)
        return pcm.astype(np.float32) / 32768

    clean = read(DIR / "out/clean-none.wav")
    scores = {}
    for r in rows:
        out = read(DIR / f"out/{r['scenario']}-{r['filter']}.wav")[round(r["delay_ms"] * 48):]
        n = min(len(out), len(clean))
        wideband = pesq(16000, resample_poly(clean[:n], 1, 3), resample_poly(out[:n], 1, 3), "wb")
        scores[(r["scenario"], r["filter"])] = (wideband, stoi(clean[:n], out[:n], 48000))
    return scores


if __name__ == "__main__":
    main()
