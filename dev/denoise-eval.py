#!/usr/bin/env python3
"""Test bench for microphone noise suppression. Opens no microphone or speaker.

    python3 dev/denoise-eval.py                 # every chain
    python3 dev/denoise-eval.py webrtc,dfn      # only these

Builds test scenes from real voices and real noise recordings, runs them
through a call's microphone processing (crates/core/examples/denoise_eval.rs:
WebRTC's audio processing set up as in a call, our filter in its hook) and
scores every chain. Listen in target/denoise-eval/out/<scene>/: mic.wav is
what the microphone heard, clean.wav the voice alone, <chain>.wav the result.

The first run makes a Python environment in target/denoise-eval/venv (numpy,
scipy, pesq, pystoi) and downloads about 13 MB of recordings, each checked
against its hash.

Scores, over the part after a 4 s warm-up (gain control settles):
  level   voice loudness against the clean voice, dB (median)
  dips    how far the voice falls in its quietest 10%, dB: the "volume drops"
          (only where the voice is louder than the noise)
  drops   share of the voice more than 6 dB below its median level, %
  noise   noise cut in pauses, relative to the voice, dB
  pesq    speech quality against the clean voice (wideband, 1-4.6)
  stoi    intelligibility against the clean voice (0-1)
  after   bark scene: voice level after a loud bark against before, dB
  overlap echo scene: own voice while the other side talks against alone, dB
  echo    echo scene: echo left while only the other side talks, dB below voice

Voices (LibriVox, public domain, archive.org/details/<item>):
  en_f   shortstory078_1902_librivox, ss078_12      en_f2  same, ss078_11
  en_m   shortstory078_1902_librivox, ss078_07
  ru_f   aesops_fables_russian_0905_librivox, 12    ru_m   same, 14
Noise (Wikimedia Commons):
  bark    "Barking of a dog.ogg", Amada44, CC BY-SA 3.0
  wrapper "Food wrapper crumpling.ogg", stilgar, public domain
  cars    "Cars passing over bridge.ogg", Dsw4, public domain
  typing  "Typing - Model M 1986.ogg", Raymangold22, CC0
  paper   "Rejection letter cut into pieces.ogg", hugh, public domain
  mouse1, mouse2  "Computer mouse single/double click.ogg", Darklanlan, CC0
  pen     "Retractable pen clickin.ogg", stilgar, public domain
  cafe    "Restaurant ambience.ogg", stephan, public domain
  siren   "Civil-defense-siren-waver.ogg", Techtonic, public domain
  music   "Pleasant Moments Piano Roll.ogg", Scott Joplin, public domain
  knock   "Knocking on wood or door.ogg", stephan, public domain
  fan     synthetic: pink noise with 100 and 200 Hz hum
"""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
DIR = ROOT / "target/denoise-eval"
RATE = 48000
WARM = 4 * RATE
UA = {"User-Agent": "quipcall-denoise-eval/0.2"}

ARCHIVE = "https://archive.org/download/"
# The first 1.5 MB of each recording (about 90 s), checked by SHA-256.
VOICES = {
    "en_f": ("shortstory078_1902_librivox/ss078_12_various.mp3",
             "3b1343eda14755200facf5f390d18ea9b97c4628e49deda6a38a97448f19f6ec"),
    "en_f2": ("shortstory078_1902_librivox/ss078_11_various.mp3",
              "119c0b954f25567a6a736bf48a65c45f48b458f1cbcf91ee5e36fb61fd40ff8c"),
    "en_m": ("shortstory078_1902_librivox/ss078_07_various.mp3",
             "97ffea15a1e24bdb1b7d6e182d59c7dd3df8acbfd44deee4de32e953b5ff4f4f"),
    "ru_f": ("aesops_fables_russian_0905_librivox/basni_12_ezop.mp3",
             "ae79e2d8005e5f61b72877434dac26a917b633b39ed0d489f240805e1ccbc3c3"),
    "ru_m": ("aesops_fables_russian_0905_librivox/basni_14_ezop.mp3",
             "0d9e56d2bbf73ec24ccbbec3b693dd4e641b80d0838b03f895e959c386d92743"),
}
PREFIX = 1_500_000

