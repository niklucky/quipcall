//! One voice cut into utterances, from Silero's speech probability per 32 ms.
//!
//! Speech starts at a probability of 0.5 and keeps the 320 ms before it (the
//! VAD notices a word after it has begun). It ends after 600 ms below 0.35.
//! Under 250 ms of speech was a cough or a click. A monologue is cut every
//! 25 s, so its text doesn't wait for a pause. While someone talks, a draft
//! goes out every second.

use std::collections::VecDeque;
use std::time::Instant;

/// Samples per VAD frame at 16 kHz: 32 ms.
pub(crate) const FRAME: usize = 512;
const FRAME_SECS: f32 = FRAME as f32 / 16_000.0;
const START: f32 = 0.5;
const STOP: f32 = 0.35;
const PREROLL: usize = 10;
const HANGOVER: usize = 19;
/// Silence kept at the end of an utterance: 190 ms.
const TAIL: usize = 6;
const MIN_SPEECH: usize = 8;
const MAX_FRAMES: usize = 781;
const DRAFT_EVERY: usize = 31;
/// No drafts of under a second: too little to say.
const MIN_DRAFT: usize = 31;

#[derive(Debug)]
pub(crate) struct Cut {
    pub(crate) utterance: u64,
    /// When its first sample was heard.
    pub(crate) at: Instant,
    /// The utterance so far, or all of it when `done`. Empty when it turned
    /// out to be noise (then a draft on screen should go away).
    pub(crate) audio: Vec<f32>,
    pub(crate) done: bool,
}

struct Utterance {
    id: u64,
    at: Instant,
}

struct Speech {
    utterance: Utterance,
    audio: Vec<f32>,
    voiced: usize,
    silent: usize,
    since_draft: usize,
    drafted: bool,
}

pub(crate) struct Segmenter {
    /// Recent frames before speech, oldest first.
    preroll: VecDeque<(Box<[f32]>, Instant)>,
    speech: Option<Speech>,
}

impl Segmenter {
    pub(crate) fn new() -> Self {
        Self {
            preroll: VecDeque::with_capacity(PREROLL + 1),
            speech: None,
        }
    }

    /// One frame of [`FRAME`] samples, heard at `at`, with its speech
    /// probability. `next_id` numbers new utterances.
    pub(crate) fn push(
        &mut self,
        frame: &[f32],
        p: f32,
        at: Instant,
        next_id: &mut dyn FnMut() -> u64,
    ) -> Option<Cut> {
        let Some(s) = &mut self.speech else {
            if p < START {
                self.preroll.push_back((frame.into(), at));
                if self.preroll.len() > PREROLL {
                    self.preroll.pop_front();
                }
                return None;
            }
            let start = self.preroll.front().map_or(at, |(_, at)| *at);
            let mut audio = Vec::with_capacity(FRAME * 64);
            for (f, _) in self.preroll.drain(..) {
                audio.extend_from_slice(&f);
            }
            audio.extend_from_slice(frame);
            self.speech = Some(Speech {
                utterance: Utterance {
                    id: next_id(),
                    at: start,
                },
                audio,
                voiced: 1,
                silent: 0,
                since_draft: 0,
                drafted: false,
            });
            return None;
        };

        s.audio.extend_from_slice(frame);
        s.since_draft += 1;
        if p >= STOP {
            s.voiced += 1;
            s.silent = 0;
        } else {
            s.silent += 1;
        }
        if s.silent >= HANGOVER {
            let cut = self.finish();
            return cut;
        }
        if s.audio.len() >= MAX_FRAMES * FRAME {
            // Carry on in a new utterance straight away.
            let cut = self.finish();
            self.speech = Some(Speech {
                utterance: Utterance {
                    id: next_id(),
                    at: at + std::time::Duration::from_secs_f32(FRAME_SECS),
                },
                audio: Vec::with_capacity(FRAME * 64),
                voiced: 0,
                silent: 0,
                since_draft: 0,
                drafted: false,
            });
            return cut;
        }
        if s.since_draft >= DRAFT_EVERY && s.audio.len() >= MIN_DRAFT * FRAME && s.silent == 0 {
            s.since_draft = 0;
            s.drafted = true;
            return Some(Cut {
                utterance: s.utterance.id,
                at: s.utterance.at,
                audio: s.audio.clone(),
                done: false,
            });
        }
        None
    }

