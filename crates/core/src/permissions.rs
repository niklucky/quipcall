//! OS media permissions.
//!
//! On macOS the first access to the microphone or camera shows a system
//! prompt. If WebRTC's audio thread is what triggers it, that thread blocks
//! until the user answers and the whole call freezes. So we always ask here,
//! asynchronously, before any device is opened.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDevice {
    Microphone,
    Camera,
}

impl MediaDevice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Camera => "camera",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Granted,
    /// Denied by the user or blocked by policy. Only System Settings can change it.
    Denied,
    /// The user hasn't been asked yet; [`request`] will show the prompt.
    Undecided,
}

#[cfg(target_os = "macos")]
pub use mac::{request, status};

#[cfg(not(target_os = "macos"))]
pub fn status(_device: MediaDevice) -> Access {
    Access::Granted
}

#[cfg(not(target_os = "macos"))]
pub async fn request(_device: MediaDevice) -> bool {
    true
}

#[cfg(target_os = "macos")]
mod mac {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{
        AVAuthorizationStatus, AVCaptureDevice, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
    };

    use super::{Access, MediaDevice};

    fn media_type(device: MediaDevice) -> &'static AVMediaType {
        // SAFETY: AVFoundation's media type constants are immutable statics.
        let ty = unsafe {
            match device {
                MediaDevice::Microphone => AVMediaTypeAudio,
                MediaDevice::Camera => AVMediaTypeVideo,
            }
        };
        ty.expect("AVFoundation media type constant")
    }

    pub fn status(device: MediaDevice) -> Access {
        // SAFETY: valid media type; the call has no other preconditions.
        let status =
            unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type(device)) };
        match status {
            AVAuthorizationStatus::Authorized => Access::Granted,
            AVAuthorizationStatus::NotDetermined => Access::Undecided,
            _ => Access::Denied,
        }
    }

    /// Ask for access, showing the system prompt if needed. Resolves when the
    /// user answers; returns whether access is granted.
    pub async fn request(device: MediaDevice) -> bool {
        match status(device) {
            Access::Granted => return true,
            Access::Denied => return false,
            Access::Undecided => {}
        }
        let (tx, rx) = async_channel::bounded::<bool>(1);
        {
            // AVFoundation copies the block; ours is dropped before we await.
            let handler = RcBlock::new(move |granted: Bool| {
                let _ = tx.try_send(granted.as_bool());
            });
            // SAFETY: valid media type; the handler only touches a channel
            // sender, which is fine from AVFoundation's queue.
            unsafe {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(
                    media_type(device),
                    &handler,
                )
            };
        }
        rx.recv().await.unwrap_or(false)
    }
}
