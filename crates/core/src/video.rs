//! Remote video plumbing: decoded frames are pushed into a UI-provided sink on
//! an engine thread, so pixel conversion never runs on the UI thread.

use std::sync::Arc;

use futures_util::StreamExt;
use livekit::prelude::RemoteVideoTrack;
use livekit::webrtc::video_frame::{BoxVideoBuffer, VideoFrame};
use livekit::webrtc::video_stream::native::NativeVideoStream;

pub trait FrameSink: Send + Sync + 'static {
    /// Called for every decoded frame, on an engine thread.
    fn on_frame(&self, frame: &VideoFrame<BoxVideoBuffer>);
}

/// Keeps a remote video stream flowing; dropping it stops the frame pump.
pub struct VideoSubscription {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for VideoSubscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) fn attach(
    rt: &crate::tasks::Tasks,
    track: &RemoteVideoTrack,
    sink: Arc<dyn FrameSink>,
) -> VideoSubscription {
    let mut stream = NativeVideoStream::new(track.rtc_track());
    let task = rt.spawn(async move {
        while let Some(frame) = stream.next().await {
            crate::autorelease_pool(|| sink.on_frame(&frame));
        }
    });
    VideoSubscription { task }
}
