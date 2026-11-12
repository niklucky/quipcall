//! Native room creation and access management. All permissions are checked by
//! the server; callbacks are discarded when the account or window changes.
use crate::{
    Args, Shell,
    assets::Icon,
    auth,
    store::{RoomKind, SavedRoom},
    text_input::{InputEvent, TextInput},
    theme::{ACCENT, BG, DANGER, FIELD, LINE, TEXT, TEXT_DIM, TEXT_FAINT},
    ui::{Kind, button},
};
use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, Div, Entity, FontWeight, Subscription, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, px, rgb, size,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Deserialize)]
struct Member {
    id: String,
    name: String,
    kind: String,
    revoked: bool,
}
#[derive(Clone, Deserialize)]
struct Invitation {
    hash: String,
    kind: String,
    email: Option<String>,
    expires: i64,
}
#[derive(Clone, Deserialize)]
struct Detail {
    room: SavedRoom,
    #[serde(default)]
    members: Vec<Member>,
    #[serde(default)]
    invites: Vec<Invitation>,
}
#[derive(Clone, PartialEq)]
enum Confirm {
    Remove(String),
    Close,
}
#[derive(Clone)]
enum Action {
    Load,
    Create,
    Keep,
    Rename,
    InviteGuest,
    InviteMember,
    Revoke(String),
    Remove(String),
    Close,
}
struct Reply {
    detail: Option<Detail>,
    link: Option<String>,
    mutation_error: Option<String>,
}

