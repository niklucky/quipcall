use crate::{
    AppError, AppState, Result,
    auth::{self, Principal, hash, now, random},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use livekit_api::access_token::{AccessToken, TokenVerifier, VideoGrants};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, SqliteConnection};
use std::time::Duration;

#[derive(Serialize, FromRow)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub owner: String,
    pub guest: Option<String>,
    pub closed: bool,
    pub expires: Option<i64>,
}
async fn room(db: &mut SqliteConnection, id: &str) -> Result<Room> {
    sqlx::query_as("SELECT * FROM rooms WHERE id=? AND closed=0 AND (expires IS NULL OR expires>?)")
        .bind(id)
        .bind(now())
        .fetch_optional(db)
        .await?
        .ok_or_else(AppError::missing)
}
async fn membership(
    db: &mut SqliteConnection,
    room: &str,
    principal: &str,
) -> Result<Option<bool>> {
    Ok(
        sqlx::query_scalar("SELECT revoked FROM memberships WHERE room=? AND principal=?")
            .bind(room)
            .bind(principal)
            .fetch_optional(db)
            .await?,
    )
}
fn permits(r: &Room, principal: &str, revoked: Option<bool>) -> bool {
    if r.owner == principal {
        return true;
    }
    if revoked == Some(true) {
        return false;
    }
    match r.kind.as_str() {
        "public" => true,
        "direct" => r.guest.as_deref() == Some(principal),
        _ => revoked == Some(false),
    }
}
async fn allowed(db: &mut SqliteConnection, r: &Room, principal: &str) -> Result<bool> {
    Ok(permits(
        r,
        principal,
        membership(db, &r.id, principal).await?,
    ))
}
async fn owned(s: &AppState, h: &HeaderMap, id: &str) -> Result<Room> {
    let p = auth::user(s, h).await?;
    let r = room(&mut *s.db.acquire().await?, id).await?;
    if p.id != r.owner {
        return Err(AppError::forbidden("Only the room owner can manage access"));
    }
    Ok(r)
}
pub async fn list(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Vec<Room>>> {
    let p = auth::current(&s, &h).await?;
    Ok(Json(sqlx::query_as("SELECT r.* FROM rooms r WHERE r.closed=0 AND r.expires IS NULL AND (r.owner=? OR EXISTS(SELECT 1 FROM memberships m WHERE m.room=r.id AND m.principal=? AND m.revoked=0)) ORDER BY r.rowid DESC").bind(&p.id).bind(&p.id).fetch_all(&s.db).await?))
}
#[derive(Deserialize)]
pub struct Create {
    pub name: String,
    pub kind: String,
}
pub async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(req): Json<Create>,
) -> Result<Json<Room>> {
    let p = auth::user(&s, &h).await?;
    auth::rate(&s, &format!("rooms:{}", p.id), 30, 60).await?;
    let name = auth::clean_name(&req.name);
    if name.is_empty() || !matches!(req.kind.as_str(), "direct" | "public" | "private") {
        return Err(AppError::bad("Choose a room name and type"));
    }
    let id = random();
    sqlx::query("INSERT INTO rooms(id,name,kind,owner,expires) VALUES (?,?,?,?,?)")
        .bind(&id)
        .bind(name)
        .bind(&req.kind)
        .bind(p.id)
        .bind((req.kind == "direct").then(|| now() + 86400))
        .execute(&s.db)
        .await?;
    Ok(Json(room(&mut *s.db.acquire().await?, &id).await?))
}
pub async fn keep(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Room>> {
    owned(&s, &h, &id).await?;
    sqlx::query(
        "UPDATE rooms SET expires=NULL WHERE id=? AND closed=0 AND (expires IS NULL OR expires>?)",
    )
    .bind(&id)
    .bind(now())
    .execute(&s.db)
    .await?;
    Ok(Json(room(&mut *s.db.acquire().await?, &id).await?))
}

pub async fn detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let p = auth::current(&s, &h).await?;
    let mut db = s.db.acquire().await?;
    let r = room(&mut db, &id).await?;
    // Unclaimed direct calls can be previewed, but only join reserves the slot.
    if !allowed(&mut db, &r, &p.id).await? && !(r.kind == "direct" && r.guest.is_none()) {
        return Err(AppError::missing());
    }
    let mut result = json!({"room":r});
    if r.owner == p.id {
        #[derive(Serialize, FromRow)]
        struct Member {
            id: String,
            name: String,
            kind: String,
            revoked: bool,
        }
        let members: Vec<Member> = sqlx::query_as("SELECT p.id,p.name,p.kind,m.revoked FROM memberships m JOIN principals p ON p.id=m.principal WHERE m.room=?").bind(&id).fetch_all(&mut *db).await?;
        #[derive(Serialize, FromRow)]
        struct Invite {
            hash: String,
            kind: String,
            email: Option<String>,
            expires: i64,
        }
        let invites: Vec<Invite> = sqlx::query_as("SELECT hash,kind,email,expires FROM room_invites WHERE room=? AND used_by IS NULL AND expires>?").bind(&id).bind(now()).fetch_all(&mut *db).await?;
        result["members"] = json!(members);
        result["invites"] = json!(invites);
    }
    Ok(Json(result))
}
pub async fn rename(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<auth::Name>,
) -> Result<Json<Value>> {
    owned(&s, &h, &id).await?;
    let name = auth::clean_name(&req.name);
    if name.is_empty() {
        return Err(AppError::bad("Enter a room name"));
    }
    sqlx::query("UPDATE rooms SET name=? WHERE id=?")
        .bind(name)
        .bind(id)
        .execute(&s.db)
        .await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
pub struct Invite {
    pub kind: String,
    pub email: Option<String>,
}
pub async fn invite(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<Invite>,
) -> Result<Json<Value>> {
    let r = owned(&s, &h, &id).await?;
    if r.kind != "private" || !matches!(req.kind.as_str(), "member" | "guest") {
        return Err(AppError::bad("Invitations are for private rooms"));
    }
    let email = if req.kind == "member" {
        Some(auth::email(req.email.as_deref().unwrap_or_default())?)
    } else {
        None
    };
    let token = random();
    sqlx::query("INSERT INTO room_invites(hash,room,kind,email,expires) VALUES (?,?,?,?,?)")
        .bind(hash(&token))
        .bind(id)
        .bind(req.kind)
        .bind(email)
        .bind(now() + 7 * 86400)
        .execute(&s.db)
        .await?;
    Ok(Json(
        json!({"url":format!("{}/#roomInvite={token}",s.config.public_url)}),
    ))
}
pub async fn revoke_invite(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, invite)): Path<(String, String)>,
) -> Result<Json<Value>> {
    owned(&s, &h, &id).await?;
    sqlx::query("DELETE FROM room_invites WHERE room=? AND hash=? AND used_by IS NULL")
        .bind(id)
        .bind(invite)
        .execute(&s.db)
        .await?;
    Ok(Json(json!({"ok":true})))
}
pub async fn redeem(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(req): Json<auth::Token>,
) -> Result<Json<Room>> {
    let p = auth::current(&s, &h).await?;
    let mut tx = s.db.begin_with("BEGIN IMMEDIATE").await?;
    let (id, kind, email, used): (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT room,kind,email,used_by FROM room_invites WHERE hash=? AND (used_by IS NOT NULL OR expires>?)",
    )
    .bind(hash(&req.token))
    .bind(now())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::bad("Invitation invalid or expired"))?;
    let r = room(&mut tx, &id).await?;
    // Owners and existing members already have access. Inspecting their own
    // link must not consume the guest's claim or add an owner membership.
    if allowed(&mut tx, &r, &p.id).await? {
        return Ok(Json(r));
    }
    if used.is_some() {
        return Err(AppError::forbidden("This invitation has already been used"));
    }
    if kind == "member" && (p.kind != "user" || p.email != email) {
        return Err(AppError::forbidden(
            "Sign in with the invited email to accept membership",
        ));
    }
    let claimed = sqlx::query(
        "UPDATE room_invites SET used_by=? WHERE hash=? AND used_by IS NULL AND expires>?",
    )
    .bind(&p.id)
    .bind(hash(&req.token))
    .bind(now())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed != 1 {
        return Err(AppError::forbidden(
            "This invitation has already been used or expired",
        ));
    }
    sqlx::query("INSERT INTO memberships VALUES (?,?,0) ON CONFLICT(room,principal) DO UPDATE SET revoked=0").bind(&id).bind(&p.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(r))
}
/// Read-only preflight. Opening an invitation must never consume it or claim
/// the direct-call slot. Private metadata is disclosed only with access or an
/// unused, valid invitation; denial responses contain no room details.
#[derive(Deserialize)]
pub struct Entry {
    pub room: Option<String>,
    pub invite: Option<String>,
}
pub async fn entry(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(req): Json<Entry>,
) -> Result<Json<Value>> {
    let p = match auth::current(&s, &h).await {
        Ok(p) => Some(p),
        Err(e) if e.status == StatusCode::UNAUTHORIZED => None,
        Err(e) => return Err(e),
    };
    let mut db = s.db.acquire().await?;
    #[derive(FromRow)]
    struct InvitationPreview {
        room: String,
        kind: String,
        email: Option<String>,
        used_by: Option<String>,
        expires: i64,
    }
    let mut invitation = None;
    let id = match (req.room, req.invite) {
        (Some(id), None) => id,
        (None, Some(token)) => {
            let value: Option<InvitationPreview> = sqlx::query_as(
                "SELECT room,kind,email,used_by,expires FROM room_invites WHERE hash=?",
            )
            .bind(hash(&token))
            .fetch_optional(&mut *db)
            .await?;
            let Some(InvitationPreview {
                room: id,
                kind,
                email,
                used_by: used,
                expires,
            }) = value
            else {
                return Ok(Json(json!({"state":"expired"})));
            };
            if used.is_none() && expires <= now() {
                return Ok(Json(json!({"state":"expired"})));
            }
            invitation = Some((kind, email, used));
            id
        }
        _ => return Err(AppError::bad("Provide a room or an invitation")),
    };
    let r = match room(&mut db, &id).await {
        Ok(r) => r,
        Err(e) if e.status == StatusCode::NOT_FOUND => {
            return Ok(Json(json!({"state":"unavailable"})));
        }
        Err(e) => return Err(e),
    };
    let membership = if let Some(p) = &p {
        membership(&mut db, &id, &p.id).await?
    } else {
        None
    };
    let revoked = membership == Some(true);
    let already_allowed = p.as_ref().is_some_and(|p| permits(&r, &p.id, membership));
    let state = if already_allowed {
        "ready"
    } else if let Some((kind, email, used)) = &invitation {
        // A fresh invitation intentionally allows the owner to re-invite a
        // removed guest. An already consumed link cannot restore access.
        if used.is_some() {
            if used.as_deref() != p.as_ref().map(|p| p.id.as_str()) {
                "used"
            } else if revoked {
                "removed"
            } else {
                "private"
            }
        } else if kind == "member"
            && p.as_ref()
                .is_none_or(|p| p.kind != "user" || p.email != *email)
        {
            "signin"
        } else {
            "invite"
        }
    } else if revoked {
        "removed"
    } else {
        match r.kind.as_str() {
            "public" => "ready",
            "direct" if r.guest.is_none() => "ready",
            "direct" => "full",
            _ => "private",
        }
    };
    let mut value = json!({"state":state});
    if matches!(state, "ready" | "invite" | "signin") {
        value["room"] = json!({"id":r.id,"name":r.name,"kind":r.kind});
        if let Some((kind, _, _)) = invitation {
            value["inviteKind"] = json!(kind);
        }
    }
    Ok(Json(value))
}

