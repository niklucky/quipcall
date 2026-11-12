//! Browser approval keeps email/OAuth out of the native UI. Only the app that
//! holds the polling secret can exchange an approved challenge for a session.
use crate::{Shell, call::RoomRef, store::SavedRoom};
use gpui::Context;
use serde::Deserialize;
use serde_json::json;

#[derive(Clone, Deserialize)]
pub struct Principal {
    pub id: String,
    pub name: String,
    pub kind: String,
}
fn key_account(server: &str) -> String {
    format!(
        "{}:{}",
        server.trim_end_matches('/'),
        std::env::var("QUIPCALL_DATA_DIR").unwrap_or_default()
    )
}
pub fn load(server: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::get_generic_password(
            "com.quipcall.session",
            &key_account(server),
        )
        .ok()
        .and_then(|v| String::from_utf8(v).ok())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = server;
        None
    }
}
fn save(server: &str, token: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(
            "com.quipcall.session",
            &key_account(server),
            token.as_bytes(),
        )?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (server, token);
        anyhow::bail!("Secure credential storage is not implemented on this platform");
    }
    #[allow(unreachable_code)]
    Ok(())
}
fn clear(server: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = security_framework::passwords::delete_generic_password(
            "com.quipcall.session",
            &key_account(server),
        );
    }
}
pub(crate) use quipcall_core::http::{RequestError, request};

/// A completion can only modify the operation generation that started it.
pub struct Generation<T> {
    generation: u64,
    pub payload: Option<T>,
}
impl<T> Default for Generation<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            payload: None,
        }
    }
}
impl<T> Generation<T> {
    fn start(&mut self, payload: T) -> u64 {
        self.generation += 1;
        self.payload = Some(payload);
        self.generation
    }
    fn current(&self, generation: u64) -> bool {
        self.pending() && self.generation == generation
    }
    fn cancel(&mut self) {
        self.payload = None;
    }
    fn finish(&mut self, generation: u64) -> bool {
        if !self.current(generation) {
            return false;
        }
        self.cancel();
        true
    }
    pub fn pending(&self) -> bool {
        self.payload.is_some()
    }
}
#[derive(Default)]
pub struct SignIn {
    pub code: Option<String>,
    room: Option<String>,
}
pub type SignInAttempt = Generation<SignIn>;
pub type PendingOperation = Generation<()>;
impl SignInAttempt {
    fn request(&mut self, room: Option<String>) -> Option<u64> {
        if self.pending() && room.is_none() {
            return None;
        }
        Some(self.start(SignIn { code: None, room }))
    }
    pub fn code(&self) -> Option<String> {
        self.payload.as_ref().and_then(|p| p.code.clone())
    }
    fn room(&self) -> Option<String> {
        self.payload.as_ref().and_then(|p| p.room.clone())
    }
}
pub(crate) fn unauthorized(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RequestError>()
        .is_some_and(|e| e.status == 401)
}
fn session_matches(current: Option<&str>, expected: Option<&str>) -> bool {
    current == expected
}
fn forget_session(
    session: &mut Option<String>,
    principal: &mut Option<Principal>,
    store: &mut crate::store::Store,
    clear_saved: impl FnOnce(),
) {
    clear_saved();
    *session = None;
    *principal = None;
    store.rooms.clear();
    store.bind_rooms(None);
}

async fn load_account(
    server: &str,
    session: Option<&str>,
) -> anyhow::Result<(Principal, Vec<SavedRoom>)> {
    let (principal, rooms) = quipcall_core::http::join(
        request(server, session, "/auth/me", None, "GET"),
        request(server, session, "/rooms", None, "GET"),
    )
    .await;
    // A 401 takes precedence over an unrelated transport error from its peer.
    if let Err(e) = &principal
        && unauthorized(e)
    {
        return Err(principal.unwrap_err());
    }
    if let Err(e) = &rooms
        && unauthorized(e)
    {
        return Err(rooms.unwrap_err());
    }
    Ok((
        serde_json::from_value(principal?)?,
        serde_json::from_value(rooms?)?,
    ))
}

impl Shell {
    pub fn auth_busy(&self) -> bool {
        self.auth_attempt.pending() || self.room_creation.pending()
    }
    pub fn expire_session(&mut self, expected: Option<&str>, cx: &mut Context<Self>) -> bool {
        if !session_matches(self.args.session.as_deref(), expected) {
            return false;
        }
        self.room_creation.cancel();
        forget_session(
            &mut self.args.session,
            &mut self.principal,
            &mut self.store,
            || {
                if expected.is_some() {
                    clear(&self.args.server);
                }
            },
        );
        self.store.save();
        self.auth_message = "Your session expired. Sign in to continue.".into();
        cx.notify();
        true
    }

