//! Web dashboard at `/_/`: a static single-page app plus a JSON API.
//!
//! Sign-in uses the same users file as backup clients and sets an HttpOnly,
//! SameSite=Strict session cookie. Every state-changing API call must also
//! carry an `X-Bak: 1` header, which a cross-site form or image cannot send.
//! The dashboard can read repository sizes and timestamps but never
//! repository contents: the server does not have the passphrase.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::header::{HeaderValue, CONTENT_TYPE, COOKIE, SET_COOKIE};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};

use crate::guard::{self, MIN_PASSWORD_LEN};
use crate::{auth, full, record_failure, Body, State};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const APP_CSS: &str = include_str!("../assets/app.css");
const COOKIE_NAME: &str = "bak_session";
const MAX_JSON: usize = 16 * 1024;

pub(crate) async fn handle(
    state: &State,
    req: Request<Incoming>,
    peer: SocketAddr,
) -> Response<Body> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let resp = match (&method, path.as_str()) {
        (&Method::GET, "/_" | "/_/" | "/_/index.html") => {
            asset(INDEX_HTML, "text/html; charset=utf-8")
        }
        (&Method::GET, "/_/app.js") => asset(APP_JS, "text/javascript; charset=utf-8"),
        (&Method::GET, "/_/app.css") => asset(APP_CSS, "text/css; charset=utf-8"),
        (&Method::GET, "/_/ca.pem") => ca_pem(state).await,
        _ if path.starts_with("/_/api/") => api(state, req, peer).await,
        _ => error(StatusCode::NOT_FOUND, "not found"),
    };
    secure_headers(resp)
}

async fn api(state: &State, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    let method = req.method().clone();
    let path = req.uri().path().trim_start_matches("/_/api/").to_string();
    let mutating = method != Method::GET;
    if mutating && req.headers().get("x-bak").and_then(|v| v.to_str().ok()) != Some("1") {
        return error(StatusCode::FORBIDDEN, "missing X-Bak header");
    }
    if method == Method::POST && path == "login" {
        return login(state, req, peer).await;
    }
    let token = session_token(&req);
    let Some(user) = token.as_deref().and_then(|t| state.sessions.user(t)) else {
        return error(StatusCode::UNAUTHORIZED, "sign in required");
    };
    let admin = is_admin(state, &user);
    let segs: Vec<&str> = path.split('/').collect();
    match (&method, segs.as_slice()) {
        (&Method::POST, ["logout"]) => {
            if let Some(t) = &token {
                state.sessions.remove(t);
            }
            let mut r = json_resp(StatusCode::OK, json!({}));
            set_cookie(&mut r, "", 0, state.tls);
            r
        }
        (&Method::GET, ["me"]) => json_resp(
            StatusCode::OK,
            json!({ "user": user, "admin": admin, "version": env!("CARGO_PKG_VERSION") }),
        ),
        (&Method::GET, ["overview"]) => overview(state, &user, admin).await,
        (&Method::GET, ["repos"]) => match scan_repos(state, &user, admin).await {
            Ok(repos) => json_resp(
                StatusCode::OK,
                Value::Array(repos.iter().map(Repo::to_json).collect()),
            ),
            Err(e) => internal(e),
        },
        (&Method::GET, ["activity"]) => {
            let events = state
                .activity
                .recent(if admin { None } else { Some(&user) }, 300);
            json_resp(
                StatusCode::OK,
                Value::Array(events.iter().map(guard::Event::to_json).collect()),
            )
        }
        (&Method::GET, ["users"]) if admin => list_users(state).await,
        (&Method::POST, ["users"]) if admin => add_user(state, req, &user, peer).await,
        (&Method::POST, ["users", name, "password"]) => {
            let name = name.to_string();
            if !admin && name != user {
                return error(StatusCode::FORBIDDEN, "admins only");
            }
            set_password(state, req, &user, &name, token.as_deref(), peer).await
        }
        (&Method::POST, ["users", name, "admin"]) if admin => {
            let name = name.to_string();
            set_admin_flag(state, req, &user, &name, peer).await
        }
        (&Method::DELETE, ["users", name]) if admin => {
            let name = name.to_string();
            remove_user(state, &user, &name, peer).await
        }
        (_, ["users", ..]) => error(StatusCode::FORBIDDEN, "admins only"),
        _ => error(StatusCode::NOT_FOUND, "no such API call"),
    }
}

