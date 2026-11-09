//! Transcribing the call on this Mac: everyone's voice (see [`audio_tap`])
//! into [`quipcall_stt`], lines out as [`Event::TranscriptLine`].
//!
//! [`audio_tap`]: crate::audio_tap
//! [`Event::TranscriptLine`]: crate::Event::TranscriptLine

use std::thread::JoinHandle;

use quipcall_stt::Transcriber;

use crate::audio_tap::{Listener, Speaker, VoiceFrame};

pub(crate) struct Transcribing {
    /// A handle on the listener's queue, to close it from outside.
    frames: async_channel::Receiver<VoiceFrame>,
    thread: Option<JoinHandle<()>>,
}

impl Transcribing {
    /// Feeds `listener` to `transcriber` on a thread of its own. Your voice
    /// is filed under `you`, everyone else's under their identity.
    pub(crate) fn start(listener: Listener, transcriber: Transcriber, you: String) -> Self {
        let frames = listener.frames.clone();
        let thread = std::thread::Builder::new()
            .name("transcribe-feed".into())
            .spawn(move || {
                while let Ok(frame) = listener.frames.recv_blocking() {
                    let speaker = match &frame.speaker {
                        Speaker::Local => &you,
                        Speaker::Remote(identity) => identity,
                    };
                    transcriber.feed(speaker, frame.samples);
                }
                drop(listener);
                transcriber.stop();
            })
            .expect("transcription thread");
        Self {
            frames,
            thread: Some(thread),
        }
    }

    /// Stops listening, transcribes what was being said, and returns once
    /// the last line is out. Blocks for a moment.
    pub(crate) fn stop(mut self) {
        self.frames.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Transcribing {
    fn drop(&mut self) {
        self.frames.close();
    }
}
