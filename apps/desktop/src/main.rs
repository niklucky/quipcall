mod assets;
mod auth;
mod call;
mod capsule;
mod fullscreen;
mod handle;
mod home;
mod mac;
mod models;
mod onboarding;
mod regular;
mod rooms;
mod screen_window;
mod settings;
mod share_picker;
#[cfg(feature = "snapshot")]
mod snapshot;
mod store;
mod text_input;
mod theme;
mod timeline;
mod transcript;
mod ui;
mod video_mac;

use std::io::IsTerminal as _;

use gpui::{App, AppContext as _, Context, Entity, Global, Subscription, WindowHandle};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::call::{Call, Ended, RoomRef};
use crate::home::Home;
use crate::store::Store;

/// Launch options. Login and contacts replace the dev parts in Stage 4.
#[derive(Clone)]
pub struct Args {
    pub session: Option<String>,
    pub server: String,
    /// Base URL of the web client, for call links.
    pub web: String,
    pub name: String,
    /// `--name` was given: don't ask for a name on first launch.
    pub name_given: bool,
    /// Join this room right away instead of showing home (dev and tests).
    pub room: Option<String>,
    /// Turn the camera on as soon as the call starts.
    pub camera: bool,
    /// Start calls in regular mode instead of minimal.
    pub regular: bool,
}

/// The server release builds talk to, baked in at build time:
/// `QUIPCALL_SERVER=https://quipcall.com cargo build --release` (see deploy/).
const SERVER: &str = match option_env!("QUIPCALL_SERVER") {
    Some(url) => url,
    None => "http://127.0.0.1:8080",
};

/// Where call links point. Deployed, the web client shares the server's origin.
const WEB: &str = match (option_env!("QUIPCALL_WEB"), option_env!("QUIPCALL_SERVER")) {
    (Some(url), _) | (None, Some(url)) => url,
    (None, None) => "http://127.0.0.1:5173",
};

fn parse_args() -> Args {
    let mut args = Args {
        session: None,
        server: SERVER.into(),
        web: WEB.into(),
        name: default_name(),
        name_given: false,
        room: None,
        camera: false,
        regular: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--camera" => args.camera = true,
            "--regular" => args.regular = true,
            "--server" | "--web" | "--room" | "--name" => {
                let Some(value) = it.next() else { usage() };
                match flag.as_str() {
                    "--server" => args.server = value,
                    "--web" => args.web = value,
                    "--room" => args.room = Some(value),
                    _ => {
                        args.name = value;
                        args.name_given = true;
                    }
                }
            }
            _ => usage(),
        }
    }
    args.session = auth::load(&args.server);
    #[cfg(feature = "snapshot")]
    if let Ok(path) = std::env::var("QUIPCALL_TEST_SESSION_FILE") {
        let url = reqwest::Url::parse(&args.server).expect("test server URL");
        assert!(
            matches!(url.host_str(), Some("127.0.0.1" | "localhost")),
            "test sessions are local only"
        );
        args.session = Some(
            std::fs::read_to_string(path)
                .expect("test session")
                .trim()
                .into(),
        );
    }
    args
}