async fn login(state: &State, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    if state.limiter.is_locked(peer.ip()) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed sign-ins; try again in 15 minutes",
        );
    }
    let Some(body) = read_json(req).await else {
        return error(StatusCode::BAD_REQUEST, "expected JSON");
    };
    let user = body["user"].as_str().unwrap_or("").to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let Some(htpasswd) = &state.auth else {
        return error(StatusCode::NOT_FOUND, "authentication is off");
    };
    match htpasswd.verify(&user, &password).await {
        Ok(true) => {
            state.limiter.succeed(peer.ip());
            state.activity.seen(&user);
            state
                .activity
                .record(Some(&user), peer.ip(), "login", "dashboard");
            let token = state.sessions.create(&user);
            let mut r = json_resp(StatusCode::OK, json!({ "user": user }));
            set_cookie(&mut r, &token, guard::SESSION_TTL.as_secs(), state.tls);
            r
        }
        Ok(false) => {
            record_failure(state, peer, &user, "dashboard");
            error(StatusCode::UNAUTHORIZED, "wrong user name or password")
        }
        Err(e) => internal(e),
    }
}

async fn overview(state: &State, user: &str, admin: bool) -> Response<Body> {
    let repos = match scan_repos(state, user, admin).await {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    let users =
        auth::parse(&std::fs::read_to_string(htpasswd_path(state)).unwrap_or_default()).len();
    let day_ago = guard::unix_now().saturating_sub(86_400);
    let mut v = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started.elapsed().as_secs(),
        "append_only": state.cfg.append_only,
        "private_repos": state.cfg.private_repos,
        "tls": state.tls,
        "ca_available": ca_path(state).exists(),
        "repos": repos.len(),
        "total_bytes": repos.iter().map(|r| r.bytes).sum::<u64>(),
        "snapshots": repos.iter().map(|r| r.snapshots).sum::<u64>(),
        "last_backup": repos.iter().filter_map(|r| r.last_backup).max(),
        "backups_24h": state.activity.count_since("backup", day_ago),
    });
    if admin {
        v["users"] = json!(users);
        v["requests"] = json!(state.requests.load(Ordering::Relaxed));
        v["bytes_in"] = json!(state.bytes_in.load(Ordering::Relaxed));
        v["bytes_out"] = json!(state.bytes_out.load(Ordering::Relaxed));
        v["failed_logins_24h"] = json!(state.activity.count_since("login_failed", day_ago));
        v["locked_ips"] = json!(state.limiter.locked_count());
        v["data_path"] = json!(state.cfg.path);
    }
    json_resp(StatusCode::OK, v)
}

struct Repo {
    path: String,
    owner: Option<String>,
    bytes: u64,
    files: u64,
    packs: u64,
    snapshots: u64,
    last_backup: Option<u64>,
    created: Option<u64>,
}

impl Repo {
    fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "owner": self.owner,
            "bytes": self.bytes,
            "files": self.files,
            "packs": self.packs,
            "snapshots": self.snapshots,
            "last_backup": self.last_backup,
            "created": self.created,
        })
    }
}

/// Repositories visible to `user`: all of them for admins and when repos
/// are shared, otherwise only those under `/<user>/`.
async fn scan_repos(state: &State, user: &str, admin: bool) -> std::io::Result<Vec<Repo>> {
    let root = state.cfg.path.clone();
    let only = (state.cfg.private_repos && !admin).then(|| user.to_string());
    let private = state.cfg.private_repos;
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        find_repos(&root, &root, &mut out)?;
        let mut repos: Vec<Repo> = out
            .into_iter()
            .map(|dir| summarize(&root, &dir, private))
            .collect::<std::io::Result<_>>()?;
        if let Some(u) = only {
            repos.retain(|r| r.owner.as_deref() == Some(&u));
        }
        repos.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(repos)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn find_repos(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if dir != root && dir.join("config").is_file() && dir.join("data").is_dir() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        let name = name.to_string_lossy();
        if e.file_type()?.is_dir() && !name.starts_with('.') && !name.starts_with('_') {
            find_repos(root, &e.path(), out)?;
        }
    }
    Ok(())
}

fn summarize(root: &Path, dir: &Path, private: bool) -> std::io::Result<Repo> {
    let rel: Vec<String> = dir
        .strip_prefix(root)
        .unwrap_or(dir)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut repo = Repo {
        path: format!("/{}", rel.join("/")),
        owner: if private { rel.first().cloned() } else { None },
        bytes: 0,
        files: 0,
        packs: 0,
        snapshots: 0,
        last_backup: None,
        created: mtime(&dir.join("config")),
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let meta = e.metadata()?;
            if meta.is_dir() {
                stack.push(e.path());
                continue;
            }
            repo.bytes += meta.len();
            repo.files += 1;
            let parent = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let in_data = d
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "data");
            if in_data {
                repo.packs += 1;
            } else if parent == "snapshots" {
                repo.snapshots += 1;
                let t = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                if let Some(t) = t {
                    repo.last_backup = repo.last_backup.max(Some(t.as_secs()));
                }
            }
        }
    }
    Ok(repo)
}