pub fn open(id: Option<String>, cx: &mut App) {
    // Reuse only the same room's window; a new room never replaces a pending
    // invitation being copied from another room.
    for handle in cx.windows() {
        if let Some(existing) = handle.downcast::<RoomAccess>() {
            let same = existing
                .read(cx)
                .is_ok_and(|v| v.id == id && !v.invalid_session && !v.closed);
            if same {
                let _ = existing.update(cx, |v, w, cx| {
                    w.activate_window();
                    if !v.busy {
                        v.run(Action::Load, cx);
                    }
                });
                return;
            }
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(540.0), px(650.0)),
            cx,
        ))),
        window_min_size: Some(size(px(460.0), px(420.0))),
        titlebar: Some(TitlebarOptions {
            title: Some(
                if id.is_some() {
                    "Room access"
                } else {
                    "New room"
                }
                .into(),
            ),
            appears_transparent: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    // Often called from inside a Call or Home entity update.
    cx.defer(move |cx| {
        let _ = cx
            .open_window(options, |w, cx| cx.new(|cx| RoomAccess::new(id, w, cx)))
            .inspect_err(|e| tracing::warn!("open room access: {e:#}"));
    });
}

pub struct RoomAccess {
    id: Option<String>,
    args: Args,
    name: Entity<TextInput>,
    email: Entity<TextInput>,
    kind: RoomKind,
    detail: Option<Detail>,
    busy: bool,
    invalid_session: bool,
    closed: bool,
    error: Option<String>,
    notice: Option<String>,
    link: Option<String>,
    copied: Option<String>,
    confirm: Option<Confirm>,
    _subscriptions: Vec<Subscription>,
}
impl RoomAccess {
    fn new(id: Option<String>, w: &mut Window, cx: &mut Context<Self>) -> Self {
        let shell = Shell::global(cx);
        let args = shell.read(cx).args.clone();
        let name = cx.new(|cx| TextInput::new("", "Room name", cx));
        let email = cx.new(|cx| TextInput::new("", "Member’s account email", cx).with_max_len(254));
        if id.is_none() {
            name.update(cx, |v, cx| v.focus(w, cx));
        }
        let subscriptions = vec![
            cx.subscribe_in(&name, w, |this, _, event, w, cx| match event {
                InputEvent::Submit => this.run(
                    if this.id.is_none() {
                        Action::Create
                    } else {
                        Action::Rename
                    },
                    cx,
                ),
                InputEvent::Cancel => w.remove_window(),
                InputEvent::Changed => cx.notify(),
            }),
            cx.subscribe_in(&email, w, |this, _, event, w, cx| match event {
                InputEvent::Submit => this.run(Action::InviteMember, cx),
                InputEvent::Cancel => w.remove_window(),
                InputEvent::Changed => cx.notify(),
            }),
            cx.observe(&shell, |this, shell, cx| {
                if shell.read(cx).args.session != this.args.session {
                    this.invalid_session = true;
                    this.detail = None;
                    this.link = None;
                    this.confirm = None;
                    this.error = Some("Your account changed. Close and reopen this window.".into());
                }
                cx.notify();
            }),
        ];
        let mut this = Self {
            id,
            args,
            name,
            email,
            kind: RoomKind::Private,
            detail: None,
            busy: false,
            invalid_session: false,
            closed: false,
            error: None,
            notice: None,
            link: None,
            copied: None,
            confirm: None,
            _subscriptions: subscriptions,
        };
        if this.id.is_some() {
            this.run(Action::Load, cx);
        }
        this
    }
    fn owner(&self, cx: &App) -> bool {
        self.detail.as_ref().is_some_and(|d| {
            Shell::global(cx)
                .read(cx)
                .principal
                .as_ref()
                .is_some_and(|p| p.id == d.room.owner && p.kind == "user")
        })
    }
    fn can_create(&self, cx: &App) -> bool {
        let shell = Shell::global(cx);
        let shell = shell.read(cx);
        shell.args.session.is_some() && shell.principal.as_ref().is_none_or(|p| p.kind == "user")
    }
    fn run(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy || self.invalid_session || self.closed {
            return;
        }
        if matches!(action, Action::Load) && self.id.is_none() {
            return;
        }
        if !matches!(action, Action::Load | Action::Create) && !self.owner(cx) {
            return;
        }
        if matches!(action, Action::Create) && !self.can_create(cx) {
            return;
        }
        let name = self.name.read(cx).text().trim().to_owned();
        let email = self.email.read(cx).text().trim().to_owned();
        if matches!(action, Action::Create | Action::Rename) && name.is_empty() {
            self.error = Some("Enter a room name.".into());
            cx.notify();
            return;
        }
        if matches!(action, Action::InviteMember) && email.is_empty() {
            self.error = Some("Enter the member’s account email.".into());
            cx.notify();
            return;
        }
        let args = self.args.clone();
        let id = self.id.clone();
        let kind = self.kind;
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.confirm = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let performed = action.clone();
            let result = execute(&args, id.as_deref(), kind, &name, &email, &action).await;
            let _ = this.update(cx, |v, cx| {
                if v.invalid_session || Shell::global(cx).read(cx).args.session != v.args.session {
                    return;
                }
                v.busy = false;
                match result {
                    Ok(reply) => {
                        let mutation_failed = reply.mutation_error.is_some();
                        v.error = reply.mutation_error;
                        if let Some(detail) = reply.detail {
                            let created = v.id.is_none();
                            v.id = Some(detail.room.id.clone());
                            v.kind = detail.room.kind;
                            if matches!(performed, Action::Load | Action::Create | Action::Rename) {
                                v.name.update(cx, |input, cx| {
                                    input.set_text(detail.room.name.clone(), cx)
                                });
                            }
                            let room = detail.room.clone();
                            Shell::global(cx).update(cx, |s, cx| {
                                s.store.cache_room(room.clone());
                                s.store.save();
                                cx.notify();
                            });
                            v.detail = Some(detail);
                            if created {
                                let id = room.id;
                                crate::home::with_home(cx, move |home, _, cx| {
                                    home.select_room(id.clone(), cx)
                                });
                            }
                        } else if matches!(performed, Action::Close) && !mutation_failed {
                            v.closed = true;
                            v.detail = None;
                            v.link = None;
                            if let Some(id) = &v.id {
                                Shell::global(cx).update(cx, |s, cx| {
                                    s.store.remove_room(id);
                                    s.store.save();
                                    cx.notify();
                                });
                                let id = id.clone();
                                crate::home::with_home(cx, move |home, _, cx| {
                                    home.forget_room(&id, cx)
                                });
                            }
                        }
                        if let Some(link) = reply.link {
                            v.link = Some(link);
                            v.copied = None;
                        }
                        if !mutation_failed {
                            v.notice = Some(
                                match performed {
                                    Action::Load => "Room access refreshed.",
                                    Action::Create => "Room created.",
                                    Action::Keep => "Room kept. Its link no longer expires.",
                                    Action::Rename => "Room name saved.",
                                    Action::InviteGuest | Action::InviteMember => {
                                        "Invitation ready. Copy the link and share it."
                                    }
                                    Action::Revoke(_) => "Invitation revoked.",
                                    Action::Remove(_) => "Access removed.",
                                    Action::Close => "Room closed.",
                                }
                                .into(),
                            );
                            if matches!(performed, Action::Revoke(_) | Action::Remove(_)) {
                                v.link = None;
                                v.copied = None;
                            }
                        }
                    }
                    Err(e) => {
                        let recovered = Shell::global(cx)
                            .update(cx, |s, cx| s.recover_auth(&e, args.session.as_deref(), cx));
                        v.invalid_session |= recovered;
                        v.error = Some(e.to_string());
                    }
                }
                #[cfg(feature = "snapshot")]
                v.export_test_state();
                cx.notify();
            });
        })
        .detach();
    }
    #[cfg(feature = "snapshot")]
    pub fn scripted(&mut self, action: &str, cx: &mut Context<Self>) {
        match action {
            "room-create-private" | "room-create-public" => {
                self.kind = if action.ends_with("public") {
                    RoomKind::Public
                } else {
                    RoomKind::Private
                };
                self.name.update(cx, |v, cx| v.set_text("Project room", cx));
                self.run(Action::Create, cx);
            }
            "room-guest" => self.run(Action::InviteGuest, cx),
            "room-member" => {
                self.email
                    .update(cx, |v, cx| v.set_text("member@example.test", cx));
                self.run(Action::InviteMember, cx);
            }
            "room-refresh" => self.run(Action::Load, cx),
            "room-remove" => {
                if let Some(m) = self
                    .detail
                    .as_ref()
                    .and_then(|d| d.members.iter().find(|m| !m.revoked))
                    .cloned()
                {
                    self.run(Action::Remove(m.id), cx);
                }
            }
            "room-retry-remove" => {
                if let Some(m) = self
                    .detail
                    .as_ref()
                    .and_then(|d| d.members.iter().find(|m| m.revoked))
                    .cloned()
                {
                    self.run(Action::Remove(m.id), cx);
                }
            }
            "room-revoke" => {
                if let Some(i) = self
                    .detail
                    .as_ref()
                    .and_then(|d| d.invites.first())
                    .cloned()
                {
                    self.run(Action::Revoke(i.hash), cx);
                }
            }
            "room-close" => self.run(Action::Close, cx),
            _ => {}
        }
    }
    #[cfg(feature = "snapshot")]
    fn export_test_state(&self) {
        let Ok(path) = std::env::var("QUIPCALL_ROOM_TEST_STATE") else {
            return;
        };
        let value = json!({"room":self.id,"kind":self.kind,"closed":self.closed,"link":self.link,"error":self.error,"notice":self.notice,
            "members":self.detail.as_ref().map(|d|d.members.iter().map(|m|json!({"id":m.id,"revoked":m.revoked})).collect::<Vec<_>>()),
            "invites":self.detail.as_ref().map(|d|d.invites.len())});
        let _ = std::fs::write(path, serde_json::to_vec(&value).unwrap());
    }

    fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        if self.invalid_session {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.copied = Some(text);
        cx.notify();
    }
}

