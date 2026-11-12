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
                    async move {
                        if uri.path().ends_with("ListParticipants") {
                            let participants: Vec<_> = active
                                .lock()
                                .unwrap()
                                .iter()
                                .map(|id| json!({"identity":id}))
                                .collect();
                            return (StatusCode::OK, Json(json!({"participants":participants})));
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
        "invite"
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
    // Pending requests survive restart and are processed independently.
    let restarted = AppState::new((*t.state.config).clone()).await.unwrap();
    auth::deliver_pending(&restarted).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM magic_links")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rate_limits WHERE key LIKE 'email:%'")
            .fetch_one(&t.state.db)
            .await
            .unwrap(),
        0
    );
    std::fs::remove_file(t.dir.path().join("mail")).unwrap();
    assert!(!t.magic("admin@example.com", None).await.is_empty());
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
async fn multiple_connections_are_removed_together_and_direct_slot_can_be_reused() {
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
        "ready"
    );
    assert_eq!(t.join(&replacement, &room).await.0, StatusCode::OK);
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
    // Retrying A's removal must not clear B's newly claimed slot.
    assert_eq!(
        t.req("POST", "/api/entry", "", json!({"room":room}))
            .await
            .1["state"],
        "full"
    );
    assert_eq!(t.join(&replacement, &room).await.0, StatusCode::OK);
}