fn mtime(p: &Path) -> Option<u64> {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

async fn list_users(state: &State) -> Response<Body> {
    let text = std::fs::read_to_string(htpasswd_path(state)).unwrap_or_default();
    let admins = guard::load_admins(&guard::admins_path(&htpasswd_path(state)));
    let repos = scan_repos(state, "", true).await.unwrap_or_default();
    let mut names: Vec<String> = auth::parse(&text).into_keys().collect();
    names.sort();
    let users: Vec<Value> = names
        .iter()
        .map(|n| {
            json!({
                "name": n,
                "admin": admins.contains(n),
                "last_seen": state.activity.last_seen(n),
                "repos": repos.iter().filter(|r| r.owner.as_deref() == Some(n.as_str())).count(),
            })
        })
        .collect();
    json_resp(StatusCode::OK, Value::Array(users))
}

/// Resolve the requested password: `generate: true` or an explicit one.
/// Returns (password, was_generated).
fn chosen_password(body: &Value) -> Result<(String, bool), &'static str> {
    if body["generate"].as_bool() == Some(true) {
        return Ok((guard::generate_password(), true));
    }
    let p = body["password"].as_str().unwrap_or("");
    if p.chars().count() < MIN_PASSWORD_LEN {
        return Err("password must be at least 12 characters (or generate one)");
    }
    Ok((p.to_string(), false))
}

