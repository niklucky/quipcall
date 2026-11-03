mod assets;
mod bubble;
mod call;
mod home;
mod mac;
mod regular;
mod screen_window;
#[cfg(feature = "snapshot")]
mod snapshot;
mod store;
mod theme;
mod toolbar;
mod video_mac;

use gpui::{App, AppContext as _, Context, Entity, Global, Subscription, WindowHandle};
use tracing_subscriber::EnvFilter;

use crate::call::{Call, Ended, RoomRef};
use crate::home::Home;
use crate::store::Store;

/// Launch options. Login and contacts replace the dev parts in Stage 4.
#[derive(Clone)]
pub struct Args {
    pub server: String,
    /// Base URL of the web client, for call links.
    pub web: String,
    pub name: String,
    /// Join this room right away instead of showing home (dev and tests).
    pub room: Option<String>,
    /// Turn the camera on as soon as the call starts.
    pub camera: bool,
    /// Start calls in regular mode instead of minimal.
    pub regular: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        server: "http://127.0.0.1:8080".into(),
        web: "http://127.0.0.1:5173".into(),
        name: std::env::var("USER").unwrap_or_else(|_| "desktop".into()),
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
                    _ => args.name = value,
                }
            }
            _ => usage(),
        }
    }
    args
}

fn usage() -> ! {
    eprintln!(
        "usage: quipcall-desktop [--server URL] [--web URL] [--name NAME] [--room ROOM] [--camera] [--regular]"
    );
    std::process::exit(2);
}

/// App-wide state: the home window, the current call and local data.
pub struct Shell {
    pub args: Args,
    pub store: Store,
    call: Option<(Entity<Call>, Subscription)>,
    home: Option<WindowHandle<Home>>,
    home_pending: bool,
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

    pub fn quick_call(&mut self, cx: &mut Context<Self>) {
        self.start_call(
            RoomRef {
                id: store::quick_room_id(),
                name: "Quick call".into(),
                one_time: true,
            },
            cx,
        );
    }

    pub fn join_saved(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(room) = self.store.rooms.iter().find(|r| r.id == id) {
            let room = RoomRef {
                id: room.id.clone(),
                name: room.name.clone(),
                one_time: false,
            };
            self.start_call(room, cx);
        }
    }

    pub fn new_room(&mut self, cx: &mut Context<Self>) {
        let name = self.store.next_room_name();
        self.store.add_room(&name);
        self.store.save();
        cx.notify();
    }

    pub fn remove_room(&mut self, id: &str, cx: &mut Context<Self>) {
        self.store.remove_room(id);
        self.store.save();
        cx.notify();
    }

    fn start_call(&mut self, room: RoomRef, cx: &mut Context<Self>) {
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

    /// Deferred: the home window reads the shell while it's being built.
    fn open_home(&mut self, cx: &mut Context<Self>) {
        if self.home.is_some() || self.home_pending {
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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            "quipcall_desktop=debug,quipcall_core=debug,livekit=info,warn".into()
        }))
        .init();
    let args = parse_args();

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            let shell = cx.new(|_| Shell {
                args: args.clone(),
                store: Store::load(),
                call: None,
                home: None,
                home_pending: false,
            });
            cx.set_global(GlobalShell(shell.clone()));

            shell.update(cx, |shell, cx| match shell.args.room.clone() {
                Some(id) => shell.start_call(
                    RoomRef {
                        name: id.clone(),
                        id,
                        one_time: false,
                    },
                    cx,
                ),
                None => shell.open_home(cx),
            });

            // Quit once the last window is gone (home closed, no call).
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            #[cfg(feature = "snapshot")]
            snapshot::start(cx);
            cx.activate(true);
        });
}