COMMONS = "https://upload.wikimedia.org/wikipedia/commons/"
# Whole files, checked by SHA-1.
NOISES = {
    "bark": ("a/a2/Barking_of_a_dog.ogg", "2dc95fc596ea2601b685a9dc569b2e82859cc6cc"),
    "wrapper": ("f/f0/Food_wrapper_crumpling.ogg", "786523e8c98fc2c2f75b2e30783110b8af446bf9"),
    "cars": ("9/96/Cars_passing_over_bridge.ogg", "ce5294fd9e53bba450d763382dbe9ecbc4d7eb73"),
    "typing": ("0/0a/Typing_-_Model_M_1986.ogg", "99004320c87964668f91cbb9e039c3513200289d"),
    "paper": ("a/a6/Rejection_letter_cut_into_pieces.ogg", "36a5580e6bd96ee4c5a42b7ec7b5a9f5c6f74d8c"),
    "mouse1": ("2/26/Computer_mouse_single_click.ogg", "6a3eab3ad7f02f630b967a03636173de88552dde"),
    "mouse2": ("9/9f/Computer_mouse_double_click.ogg", "e50a8a1038efa84956121a4f0bf883c72a7c26f1"),
    "pen": ("9/95/Retractable_pen_clickin.ogg", "34518dd4cb0d7d1da852d838f3d3efdbf8d6e23d"),
    "cafe": ("b/b5/Restaurant_ambience.ogg", "34e0b1f53ebe79dc106cdc89accd89fbae0a9e8c"),
    "siren": ("6/69/Civil-defense-siren-waver.ogg", "2c1726c43ce5ff58d78834e9c55328079a1f3349"),
    "music": ("9/9a/Pleasant_Moments_Piano_Roll.ogg", "0fe15aea7c051ab3e60a8b2e1d443d422a0c0349"),
    "knock": ("1/1c/Knocking_on_wood_or_door.ogg", "7fae9ee6914c84f37bdf558643a07efc5f7587b0"),
}

# Speaking level of a normal call into a laptop microphone, and a quiet one.
NORMAL, QUIET = -30, -45
# name: (voice, voice level dBFS, noise, noise level against the voice dB)
SCENES = {
    "clean-en_f": ("en_f", NORMAL, None, 0),
    "clean-en_m": ("en_m", NORMAL, None, 0),
    "clean-ru_f": ("ru_f", NORMAL, None, 0),
    "clean-ru_m": ("ru_m", NORMAL, None, 0),
    "quiet-ru_f": ("ru_f", QUIET, None, 0),
    "quieter-en_m": ("en_m", -50, None, 0),
    "loud-en_m": ("en_m", -20, None, 0),
    "bark-ru_m": ("ru_m", NORMAL, "bark", 0),
    "chips-ru_f": ("ru_f", NORMAL, "wrapper", 0),
    "cars-en_f": ("en_f", NORMAL, "cars", -5),
    "paper-en_m": ("en_m", NORMAL, "paper", -5),
    "typing-ru_m": ("ru_m", NORMAL, "typing", -5),
    "clicks-en_f": ("en_f", NORMAL, "clicks", -5),
    "cafe-en_m": ("en_m", NORMAL, "cafe", -5),
    "music-ru_f": ("ru_f", NORMAL, "music", -10),
    "siren-en_m": ("en_m", NORMAL, "siren", -5),
    "knock-ru_m": ("ru_m", NORMAL, "knock", -5),
    "fan-en_f": ("en_f", NORMAL, "fan", -15),
    "quiet_cars-ru_m": ("ru_m", QUIET, "cars", -10),
    # Special scenes, built below.
    "bark_then_talk-en_f": ("en_f", NORMAL, "bark", None),
    "two_voices-en_m": ("en_m", NORMAL, "en_f2", -6),
    "double_talk-ru_f": ("ru_f", NORMAL, "en_m", None),
}