async fn add_user(
    state: &State,
    req: Request<Incoming>,
    actor: &str,
    peer: SocketAddr,
) -> Response<Body> {
    let Some(body) = read_json(req).await else {
        return error(StatusCode::BAD_REQUEST, "expected JSON");
    };
    let name = body["name"].as_str().unwrap_or("").to_string();
    if !auth::valid_username(&name) || name.starts_with('_') {
        return error(
            StatusCode::BAD_REQUEST,
            "user names use letters, digits, - _ . and cannot start with . or _",
        );
    }
    let path = htpasswd_path(state);
    let existing = auth::parse(&std::fs::read_to_string(&path).unwrap_or_default());
    if existing.contains_key(&name) {
        return error(StatusCode::CONFLICT, "that user already exists");
    }
    let (password, generated) = match chosen_password(&body) {
        Ok(p) => p,
        Err(msg) => return error(StatusCode::BAD_REQUEST, msg),
    };
    let make_admin = body["admin"].as_bool() == Some(true);
    let (p2, n2, pw2) = (path.clone(), name.clone(), password.clone());
    let res = tokio::task::spawn_blocking(move || {
        auth::set_user(&p2, &n2, &pw2)?;
        if make_admin {
            guard::set_admin(&guard::admins_path(&p2), &n2, true)?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(std::io::Error::other)
    .and_then(|r| r);
    if let Err(e) = res {
        return internal(e);
    }
    let role = if make_admin { " (admin)" } else { "" };
    state.activity.record(
        Some(actor),
        peer.ip(),
        "user_added",
        format!("{name}{role}"),
    );
    json_resp(
        StatusCode::OK,
        json!({ "name": name, "password": generated.then_some(password) }),
    )
}

async fn set_password(
    state: &State,
    req: Request<Incoming>,
    actor: &str,
    name: &str,
    keep_session: Option<&str>,
    peer: SocketAddr,
) -> Response<Body> {
    let Some(body) = read_json(req).await else {
        return error(StatusCode::BAD_REQUEST, "expected JSON");
    };
    let path = htpasswd_path(state);
    if !auth::parse(&std::fs::read_to_string(&path).unwrap_or_default()).contains_key(name) {
        return error(StatusCode::NOT_FOUND, "no such user");
    }
    // Changing your own password needs the current one, even for admins.
    if actor == name {
        let current = body["current_password"].as_str().unwrap_or("");
        let ok = match &state.auth {
            Some(h) => h.verify(name, current).await.unwrap_or(false),
            None => false,
        };
        if !ok {
            record_failure(state, peer, name, "password change");
            return error(StatusCode::FORBIDDEN, "current password is wrong");
        }
    }
    let (password, generated) = match chosen_password(&body) {
        Ok(p) => p,
        Err(msg) => return error(StatusCode::BAD_REQUEST, msg),
    };
    let (p2, n2, pw2) = (path.clone(), name.to_string(), password.clone());
    match tokio::task::spawn_blocking(move || auth::set_user(&p2, &n2, &pw2)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(std::io::Error::other(e)),
    }
    // Sign the user out everywhere else.
    state
        .sessions
        .remove_user(name, if actor == name { keep_session } else { None });
    state
        .activity
        .record(Some(actor), peer.ip(), "password_changed", name.to_string());
    json_resp(
        StatusCode::OK,
        json!({ "password": generated.then_some(password) }),
    )
}

async fn set_admin_flag(
    state: &State,
    req: Request<Incoming>,
    actor: &str,
    name: &str,
    peer: SocketAddr,
) -> Response<Body> {
    let Some(body) = read_json(req).await else {
        return error(StatusCode::BAD_REQUEST, "expected JSON");
    };
    let make = body["admin"].as_bool() == Some(true);
    if !make && name == actor {
        return error(
            StatusCode::BAD_REQUEST,
            "you cannot remove your own admin role",
        );
    }
    if let Err(e) = guard::set_admin(&guard::admins_path(&htpasswd_path(state)), name, make) {
        return internal(e);
    }
    let what = if make { "granted" } else { "revoked" };
    state.activity.record(
        Some(actor),
        peer.ip(),
        "admin_changed",
        format!("{name}: admin {what}"),
    );
    json_resp(StatusCode::OK, json!({ "name": name, "admin": make }))
}

async fn remove_user(state: &State, actor: &str, name: &str, peer: SocketAddr) -> Response<Body> {
    if name == actor {
        return error(StatusCode::BAD_REQUEST, "you cannot remove yourself");
    }
    let path = htpasswd_path(state);
    match auth::remove_user(&path, name) {
        Ok(true) => {}
        Ok(false) => return error(StatusCode::NOT_FOUND, "no such user"),
        Err(e) => return internal(e),
    }
    let _ = guard::set_admin(&guard::admins_path(&path), name, false);
    state.sessions.remove_user(name, None);
    state.activity.record(
        Some(actor),
        peer.ip(),
        "user_removed",
        format!("{name} (repositories kept)"),
    );
    json_resp(StatusCode::OK, json!({}))
}

fn is_admin(state: &State, user: &str) -> bool {
    guard::load_admins(&guard::admins_path(&htpasswd_path(state))).contains(user)
}

fn htpasswd_path(state: &State) -> PathBuf {
    state
        .cfg
        .htpasswd
        .clone()
        .unwrap_or_else(|| state.cfg.path.join(".htpasswd"))
}

fn ca_path(state: &State) -> PathBuf {
    state.cfg.path.join(".tls").join("ca.pem")
}

/// The generated CA certificate is public; clients download it to trust the server.
async fn ca_pem(state: &State) -> Response<Body> {
    match tokio::fs::read(ca_path(state)).await {
        Ok(bytes) => {
            let mut r = Response::new(full(Bytes::from(bytes)));
            r.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/x-pem-file"),
            );
            r.headers_mut().insert(
                "content-disposition",
                HeaderValue::from_static("attachment; filename=\"bak-server-ca.pem\""),
            );
            r
        }
        Err(_) => error(StatusCode::NOT_FOUND, "no generated CA on this server"),
    }
}

fn session_token(req: &Request<Incoming>) -> Option<String> {
    req.headers()
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE_NAME)
        .map(|(_, v)| v.to_string())
}

fn set_cookie(r: &mut Response<Body>, token: &str, max_age: u64, secure: bool) {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{COOKIE_NAME}={token}; Path=/_/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    );
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        r.headers_mut().insert(SET_COOKIE, v);
    }
}

async fn read_json(req: Request<Incoming>) -> Option<Value> {
    let limited = http_body_util::Limited::new(req.into_body(), MAX_JSON);
    let bytes = limited.collect().await.ok()?.to_bytes();
    serde_json::from_slice(&bytes).ok()
}

fn asset(text: &'static str, content_type: &'static str) -> Response<Body> {
    let mut r = Response::new(full(Bytes::from_static(text.as_bytes())));
    r.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    r
}

fn json_resp(code: StatusCode, v: Value) -> Response<Body> {
    let mut r = Response::new(full(Bytes::from(v.to_string())));
    *r.status_mut() = code;
    r.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

fn error(code: StatusCode, msg: &str) -> Response<Body> {
    json_resp(code, json!({ "error": msg }))
}

fn internal(e: std::io::Error) -> Response<Body> {
    eprintln!("bak-server: dashboard: {e}");
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error (see server log)",
    )
}

fn secure_headers(mut r: Response<Body>) -> Response<Body> {
    let h = r.headers_mut();
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
             connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("cache-control", HeaderValue::from_static("no-store"));
    r
}