async fn execute(
    args: &Args,
    id: Option<&str>,
    kind: RoomKind,
    name: &str,
    email: &str,
    action: &Action,
) -> anyhow::Result<Reply> {
    let request = |path: &str, body: Option<Value>, method: &str| {
        auth::request(&args.server, args.session.as_deref(), path, body, method)
    };
    if matches!(action, Action::Create) {
        let room: SavedRoom = serde_json::from_value(
            request("/rooms", Some(json!({"name":name,"kind":kind})), "POST").await?,
        )?;
        return Ok(Reply {
            detail: Some(Detail {
                room,
                members: vec![],
                invites: vec![],
            }),
            link: None,
            mutation_error: None,
        });
    }
    let room_id = id.unwrap_or_default();
    let path = format!("/rooms/{room_id}");
    let mut link = None;
    let mutation = match action {
        Action::Load => Ok(json!({})),
        Action::Create => unreachable!("handled above"),
        Action::Keep => request(&format!("{path}/keep"), Some(json!({})), "POST").await,
        Action::Rename => request(&path, Some(json!({"name":name})), "PATCH").await,
        Action::InviteGuest | Action::InviteMember => {
            let kind = if matches!(action, Action::InviteGuest) {
                "guest"
            } else {
                "member"
            };
            request(
                &format!("{path}/invites"),
                Some(json!({"kind":kind,"email":email})),
                "POST",
            )
            .await
            .and_then(|value| {
                link = Some(
                    value["url"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing invitation link"))?
                        .into(),
                );
                Ok(value)
            })
        }
        Action::Revoke(hash) => {
            request(&format!("{path}/invites/{hash}"), Some(json!({})), "DELETE").await
        }
        Action::Remove(id) => {
            request(&format!("{path}/members/{id}"), Some(json!({})), "DELETE").await
        }
        Action::Close => request(&path, Some(json!({})), "DELETE").await,
    };
    let error = match mutation {
        Err(e) if auth::unauthorized(&e) => return Err(e),
        other => other.err().map(|e| e.to_string()),
    };
    // Removal can persist revocation even when LiveKit disconnect fails. Refresh
    // that state, keep the error and offer retry instead of claiming success.
    if matches!(action, Action::Close) {
        return Ok(Reply {
            detail: None,
            link: None,
            mutation_error: error,
        });
    }
    if let Some(error) = error
        .as_ref()
        .filter(|_| !matches!(action, Action::Remove(_)))
    {
        anyhow::bail!("{error}");
    }
    let detail = request(&format!("/rooms/{room_id}"), None, "GET")
        .await
        .and_then(|v| serde_json::from_value::<Detail>(v).map_err(Into::into));
    match detail {
        Ok(detail) => Ok(Reply {
            detail: Some(detail),
            link,
            mutation_error: error,
        }),
        Err(e) if auth::unauthorized(&e) => Err(e),
        // Don't lose a freshly issued single-use link if the follow-up read fails.
        Err(e) if link.is_some() => Ok(Reply {
            detail: None,
            link,
            mutation_error: Some(format!(
                "Invitation created, but refreshing the list failed: {e}"
            )),
        }),
        Err(e) => Err(e),
    }
}

fn field(input: &Entity<TextInput>) -> Div {
    div()
        .h(px(36.0))
        .flex_1()
        .min_w_0()
        .px(px(11.0))
        .flex()
        .items_center()
        .rounded(px(8.0))
        .bg(rgb(FIELD))
        .border_1()
        .border_color(rgb(LINE))
        .child(input.clone())
}
fn note(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(12.0))
        .text_color(rgb(TEXT_DIM))
        .child(text.into())
}
fn heading(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(14.0))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

