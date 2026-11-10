//! 48 kHz to 16 kHz: a low-pass FIR, then every third sample.

/// Taps of the anti-aliasing filter: 64 at 48 kHz is 1.3 ms.
const TAPS: usize = 64;
const FACTOR: usize = 3;

pub(crate) struct Downsampler {
    taps: [f32; TAPS],
    /// Input not yet consumed, after `TAPS - 1` samples of history.
    pending: Vec<f32>,
}

impl Downsampler {
    pub(crate) fn new() -> Self {
        // Windowed sinc (Blackman), cut at 7.2 kHz: below the new 8 kHz
        // Nyquist, above everything a speech model listens to.
        let cutoff = 7_200.0 / 48_000.0;
        let mid = (TAPS - 1) as f32 / 2.0;
        let mut taps = [0.0f32; TAPS];
        for (i, t) in taps.iter_mut().enumerate() {
            let x = i as f32 - mid;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f32::consts::PI * cutoff * x).sin() / (std::f32::consts::PI * x)
            };
            let w = i as f32 / (TAPS - 1) as f32 * 2.0 * std::f32::consts::PI;
            *t = sinc * (0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos());
        }
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Self {
            taps,
            pending: vec![0.0; TAPS - 1],
        }
    }

    /// Appends the 16 kHz samples that `input` completes to `out`.
    pub(crate) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.pending.extend_from_slice(input);
        let mut at = 0;
        while at + TAPS <= self.pending.len() {
            let window = &self.pending[at..at + TAPS];
            out.push(window.iter().zip(&self.taps).map(|(s, t)| s * t).sum());
            at += FACTOR;
        }
        self.pending.drain(..at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(freq: f32) -> f32 {
        let input: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin())
            .collect();
        let mut down = Downsampler::new();
        let mut out = Vec::new();
        // In 10 ms pieces, as calls deliver it.
        for frame in input.chunks(480) {
            down.push(frame, &mut out);
        }
        assert!((out.len() as i64 - 16_000).abs() <= 1, "{}", out.len());
        // The amplitude, from the RMS after the filter's warm-up.
        let rest = &out[100..];
        (rest.iter().map(|s| s * s).sum::<f32>() / rest.len() as f32 * 2.0).sqrt()
    }

    #[test]
    fn keeps_speech_and_drops_what_would_alias() {
        for f in [300.0, 1_000.0, 3_000.0, 5_000.0] {
            assert!((level(f) - 1.0).abs() < 0.02, "{f} Hz: {}", level(f));
        }
        // 12 kHz would fold onto 4 kHz.
        assert!(level(12_000.0) < 0.01, "{}", level(12_000.0));
    }
}