def main():
    ensure_numpy()
    chains = sys.argv[1] if len(sys.argv) > 1 else None
    sources = fetch()
    build(sources)
    timing = run(chains)
    results = score(timing)
    (DIR / "results.json").write_text(json.dumps(results, indent=1))
    report(results)
    print(f"\nListen: {DIR / 'out'}/<scene>/  (mic.wav, clean.wav, <chain>.wav)")


def ensure_numpy():
    """Re-run in target/denoise-eval/venv unless numpy is here."""
    try:
        import numpy  # noqa: F401
        return
    except ImportError:
        pass
    venv = DIR / "venv"
    python = venv / "bin/python"
    if not python.exists():
        print("Making target/denoise-eval/venv (numpy scipy pesq pystoi)...")
        subprocess.run([sys.executable, "-m", "venv", str(venv)], check=True)
        subprocess.run([str(venv / "bin/pip"), "install", "-q", "numpy", "scipy", "pesq", "pystoi"],
                       check=True)
    os.execv(str(python), [str(python), *sys.argv])


def download(url, dst, digest, algo, prefix=None):
    if dst.exists() and hashlib.new(algo, dst.read_bytes()).hexdigest() == digest:
        return
    headers = dict(UA)
    if prefix:
        headers["Range"] = f"bytes=0-{prefix - 1}"
    for attempt in range(6):
        try:
            data = urllib.request.urlopen(urllib.request.Request(url, headers=headers)).read()
            break
        except urllib.error.HTTPError as e:
            # Wikimedia limits bursts of downloads.
            if e.code != 429 or attempt == 5:
                raise
            wait = int(e.headers.get("Retry-After") or 0) or 10 * 2 ** attempt
            print(f"{dst.name}: rate limited, retrying in {wait} s")
            time.sleep(wait)
    if prefix:
        data = data[:prefix]
    if hashlib.new(algo, data).hexdigest() != digest:
        raise SystemExit(f"{dst.name}: checksum mismatch, not used")
    dst.write_bytes(data)


def decode(path):
    import numpy as np
    pcm = subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-ac", "1", "-ar", str(RATE),
                          "-f", "s16le", "-"], check=True, capture_output=True).stdout
    return np.frombuffer(pcm, np.int16).astype(np.float32)


def fetch():
    src = DIR / "sources"
    src.mkdir(parents=True, exist_ok=True)
    sources = {}
    for name, (path, sha256) in VOICES.items():
        dst = src / f"{name}.mp3"
        download(ARCHIVE + path, dst, sha256, "sha256", PREFIX)
        sources[name] = decode(dst)
    for name, (path, sha1) in NOISES.items():
        dst = src / f"{name}.ogg"
        download(COMMONS + path, dst, sha1, "sha1")
        sources[name] = decode(dst)
    return sources


# --- Scenes ---------------------------------------------------------------

def db(x):
    import numpy as np
    return 10 * np.log10(np.maximum(x, 1e-3))


def active_rms(x, range_db=30):
    """RMS over the 10 ms frames within `range_db` of the loudest."""
    import numpy as np
    n = len(x) // 480
    e = (x[: n * 480].reshape(n, 480).astype(np.float64) ** 2).mean(1)
    on = e > e.max() / 10 ** (range_db / 10)
    return np.sqrt(e[on].mean())


def at_level(x, dbfs):
    """`x` scaled so its active part sits at `dbfs`."""
    return x * (32768 * 10 ** (dbfs / 20) / active_rms(x))