/// The account's full name ("Jane Appleseed"), else the login name.
fn default_name() -> String {
    #[cfg(target_os = "macos")]
    {
        let full = objc2_foundation::NSFullUserName().to_string();
        if !full.trim().is_empty() {
            return full.trim().to_owned();
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "desktop".into())
}

/// The room in a call link: `quipcall://join/<room>`.
fn room_from_link(url: &str) -> Option<String> {
    let room = url
        .strip_prefix("quipcall://join/")
        .or_else(|| url.strip_prefix("quipcall://open/"))?;
    let room = room.split(['/', '?', '#']).next()?;
    quipcall_protocol::is_valid_room_id(room).then(|| room.to_owned())
}

fn usage() -> ! {
    eprintln!(
        "usage: quipcall-desktop [--server URL] [--web URL] [--name NAME] [--room ROOM] [--camera] [--regular]"
    );
    std::process::exit(2);
}

/// App-wide state: the home window, the current call and local data.
pub struct Shell {
    pub principal: Option<auth::Principal>,
    pub auth_busy: bool,
    pub auth_attempt: auth::SignInAttempt,
    pub auth_message: String,
    pub args: Args,
    pub store: Store,
    call: Option<(Entity<Call>, Subscription)>,
    home: Option<WindowHandle<Home>>,
    home_pending: bool,
    quitting: bool,
}

struct GlobalShell(Entity<Shell>);
impl Global for GlobalShell {}

impl Shell {
    pub fn global(cx: &App) -> Entity<Shell> {
        cx.global::<GlobalShell>().0.clone()
    }

    pub fn in_call(&self) -> bool {
        self.call.is_some()
    }

    pub fn call(&self) -> Option<Entity<Call>> {
        self.call.as_ref().map(|(call, _)| call.clone())
    }

    pub fn quick_call(&mut self, cx: &mut Context<Self>) {
        self.create_room("direct", true, cx);
    }

    pub fn join_saved(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(room) = self.store.rooms.iter().find(|r| r.id == id) {
            let room = RoomRef {
                id: room.id.clone(),
                name: room.name.clone(),
                one_time: room.kind == store::RoomKind::Direct,
            };
            self.start_call(room, cx);
        }
    }

    /// Join the room of a call link someone sent.
    pub fn join_link(&mut self, id: String, cx: &mut Context<Self>) {
        if self.in_call() {
            tracing::info!("ignoring link to {id}: already in a call");
            return;
        }
        let saved = self.store.rooms.iter().find(|r| r.id == id);
        let room = RoomRef {
            name: saved.map_or_else(|| id.clone(), |r| r.name.clone()),
            one_time: saved.is_none(),
            id,
        };
        self.start_call(room, cx);
    }

    pub fn rename_room(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.rename_remote_room(id, name, cx);
    }

    /// Keep a quick call's room in the rooms list.
    pub fn keep_room(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.store.keep_room(id, name);
        self.store.save();
        cx.notify();
    }

    /// Call a room from history again (same room, same link).
    pub fn join_again(&mut self, id: &str, name: &str, one_time: bool, cx: &mut Context<Self>) {
        self.start_call(
            RoomRef {
                id: id.to_owned(),
                name: name.to_owned(),
                one_time,
            },
            cx,
        );
    }

    /// The call link for a room.
    pub fn link_for(&self, id: &str) -> String {
        call::link(&self.args.web, id)
    }

    pub fn set_name(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if self.args.session.is_some() {
            self.rename_profile(name, cx);
            return;
        }
        self.args.name = name.to_owned();
        self.store.name = Some(name.to_owned());
        self.store.save();
        cx.notify();
    }

    fn start_call(&mut self, room: RoomRef, cx: &mut Context<Self>) {
        if self.args.session.is_none() {
            self.sign_in(Some(room.id), cx);
            return;
        }
        if self.call.is_some() {
            return;
        }
        let call = Call::start(room, &self.args, cx);
        let subscription = cx.subscribe(&call, |shell, _, ended: &Ended, cx| {
            shell.call_ended(ended.clone(), cx)
        });
        self.call = Some((call, subscription));
        // The call's windows open first (deferred), then home goes away.
        if let Some(home) = self.home.take() {
            cx.defer(move |cx| {
                let _ = home.update(cx, |_, window, _| window.remove_window());
            });
        }
        cx.notify();
    }

    fn call_ended(&mut self, ended: Ended, cx: &mut Context<Self>) {
        self.store.record(&ended);
        self.store.save();
        // Keep the model alive until its windows have closed.
        let call = self.call.take();
        cx.defer(move |_| drop(call));
        self.open_home(cx);
        cx.notify();
    }

    /// Quitting mid-call: leave properly, or the server shows a ghost of you
    /// in the room for another 20 s. Resolves once it has heard you leave.
    fn leave_for_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl Future<Output = ()> + use<>> {
        self.quitting = true;
        let call = self.call()?;
        Some(call.update(cx, |call, cx| {
            call.leave_now(cx);
            call.closed()
        }))
    }

    /// Deferred: the home window reads the shell while it's being built.
    pub fn open_home(&mut self, cx: &mut Context<Self>) {
        if self.home.is_some() || self.home_pending || self.quitting {
            return;
        }
        self.home_pending = true;
        let shell = cx.entity();
        cx.defer(move |cx| {
            let home = home::open(cx);
            shell.update(cx, |s, _| {
                s.home = home;
                s.home_pending = false;
            });
        });
    }
}

/// Logs go to the terminal, or to ~/Library/Logs/Quipcall/quipcall.log when
/// started from Finder. Each launch starts a fresh file; the last one is kept.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "quipcall_desktop=debug,quipcall_core=debug,livekit=info,warn".into());
    // DeepFilterNet warns of "clipping" on every frame above full scale, from
    // the audio thread. Our input gain puts loud voices there on purpose, and
    // the model works in floating point, so nothing clips. A separate filter,
    // so no RUST_LOG directive brings it back.
    let quiet_model = tracing_subscriber::filter::filter_fn(|meta| {
        let model = meta.target() == "df" || meta.target().starts_with("df::");
        !model || *meta.level() <= tracing::Level::ERROR
    });
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let file = if std::io::stderr().is_terminal() {
        None
    } else {
        log_file()
    };
    match file {
        Some(file) => builder
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .finish()
            .with(quiet_model)
            .init(),
        None => builder.finish().with(quiet_model).init(),
    }
    // Panics would otherwise vanish with stderr.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}");
        default_hook(info);
    }));
}