#[derive(Deserialize)]
pub struct Join {
    pub room: String,
}
pub async fn join(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(req): Json<Join>,
) -> Result<Json<quipcall_protocol::JoinInfo>> {
    let p = auth::current(&s, &h).await?;
    let mut tx = s.db.begin_with("BEGIN IMMEDIATE").await?;
    let mut r = room(&mut tx, &req.room).await?;
    let membership = membership(&mut tx, &r.id, &p.id).await?;
    if r.kind == "direct" && r.owner != p.id && r.guest.is_none() {
        let blocked = membership == Some(true);
        if blocked {
            return Err(AppError::forbidden("Your access was removed"));
        }
        let claimed = sqlx::query("UPDATE rooms SET guest=? WHERE id=? AND guest IS NULL AND closed=0 AND (expires IS NULL OR expires>?)")
            .bind(&p.id)
            .bind(&r.id).bind(now())
            .execute(&mut *tx)
            .await?.rows_affected();
        if claimed != 1 {
            return Err(AppError::forbidden("This call is full or unavailable"));
        }
        r.guest = Some(p.id.clone());
    }
    if !permits(&r, &p.id, membership) {
        return Err(AppError::forbidden(
            "This room is private, full, or your access was removed",
        ));
    }
    if p.id != r.owner {
        sqlx::query("INSERT OR IGNORE INTO memberships VALUES (?,?,0)")
            .bind(&r.id)
            .bind(&p.id)
            .execute(&mut *tx)
            .await?;
    }
    if r.expires.is_some() {
        touch(&mut tx, &r.id).await?;
    }
    // LiveKit identities belong to connections, access belongs to principals.
    // The signed subject preserves this binding across LiveKit token refreshes.
    let identity = format!("{}:{}", p.id, random());
    let token = AccessToken::with_api_key(&s.config.livekit_api_key, &s.config.livekit_api_secret)
        .with_identity(&identity)
        .with_name(&p.name)
        .with_ttl(Duration::from_secs(120))
        .with_grants(VideoGrants {
            room_join: true,
            room: r.id,
            can_publish: Some(true),
            can_subscribe: Some(true),
            can_publish_data: Some(true),
            can_update_own_metadata: Some(true),
            ..Default::default()
        })
        .to_jwt()?;
    tx.commit().await?;
    Ok(Json(quipcall_protocol::JoinInfo {
        url: s.config.livekit_public_url.clone(),
        token,
        identity,
    }))
}
/// Called by nginx auth_request on *every* LiveKit signaling handshake, including
/// reconnects with LiveKit-refreshed tokens. The public signaling port is loopback-only.
pub async fn media_check(State(s): State<AppState>, h: HeaderMap) -> Result<StatusCode> {
    let uri = h
        .get("x-original-uri")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let query = uri.split_once('?').map_or("", |(_, q)| q);
    // Match LiveKit's precedence exactly: a nonempty Authorization header
    // wins over the query. Otherwise a revoked header token could ride past
    // this gate alongside an allowed query token.
    let token = if let Some(header) = h.get("authorization").filter(|h| !h.is_empty()) {
        header
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned)
    } else {
        let mut tokens =
            url::form_urlencoded::parse(query.as_bytes()).filter(|(k, _)| k == "access_token");
        let first = tokens.next().map(|(_, v)| v.into_owned());
        if tokens.next().is_some() {
            return Err(AppError::unauthorized());
        }
        first
    }
    .ok_or_else(AppError::unauthorized)?;
    let claims =
        TokenVerifier::with_api_key(&s.config.livekit_api_key, &s.config.livekit_api_secret)
            .verify(&token)
            .map_err(|_| AppError::unauthorized())?;
    if !claims.video.room_join || claims.sub.is_empty() {
        return Err(AppError::unauthorized());
    }
    let mut db = s.db.acquire().await?;
    // auth_request understands 401/403, not 404.
    let r: Room = sqlx::query_as("SELECT * FROM rooms WHERE id=? AND closed=0")
        .bind(&claims.video.room)
        .fetch_optional(&mut *db)
        .await?
        .ok_or_else(|| AppError::forbidden("Room unavailable"))?;
    let principal = media_principal(&claims.sub);
    if !allowed(&mut db, &r, principal).await? {
        return Err(AppError::forbidden("Room access removed"));
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM principals WHERE id=?)")
        .bind(principal)
        .fetch_one(&mut *db)
        .await?;
    if !exists {
        return Err(AppError::unauthorized());
    }
    if r.expires.is_some() && !touch(&mut db, &r.id).await? {
        return Err(AppError::forbidden("Room unavailable"));
    }
    Ok(StatusCode::NO_CONTENT)
}
// Also accept legacy tokens whose signed identity was the principal itself.
fn media_principal(identity: &str) -> &str {
    identity
        .split_once(':')
        .map_or(identity, |(principal, _)| principal)
}
pub async fn cleanup_invites(db: &sqlx::SqlitePool) -> Result<()> {
    // Claimed links remain return addresses; membership still controls access.
    sqlx::query("DELETE FROM room_invites WHERE expires<? AND used_by IS NULL")
        .bind(now())
        .execute(db)
        .await?;
    Ok(())
}
/// Renew temporary room activity; closed rooms are never resurrected.
async fn touch(db: &mut SqliteConnection, id: &str) -> Result<bool> {
    Ok(sqlx::query("UPDATE rooms SET expires=CASE WHEN expires IS NOT NULL THEN ? ELSE NULL END,empty_since=NULL,cleanup_after=0 WHERE id=? AND closed=0")
        .bind(now()+86400).bind(id).execute(db).await?.rows_affected() == 1)
}
pub async fn cleanup_quick_calls(s: &AppState) -> Result<()> {
    let expired: Vec<Room> = sqlx::query_as("SELECT * FROM rooms WHERE expires<=? AND cleanup_after<=? ORDER BY cleanup_after,expires,id LIMIT 100")
        .bind(now()+600).bind(now()).fetch_all(&s.db).await?;
    for room in expired {
        // Move this room behind unattempted work even if LiveKit fails. Process
        // every candidate independently; retry closed rooms until deletion works.
        sqlx::query("UPDATE rooms SET cleanup_after=? WHERE id=? AND cleanup_after IS NOT NULL")
            .bind(now() + 600)
            .bind(&room.id)
            .execute(&s.db)
            .await?;
        if let Err(e) = cleanup_quick_call(s, &room).await {
            tracing::warn!(room = %room.id, "quick call cleanup: {}", e.message);
        }
    }
    Ok(())
}
async fn cleanup_quick_call(s: &AppState, room: &Room) -> Result<()> {
    if !room.closed {
        let active = livekit(s, "ListParticipants", room, None).await?;
        let occupied = match active.get("participants") {
            Some(Value::Array(participants)) => !participants.is_empty(),
            None if active.is_object() => false, // omitted empty protobuf list
            _ => {
                return Err(AppError::new(
                    StatusCode::BAD_GATEWAY,
                    "Invalid room presence response",
                ));
            }
        };
        if occupied {
            touch(&mut *s.db.acquire().await?, &room.id).await?;
            return Ok(());
        }
        // Two empty observations ten minutes apart leave room for reconnects.
        sqlx::query("UPDATE rooms SET empty_since=COALESCE(empty_since,?) WHERE id=? AND closed=0 AND expires<=?")
            .bind(now()).bind(&room.id).bind(now()).execute(&s.db).await?;
        let closed = sqlx::query(
            "UPDATE rooms SET closed=1 WHERE id=? AND closed=0 AND expires<=? AND empty_since<=?",
        )
        .bind(&room.id)
        .bind(now())
        .bind(now() - 600)
        .execute(&s.db)
        .await?
        .rows_affected();
        if closed == 0 {
            return Ok(());
        } // renewed, kept, or still in the grace period
    }
    livekit(s, "DeleteRoom", room, None).await?;
    sqlx::query("UPDATE rooms SET cleanup_after=NULL WHERE id=? AND closed=1")
        .bind(&room.id)
        .execute(&s.db)
        .await?;
    Ok(())
}

