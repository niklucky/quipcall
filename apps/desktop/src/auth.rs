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

pub(crate) fn request(
    server: &str,
    token: Option<&str>,
    path: &str,
    body: Option<Value>,
    method: &str,
) -> anyhow::Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
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
        self.auth_generation += 1;
        let generation = self.auth_generation;
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
                let mut url = format!("{}/?desktop={id}", args.web.trim_end_matches('/'));
                if let Some(room) = &room {
                    url.push_str(&format!("&room={room}"));
                }
                this.update(cx, |s, cx| {
                    s.auth_message = "Approve sign-in in your browser".into();
                    cx.open_url(&url);
                    cx.notify();
                })?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
                let mut delay = std::time::Duration::from_secs(2);
                while std::time::Instant::now() < deadline {
                    if !this.update(cx, |s, _| s.auth_generation == generation)? {
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
                    if !this.update(cx, |s, _| s.auth_generation == generation)? {
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
                if s.auth_generation != generation {
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
                    Err(e) => {
                        s.auth_message = format!("{e}. Sign in again if your session expired.");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        let args = self.args.clone();
        self.auth_generation += 1;
        self.auth_busy = false;
        clear(&args.server);
        self.args.session = None;
        self.principal = None;
        self.store.rooms.clear();
        self.store.bind_rooms(None);
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
