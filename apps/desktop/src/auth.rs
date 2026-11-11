//! Browser approval keeps email/OAuth out of the native UI. Only the app that
//! holds the polling secret can exchange an approved challenge for a session.
use crate::{Shell, call::RoomRef, store::SavedRoom};
use gpui::Context;
use serde::Deserialize;
use serde_json::{Value, json};

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
#[derive(Debug)]
struct RequestError {
    status: reqwest::StatusCode,
    message: String,
}
impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RequestError {}

/// A cancelled attempt can never open a browser or install a late session.
#[derive(Default)]
pub struct SignInAttempt {
    generation: u64,
    pub pending: bool,
    pub code: Option<String>,
}
impl SignInAttempt {
    fn start(&mut self) -> u64 {
        self.cancel();
        self.pending = true;
        self.generation
    }
    fn current(&self, generation: u64) -> bool {
        self.pending && self.generation == generation
    }
    fn cancel(&mut self) {
        self.generation += 1;
        self.pending = false;
        self.code = None;
    }
    fn finish(&mut self, generation: u64) -> bool {
        if !self.current(generation) {
            return false;
        }
        self.cancel();
        true
    }
}
fn client() -> anyhow::Result<&'static reqwest::blocking::Client> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::blocking::Client, reqwest::Error>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}
fn unauthorized(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RequestError>()
        .is_some_and(|e| e.status == reqwest::StatusCode::UNAUTHORIZED)
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

pub(crate) fn request(
    server: &str,
    token: Option<&str>,
    path: &str,
    body: Option<Value>,
    method: &str,
) -> anyhow::Result<Value> {
    let client = client()?;
    let mut req = client
        .request(
            method.parse()?,
            format!("{}/api{path}", server.trim_end_matches('/')),
        )
        .header("X-Quipcall", "1");
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let res = req.send()?;
    let status = res.status();
    if !status.is_success() {
        let value: Value = res.json().unwrap_or_default();
        return Err(RequestError {
            status,
            message: value["error"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Request failed ({status})")),
        }
        .into());
    }
    Ok(res.json()?)
}
impl Shell {
    pub fn sign_in(&mut self, room: Option<String>, cx: &mut Context<Self>) {
        if self.auth_busy {
            return;
        }
        self.auth_busy = true;
        let generation = self.auth_attempt.start();
        self.auth_message = "Opening browser sign-in…".into();
        cx.notify();
        let args = self.args.clone();
        cx.spawn(async move |this, cx| {
            let server = args.server.clone();
            let start = cx
                .background_executor()
                .spawn(async move {
                    request(
                        &server,
                        None,
                        "/auth/desktop/start",
                        Some(json!({})),
                        "POST",
                    )
                })
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
                    s.auth_attempt.code = Some(code);
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
                    let server = args.server.clone();
                    let id = id.clone();
                    let secret = secret.clone();
                    let value = cx
                        .background_executor()
                        .spawn(async move {
                            request(
                                &server,
                                None,
                                "/auth/desktop/poll",
                                Some(json!({"id":id,"secret":secret})),
                                "POST",
                            )
                        })
                        .await;
                    let value = match value {
                        Err(e)
                            if e.downcast_ref::<RequestError>().is_some_and(|e| {
                                e.status == reqwest::StatusCode::TOO_MANY_REQUESTS
                            }) =>
                        {
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
            let _ = this.update(cx, |s, cx| {
                if !s.auth_attempt.finish(generation) {
                    return;
                }
                s.auth_busy = false;
                match result {
                    Ok((token, p)) => {
                        if let Err(e) = save(&s.args.server, &token) {
                            s.auth_message = format!("Cannot save session in Keychain: {e}");
                            cx.notify();
                            return;
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
                    Err(e) => s.auth_message = e.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub fn refresh_account(&mut self, cx: &mut Context<Self>) {
        let args = self.args.clone();
        if args.session.is_none() {
            return;
        }
        let expected = args.session.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let p: Principal = serde_json::from_value(request(
                        &args.server,
                        args.session.as_deref(),
                        "/auth/me",
                        None,
                        "GET",
                    )?)?;
                    let rooms: Vec<SavedRoom> = serde_json::from_value(request(
                        &args.server,
                        args.session.as_deref(),
                        "/rooms",
                        None,
                        "GET",
                    )?)?;
                    Ok::<_, anyhow::Error>((p, rooms))
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                if s.args.session != expected {
                    return;
                }
                match result {
                    Ok((p, rooms)) => {
                        s.store.bind_rooms(Some(&p.id));
                        s.args.name = p.name.clone();
                        s.principal = Some(p);
                        s.store.rooms = rooms;
                        s.store.save();
                        s.auth_message.clear();
                    }
                    Err(e) if unauthorized(&e) => {
                        forget_session(&mut s.args.session, &mut s.principal, &mut s.store, || {
                            clear(&s.args.server)
                        });
                        s.store.save();
                        s.auth_message = "Your session expired. Sign in to continue.".into();
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
        if !self.auth_attempt.pending {
            return;
        }
        self.auth_attempt.cancel();
        self.auth_busy = false;
        self.auth_message = "Sign-in cancelled".into();
        cx.notify();
    }
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        let args = self.args.clone();
        self.auth_attempt.cancel();
        self.auth_busy = false;
        forget_session(
            &mut self.args.session,
            &mut self.principal,
            &mut self.store,
            || clear(&args.server),
        );
        self.store.save();
        self.auth_message = "Signed out".into();
        cx.notify();
        cx.background_executor()
            .spawn(async move {
                let _ = request(
                    &args.server,
                    args.session.as_deref(),
                    "/auth/logout",
                    Some(json!({})),
                    "POST",
                );
            })
            .detach();
    }
    pub fn create_room(&mut self, kind: &str, join: bool, cx: &mut Context<Self>) {
        if self.auth_busy {
            return;
        }
        if self.principal.as_ref().is_none_or(|p| p.kind != "user") {
            self.sign_in(None, cx);
            return;
        }
        self.auth_busy = true;
        let args = self.args.clone();
        let expected = args.session.clone();
        let kind = kind.to_owned();
        let name = if join {
            "Quick call".to_owned()
        } else {
            self.store.next_room_name()
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let r = request(
                        &args.server,
                        args.session.as_deref(),
                        "/rooms",
                        Some(json!({"name":name,"kind":kind})),
                        "POST",
                    )?;
                    serde_json::from_value::<SavedRoom>(r).map_err(anyhow::Error::from)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                if s.args.session != expected {
                    return;
                }
                s.auth_busy = false;
                match result {
                    Ok(r) => {
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
            let result = cx
                .background_executor()
                .spawn(async move {
                    request(
                        &args.server,
                        args.session.as_deref(),
                        "/auth/me",
                        Some(json!({"name":name})),
                        "PATCH",
                    )
                })
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
                    Err(e) => s.auth_message = e.to_string(),
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
            let room = id.clone();
            let title = name.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    request(
                        &args.server,
                        args.session.as_deref(),
                        &format!("/rooms/{room}"),
                        Some(json!({"name":title})),
                        "PATCH",
                    )
                })
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
                    Err(e) => s.auth_message = e.to_string(),
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
    fn cancelled_signin_cannot_open_browser_or_install_a_late_session() {
        let mut attempt = SignInAttempt::default();
        let old = attempt.start();
        attempt.code = Some("1234ABCD".into());
        attempt.cancel();
        assert!(!attempt.pending);
        assert!(attempt.code.is_none());
        assert!(!attempt.current(old)); // delayed /start cannot open a browser
        let retry = attempt.start();
        assert!(!attempt.finish(old)); // delayed /poll cannot replace the retry
        assert!(attempt.current(retry));
        assert!(attempt.finish(retry));
        assert!(!attempt.pending);
    }

    #[test]
    fn expired_session_clears_credentials_identity_and_cached_rooms() {
        let error: anyhow::Error = RequestError {
            status: reqwest::StatusCode::UNAUTHORIZED,
            message: "Expired".into(),
        }
        .into();
        assert!(unauthorized(&error));
        let network: anyhow::Error = RequestError {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            message: "Offline".into(),
        }
        .into();
        assert!(!unauthorized(&network));
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
    fn requests_share_one_http_client_and_runtime() {
        assert!(std::ptr::eq(client().unwrap(), client().unwrap()));
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
        let error = request(&server, None, "/auth/desktop/poll", None, "GET").unwrap_err();
        assert_eq!(
            error.downcast_ref::<RequestError>().unwrap().status,
            reqwest::StatusCode::TOO_MANY_REQUESTS
        );
        task.join().unwrap();
    }
}