async fn livekit(s: &AppState, method: &str, r: &Room, identity: Option<&str>) -> Result<Value> {
    let token = AccessToken::with_api_key(&s.config.livekit_api_key, &s.config.livekit_api_secret)
        .with_ttl(Duration::from_secs(60))
        .with_grants(VideoGrants {
            room_admin: true,
            room_create: method == "DeleteRoom",
            room: r.id.clone(),
            ..Default::default()
        })
        .to_jwt()?;
    let response = s
        .http
        .post(format!(
            "{}/twirp/livekit.RoomService/{method}",
            s.config.livekit_api_url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .json(&match identity {
            Some(identity) => json!({"room":r.id,"identity":identity}),
            None => json!({"room":r.id}),
        })
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(json!({"participants":[]}));
    }
    if response.status().is_success() {
        return Ok(response.json().await?);
    }
    Err(AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "Access revoked, but disconnection failed. Retry removal.",
    ))
}
pub async fn remove(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, principal)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let r = owned(&s, &h, &id).await?;
    if principal == r.owner {
        return Err(AppError::bad("The owner cannot be removed"));
    }
    // Persist first: even if LiveKit is down, no new/replayed tokens get in.
    let mut tx = s.db.begin().await?;
    sqlx::query("INSERT INTO memberships VALUES (?,?,1) ON CONFLICT(room,principal) DO UPDATE SET revoked=1")
        .bind(&id).bind(&principal).execute(&mut *tx).await?;
    // Free only this guest's slot, even on a retry after a replacement joined.
    sqlx::query("UPDATE rooms SET guest=NULL WHERE id=? AND kind='direct' AND guest=?")
        .bind(&id)
        .bind(&principal)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let active = livekit(&s, "ListParticipants", &r, None).await?;
    if let Some(participants) = active["participants"].as_array() {
        for identity in participants.iter().filter_map(|p| p["identity"].as_str()) {
            if media_principal(identity) == principal {
                livekit(&s, "RemoveParticipant", &r, Some(identity)).await?;
            }
        }
    }
    Ok(Json(json!({"ok":true})))
}
pub async fn close(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    // Allow retrying a failed disconnect after the room was marked closed.
    let p: Principal = auth::user(&s, &h).await?;
    let r: Room = sqlx::query_as("SELECT * FROM rooms WHERE id=? AND owner=?")
        .bind(&id)
        .bind(p.id)
        .fetch_optional(&s.db)
        .await?
        .ok_or_else(AppError::missing)?;
    sqlx::query("UPDATE rooms SET closed=1 WHERE id=?")
        .bind(id)
        .execute(&s.db)
        .await?;
    livekit(&s, "DeleteRoom", &r, None).await?;
    Ok(Json(json!({"ok":true})))
}