fn log_file() -> Option<std::fs::File> {
    let dir = std::path::PathBuf::from(std::env::var_os("HOME")?).join("Library/Logs/Quipcall");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("quipcall.log");
    let _ = std::fs::rename(&path, dir.join("quipcall.previous.log"));
    std::fs::File::create(path).ok()
}

fn main() {
    init_logging();
    let args = parse_args();
    tracing::info!(
        "quipcall {} ({}), server {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        args.server
    );

    let app = gpui_platform::application().with_assets(assets::Assets);
    // Call links (`quipcall://join/<room>`) arrive here, before or after launch.
    let (links_tx, links) = async_channel::unbounded::<String>();
    app.on_open_urls(move |urls| {
        for url in urls {
            let _ = links_tx.try_send(url);
        }
    });
    app.run(move |cx: &mut App| {
        let store = Store::load();
        let mut args = args.clone();
        let first_run = store.name.is_none() && !args.name_given && args.room.is_none();
        if !args.name_given
            && let Some(name) = &store.name
        {
            args.name = name.clone();
        }
        let shell = cx.new(|_| Shell {
            principal: None,
            auth_busy: false,
            auth_attempt: auth::SignInAttempt::default(),
            auth_message: String::new(),
            args,
            store,
            call: None,
            home: None,
            home_pending: false,
            quitting: false,
        });
        cx.set_global(GlobalShell(shell.clone()));
        shell.update(cx, |s, cx| s.refresh_account(cx));
        share_picker::bind_keys(cx);
        settings::bind_keys(cx);
        fullscreen::bind_keys(cx);
        text_input::bind_keys(cx);
        mac::log_window_events();

        shell.update(cx, |shell, cx| match shell.args.room.clone() {
            Some(id) => shell.start_call(
                RoomRef {
                    name: id.clone(),
                    id,
                    one_time: false,
                },
                cx,
            ),
            None if first_run => cx.defer(onboarding::open_first_run),
            None => shell.open_home(cx),
        });

        // Quit once home is closed and there's no call. Not merely when no
        // window is open: switching call modes swaps windows.
        cx.on_window_closed(|cx, _| {
            let shell = Shell::global(cx);
            let s = shell.read(cx);
            if cx.windows().is_empty() && !s.in_call() && !s.home_pending {
                cx.quit();
            }
        })
        .detach();

        // GPUI gives this 200 ms; saying goodbye takes a round trip at most.
        cx.on_app_quit(|cx| {
            let closed = Shell::global(cx).update(cx, |shell, cx| shell.leave_for_quit(cx));
            async move {
                if let Some(closed) = closed {
                    closed.await;
                    tracing::info!("left the call before quitting");
                }
            }
        })
        .detach();

        cx.spawn(async move |cx| {
            while let Ok(url) = links.recv().await {
                match room_from_link(&url) {
                    Some(room) => cx.update(|cx| {
                        Shell::global(cx).update(cx, |s, cx| {
                            if s.in_call() {
                                return;
                            }
                            if url.starts_with("quipcall://open/") {
                                s.open_home(cx);
                                s.sign_in(Some(room), cx);
                            } else {
                                s.join_link(room, cx);
                            }
                        })
                    }),
                    None => tracing::warn!("not a call link: {url}"),
                }
            }
        })
        .detach();

        #[cfg(feature = "snapshot")]
        snapshot::start(cx);
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::room_from_link;

    #[test]
    fn call_links() {
        let room = |url| room_from_link(url);
        assert_eq!(
            room("quipcall://join/quick-1a2b3c4d").as_deref(),
            Some("quick-1a2b3c4d")
        );
        assert_eq!(
            room("quipcall://join/room-1-ab12/").as_deref(),
            Some("room-1-ab12")
        );
        assert_eq!(room("quipcall://join/spike?x=1").as_deref(), Some("spike"));
        assert_eq!(room("quipcall://open/spike").as_deref(), Some("spike"));
        assert_eq!(room("quipcall://open/"), None);
        assert_eq!(room("quipcall://join/"), None);
        assert_eq!(room("quipcall://join/a%20b"), None);
        assert_eq!(room("https://quipcall.com/?room=spike"), None);
    }
}
