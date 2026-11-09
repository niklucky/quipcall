//! Embedded assets: Lucide icons (ISC licence, see assets/icons/LICENSE).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($variant),* }

        impl Icon {
            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file, ".svg")),* }
            }
        }

        static FILES: &[(&str, &[u8])] = &[
            $((concat!("icons/", $file, ".svg"), include_bytes!(concat!("../assets/icons/", $file, ".svg")))),*
        ];
    };
}

icons! {
    Captions => "captions",
    Check => "check",
    ChevronLeft => "chevron-left",
    ChevronRight => "chevron-right",
    CircleDot => "circle-dot",
    Copy => "copy",
    Link => "link",
    LogIn => "log-in",
    LogOut => "log-out",
    PanelRight => "panel-right",
    Pencil => "pencil",
    Phone => "phone",
    Trash => "trash-2",
    Upload => "upload",
    User => "user",
    Users => "users",
    Maximize => "maximize-2",
    Mic => "mic",
    Monitor => "monitor",
    AppWindow => "app-window",
    MoveDiagonal => "move-diagonal-2",
    MicOff => "mic-off",
    Minimize => "minimize-2",
    MonitorOff => "monitor-off",
    MonitorUp => "monitor-up",
    PhoneOff => "phone-off",
    Plus => "plus",
    UserPlus => "user-plus",
    Video => "video",
    VideoOff => "video-off",
    X => "x",
    Zap => "zap",
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}
