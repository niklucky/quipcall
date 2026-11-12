use super::*;
use axum::{
    body::Body,
    http::{HeaderMap, Request},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Test {
    state: AppState,
    app: Router,
    dir: tempfile::TempDir,
    participants: Arc<std::sync::Mutex<Vec<String>>>,
    disconnected: Arc<std::sync::Mutex<Vec<String>>>,
    fail_disconnect: Arc<std::sync::atomic::AtomicBool>,
    fail_rooms: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    deleted_rooms: Arc<std::sync::Mutex<Vec<String>>>,
}
impl Test {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Fake LiveKit room service: exercise the actual disconnect HTTP path.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let participants = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let disconnected = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let fail_disconnect = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_rooms = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let deleted_rooms = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (failed_rooms, deleted) = (fail_rooms.clone(), deleted_rooms.clone());
        let (active, removed, fail) = (
            participants.clone(),
            disconnected.clone(),
            fail_disconnect.clone(),
        );
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(move |uri: axum::http::Uri, Json(body): Json<Value>| {
                    let (active, removed, fail) = (active.clone(), removed.clone(), fail.clone());
                    let (failed_rooms, deleted) = (failed_rooms.clone(), deleted.clone());
                    async move {
                        if uri.path().ends_with("DeleteRoom")
                            && failed_rooms
                                .lock()
                                .unwrap()
                                .contains(body["room"].as_str().unwrap_or_default())
                        {
                            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
                        }
                        if uri.path().ends_with("DeleteRoom") {
                            deleted
                                .lock()
                                .unwrap()
                                .push(body["room"].as_str().unwrap().to_owned());
                        }
                        if uri.path().ends_with("ListParticipants") {
                            let participants: Vec<_> = active
                                .lock()
                                .unwrap()
                                .iter()
                                .map(|id| json!({"identity":id}))
                                .collect();
                            let response = if participants.is_empty() {
                                json!({})
                            } else {
                                json!({"participants":participants})
                            };
                            return (StatusCode::OK, Json(response));
                        }
                        if uri.path().ends_with("RemoveParticipant") {
                            if fail.load(std::sync::atomic::Ordering::Relaxed) {
                                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
                            }
                            let id = body["identity"].as_str().unwrap().to_owned();
                            removed.lock().unwrap().push(id.clone());
                            active.lock().unwrap().retain(|p| p != &id);
                        }
                        (StatusCode::OK, Json(json!({})))
                    }
                }),
            )
            .await
            .unwrap();
        });
        let config = Config {
            bind: "127.0.0.1:0".into(),
            public_url: "http://127.0.0.1:5173".into(),
            database_url: format!("sqlite://{}", dir.path().join("test.db").display()),
            livekit_api_key: "test".into(),
            livekit_api_secret: "test-secret-that-is-at-least-32-characters".into(),
            livekit_public_url: "ws://localhost:7980".into(),
            livekit_api_url: api,
            resend_api_key: None,
            mail_from: "test@example.com".into(),
            bootstrap_email: Some("admin@example.com".into()),
            mail_dir: Some(dir.path().join("mail")),
        };
        let state = AppState::new(config).await.unwrap();
        let app = router(state.clone());
        Self {
            state,
            app,
            dir,
            participants,
            disconnected,
            fail_disconnect,
            fail_rooms,
            deleted_rooms,
        }
    }
    async fn req(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value, Option<String>) {
        self.req_from("local", method, path, token, body).await
    }
    async fn req_from(
        &self,
        ip: &str,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value, Option<String>) {
        let mut req = Request::builder()
            .header("x-real-ip", ip)
            .method(method)
            .uri(path)
            .header("x-quipcall", "1")
            .header("content-type", "application/json");
        if !token.is_empty() {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let res = self
            .app
            .clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let cookie = res.headers().get("set-cookie").map(|v| {
            v.to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .trim_start_matches("qc_session=")
                .to_owned()
        });
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"body":String::from_utf8_lossy(&bytes)}));
        (status, value, cookie)
    }
    async fn magic(&self, email: &str, invite: Option<&str>) -> String {
        let (status, v, _) = self
            .req(
                "POST",
                "/api/auth/request",
                "",
                json!({"email":email,"invite":invite}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        auth::deliver_pending(&self.state).await.unwrap();
        let email = std::fs::read_to_string(
            self.dir
                .path()
                .join("mail")
                .join(format!("{}.txt", auth::hash(email))),
        )
        .unwrap();
        email.split("#magic=").nth(1).unwrap().trim().into()
    }
    async fn admin(&self) -> String {
        let magic = self.magic("admin@example.com", None).await;
        let (status, v, token) = self
            .req("POST", "/api/auth/verify", "", json!({"token":magic}))
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        token.unwrap()
    }
    async fn guest(&self, name: &str) -> (String, String) {
        let (status, v, token) = self
            .req("POST", "/api/auth/guest", "", json!({"name":name}))
            .await;
        assert_eq!(status, StatusCode::OK);
        (token.unwrap(), v["id"].as_str().unwrap().into())
    }
    async fn room(&self, token: &str, kind: &str) -> String {
        let (status, v, _) = self
            .req(
                "POST",
                "/api/rooms",
                token,
                json!({"name":"Project","kind":kind}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["id"].as_str().unwrap().into()
    }
    async fn join(&self, token: &str, room: &str) -> (StatusCode, Value, Option<String>) {
        self.req(
            "POST",
            "/api/join",
            token,
            json!({"room":room,"identity":"forged","name":"forged"}),
        )
        .await
    }
    async fn gate(&self, token: &str) -> StatusCode {
        let mut h = HeaderMap::new();
        h.insert(
            "x-original-uri",
            format!("/rtc?access_token={token}").parse().unwrap(),
        );
        match rooms::media_check(State(self.state.clone()), h).await {
            Ok(s) => s,
            Err(e) => e.status,
        }
    }
}
#[tokio::test]
async fn invite_only_magic_links_sessions_and_admin() {
    let t = Test::new().await;
    let (s, _, _) = t
        .req(
            "POST",
            "/api/auth/request",
            "",
            json!({"email":"stranger@example.com"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM magic_links")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        0
    );
    let admin = t.admin().await;
    let (s, v, _) = t
        .req(
            "POST",
            "/api/auth/invites",
            &admin,
            json!({"email":"member@example.com"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let invite = v["url"].as_str().unwrap().split('=').nth(1).unwrap();
    let magic = t.magic("member@example.com", Some(invite)).await;
    let (s, v, session) = t
        .req("POST", "/api/auth/verify", "", json!({"token":magic}))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["admin"], false);
    assert_eq!(
        t.req("POST", "/api/auth/verify", "", json!({"token":magic}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let session = session.unwrap();
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/invites",
            &session,
            json!({"email":"other@example.com"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        t.req("POST", "/api/auth/logout", &session, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        t.req("GET", "/api/auth/me", &session, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Existing account doesn't need its invitation again.
    let next = t.magic("member@example.com", None).await;
    sqlx::query("UPDATE magic_links SET expires=0")
        .execute(&t.state.db)
        .await
        .unwrap();
    assert_eq!(
        t.req("POST", "/api/auth/verify", "", json!({"token":next}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn private_guest_claim_return_remove_and_cached_media_token() {
    let t = Test::new().await;
    let admin = t.admin().await;
    let room = t.room(&admin, "private").await;
    let (g, gid) = t.guest("Guest").await;
    let (other, _) = t.guest("Other").await;
    assert_eq!(t.join(&g, &room).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        t.req(
            "POST",
            &format!("/api/rooms/{room}/invites"),
            &g,
            json!({"kind":"guest"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, v, _) = t
        .req(
            "POST",
            &format!("/api/rooms/{room}/invites"),
            &admin,
            json!({"kind":"guest"}),
        )
        .await;
    let invite = v["url"].as_str().unwrap().split('=').nth(1).unwrap();
    let (a, b) = tokio::join!(
        t.req("POST", "/api/invites/redeem", &g, json!({"token":invite})),
        t.req(
            "POST",
            "/api/invites/redeem",
            &other,
            json!({"token":invite})
        )
    );
    assert_eq!(
        usize::from(a.0 == StatusCode::OK) + usize::from(b.0 == StatusCode::OK),
        1
    );
    let (winner, id) = if a.0 == StatusCode::OK {
        (g, gid)
    } else {
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {other}").parse().unwrap());
        let p = auth::current(&t.state, &h).await.unwrap();
        (other, p.id)
    };
    let (s, v, _) = t.join(&winner, &room).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        v["identity"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{id}:"))
    );
    let media = v["token"].as_str().unwrap();
    assert_eq!(t.gate(media).await, StatusCode::NO_CONTENT);
    assert_eq!(t.join(&winner, &room).await.0, StatusCode::OK); // returns without redeeming again
    let (s, v, _) = t
        .req(
            "DELETE",
            &format!("/api/rooms/{room}/members/{id}"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(t.join(&winner, &room).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.gate(media).await, StatusCode::FORBIDDEN);
    // Database reopening preserves membership revocation and account sessions.
    let restarted = AppState::new((*t.state.config).clone()).await.unwrap();
    let mut restored_headers = HeaderMap::new();
    restored_headers.insert("authorization", format!("Bearer {winner}").parse().unwrap());
    assert!(auth::current(&restarted, &restored_headers).await.is_ok());
    assert_eq!(
        rooms::join(
            State(restarted),
            restored_headers,
            Json(rooms::Join { room: room.clone() })
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::FORBIDDEN
    );

    let owner_media = t.join(&admin, &room).await.1["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut h = HeaderMap::new();
    h.insert(
        "x-original-uri",
        format!("/rtc?access_token={owner_media}").parse().unwrap(),
    );
    h.insert("authorization", format!("Bearer {media}").parse().unwrap());
    assert_eq!(
        rooms::media_check(State(t.state.clone()), h)
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    // cached or refreshed token cannot re-enter
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &winner,
            json!({"token":invite})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn public_and_direct_rooms_use_server_identity_and_two_slots() {
    let t = Test::new().await;
    let admin = t.admin().await;
    let (g, gid) = t.guest("Guest").await;
    let (other, _) = t.guest("Other").await;
    assert_eq!(
        t.req(
            "POST",
            "/api/rooms",
            &g,
            json!({"name":"bad","kind":"public"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let public = t.room(&admin, "public").await;
    assert_eq!(t.join(&g, &public).await.0, StatusCode::OK);
    assert_eq!(t.join(&other, &public).await.0, StatusCode::OK);
    let direct = t.room(&admin, "direct").await;
    assert_eq!(t.join(&admin, &direct).await.0, StatusCode::OK);
    let (a, b) = tokio::join!(t.join(&g, &direct), t.join(&other, &direct));
    assert_eq!(
        usize::from(a.0 == StatusCode::OK) + usize::from(b.0 == StatusCode::OK),
        1
    );
    if a.0 == StatusCode::OK {
        assert!(
            a.1["identity"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{gid}:"))
        );
        assert_eq!(t.join(&g, &direct).await.0, StatusCode::OK);
    }
    assert_eq!(
        t.req(
            "GET",
            "/api/join?room=anything&identity=spoof",
            "",
            json!({})
        )
        .await
        .0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(t.join("", &public).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(t.join(&g, "uncreated").await.0, StatusCode::NOT_FOUND);
}
#[tokio::test]
async fn membership_email_invites_and_desktop_proof() {
    let t = Test::new().await;
    let admin = t.admin().await;
    let room = t.room(&admin, "private").await;
    let (g, _) = t.guest("Guest").await;
    let (_, v, _) = t
        .req(
            "POST",
            &format!("/api/rooms/{room}/invites"),
            &admin,
            json!({"kind":"member","email":"admin@example.com"}),
        )
        .await;
    let invite = v["url"].as_str().unwrap().split('=').nth(1).unwrap();
    assert_eq!(
        t.req("POST", "/api/invites/redeem", &g, json!({"token":invite}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &admin,
            json!({"token":invite})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, start, _) = t
        .req("POST", "/api/auth/desktop/start", "", json!({}))
        .await;
    assert_eq!(
        t.req("POST", "/api/auth/desktop/poll", "", start.clone())
            .await
            .1["pending"],
        true
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/desktop/approve",
            &g,
            json!({"id":start["id"],"code":start["code"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/desktop/poll",
            "",
            json!({"id":start["id"],"secret":"wrong"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (s, v, _) = t
        .req("POST", "/api/auth/desktop/poll", "", start.clone())
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["token"].is_string());
    assert_eq!(
        t.req("POST", "/api/auth/desktop/poll", "", start).await.0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn csrf_and_email_rate_limits() {
    let t = Test::new().await;
    for headers in [
        vec![],
        vec![("x-quipcall", "1"), ("origin", "https://evil.example")],
    ] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/auth/guest")
            .header("content-type", "application/json");
        for (key, value) in headers {
            req = req.header(key, value);
        }
        let res = t
            .app
            .clone()
            .oneshot(req.body(Body::from(r#"{"name":"Guest"}"#)).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }
    for _ in 0..3 {
        assert_eq!(
            t.req(
                "POST",
                "/api/auth/request",
                "",
                json!({"email":"unknown@example.com"})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/request",
            "",
            json!({"email":"unknown@example.com"})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn entry_preview_never_claims_access_and_hides_private_metadata() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let private = t.room(&owner, "private").await;
    let (_, link, _) = t
        .req(
            "POST",
            &format!("/api/rooms/{private}/invites"),
            &owner,
            json!({"kind":"guest"}),
        )
        .await;
    let invite = link["url"]
        .as_str()
        .unwrap()
        .split("roomInvite=")
        .nth(1)
        .unwrap();
    let (_, entry, _) = t
        .req("POST", "/api/entry", "", json!({"room":private}))
        .await;
    assert_eq!(entry, json!({"state":"private"}));
    for _ in 0..2 {
        let (_, entry, _) = t
            .req("POST", "/api/entry", "", json!({"invite":invite}))
            .await;
        assert_eq!(entry["state"], "invite");
        assert_eq!(entry["room"]["name"], "Project");
        assert!(entry["room"].get("owner").is_none());
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM principals")
        .fetch_one(&t.state.db)
        .await
        .unwrap();
    assert_eq!(count, 1, "preview must not create guest identities");
    let (guest, id) = t.guest("Guest").await;
    assert_eq!(t.join(&guest, &private).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &guest,
            json!({"token":invite})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, entry, _) = t
        .req("POST", "/api/entry", &guest, json!({"invite":invite}))
        .await;
    assert_eq!(entry["state"], "ready");
    let (_, entry, _) = t
        .req("POST", "/api/entry", "", json!({"invite":invite}))
        .await;
    assert_eq!(entry["state"], "used");
    assert!(entry.get("room").is_none());
    assert_eq!(
        t.req(
            "DELETE",
            &format!("/api/rooms/{private}/members/{id}"),
            &owner,
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    for body in [json!({"room":private}), json!({"invite":invite})] {
        let (_, entry, _) = t.req("POST", "/api/entry", &guest, body).await;
        assert_eq!(entry["state"], "removed");
        assert!(entry.get("room").is_none());
    }
    let (_, link, _) = t
        .req(
            "POST",
            &format!("/api/rooms/{private}/invites"),
            &owner,
            json!({"kind":"guest"}),
        )
        .await;
    let fresh = link["url"]
        .as_str()
        .unwrap()
        .split("roomInvite=")
        .nth(1)
        .unwrap();
    let (_, entry, _) = t
        .req("POST", "/api/entry", &guest, json!({"invite":fresh}))
        .await;
    assert_eq!(
        entry["state"], "invite",
        "fresh invitations allow deliberate re-inviting"
    );
    sqlx::query("UPDATE room_invites SET expires=0 WHERE hash=?")
        .bind(auth::hash(fresh))
        .execute(&t.state.db)
        .await
        .unwrap();
    assert_eq!(
        t.req("POST", "/api/entry", &guest, json!({"invite":fresh}))
            .await
            .1["state"],
        "expired"
    );
}

#[tokio::test]
async fn entry_preview_reports_member_signin_direct_capacity_and_closed_rooms() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let direct = t.room(&owner, "direct").await;
    for _ in 0..2 {
        assert_eq!(
            t.req("POST", "/api/entry", "", json!({"room":direct}))
                .await
                .1["state"],
            "ready"
        );
    }
    let (guest, _) = t.guest("Guest").await;
    assert_eq!(t.join(&guest, &direct).await.0, StatusCode::OK);
    assert_eq!(
        t.req("POST", "/api/entry", "", json!({"room":direct}))
            .await
            .1,
        json!({"state":"full"})
    );
    assert_eq!(
        t.req("POST", "/api/entry", &guest, json!({"room":direct}))
            .await
            .1["state"],
        "ready"
    );
    let public = t.room(&owner, "public").await;
    assert_eq!(
        t.req("POST", "/api/entry", "", json!({"room":public}))
            .await
            .1["state"],
        "ready"
    );
    t.req("DELETE", &format!("/api/rooms/{public}"), &owner, json!({}))
        .await;
    assert_eq!(
        t.req("POST", "/api/entry", "", json!({"room":public}))
            .await
            .1,
        json!({"state":"unavailable"})
    );
    let private = t.room(&owner, "private").await;
    let (_, link, _) = t
        .req(
            "POST",
            &format!("/api/rooms/{private}/invites"),
            &owner,
            json!({"kind":"member","email":"admin@example.com"}),
        )
        .await;
    let token = link["url"]
        .as_str()
        .unwrap()
        .split("roomInvite=")
        .nth(1)
        .unwrap();
    let (_, entry, _) = t
        .req("POST", "/api/entry", &guest, json!({"invite":token}))
        .await;
    assert_eq!(entry["state"], "signin");
    assert!(entry.get("email").is_none());
    assert_eq!(
        t.req("POST", "/api/entry", &owner, json!({"invite":token}))
            .await
            .1["state"],
        "ready"
    );
}

#[tokio::test]
async fn desktop_polling_does_not_starve_shared_ip_signin_or_approval() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let mut challenges = Vec::new();
    for _ in 0..4 {
        let (status, challenge, _) = t
            .req("POST", "/api/auth/desktop/start", "", json!({}))
            .await;
        assert_eq!(status, StatusCode::OK);
        challenges.push(challenge);
    }
    // More than the shared auth allowance, but ordinary traffic per challenge.
    for _ in 0..35 {
        for challenge in &challenges {
            let (status, body, _) = t
                .req("POST", "/api/auth/desktop/poll", "", challenge.clone())
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["pending"], true);
        }
    }
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/request",
            "",
            json!({"email":"someone@example.com"})
        )
        .await
        .0,
        StatusCode::OK
    );
    // Throttle one valid proof, while the other Macs and browser remain usable.
    for _ in 0..25 {
        assert_eq!(
            t.req("POST", "/api/auth/desktop/poll", "", challenges[0].clone())
                .await
                .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        t.req("POST", "/api/auth/desktop/poll", "", challenges[0].clone())
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/desktop/approve",
            &owner,
            json!({"id":challenges[1]["id"],"code":challenges[1]["code"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, result, _) = t
        .req("POST", "/api/auth/desktop/poll", "", challenges[1].clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(result["token"].is_string());
    assert_eq!(
        t.req("POST", "/api/auth/logout", &owner, json!({})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn room_rename_is_owner_only_and_survives_reload() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let room = t.room(&owner, "public").await;
    let (guest, _) = t.guest("Guest").await;
    assert_eq!(
        t.req(
            "PATCH",
            &format!("/api/rooms/{room}"),
            &guest,
            json!({"name":"Not mine"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        t.req(
            "PATCH",
            &format!("/api/rooms/{room}"),
            &owner,
            json!({"name":"  Project lounge  "})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        t.req("GET", &format!("/api/rooms/{room}"), &owner, json!({}))
            .await
            .1["room"]["name"],
        "Project lounge"
    );
    assert_eq!(
        t.req("GET", "/api/rooms", &owner, json!({})).await.1[0]["name"],
        "Project lounge"
    );
    assert_eq!(
        t.req("POST", "/api/entry", "", json!({"room":room}))
            .await
            .1["room"]["name"],
        "Project lounge"
    );
}

#[tokio::test]
async fn desktop_approval_requires_the_initiating_device_code_and_limits_guesses() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let challenge = t
        .req("POST", "/api/auth/desktop/start", "", json!({}))
        .await
        .1;
    assert_eq!(challenge["code"].as_str().unwrap().len(), 8);
    for code in ["", "incorrect"] {
        assert_eq!(
            t.req(
                "POST",
                "/api/auth/desktop/approve",
                &owner,
                json!({"id":challenge["id"],"code":code})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        t.req("POST", "/api/auth/desktop/poll", "", challenge.clone())
            .await
            .1["pending"],
        true
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/desktop/approve",
            &owner,
            json!({"id":challenge["id"],"code":challenge["code"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(
        t.req("POST", "/api/auth/desktop/poll", "", challenge)
            .await
            .1["token"]
            .is_string()
    );

    let locked = t
        .req("POST", "/api/auth/desktop/start", "", json!({}))
        .await
        .1;
    for _ in 0..5 {
        assert_eq!(
            t.req(
                "POST",
                "/api/auth/desktop/approve",
                &owner,
                json!({"id":locked["id"],"code":"wrong"})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/desktop/approve",
            &owner,
            json!({"id":locked["id"],"code":locked["code"]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.req("POST", "/api/auth/desktop/poll", "", locked).await.1["pending"],
        true
    );
}

#[tokio::test]
async fn email_abuse_is_scoped_and_delivered_links_still_work() {
    let t = Test::new().await;
    // One IP cannot exhaust a global email budget or block other auth endpoints.
    for i in 0..120 {
        assert_eq!(
            t.req_from(
                "attacker",
                "POST",
                "/api/auth/request",
                "",
                json!({"email":format!("unknown{i}@example.com")})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        t.req_from(
            "attacker",
            "POST",
            "/api/auth/request",
            "",
            json!({"email":"more@example.com"})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    auth::deliver_pending(&t.state).await.unwrap();
    auth::deliver_pending(&t.state).await.unwrap();
    let token = t.magic("admin@example.com", None).await;
    for _ in 0..5 {
        assert_eq!(
            t.req_from(
                "mail-attacker",
                "POST",
                "/api/auth/request",
                "",
                json!({"email":"admin@example.com"})
            )
            .await
            .0,
            StatusCode::OK
        );
        auth::deliver_pending(&t.state).await.unwrap();
    }
    // The attacker's recipient allowance does not prevent this IP requesting
    // a fresh link, even before the older delivered link is consumed.
    let before_verify = t.magic("admin@example.com", None).await;
    assert_ne!(before_verify, token);
    assert_eq!(
        t.req("POST", "/api/auth/verify", "", json!({"token":token}))
            .await
            .0,
        StatusCode::OK
    );
    // Verification clears mail suppression; the next sign-in can get a link.
    let next = t.magic("admin@example.com", None).await;
    assert_ne!(next, token);
    t.guest("Unaffected").await;
    assert_eq!(
        t.req("POST", "/api/auth/desktop/start", "", json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let globals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rate_limits WHERE key IN ('email-global','verify','guest','desktop-start')")
        .fetch_one(&t.state.db).await.unwrap();
    assert_eq!(globals, 0);
}

#[tokio::test]
async fn mail_eligibility_and_delivery_failure_are_off_the_response_path() {
    let t = Test::new().await;
    // Make the local provider fail; neither known nor unknown requests touch it.
    std::fs::write(t.dir.path().join("mail"), "not a directory").unwrap();
    for email in ["admin@example.com", "unknown@example.com"] {
        let (status, value, _) = t
            .req("POST", "/api/auth/request", "", json!({"email":email}))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value, json!({"ok":true}));
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_requests")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM magic_links")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        0
    );
    // Failure schedules durable backoff; it neither drops nor immediately retries.
    let restarted = AppState::new((*t.state.config).clone()).await.unwrap();
    assert_eq!(auth::deliver_pending(&restarted).await.unwrap(), 2);
    let (attempts, available): (i64, i64) =
        sqlx::query_as("SELECT attempts,available FROM mail_requests")
            .fetch_one(&t.state.db)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    assert!(available >= auth::now() + 29);
    assert_eq!(auth::deliver_pending(&restarted).await.unwrap(), 0);
    // The failed send might have reached the provider before a timeout; do not
    // invalidate a late-delivered token or refund an ambiguous delivery attempt.
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM magic_links")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        1
    );
    std::fs::remove_file(t.dir.path().join("mail")).unwrap();
    // Restart with a retry due soon: it must wake at the deadline, without a
    // new enqueue/notification and without waiting for the five-minute fallback.
    sqlx::query("UPDATE mail_requests SET available=?")
        .bind(auth::now() + 1)
        .execute(&t.state.db)
        .await
        .unwrap();
    let restarted_again = AppState::new((*t.state.config).clone()).await.unwrap();
    let worker = tokio::spawn(auth::mail_worker(restarted_again));
    tokio::time::timeout(Duration::from_secs(5), async {
        while sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_requests")
            .fetch_one(&t.state.db)
            .await
            .unwrap()
            != 0
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    let _ = worker.await;
    let mail = std::fs::read_to_string(
        t.dir
            .path()
            .join("mail")
            .join(format!("{}.txt", auth::hash("admin@example.com"))),
    )
    .unwrap();
    let token = mail.split("#magic=").nth(1).unwrap().trim();
    assert_eq!(
        t.req("POST", "/api/auth/verify", "", json!({"token":token}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn mail_worker_drains_backlog_and_wakes_for_new_requests() {
    let t = Test::new().await;
    // More than one delivery batch is already durable when the process starts.
    for _ in 0..101 {
        sqlx::query("INSERT INTO mail_requests(email,requester,created) VALUES ('unknown@example.com','test',?)")
            .bind(auth::now()).execute(&t.state.db).await.unwrap();
    }
    let worker = tokio::spawn(auth::mail_worker(t.state.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_requests")
                .fetch_one(&t.state.db)
                .await
                .unwrap();
            if remaining == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let path = t
        .dir
        .path()
        .join("mail")
        .join(format!("{}.txt", auth::hash("admin@example.com")));
    // The worker's fallback is five minutes; this delivery must be notified.
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/request",
            "",
            json!({"email":"admin@example.com"})
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    let _ = worker.await;
}

#[tokio::test]
async fn claimed_invite_survives_expiry_and_cleanup_without_restoring_removed_access() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let room = t.room(&owner, "private").await;
    let (guest, id) = t.guest("Guest").await;
    let link = t
        .req(
            "POST",
            &format!("/api/rooms/{room}/invites"),
            &owner,
            json!({"kind":"guest"}),
        )
        .await
        .1;
    let token = link["url"].as_str().unwrap().split('=').nth(1).unwrap();
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &guest,
            json!({"token":token})
        )
        .await
        .0,
        StatusCode::OK
    );
    t.req(
        "POST",
        &format!("/api/rooms/{room}/invites"),
        &owner,
        json!({"kind":"guest"}),
    )
    .await;
    sqlx::query("UPDATE room_invites SET expires=0")
        .execute(&t.state.db)
        .await
        .unwrap();
    rooms::cleanup_invites(&t.state.db).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM room_invites")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        t.req("POST", "/api/entry", &guest, json!({"invite":token}))
            .await
            .1["state"],
        "ready"
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &guest,
            json!({"token":token})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        t.req("POST", "/api/entry", "", json!({"invite":token}))
            .await
            .1,
        json!({"state":"used"})
    );
    assert_eq!(
        t.req(
            "DELETE",
            &format!("/api/rooms/{room}/members/{id}"),
            &owner,
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        t.req("POST", "/api/entry", &guest, json!({"invite":token}))
            .await
            .1["state"],
        "removed"
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &guest,
            json!({"token":token})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn multiple_connections_are_removed_and_fresh_identities_cannot_reclaim_direct_slot() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let room = t.room(&owner, "direct").await;
    let (guest, id) = t.guest("Guest A").await;
    let (replacement, _) = t.guest("Guest B").await;
    let first = t.join(&guest, &room).await.1;
    let second = t.join(&guest, &room).await.1;
    let owner_join = t.join(&owner, &room).await.1;
    assert_ne!(first["identity"], second["identity"]);
    for value in [&first, &second] {
        assert_eq!(
            t.gate(value["token"].as_str().unwrap()).await,
            StatusCode::NO_CONTENT
        );
    }
    *t.participants.lock().unwrap() = vec![
        first["identity"].as_str().unwrap().into(),
        second["identity"].as_str().unwrap().into(),
        id.clone(),
        owner_join["identity"].as_str().unwrap().into(),
    ];
    t.fail_disconnect
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let path = format!("/api/rooms/{room}/members/{id}");
    assert_eq!(
        t.req("DELETE", &path, &owner, json!({})).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        t.req("POST", "/api/entry", &replacement, json!({"room":room}))
            .await
            .1["state"],
        "full"
    );
    assert_eq!(t.join(&replacement, &room).await.0, StatusCode::FORBIDDEN);
    t.fail_disconnect
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        t.req("DELETE", &path, &owner, json!({})).await.0,
        StatusCode::OK
    );
    assert_eq!(t.disconnected.lock().unwrap().len(), 3); // both connections and a legacy identity
    assert_eq!(
        *t.participants.lock().unwrap(),
        vec![owner_join["identity"].as_str().unwrap().to_owned()]
    );
    assert_eq!(t.join(&guest, &room).await.0, StatusCode::FORBIDDEN);
    for value in [&first, &second] {
        assert_eq!(
            t.gate(value["token"].as_str().unwrap()).await,
            StatusCode::FORBIDDEN
        );
    }
    // Retrying removal must not reopen the bearer link.
    assert_eq!(
        t.req("POST", "/api/entry", "", json!({"room":room}))
            .await
            .1["state"],
        "full"
    );
    assert_eq!(t.join(&replacement, &room).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn desktop_session_replacement_revokes_only_the_previous_mac_credential() {
    let t = Test::new().await;
    let browser = t.admin().await;
    let (old, _) = t.guest("Old Mac identity").await;
    let challenge = t
        .req("POST", "/api/auth/desktop/start", "", json!({}))
        .await
        .1;
    assert_eq!(
        t.req("POST", "/api/auth/desktop/poll", &old, challenge.clone())
            .await
            .1["pending"],
        true
    );
    assert_eq!(
        t.req("GET", "/api/auth/me", &old, json!({})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/desktop/poll",
            &old,
            json!({"id":challenge["id"],"secret":"wrong"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.req("GET", "/api/auth/me", &old, json!({})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        t.req(
            "POST",
            "/api/auth/desktop/approve",
            &browser,
            challenge.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, result, _) = t
        .req("POST", "/api/auth/desktop/poll", &old, challenge)
        .await;
    assert_eq!(status, StatusCode::OK);
    let new = result["token"].as_str().unwrap();
    assert_eq!(
        t.req("GET", "/api/auth/me", &old, json!({})).await.0,
        StatusCode::OK
    );
    // Only using the stored replacement activates it. Parallel initial requests
    // can both succeed, but the old credential is revoked once.
    let (me, rooms) = tokio::join!(
        t.req("GET", "/api/auth/me", new, json!({})),
        t.req("GET", "/api/rooms", new, json!({}))
    );
    assert_eq!(me.0, StatusCode::OK);
    assert_eq!(rooms.0, StatusCode::OK);
    assert_eq!(
        t.req("GET", "/api/auth/me", &old, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        t.req("GET", "/api/auth/me", &browser, json!({})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn owners_and_existing_members_do_not_consume_guest_invitations() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let room = t.room(&owner, "private").await;
    let link = t
        .req(
            "POST",
            &format!("/api/rooms/{room}/invites"),
            &owner,
            json!({"kind":"guest"}),
        )
        .await
        .1;
    let token = link["url"].as_str().unwrap().split('=').nth(1).unwrap();
    for _ in 0..2 {
        assert_eq!(
            t.req("POST", "/api/entry", &owner, json!({"invite":token}))
                .await
                .1["state"],
            "ready"
        );
        assert_eq!(
            t.req(
                "POST",
                "/api/invites/redeem",
                &owner,
                json!({"token":token})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let (guest, guest_id) = t.guest("Actual guest").await;
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &guest,
            json!({"token":token})
        )
        .await
        .0,
        StatusCode::OK
    );
    let detail = t
        .req("GET", &format!("/api/rooms/{room}"), &owner, json!({}))
        .await
        .1;
    assert_eq!(detail["members"].as_array().unwrap().len(), 1);
    assert_eq!(detail["members"][0]["id"], guest_id);
    let link = t
        .req(
            "POST",
            &format!("/api/rooms/{room}/invites"),
            &owner,
            json!({"kind":"guest"}),
        )
        .await
        .1;
    let token = link["url"].as_str().unwrap().split('=').nth(1).unwrap();
    assert_eq!(
        t.req(
            "POST",
            "/api/invites/redeem",
            &guest,
            json!({"token":token})
        )
        .await
        .0,
        StatusCode::OK
    );
    let used: Option<String> = sqlx::query_scalar("SELECT used_by FROM room_invites WHERE hash=?")
        .bind(auth::hash(token))
        .fetch_one(&t.state.db)
        .await
        .unwrap();
    assert!(used.is_none());
}

#[tokio::test]
async fn claims_are_atomic_with_multiple_connections_and_zero_row_updates_are_denied() {
    let mut t = Test::new().await;
    // Exercise real concurrent SQLite connections, not the alpha single pool slot.
    t.state.db = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            t.state
                .config
                .database_url
                .parse::<SqliteConnectOptions>()
                .unwrap()
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    t.app = router(t.state.clone());
    let owner = t.admin().await;
    let (a, _) = t.guest("A").await;
    let (b, _) = t.guest("B").await;
    let direct = t.room(&owner, "direct").await;
    let (first, second) = tokio::join!(t.join(&a, &direct), t.join(&b, &direct));
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::FORBIDDEN]);
    let private = t.room(&owner, "private").await;
    let link = t
        .req(
            "POST",
            &format!("/api/rooms/{private}/invites"),
            &owner,
            json!({"kind":"guest"}),
        )
        .await
        .1;
    let token = link["url"].as_str().unwrap().split('=').nth(1).unwrap();
    let (first, second) = tokio::join!(
        t.req("POST", "/api/invites/redeem", &a, json!({"token":token})),
        t.req("POST", "/api/invites/redeem", &b, json!({"token":token}))
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::FORBIDDEN]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memberships WHERE room=?")
            .bind(&private)
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        1
    );
    // Simulate a conditional update losing its claim even with a fresh read.
    sqlx::query("CREATE TRIGGER ignore_slot BEFORE UPDATE OF guest ON rooms BEGIN SELECT RAISE(IGNORE); END").execute(&t.state.db).await.unwrap();
    let unclaimed = t.room(&owner, "direct").await;
    assert_eq!(t.join(&a, &unclaimed).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memberships WHERE room=?")
            .bind(&unclaimed)
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        0
    );
    sqlx::query("CREATE TRIGGER ignore_invite BEFORE UPDATE OF used_by ON room_invites BEGIN SELECT RAISE(IGNORE); END").execute(&t.state.db).await.unwrap();
    let private = t.room(&owner, "private").await;
    let link = t
        .req(
            "POST",
            &format!("/api/rooms/{private}/invites"),
            &owner,
            json!({"kind":"guest"}),
        )
        .await
        .1;
    let token = link["url"].as_str().unwrap().split('=').nth(1).unwrap();
    assert_eq!(
        t.req("POST", "/api/invites/redeem", &a, json!({"token":token}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memberships WHERE room=?")
            .bind(&private)
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn temporary_direct_calls_expire_and_only_the_owner_can_keep_them() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let (guest, _) = t.guest("Guest").await;
    let quick = t.room(&owner, "direct").await;
    assert_eq!(
        t.req("GET", "/api/rooms", &owner, json!({})).await.1,
        json!([])
    );
    let token = t.join(&owner, &quick).await.1["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        t.req(
            "POST",
            &format!("/api/rooms/{quick}/keep"),
            &guest,
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE rooms SET expires=? WHERE id=?")
        .bind(auth::now() - 1)
        .bind(&quick)
        .execute(&t.state.db)
        .await
        .unwrap();
    // Both admission paths agree until cleanup commits closure, in either order.
    assert_eq!(t.join(&guest, &quick).await.0, StatusCode::OK);
    assert_eq!(t.gate(&token).await, StatusCode::NO_CONTENT);
    sqlx::query("UPDATE rooms SET expires=? WHERE id=?")
        .bind(auth::now() - 1)
        .bind(&quick)
        .execute(&t.state.db)
        .await
        .unwrap();
    assert_eq!(t.gate(&token).await, StatusCode::NO_CONTENT);
    assert_eq!(t.join(&guest, &quick).await.0, StatusCode::OK);
    assert_eq!(
        t.req("POST", "/api/entry", &guest, json!({"room":quick}))
            .await
            .1["state"],
        "ready"
    );
    sqlx::query("UPDATE rooms SET expires=? WHERE id=?")
        .bind(auth::now() - 1)
        .bind(&quick)
        .execute(&t.state.db)
        .await
        .unwrap();
    rooms::cleanup_quick_calls(&t.state).await.unwrap();
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT closed FROM rooms WHERE id=?")
            .bind(&quick)
            .fetch_one(&t.state.db)
            .await
            .unwrap()
    );
    sqlx::query("UPDATE rooms SET empty_since=?,cleanup_after=0 WHERE id=?")
        .bind(auth::now() - 601)
        .bind(&quick)
        .execute(&t.state.db)
        .await
        .unwrap();
    rooms::cleanup_quick_calls(&t.state).await.unwrap();
    assert_eq!(t.join(&guest, &quick).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        t.req("POST", "/api/entry", &guest, json!({"room":quick}))
            .await
            .1["state"],
        "unavailable"
    );
    assert_eq!(t.gate(&token).await, StatusCode::FORBIDDEN);
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT closed FROM rooms WHERE id=?")
            .bind(&quick)
            .fetch_one(&t.state.db)
            .await
            .unwrap()
    );
    let saved = t.room(&owner, "direct").await;
    assert_eq!(
        t.req(
            "POST",
            &format!("/api/rooms/{saved}/keep"),
            &owner,
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let rooms = t.req("GET", "/api/rooms", &owner, json!({})).await.1;
    assert_eq!(rooms.as_array().unwrap().len(), 1);
    assert_eq!(rooms[0]["id"], saved);
    assert!(rooms[0]["expires"].is_null());
    assert_eq!(t.join(&guest, &saved).await.0, StatusCode::OK);
}

#[tokio::test]
async fn rotating_ips_cannot_bypass_the_recipient_delivery_cap() {
    let t = Test::new().await;
    for i in 0..20 {
        assert_eq!(
            t.req_from(
                &format!("ip-{i}"),
                "POST",
                "/api/auth/request",
                "",
                json!({"email":"admin@example.com"})
            )
            .await
            .0,
            StatusCode::OK
        );
        auth::deliver_pending(&t.state).await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM magic_links WHERE email='admin@example.com'"
        )
        .fetch_one(&t.state.db)
        .await
        .unwrap(),
        6
    );
    // Suppression preserves delivered links and never leaks eligibility.
    let content = std::fs::read_to_string(
        t.dir
            .path()
            .join("mail")
            .join(format!("{}.txt", auth::hash("admin@example.com"))),
    )
    .unwrap();
    let token = content.split("#magic=").nth(1).unwrap().trim();
    assert_eq!(
        t.req("POST", "/api/auth/verify", "", json!({"token":token}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn mail_retries_are_bounded_and_dont_spin_on_permanent_failure() {
    let t = Test::new().await;
    std::fs::write(t.dir.path().join("mail"), "not a directory").unwrap();
    t.req(
        "POST",
        "/api/auth/request",
        "",
        json!({"email":"admin@example.com"}),
    )
    .await;
    for attempt in 1..=5 {
        assert_eq!(auth::deliver_pending(&t.state).await.unwrap(), 1);
        assert_eq!(auth::deliver_pending(&t.state).await.unwrap(), 0);
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_requests")
            .fetch_one(&t.state.db)
            .await
            .unwrap();
        assert_eq!(queued, i64::from(attempt < 5));
        sqlx::query("UPDATE mail_requests SET available=0")
            .execute(&t.state.db)
            .await
            .unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM magic_links")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        5
    );
}

#[tokio::test]
async fn abandoned_desktop_exchanges_keep_the_previous_session_usable() {
    let t = Test::new().await;
    let browser = t.admin().await;
    let (old, _) = t.guest("Mac").await;
    for cancel in [false, true] {
        let challenge = t
            .req("POST", "/api/auth/desktop/start", "", json!({}))
            .await
            .1;
        t.req(
            "POST",
            "/api/auth/desktop/approve",
            &browser,
            challenge.clone(),
        )
        .await;
        let issued = t
            .req("POST", "/api/auth/desktop/poll", &old, challenge)
            .await
            .1;
        let new = issued["token"].as_str().unwrap();
        if cancel {
            t.req("POST", "/api/auth/logout", new, json!({})).await;
        } else {
            // Lost response: the provisional credential expires unused.
            sqlx::query("UPDATE sessions SET expires=0 WHERE hash=?")
                .bind(auth::hash(new))
                .execute(&t.state.db)
                .await
                .unwrap();
        }
        assert_eq!(
            t.req("GET", "/api/auth/me", new, json!({})).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            t.req("GET", "/api/auth/me", &old, json!({})).await.0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn cleanup_is_fair_retries_failures_and_preserves_active_calls() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let active = t.room(&owner, "direct").await;
    let owner_id = t.req("GET", "/api/auth/me", &owner, json!({})).await.1["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Joining near the deadline renews it for a full day of activity.
    sqlx::query("UPDATE rooms SET expires=? WHERE id=?")
        .bind(auth::now() + 1)
        .bind(&active)
        .execute(&t.state.db)
        .await
        .unwrap();
    let token = t.join(&owner, &active).await.1["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline: i64 = sqlx::query_scalar("SELECT expires FROM rooms WHERE id=?")
        .bind(&active)
        .fetch_one(&t.state.db)
        .await
        .unwrap();
    assert!(deadline >= auth::now() + 86399);
    // An uninterrupted long call also stays open without a fresh API join.
    t.participants
        .lock()
        .unwrap()
        .push("still-connected".into());
    sqlx::query("UPDATE rooms SET expires=? WHERE id=?")
        .bind(auth::now() - 1)
        .bind(&active)
        .execute(&t.state.db)
        .await
        .unwrap();
    rooms::cleanup_quick_calls(&t.state).await.unwrap();
    assert!(t.deleted_rooms.lock().unwrap().is_empty());
    assert_eq!(t.gate(&token).await, StatusCode::NO_CONTENT);
    t.participants.lock().unwrap().clear();
    // More failing rooms than a batch must not starve the healthy tail.
    for i in 0..102 {
        let id = format!("expired-{i:03}");
        sqlx::query("INSERT INTO rooms(id,name,kind,owner,expires,empty_since) VALUES (?,'test','direct',?,?,?)")
            .bind(&id).bind(&owner_id).bind(auth::now()-900).bind(auth::now()-700).execute(&t.state.db).await.unwrap();
        if i < 100 {
            t.fail_rooms.lock().unwrap().insert(id);
        }
    }
    rooms::cleanup_quick_calls(&t.state).await.unwrap();
    rooms::cleanup_quick_calls(&t.state).await.unwrap();
    assert_eq!(t.deleted_rooms.lock().unwrap().len(), 2);
    t.fail_rooms.lock().unwrap().clear();
    sqlx::query(
        "UPDATE rooms SET cleanup_after=0 WHERE id LIKE 'expired-%' AND cleanup_after IS NOT NULL",
    )
    .execute(&t.state.db)
    .await
    .unwrap();
    rooms::cleanup_quick_calls(&t.state).await.unwrap();
    assert_eq!(t.deleted_rooms.lock().unwrap().len(), 102);
    rooms::cleanup_quick_calls(&t.state).await.unwrap();
    assert_eq!(t.deleted_rooms.lock().unwrap().len(), 102); // completed work stays done
}

#[tokio::test]
async fn failed_close_reports_closure_and_can_be_retried() {
    let t = Test::new().await;
    let owner = t.admin().await;
    let room = t.room(&owner, "public").await;
    t.fail_rooms.lock().unwrap().insert(room.clone());
    let (status, body, _) = t
        .req("DELETE", &format!("/api/rooms/{room}"), &owner, json!({}))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        "Room closed, but disconnection failed. Retry closing the room."
    );
    assert_eq!(t.join(&owner, &room).await.0, StatusCode::NOT_FOUND);
    t.fail_rooms.lock().unwrap().clear();
    assert_eq!(
        t.req("DELETE", &format!("/api/rooms/{room}"), &owner, json!({}))
            .await
            .0,
        StatusCode::OK
    );
}