    /// Ends the utterance in progress, if any: the voice paused, left, or
    /// transcription stopped.
    pub(crate) fn finish(&mut self) -> Option<Cut> {
        let s = self.speech.take()?;
        let mut audio = s.audio;
        if s.voiced < MIN_SPEECH {
            if !s.drafted {
                return None;
            }
            audio.clear();
        }
        let trim = s.silent.saturating_sub(TAIL) * FRAME;
        audio.truncate(audio.len().saturating_sub(trim));
        Some(Cut {
            utterance: s.utterance.id,
            at: s.utterance.at,
            audio,
            done: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Feeds probabilities, one frame each; returns the cuts with the frame
    /// they came at.
    fn run(probs: &[f32]) -> Vec<(usize, Cut)> {
        let mut seg = Segmenter::new();
        let mut id = 0;
        let mut next = || {
            id += 1;
            id
        };
        let t0 = Instant::now();
        let mut cuts = Vec::new();
        for (i, p) in probs.iter().enumerate() {
            let frame = vec![i as f32; FRAME];
            let at = t0 + Duration::from_secs_f32(i as f32 * FRAME_SECS);
            if let Some(cut) = seg.push(&frame, *p, at, &mut next) {
                cuts.push((i, cut));
            }
        }
        if let Some(cut) = seg.finish() {
            cuts.push((probs.len(), cut));
        }
        cuts
    }

    fn frames(cut: &Cut) -> Vec<usize> {
        cut.audio.chunks(FRAME).map(|f| f[0] as usize).collect()
    }

    #[test]
    fn a_phrase_with_what_came_before_and_a_short_tail() {
        let mut probs = vec![0.0; 20];
        probs.extend([0.9; 20]);
        probs.extend([0.1; 40]);
        let cuts = run(&probs);
        assert_eq!(cuts.len(), 1);
        let (at, cut) = &cuts[0];
        assert_eq!(*at, 40 + HANGOVER - 1, "ends after the hangover");
        let f = frames(cut);
        assert_eq!(f.first(), Some(&(20 - PREROLL)));
        assert_eq!(f.last(), Some(&(40 + TAIL - 1)));
    }

    #[test]
    fn short_pauses_dont_split_and_coughs_dont_count() {
        let mut probs = vec![0.9; 20];
        probs.extend([0.1; HANGOVER - 1]);
        probs.extend([0.9; 20]);
        probs.extend([0.1; 30]);
        // A cough.
        probs.extend([0.9; 3]);
        probs.extend([0.1; 30]);
        let cuts = run(&probs);
        let done = cuts.iter().filter(|(_, c)| c.done).count();
        assert_eq!(done, 1);
    }

    #[test]
    fn drafts_while_talking_and_cuts_monologues() {
        let probs = vec![0.9; MAX_FRAMES + 100];
        let cuts = run(&probs);
        let drafts = cuts
            .iter()
            .filter(|(_, c)| !c.done && c.utterance == 1)
            .count();
        assert_eq!(drafts, MAX_FRAMES / DRAFT_EVERY);
        let done: Vec<_> = cuts
            .iter()
            .filter(|(_, c)| c.done)
            .map(|(_, c)| (c.utterance, c.audio.len() / FRAME))
            .collect();
        assert_eq!(done, [(1, MAX_FRAMES), (2, 100)]);
    }

    #[test]
    fn noise_that_was_drafted_is_taken_back() {
        // A click, then murmurs that keep the utterance open long enough for
        // a draft but are too few to be speech.
        let mut probs = vec![0.9];
        probs.extend((0..40).map(|i| if i % (HANGOVER - 1) == 0 { 0.4 } else { 0.1 }));
        let cuts = run(&probs);
        assert_eq!(cuts.len(), 2);
        assert!(!cuts[0].1.done);
        let last = &cuts[1].1;
        assert!(last.done && last.utterance == 1 && last.audio.is_empty());
    }
}