def speech(sources, voice, dbfs):
    """4 s of warm-up, then three 5 s turns with pauses between them."""
    import numpy as np
    v = sources[voice]
    sec = lambda a, b: v[int(a * RATE):int(b * RATE)]  # noqa: E731
    gap = np.zeros(int(1.5 * RATE), np.float32)
    parts = [sec(30, 34), sec(36, 41), gap, sec(42, 47), gap, sec(48, 53)]
    return at_level(np.concatenate(parts), dbfs)


def loop(x, n):
    import numpy as np
    return np.resize(x, n)


def noise(sources, name, n):
    import numpy as np
    rng = np.random.default_rng(7)
    if name == "fan":
        spectrum = np.fft.rfft(rng.standard_normal(n))
        spectrum /= np.sqrt(np.maximum(np.arange(len(spectrum)), 1))
        pink = np.fft.irfft(spectrum, n)
        t = np.arange(n) / RATE
        hum = 0.3 * np.sin(2 * np.pi * 100 * t) + 0.15 * np.sin(2 * np.pi * 200 * t)
        return (pink / pink.std() + hum).astype(np.float32)
    if name == "clicks":
        out = np.zeros(n, np.float32)
        clicks = [sources["mouse1"], sources["mouse2"], sources["pen"][: RATE * 2]]
        t = int(0.2 * RATE)
        while t < n:
            c = clicks[rng.integers(len(clicks))]
            c = c[: n - t]
            out[t:t + len(c)] += c
            t += int(rng.uniform(0.4, 1.4) * RATE)
        return out
    return loop(sources[name], n)


def mix(clean, other, against_db):
    """`other` at `against_db` relative to `clean`, both by active level."""
    return other * (active_rms(clean) / active_rms(other) * 10 ** (against_db / 20))