impl Render for RoomAccess {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = self.owner(cx);
        let ready = !self.busy && !self.invalid_session && !self.closed;
        let mut body = div().flex().flex_col().gap(px(14.0));
        if self.invalid_session {
            body = body.child(note(
                "Sign in to the intended account, then reopen Room access.",
            ));
        } else if self.closed {
            body = body.child(heading("Room closed")).child(note(
                "The link no longer admits anyone. Your local call history is kept.",
            ));
        } else if self.id.is_none() {
            body = body
                .child(heading("Create a room"))
                .child(field(&self.name));
            let mut choices = div().flex().gap(px(8.0));
            for (key, kind) in [("private", RoomKind::Private), ("public", RoomKind::Public)] {
                choices = choices.child(
                    button(
                        key,
                        kind.label(),
                        Some(Icon::Users),
                        if self.kind == kind {
                            Kind::Primary
                        } else {
                            Kind::Secondary
                        },
                        false,
                    )
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if !v.busy {
                            v.kind = kind;
                            cx.notify();
                        }
                    })),
                );
            }
            body = body.child(choices).child(note(self.kind.description()));
            if !self.can_create(cx) {
                body = body
                    .child(note("Sign in with an account to create rooms."))
                    .child(
                        button(
                            "signin",
                            "Sign in",
                            Some(Icon::LogIn),
                            Kind::Secondary,
                            false,
                        )
                        .on_click(|_, _, cx| {
                            Shell::global(cx).update(cx, |s, cx| s.sign_in(None, cx))
                        }),
                    );
            }
            body = body.child(
                button(
                    "create",
                    if self.busy {
                        "Creating…"
                    } else {
                        "Create room"
                    },
                    Some(Icon::Plus),
                    Kind::Primary,
                    false,
                )
                .when(!ready || !self.can_create(cx), |v| v.opacity(0.45))
                .on_click(cx.listener(|v, _, _, cx| v.run(Action::Create, cx))),
            );
        } else if let Some(d) = self.detail.clone() {
            body = body
                .child(heading(d.room.kind.label()))
                .child(note(d.room.kind.description()));
            if owner {
                body = body.child(
                    div().flex().gap(px(8.0)).child(field(&self.name)).child(
                        button("rename", "Save name", None, Kind::Secondary, false)
                            .when(!ready, |v| v.opacity(0.45))
                            .on_click(cx.listener(|v, _, _, cx| v.run(Action::Rename, cx))),
                    ),
                );
            } else {
                body = body.child(heading(d.room.name.clone()));
            }
            let url = crate::call::link(&self.args.web, &d.room.id);
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(rgb(TEXT_FAINT))
                            .child(url.clone()),
                    )
                    .child(
                        button(
                            "room-link",
                            if self.copied.as_ref() == Some(&url) {
                                "Copied"
                            } else {
                                "Copy room address"
                            },
                            Some(Icon::Link),
                            Kind::Secondary,
                            true,
                        )
                        .on_click(cx.listener(move |v, _, _, cx| v.copy(url.clone(), cx))),
                    ),
            );
            if let Some(expires) = d.room.expires {
                let at = expires.max(0) as u64;
                body = body.child(note(format!(
                    "Available until {} at {} if unused. Active calls stay open.",
                    crate::store::day(at),
                    crate::store::clock(at)
                )));
                if owner {
                    body = body.child(
                        button(
                            "keep-room",
                            "Keep as room",
                            Some(Icon::Plus),
                            Kind::Secondary,
                            true,
                        )
                        .when(!ready, |v| v.opacity(0.45))
                        .on_click(cx.listener(|v, _, _, cx| v.run(Action::Keep, cx))),
                    );
                }
            }
            if d.room.kind == RoomKind::Private {
                body = body.child(note(
                    "The room address is for people who already have access.",
                ));
            }
            if owner && d.room.kind == RoomKind::Private {
                body=body.child(heading("Invite someone"))
                    .child(button("guest-link","Create single-use guest link",Some(Icon::UserPlus),Kind::Primary,false).when(!ready,|v|v.opacity(0.45)).on_click(cx.listener(|v,_,_,cx|v.run(Action::InviteGuest,cx))))
                    .child(note("One person can claim it, then return until you remove them. Unclaimed links expire in 7 days."))
                    .child(div().flex().gap(px(8.0)).child(field(&self.email)).child(button("member-invite","Invite member",None,Kind::Secondary,false).when(!ready,|v|v.opacity(0.45)).on_click(cx.listener(|v,_,_,cx|v.run(Action::InviteMember,cx)))))
                    .child(note("Members sign in with the invited email. This does not create a new account."));
            } else if !owner && d.room.kind == RoomKind::Private {
                body = body.child(note(
                    "Ask the room owner to invite someone or change access.",
                ));
            }
            if let Some(link) = self.link.clone() {
                body = body.child(
                    div()
                        .p(px(12.0))
                        .rounded(px(9.0))
                        .bg(rgb(FIELD))
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(heading("Invitation ready"))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(rgb(TEXT_DIM))
                                .child(link.clone()),
                        )
                        .child(
                            button(
                                "copy-invitation",
                                if self.copied.as_ref() == Some(&link) {
                                    "Copied"
                                } else {
                                    "Copy invitation link"
                                },
                                Some(if self.copied.as_ref() == Some(&link) {
                                    Icon::Check
                                } else {
                                    Icon::Copy
                                }),
                                Kind::Primary,
                                false,
                            )
                            .on_click(cx.listener(move |v, _, _, cx| v.copy(link.clone(), cx))),
                        ),
                );
            }
            if owner {
                if !d.members.is_empty() {
                    body = body.child(heading("People"));
                }
                for (index, m) in d
                    .members
                    .iter()
                    .filter(|m| m.id != d.room.owner)
                    .enumerate()
                {
                    let id = m.id.clone();
                    let confirmation = self.confirm == Some(Confirm::Remove(id.clone()));
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().child(m.name.clone()))
                                    .child(note(format!(
                                        "{}{}",
                                        if m.kind == "guest" { "Guest" } else { "Member" },
                                        if m.revoked { " · access removed" } else { "" }
                                    ))),
                            )
                            .child(
                                button(
                                    ("remove-member", index),
                                    if m.revoked {
                                        "Disconnect again"
                                    } else if confirmation {
                                        "Confirm removal"
                                    } else {
                                        "Remove"
                                    },
                                    None,
                                    if confirmation {
                                        Kind::Danger
                                    } else {
                                        Kind::Secondary
                                    },
                                    true,
                                )
                                .when(!ready, |v| v.opacity(0.45))
                                .on_click(cx.listener(
                                    move |v, _, _, cx| {
                                        if !v.busy {
                                            if confirmation
                                                || v.detail.as_ref().is_some_and(|d| {
                                                    d.members
                                                        .iter()
                                                        .any(|m| m.id == id && m.revoked)
                                                })
                                            {
                                                v.run(Action::Remove(id.clone()), cx);
                                            } else {
                                                v.confirm = Some(Confirm::Remove(id.clone()));
                                                cx.notify();
                                            }
                                        }
                                    },
                                )),
                            ),
                    );
                }
                if !d.invites.is_empty() {
                    body = body.child(heading("Pending invitations"));
                }
                for (index, i) in d.invites.iter().enumerate() {
                    let hash = i.hash.clone();
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().child(i.email.clone().unwrap_or_else(
                                        || {
                                            format!(
                                                "{} link",
                                                if i.kind == "guest" { "Guest" } else { "Member" }
                                            )
                                        },
                                    )))
                                    .child(note(format!(
                                        "Expires {}",
                                        crate::store::day(i.expires.max(0) as u64)
                                    ))),
                            )
                            .child(
                                button(
                                    ("revoke-invite", index),
                                    "Revoke",
                                    None,
                                    Kind::Secondary,
                                    true,
                                )
                                .when(!ready, |v| v.opacity(0.45))
                                .on_click(cx.listener(
                                    move |v, _, _, cx| v.run(Action::Revoke(hash.clone()), cx),
                                )),
                            ),
                    );
                }
                let confirming = self.confirm == Some(Confirm::Close);
                body=body.child(div().mt(px(12.0)).pt(px(14.0)).border_t_1().border_color(rgb(LINE)).child(button("close-room",if confirming {"Confirm: close for everyone"}else{"Close room"},Some(Icon::Trash),if confirming {Kind::Danger}else{Kind::Secondary},false).when(!ready,|v|v.opacity(0.45)).on_click(cx.listener(move|v,_,_,cx| {if !v.busy {if confirming {v.run(Action::Close,cx);}else{v.confirm=Some(Confirm::Close);cx.notify();}}}))).child(note("Closes the room and disconnects everyone. Local history stays on your Mac.")));
            }
        } else {
            body = body.child(note(if self.busy {
                "Loading room…"
            } else {
                "Room details could not be loaded."
            }));
        }
        let can_refresh = self.id.is_some() && !self.invalid_session && !self.closed;
        div()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .flex()
            .flex_col()
            .child(div().h(px(40.0)).flex_none())
            .child(
                div()
                    .id("room-access-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(24.0))
                    .pb(px(18.0))
                    .child(body),
            )
            .when(
                self.error.is_some() || self.notice.is_some() || self.busy,
                |v| {
                    let (message, color) = if let Some(error) = &self.error {
                        (error.as_str(), DANGER)
                    } else if self.busy {
                        ("Working…", ACCENT)
                    } else {
                        (self.notice.as_deref().unwrap_or_default(), TEXT_DIM)
                    };
                    v.child(
                        div()
                            .flex_none()
                            .px(px(24.0))
                            .py(px(10.0))
                            .text_size(px(12.0))
                            .text_color(rgb(color))
                            .child(message.to_owned()),
                    )
                },
            )
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .p(px(16.0))
                    .flex()
                    .gap(px(8.0))
                    .when(can_refresh, |v| {
                        v.child(
                            button("refresh", "Refresh", None, Kind::Secondary, false)
                                .when(!ready, |v| v.opacity(0.45))
                                .on_click(cx.listener(|v, _, _, cx| v.run(Action::Load, cx))),
                        )
                    })
                    .when(self.confirm.is_some(), |v| {
                        v.child(
                            button("cancel-confirm", "Cancel", None, Kind::Secondary, false)
                                .on_click(cx.listener(|v, _, _, cx| {
                                    v.confirm = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        button("done", "Done", None, Kind::Secondary, false)
                            .on_click(|_, w, _| w.remove_window()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn server(
        replies: Vec<(&'static str, &'static str, u16, Value)>,
    ) -> (Args, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            for (method, path, status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let headers = String::from_utf8(request).unwrap();
                assert!(
                    headers.starts_with(&format!("{method} {path} HTTP/1.1\r\n")),
                    "{headers}"
                );
                assert!(
                    headers
                        .to_lowercase()
                        .contains("authorization: bearer test-session\r\n")
                );
                assert!(headers.to_lowercase().contains("x-quipcall: 1\r\n"));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|s| s.parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body_bytes = vec![0; length];
                stream.read_exact(&mut body_bytes).unwrap();
                let body = body.to_string();
                write!(stream,"HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        (
            Args {
                session: Some("test-session".into()),
                server,
                web: "http://localhost".into(),
                name: "Owner".into(),
                name_given: true,
                room: None,
                camera: false,
                regular: false,
            },
            task,
        )
    }
    #[test]
    fn room_actions_preserve_401_for_session_recovery() {
        for (action, method, path) in [
            (Action::Load, "GET", "/api/rooms/r"),
            (Action::Keep, "POST", "/api/rooms/r/keep"),
            (Action::Rename, "PATCH", "/api/rooms/r"),
            (Action::Close, "DELETE", "/api/rooms/r"),
            (
                Action::Remove("guest".into()),
                "DELETE",
                "/api/rooms/r/members/guest",
            ),
        ] {
            let (args, task) = server(vec![(method, path, 401, json!({"error":"expired"}))]);
            let result = crate::test_support::block_on(execute(
                &args,
                Some("r"),
                RoomKind::Private,
                "Name",
                "",
                &action,
            ));
            assert!(auth::unauthorized(&result.err().unwrap()));
            task.join().unwrap();
        }
        let (args, task) = server(vec![
            ("PATCH", "/api/rooms/r", 200, json!({})),
            ("GET", "/api/rooms/r", 401, json!({"error":"expired"})),
        ]);
        let result = crate::test_support::block_on(execute(
            &args,
            Some("r"),
            RoomKind::Private,
            "Name",
            "",
            &Action::Rename,
        ));
        assert!(auth::unauthorized(&result.err().unwrap()));
        task.join().unwrap();
    }

    fn room() -> Value {
        json!({"id":"room","name":"Project","kind":"private","owner":"owner"})
    }
    #[test]
    fn created_room_is_returned_without_a_second_network_request() {
        let (args, task) = server(vec![("POST", "/api/rooms", 200, room())]);
        let reply = crate::test_support::block_on(execute(
            &args,
            None,
            RoomKind::Private,
            "Project",
            "",
            &Action::Create,
        ))
        .unwrap();
        assert_eq!(reply.detail.unwrap().room.owner, "owner");
        task.join().unwrap();
    }
    #[test]
    fn issued_invite_survives_followup_read_failure() {
        let (args, task) = server(vec![
            (
                "POST",
                "/api/rooms/room/invites",
                200,
                json!({"url":"https://example.test/#roomInvite=test"}),
            ),
            (
                "GET",
                "/api/rooms/room",
                503,
                json!({"error":"Temporarily unavailable"}),
            ),
        ]);
        let reply = crate::test_support::block_on(execute(
            &args,
            Some("room"),
            RoomKind::Private,
            "",
            "",
            &Action::InviteGuest,
        ))
        .unwrap();
        assert_eq!(
            reply.link.as_deref(),
            Some("https://example.test/#roomInvite=test")
        );
        assert!(reply.mutation_error.unwrap().contains("Invitation created"));
        task.join().unwrap();
    }
    #[test]
    fn disconnect_failure_keeps_revoked_membership_visible_for_retry() {
        let (args, task) = server(vec![
            (
                "DELETE",
                "/api/rooms/room/members/guest",
                503,
                json!({"error":"Access revoked, but disconnection failed. Retry removal."}),
            ),
            (
                "GET",
                "/api/rooms/room",
                200,
                json!({"room":room(),"members":[{"id":"guest","name":"Guest","kind":"guest","revoked":true}],"invites":[]}),
            ),
        ]);
        let reply = crate::test_support::block_on(execute(
            &args,
            Some("room"),
            RoomKind::Private,
            "",
            "",
            &Action::Remove("guest".into()),
        ))
        .unwrap();
        assert!(reply.detail.unwrap().members[0].revoked);
        assert!(reply.mutation_error.unwrap().contains("Retry removal"));
        task.join().unwrap();
    }
}