    pub(crate) fn recover_auth(
        &mut self,
        error: &anyhow::Error,
        expected: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !unauthorized(error) {
            return false;
        }
        if self.expire_session(expected, cx) {
            self.sign_in(None, cx);
        }
        true
    }

    pub fn sign_in(&mut self, room: Option<String>, cx: &mut Context<Self>) {
        if self.room_creation.pending() {
            if room.is_none() {
                return;
            }
            self.room_creation.cancel();
        }
        let Some(generation) = self.auth_attempt.request(room.clone()) else {
            return;
        };
        self.auth_message = "Opening browser sign-in…".into();
        cx.notify();
        let args = self.args.clone();
        cx.spawn(async move |this, cx| {
            let start = request(
                &args.server,
                None,
                "/auth/desktop/start",
                Some(json!({})),
                "POST",
            )
            .await;
            let result: anyhow::Result<(String, Principal)> = async {
                let start = start?;
                let id = start["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid sign-in response"))?
                    .to_owned();
                let secret = start["secret"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid sign-in response"))?
                    .to_owned();
                let code = start["code"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Update the server to use secure Mac sign-in"))?
                    .to_owned();
                let mut url = format!("{}/?desktop={id}", args.web.trim_end_matches('/'));
                if let Some(room) = &room {
                    url.push_str(&format!("&room={room}"));
                }
                let opened = this.update(cx, |s, cx| {
                    if !s.auth_attempt.current(generation) {
                        return false;
                    }
                    s.auth_attempt.payload.as_mut().unwrap().code = Some(code);
                    s.auth_message = "Type this code in the browser on this Mac.".into();
                    cx.open_url(&url);
                    cx.notify();
                    true
                })?;
                if !opened {
                    anyhow::bail!("Sign-in cancelled");
                }
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
                let mut delay = std::time::Duration::from_secs(2);
                while std::time::Instant::now() < deadline {
                    if !this.update(cx, |s, _| s.auth_attempt.current(generation))? {
                        anyhow::bail!("Sign-in cancelled");
                    }
                    cx.background_executor()
                        .timer(
                            delay
                                .min(deadline.saturating_duration_since(std::time::Instant::now())),
                        )
                        .await;
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    if !this.update(cx, |s, _| s.auth_attempt.current(generation))? {
                        anyhow::bail!("Sign-in cancelled");
                    }
                    let value = request(
                        &args.server,
                        args.session.as_deref(),
                        "/auth/desktop/poll",
                        Some(json!({"id":id,"secret":secret})),
                        "POST",
                    )
                    .await;
                    let value = match value {
                        Err(e) if quipcall_core::http::transient(&e) => {
                            delay = (delay * 2).min(std::time::Duration::from_secs(30));
                            continue;
                        }
                        result => result?,
                    };
                    delay = std::time::Duration::from_secs(2);
                    if value["pending"] == true {
                        continue;
                    }
                    let token = value["token"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing session"))?
                        .to_owned();
                    let principal: Principal = serde_json::from_value(value["principal"].clone())?;
                    return Ok((token, principal));
                }
                anyhow::bail!("Sign-in expired. Try again.")
            }
            .await;
            let issued = result.as_ref().ok().map(|(token, _)| token.clone());
            let installed = this
                .update(cx, |s, cx| {
                    let room = s.auth_attempt.room();
                    if !s.auth_attempt.finish(generation) {
                        return false;
                    }
                    match result {
                        Ok((token, p)) => {
                            if let Err(e) = save(&s.args.server, &token) {
                                s.auth_message = format!("Cannot save session in Keychain: {e}");
                                cx.notify();
                                return false;
                            }
                            s.store.bind_rooms(Some(&p.id));
                            s.store.save();
                            s.args.session = Some(token);
                            s.args.name = p.name.clone();
                            s.principal = Some(p);
                            s.auth_message.clear();
                            s.refresh_account(cx);
                            if let Some(room) = room {
                                s.join_link(room, cx);
                            }
                        }
                        Err(e) => {
                            s.auth_message = e.to_string();
                            cx.notify();
                            return false;
                        }
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !installed && let Some(token) = issued {
                let _ = request(
                    &args.server,
                    Some(&token),
                    "/auth/logout",
                    Some(json!({})),
                    "POST",
                )
                .await;
            }
        })
        .detach();
    }
    pub fn refresh_account(&mut self, cx: &mut Context<Self>) {
        let args = self.args.clone();
        if args.session.is_none() {
            self.principal = None;
            self.store.bind_rooms(None);
            self.store.save();
            cx.notify();
            return;
        }
        let expected = args.session.clone();
        cx.spawn(async move |this, cx| {
            let result = load_account(&args.server, args.session.as_deref()).await;
            let _ = this.update(cx, |s, cx| {
                if s.args.session != expected {
                    return;
                }
                match result {
                    Ok((p, rooms)) => {
                        s.store.bind_rooms(Some(&p.id));
                        s.args.name = p.name.clone();
                        s.principal = Some(p);
                        s.store.refresh_rooms(rooms);
                        s.store.save();
                        s.auth_message.clear();
                    }
                    Err(e) if unauthorized(&e) => {
                        s.expire_session(expected.as_deref(), cx);
                    }
                    Err(e) => {
                        s.auth_message = e.to_string();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if !self.auth_attempt.pending() {
            return;
        }
        self.auth_attempt.cancel();
        self.auth_message = "Sign-in cancelled".into();
        cx.notify();
    }
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        let args = self.args.clone();
        self.auth_attempt.cancel();
        self.room_creation.cancel();
        forget_session(
            &mut self.args.session,
            &mut self.principal,
            &mut self.store,
            || clear(&args.server),
        );
        self.store.save();
        self.auth_message = "Signed out".into();
        cx.notify();
        cx.spawn(async move |_, _| {
            let _ = request(
                &args.server,
                args.session.as_deref(),
                "/auth/logout",
                Some(json!({})),
                "POST",
            )
            .await;
        })
        .detach();
    }
    pub fn create_room(&mut self, kind: &str, join: bool, cx: &mut Context<Self>) {
        if self.auth_busy() {
            return;
        }
        if self.args.session.is_none() {
            self.sign_in(None, cx);
            return;
        }
        let operation = self.room_creation.start(());
        let args = self.args.clone();
        let expected = args.session.clone();
        let kind = kind.to_owned();
        let name = if join {
            "Quick call".to_owned()
        } else {
            self.store.next_room_name()
        };
        cx.spawn(async move |this, cx| {
            let result = async {
                let r = request(
                    &args.server,
                    args.session.as_deref(),
                    "/rooms",
                    Some(json!({"name":name,"kind":kind})),
                    "POST",
                )
                .await?;
                serde_json::from_value::<SavedRoom>(r).map_err(anyhow::Error::from)
            }
            .await;
            let _ = this.update(cx, |s, cx| {
                if !s.room_creation.finish(operation) || s.args.session != expected {
                    return;
                }
                match result {
                    Ok(r) => {
                        s.store.bind_rooms(Some(&r.owner));
                        s.store.cache_room(r.clone());
                        s.store.save();
                        if join {
                            s.start_call(
                                RoomRef {
                                    id: r.id,
                                    name: r.name,
                                    one_time: true,
                                },
                                cx,
                            );
                        }
                    }
                    Err(e) if unauthorized(&e) => {
                        if s.expire_session(expected.as_deref(), cx) {
                            s.sign_in(None, cx);
                        }
                    }
                    Err(e) => s.auth_message = e.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub fn rename_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let args = self.args.clone();
        let name = name.to_owned();
        let expected = args.session.clone();
        cx.spawn(async move |this, cx| {
            let result = request(
                &args.server,
                args.session.as_deref(),
                "/auth/me",
                Some(json!({"name":name})),
                "PATCH",
            )
            .await;
            let _ = this.update(cx, |s, cx| {
                if s.args.session != expected {
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Ok(p) = serde_json::from_value::<Principal>(value) {
                            s.args.name = p.name.clone();
                            s.store.name = Some(p.name.clone());
                            s.principal = Some(p);
                            s.store.save();
                        }
                    }
                    Err(e) => {
                        if !s.recover_auth(&e, expected.as_deref(), cx) {
                            s.auth_message = e.to_string();
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub fn rename_remote_room(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        let args = self.args.clone();
        let expected = args.session.clone();
        let id = id.to_owned();
        let name = name.to_owned();
        cx.spawn(async move |this, cx| {
            let result = request(
                &args.server,
                args.session.as_deref(),
                &format!("/rooms/{id}"),
                Some(json!({"name":name})),
                "PATCH",
            )
            .await;
            let _ = this.update(cx, |s, cx| {
                if s.args.session != expected {
                    return;
                }
                match result {
                    Ok(_) => {
                        s.store.rename_room(&id, &name);
                        s.store.save();
                    }
                    Err(e) => {
                        if !s.recover_auth(&e, expected.as_deref(), cx) {
                            s.auth_message = e.to_string();
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    #[test]
    fn unauthenticated_401_recovers_but_stale_failures_cannot_clear_a_new_session() {
        assert!(session_matches(None, None));
        assert!(session_matches(Some("old"), Some("old")));
        assert!(!session_matches(Some("new"), None));
        assert!(!session_matches(Some("new"), Some("old")));
        let mut store = crate::store::Store::default();
        store.bind_rooms(Some("old-account"));
        store.cache_room(SavedRoom {
            id: "private".into(),
            ..Default::default()
        });
        store.bind_rooms(None);
        assert!(store.rooms.is_empty());
        assert!(store.rooms_principal.is_none());
    }

    #[test]
    fn room_handoff_restarts_pending_approval_and_rejects_old_results() {
        let mut attempt = SignInAttempt::default();
        let home = attempt.request(None).unwrap();
        assert!(attempt.request(None).is_none());
        let linked = attempt.request(Some("room-a".into())).unwrap();
        assert!(!attempt.finish(home));
        assert_eq!(attempt.room().as_deref(), Some("room-a"));
        let newer = attempt.request(Some("room-b".into())).unwrap();
        assert!(!attempt.finish(linked));
        assert_eq!(attempt.room().as_deref(), Some("room-b"));
        assert!(attempt.current(newer));
        attempt.cancel();
        assert!(attempt.room().is_none());
    }
    #[test]
    fn profile_and_rooms_refresh_are_in_flight_together() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let responder = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut sockets = vec![];
            while sockets.len() < 2 {
                match listener.accept() {
                    Ok((stream, _)) => sockets.push(stream),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "second request waited for first response"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            for mut stream in sockets {
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut headers = vec![];
                let mut byte = [0];
                while !headers.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                let headers = String::from_utf8(headers).unwrap();
                let body = if headers.starts_with("GET /api/auth/me ") {
                    r#"{"id":"me","name":"Me","kind":"user"}"#
                } else {
                    assert!(headers.starts_with("GET /api/rooms "));
                    "[]"
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let (p, rooms) =
            crate::test_support::block_on(load_account(&server, Some("session"))).unwrap();
        assert_eq!(p.id, "me");
        assert!(rooms.is_empty());
        responder.join().unwrap();
    }

    #[test]
    fn expired_create_releases_busy_without_unlocking_a_newer_operation() {
        let mut creation = PendingOperation::default();
        let old = creation.start(());
        // Session invalidation releases the in-flight create immediately.
        creation.cancel();
        assert!(!creation.pending());
        let mut signin = SignInAttempt::default();
        let browser = signin.start(SignIn::default());
        assert!(!creation.finish(old));
        assert!(signin.current(browser));
        signin.finish(browser);
        let retry = creation.start(());
        assert!(!creation.finish(old));
        assert!(creation.pending());
        assert!(creation.finish(retry));
        assert!(!creation.pending());
    }

    #[test]
    fn cancelled_signin_cannot_open_browser_or_install_a_late_session() {
        let mut attempt = SignInAttempt::default();
        let old = attempt.start(SignIn::default());
        attempt.payload.as_mut().unwrap().code = Some("1234ABCD".into());
        attempt.cancel();
        assert!(!attempt.pending());
        assert!(attempt.code().is_none());
        assert!(!attempt.current(old)); // delayed /start cannot open a browser
        let retry = attempt.start(SignIn::default());
        assert!(!attempt.finish(old)); // delayed /poll cannot replace the retry
        assert!(attempt.current(retry));
        assert!(attempt.finish(retry));
        assert!(!attempt.pending());
    }

    #[test]
    fn expired_session_clears_credentials_identity_and_cached_rooms() {
        let error: anyhow::Error = RequestError {
            status: 401,
            message: "Expired".into(),
        }
        .into();
        assert!(unauthorized(&error));
        let network: anyhow::Error = RequestError {
            status: 503,
            message: "Offline".into(),
        }
        .into();
        assert!(!unauthorized(&network));
        let guest_role: anyhow::Error = RequestError {
            status: 403,
            message: "Account required".into(),
        }
        .into();
        assert!(
            !unauthorized(&guest_role),
            "a valid guest credential must survive a role denial"
        );
        let mut session = Some("old-token".into());
        let mut principal = Some(Principal {
            id: "a".into(),
            name: "A".into(),
            kind: "user".into(),
        });
        let mut store = crate::store::Store::default();
        store.bind_rooms(Some("a"));
        store.cache_room(SavedRoom {
            id: "private".into(),
            ..Default::default()
        });
        let mut cleared_keychain = false;
        forget_session(&mut session, &mut principal, &mut store, || {
            cleared_keychain = true
        });
        assert!(cleared_keychain);
        assert!(session.is_none()); // start_call now takes the sign_in path
        assert!(principal.is_none());
        assert!(store.rooms.is_empty());
        assert!(store.rooms_principal.is_none());
    }

    #[test]
    fn throttled_non_json_response_keeps_status_for_poll_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 4\r\nConnection: close\r\n\r\nBusy").unwrap();
        });
        let error = crate::test_support::block_on(request(
            &server,
            None,
            "/auth/desktop/poll",
            None,
            "GET",
        ))
        .unwrap_err();
        assert_eq!(error.downcast_ref::<RequestError>().unwrap().status, 429);
        task.join().unwrap();
    }
}