def build(sources):
    import numpy as np
    scenes = DIR / "scenes"
    for name, (voice, level, extra, against) in SCENES.items():
        meta = {"start": WARM}
        clean = speech(sources, voice, level)
        far = None
        kind = name.split("-")[0]
        if kind == "bark_then_talk":
            # A loud bark in a pause, then talking again: does the voice
            # come back at the same level?
            v = sources[voice]
            bark = sources["bark"]
            e = np.convolve(bark ** 2, np.ones(RATE // 2), "same")
            loud = bark[max(0, e.argmax() - RATE // 2):][: int(1.5 * RATE)]
            gap = np.zeros(int(2.5 * RATE), np.float32)
            parts = [v[30 * RATE:34 * RATE], v[36 * RATE:42 * RATE], gap, v[42 * RATE:48 * RATE]]
            clean = at_level(np.concatenate(parts), level)
            bark_at = 10 * RATE + RATE // 2
            mic = clean.copy()
            mic[bark_at:bark_at + len(loud)] += loud / np.abs(loud).max() * 32768 * 0.5
            meta["event"] = [bark_at, bark_at + len(loud)]
        elif kind == "two_voices":
            # Someone else talking in the room at the same time. Both are
            # speech, so the reference keeps both.
            other = speech(sources, extra, level)
            other = np.roll(other, RATE)
            clean = clean + mix(clean, other, against)
            mic = clean.copy()
        elif kind == "double_talk":
            # The other side's voice from the speakers, echoing into the
            # microphone, then both talking at once.
            rng = np.random.default_rng(3)
            far_voice = at_level(sources[extra][60 * RATE:80 * RATE], -20)
            n = len(clean)
            far = np.zeros(n, np.float32)
            far[WARM:WARM + 14 * RATE] = far_voice[: 14 * RATE]
            # Laptop speaker to microphone: direct sound after 40 ms (the
            # audio path's delay), then a 0.25 s room tail about 10 dB weaker.
            t = np.arange(int(0.25 * RATE)) / RATE
            tail = rng.standard_normal(len(t)) * np.exp(-6.9 * t / 0.25) * 0.012
            rir = np.concatenate([np.zeros(int(0.04 * RATE)), [1.0], tail])
            echo = np.convolve(far, rir)[:n]
            # Near voice silent while only the far side talks (6 s).
            clean = clean.copy()
            clean[WARM:WARM + 6 * RATE] = 0
            echo = mix(clean, echo, -6)
            mic = clean + echo
            meta["far_only"] = [WARM, WARM + 6 * RATE]
            meta["both"] = [WARM + 6 * RATE, WARM + 14 * RATE]
            meta["near_only"] = [WARM + 14 * RATE, n]
        elif extra:
            mic = clean + mix(clean, noise(sources, extra, len(clean)), against)
        else:
            mic = clean.copy()
        d = scenes / name
        d.mkdir(parents=True, exist_ok=True)
        write_pcm(d / "mic.pcm", mic)
        write_pcm(d / "clean.pcm", clean)
        if far is not None:
            write_pcm(d / "far.pcm", far)
        elif (d / "far.pcm").exists():
            (d / "far.pcm").unlink()
        (d / "meta.json").write_text(json.dumps(meta))
        out = DIR / "out" / name
        out.mkdir(parents=True, exist_ok=True)
        write_wav(out / "mic.wav", mic)
        write_wav(out / "clean.wav", clean)


def write_pcm(path, x):
    import numpy as np
    path.write_bytes(np.clip(np.round(x), -32768, 32767).astype("<i2").tobytes())


def write_wav(path, x):
    import wave
    import numpy as np
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(np.clip(np.round(x), -32768, 32767).astype("<i2").tobytes())


def read_wav(path):
    import wave
    import numpy as np
    with wave.open(str(path)) as w:
        return np.frombuffer(w.readframes(w.getnframes()), np.int16).astype(np.float32)


def read_pcm(path):
    import numpy as np
    return np.frombuffer(path.read_bytes(), np.int16).astype(np.float32)


# --- Run and score ----------------------------------------------------------

def run(chains):
    cmd = ["cargo", "run", "--release", "-q", "-p", "quipcall-core", "--example", "denoise_eval",
           "--", str(DIR)]
    if chains:
        cmd.append(chains)
    out = subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True, text=True).stdout
    return [json.loads(line) for line in out.splitlines() if line.startswith("{")]


WIN = 2400  # 50 ms


def levels(x):
    import numpy as np
    n = len(x) // WIN
    return db((x[: n * WIN].reshape(n, WIN).astype(np.float64) ** 2).mean(1))


def lag(clean, out):
    """Samples by which `out` trails `clean` (≤ 100 ms)."""
    import numpy as np
    a, b = clean[WARM:WARM + 6 * RATE], out[WARM:WARM + 6 * RATE + RATE // 10]
    corr = [np.dot(a, b[k:k + len(a)]) for k in range(0, RATE // 10)]
    return int(np.argmax(corr))


def score(timing):
    import numpy as np
    try:
        from pesq import pesq
        from pystoi import stoi
        from scipy.signal import resample_poly
    except ImportError:
        pesq = None
    chains = list(dict.fromkeys(t["chain"] for t in timing))
    lags = {}
    for chain in chains:
        clean = read_pcm(DIR / "scenes/clean-en_f/clean.pcm")
        lags[chain] = lag(clean, read_wav(DIR / f"out/clean-en_f/{chain}.wav"))
    results = []
    for t in timing:
        scene, chain = t["scene"], t["chain"]
        d = DIR / "scenes" / scene
        meta = json.loads((d / "meta.json").read_text())
        clean, mic = read_pcm(d / "clean.pcm"), read_pcm(d / "mic.pcm")
        out = read_wav(DIR / f"out/{scene}/{chain}.wav")[lags[chain]:]
        n = min(len(out), len(clean))
        clean, mic, out = clean[:n], mic[:n], out[:n]
        s = meta["start"] // WIN
        ref_l, mic_l, out_l = levels(clean), levels(mic), levels(out)
        peak = np.percentile(ref_l[s:], 95)
        # Voice, louder than whatever else the microphone hears there.
        talking = (ref_l > peak - 20) & (ref_l > levels(mic - clean))
        talking[:s] = False
        pause = ref_l < peak - 50
        pause[:s] = False
        gain = out_l - ref_l
        level = np.median(gain[talking])
        r = {"scene": scene, "chain": chain, "filter_us": t["filter_us"], "apm_us": t["apm_us"],
             "level": level,
             "dips": level - np.percentile(gain[talking], 10),
             "drops": 100 * np.mean(gain[talking] < level - 6)}
        if SCENES[scene][2] and pause.sum() > 10:
            r["noise"] = level + np.median(mic_l[pause]) - np.median(out_l[pause])
        win = lambda a, b: slice(a // WIN, b // WIN)  # noqa: E731
        if "event" in meta:
            a, b = meta["event"]
            before = talking.copy()
            before[a // WIN:] = False
            after = talking.copy()
            after[: b // WIN] = False
            after[(b + 3 * RATE) // WIN:] = False
            r["after"] = np.median(gain[after]) - np.median(gain[before])
        if "both" in meta:
            both = np.zeros_like(talking)
            both[win(*meta["both"])] = True
            alone = np.zeros_like(talking)
            alone[win(*meta["near_only"])] = True
            r["overlap"] = np.median(gain[talking & both]) - np.median(gain[talking & alone])
            far_only = np.zeros_like(talking)
            a, b = meta["far_only"]
            far_only[win(a + 2 * RATE, b)] = True
            voice = np.median(out_l[talking & alone])
            r["echo"] = np.median(out_l[far_only]) - voice
        if pesq:
            ref16 = resample_poly(clean[meta["start"]:] / 32768, 1, 3)
            out16 = resample_poly(out[meta["start"]:] / 32768, 1, 3)
            try:
                r["pesq"] = pesq(16000, ref16, out16, "wb")
            except Exception:
                pass
            r["stoi"] = stoi(clean[meta["start"]:], out[meta["start"]:], RATE)
        results.append({k: (round(float(v), 3) if isinstance(v, (float, np.floating)) else v)
                        for k, v in r.items()})
    return results


def report(results):
    import numpy as np
    chains = list(dict.fromkeys(r["chain"] for r in results))
    scenes = list(dict.fromkeys(r["scene"] for r in results))
    by = {(r["scene"], r["chain"]): r for r in results}
    width = max(len(c) for c in chains) + 1
    for metric, fmt in [("dips", "{:.1f}"), ("drops", "{:.0f}"), ("noise", "{:.1f}"),
                        ("pesq", "{:.2f}"), ("level", "{:+.1f}")]:
        print(f"\n{metric:20}" + "".join(f"{c:>{width}}" for c in chains))
        for s in scenes:
            vals = [by.get((s, c), {}).get(metric) for c in chains]
            if all(v is None for v in vals):
                continue
            print(f"{s:20}" + "".join(f"{(fmt.format(v) if v is not None else '-'):>{width}}"
                                      for v in vals))
    print(f"\n{'summary':20}" + "".join(f"{c:>{width}}" for c in chains))
    rows = [("dips dB (mean)", "dips", None), ("drops % (mean)", "drops", None),
            ("noise dB (mean)", "noise", None), ("pesq (mean)", "pesq", None),
            ("stoi (mean)", "stoi", None), ("after bark dB", "after", None),
            ("overlap dB", "overlap", None), ("echo dB", "echo", None),
            ("filter us/10ms", "filter_us", None), ("apm us/10ms", "apm_us", None)]
    for label, key, _ in rows:
        vals = []
        for c in chains:
            v = [r[key] for r in results if r["chain"] == c and key in r]
            vals.append(np.mean(v) if v else None)
        print(f"{label:20}" + "".join(f"{(f'{v:.2f}' if v is not None else '-'):>{width}}"
                                      for v in vals))


if __name__ == "__main__":
    main()
