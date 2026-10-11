//! bak-server: a backup server speaking restic's REST protocol (v1 and v2).
//!
//! Works with `bak`, `restic` and `rustic` clients. Repositories are plain
//! directories in restic's layout under `--path`; a repository may be nested
//! (`/rob/myproj`).
//!
//! Safety properties:
//! - Every uploaded file is written to a temporary file, fsynced, and linked
//!   into place without ever overwriting an existing file.
//! - Uploads are checked against their name: a file's name is the SHA-256 of
//!   its content, so a corrupted upload is rejected.
//! - In append-only mode nothing can be deleted except lock files.
//! - Paths are parsed strictly; hidden names (`.htpasswd`, `.tls`) are never served.

pub mod auth;
mod dashboard;
pub mod guard;
pub mod tls;

use std::convert::Infallible;
use std::io::SeekFrom;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE, RANGE, WWW_AUTHENTICATE};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::io::ReaderStream;

type Body = BoxBody<Bytes, std::io::Error>;

/// Object directories of a restic repository.
const TYPES: &[&str] = &["data", "keys", "locks", "snapshots", "index"];
const V2_MEDIA_TYPE: &str = "application/vnd.x.restic.rest.v2";

#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding all repositories.
    pub path: PathBuf,
    /// Refuse deletes (except lock files) and overwrites.
    pub append_only: bool,
    /// A user may only access repositories under `/<username>/`.
    pub private_repos: bool,
    /// `None` disables authentication.
    pub htpasswd: Option<PathBuf>,
    /// Log one line per request to stderr.
    pub verbose: bool,
    /// Serve the web dashboard at `/_/` (needs authentication).
    pub dashboard: bool,
}

struct State {
    cfg: Config,
    auth: Option<auth::Htpasswd>,
    tls: bool,
    tmp_counter: AtomicU64,
    started: Instant,
    limiter: guard::Limiter,
    sessions: guard::Sessions,
    activity: guard::Activity,
    requests: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
}

/// Serve until the listener fails. `tls` of `None` serves plain HTTP.
pub async fn serve(
    listener: TcpListener,
    cfg: Config,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> std::io::Result<()> {
    let auth = cfg.htpasswd.as_deref().map(auth::Htpasswd::new);
    let state = Arc::new(State {
        cfg,
        auth,
        tls: tls.is_some(),
        tmp_counter: AtomicU64::new(0),
        started: Instant::now(),
        limiter: guard::Limiter::default(),
        sessions: guard::Sessions::default(),
        activity: guard::Activity::default(),
        requests: AtomicU64::new(0),
        bytes_in: AtomicU64::new(0),
        bytes_out: AtomicU64::new(0),
    });
    let acceptor = tls.map(TlsAcceptor::from);
    loop {
        let (stream, peer) = listener.accept().await?;
        let state = state.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(handle(&state, req, peer).await) }
            });
            let http = hyper::server::conn::http1::Builder::new();
            let result = match acceptor {
                Some(acceptor) => match acceptor.accept(stream).await {
                    Ok(tls_stream) => http
                        .serve_connection(TokioIo::new(tls_stream), service)
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(format!("TLS handshake: {e}")),
                },
                None => http
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                    .map_err(|e| e.to_string()),
            };
            if let Err(e) = result {
                eprintln!("bak-server: connection from {peer}: {e}");
            }
        });
    }
}

/// Start a server on a background thread (for tests and embedding).
/// Returns the bound address.
pub fn spawn(
    listen: &str,
    cfg: Config,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> std::io::Result<SocketAddr> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let listener = runtime.block_on(TcpListener::bind(listen))?;
    let addr = listener.local_addr()?;
    std::thread::spawn(move || {
        if let Err(e) = runtime.block_on(serve(listener, cfg, tls)) {
            eprintln!("bak-server: {e}");
        }
    });
    Ok(addr)
}

async fn handle(state: &State, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    state.requests.fetch_add(1, Ordering::Relaxed);
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if path == "/" && method == Method::GET && state.cfg.dashboard && state.auth.is_some() {
        let mut r = status(StatusCode::FOUND);
        r.headers_mut()
            .insert(hyper::header::LOCATION, HeaderValue::from_static("/_/"));
        return r;
    }
    if path == "/_" || path.starts_with("/_/") {
        if !state.cfg.dashboard || state.auth.is_none() {
            return status(StatusCode::NOT_FOUND);
        }
        return dashboard::handle(state, req, peer).await;
    }
    if state.limiter.is_locked(peer.ip()) {
        return status_msg(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed logins; try again later",
        );
    }
    let user = match authenticate(state, &req, peer).await {
        Ok(user) => user,
        Err(resp) => return resp,
    };
    let resp = route(state, req, user.as_deref(), peer).await;
    if state.cfg.verbose {
        eprintln!(
            "{peer} {} {method} {path} -> {}",
            user.as_deref().unwrap_or("-"),
            resp.status().as_u16()
        );
    }
    resp
}

async fn authenticate(
    state: &State,
    req: &Request<Incoming>,
    peer: SocketAddr,
) -> Result<Option<String>, Response<Body>> {
    let Some(htpasswd) = &state.auth else {
        return Ok(None);
    };
    let unauthorized = || {
        let mut r = status(StatusCode::UNAUTHORIZED);
        r.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"bak-server\""),
        );
        r
    };
    let Some((user, password)) = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::parse_basic)
    else {
        return Err(unauthorized());
    };
    match htpasswd.verify(&user, &password).await {
        Ok(true) => {
            state.limiter.succeed(peer.ip());
            state.activity.seen(&user);
            Ok(Some(user))
        }
        Ok(false) => {
            record_failure(state, peer, &user, "backup client");
            Err(unauthorized())
        }
        Err(e) => {
            eprintln!("bak-server: {e}");
            Err(status(StatusCode::INTERNAL_SERVER_ERROR))
        }
    }
}

/// Count a failed login and log it, locking the address out when needed.
fn record_failure(state: &State, peer: SocketAddr, user: &str, via: &str) {
    state
        .activity
        .record(Some(user), peer.ip(), "login_failed", format!("via {via}"));
    if state.limiter.fail(peer.ip()) {
        eprintln!(
            "bak-server: locking out {} after repeated failed logins",
            peer.ip()
        );
        state.activity.record(
            None,
            peer.ip(),
            "lockout",
            format!(
                "{} failed logins; blocked for {} min",
                guard::MAX_FAILURES,
                guard::LOCKOUT.as_secs() / 60
            ),
        );
    }
}

/// What a request path refers to.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// `POST /<repo>/?create=true`
    Repo(Vec<String>),
    /// `/<repo>/config`
    Config(Vec<String>),
    /// `/<repo>/<type>/`
    List(Vec<String>, &'static str),
    /// `/<repo>/<type>/<name>`
    Object(Vec<String>, &'static str, String),
}

fn parse_target(path: &str, create: bool) -> Option<Target> {
    let segs: Vec<&str> = path.strip_prefix('/')?.split('/').collect();
    let (last, rest) = segs.split_last()?;
    let repo = |parts: &[&str]| -> Option<Vec<String>> {
        let parts: Vec<&str> = parts.iter().copied().filter(|s| !s.is_empty()).collect();
        parts
            .iter()
            .all(|s| valid_segment(s))
            .then(|| parts.iter().map(|s| s.to_string()).collect())
    };
    if create {
        return repo(&segs).map(Target::Repo);
    }
    if *last == "config" {
        return repo(rest).map(Target::Config);
    }
    let (ty, before) = rest.split_last()?;
    let ty = TYPES.iter().copied().find(|t| t == ty)?;
    if last.is_empty() {
        return repo(before).map(|r| Target::List(r, ty));
    }
    if is_object_name(last) {
        return repo(before).map(|r| Target::Object(r, ty, last.to_string()));
    }
    None
}

fn valid_segment(s: &str) -> bool {
    !s.starts_with('.')
        && !s.starts_with('_')
        && s.len() <= 255
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn is_object_name(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn repo_dir(state: &State, repo: &[String]) -> PathBuf {
    let mut dir = state.cfg.path.clone();
    dir.extend(repo);
    dir
}

fn object_path(dir: &Path, ty: &str, name: &str) -> PathBuf {
    if ty == "data" {
        dir.join(ty).join(&name[..2]).join(name)
    } else {
        dir.join(ty).join(name)
    }
}

async fn route(
    state: &State,
    req: Request<Incoming>,
    user: Option<&str>,
    peer: SocketAddr,
) -> Response<Body> {
    let create = req
        .uri()
        .query()
        .is_some_and(|q| q.split('&').any(|p| p == "create=true"));
    let Some(target) = parse_target(req.uri().path(), create) else {
        return status(StatusCode::NOT_FOUND);
    };
    let repo = match &target {
        Target::Repo(r) | Target::Config(r) | Target::List(r, _) | Target::Object(r, _, _) => r,
    };
    if state.cfg.private_repos && repo.first().map(String::as_str) != user {
        return status(StatusCode::UNAUTHORIZED);
    }
    let dir = repo_dir(state, repo);
    let repo_name = format!("/{}", repo.join("/"));
    let method = req.method().clone();
    let event: Option<(&'static str, String)> = match (&method, &target) {
        (&Method::POST, Target::Repo(_)) => Some(("create_repo", repo_name.clone())),
        (&Method::POST, Target::Object(_, "snapshots", id)) => {
            Some(("backup", format!("{repo_name} snapshot {}", &id[..8])))
        }
        (&Method::DELETE, Target::Object(_, ty, id)) if *ty != "locks" => {
            Some(("delete", format!("{repo_name} {ty}/{}", &id[..8])))
        }
        (&Method::DELETE, Target::Config(_)) => Some(("delete", format!("{repo_name} config"))),
        _ => None,
    };
    let result = match (&method, target) {
        (&Method::POST, Target::Repo(_)) => create_repo(&dir).await,
        (&Method::HEAD | &Method::GET, Target::Config(_)) => {
            read_file(state, &dir.join("config"), &req, method == Method::HEAD).await
        }
        (&Method::POST, Target::Config(_)) => {
            if !dir.is_dir() {
                return status(StatusCode::NOT_FOUND);
            }
            save_file(state, &dir, &dir.join("config"), None, req.into_body()).await
        }
        (&Method::DELETE, Target::Config(_)) => {
            if state.cfg.append_only {
                return status(StatusCode::FORBIDDEN);
            }
            delete_file(&dir.join("config")).await
        }
        (&Method::GET, Target::List(_, ty)) => list(&dir, ty, &req).await,
        (&Method::HEAD | &Method::GET, Target::Object(_, ty, name)) => {
            read_file(
                state,
                &object_path(&dir, ty, &name),
                &req,
                method == Method::HEAD,
            )
            .await
        }
        (&Method::POST, Target::Object(_, ty, name)) => {
            if !dir.join(ty).is_dir() {
                return status(StatusCode::NOT_FOUND);
            }
            let path = object_path(&dir, ty, &name);
            save_file(state, &dir, &path, Some(&name), req.into_body()).await
        }
        (&Method::DELETE, Target::Object(_, ty, name)) => {
            if state.cfg.append_only && ty != "locks" {
                return status(StatusCode::FORBIDDEN);
            }
            delete_file(&object_path(&dir, ty, &name)).await
        }
        _ => return status(StatusCode::METHOD_NOT_ALLOWED),
    };
    if let (Some((kind, detail)), Ok(resp)) = (event, &result) {
        if resp.status().is_success() {
            state.activity.record(user, peer.ip(), kind, detail);
        } else if resp.status() == StatusCode::FORBIDDEN {
            state.activity.record(
                user,
                peer.ip(),
                "refused",
                format!("{kind} {detail} (append-only)"),
            );
        }
    }
    result.unwrap_or_else(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            status(StatusCode::NOT_FOUND)
        } else {
            eprintln!("bak-server: {method} {}: {e}", dir.display());
            status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    })
}

async fn create_repo(dir: &Path) -> std::io::Result<Response<Body>> {
    for ty in TYPES {
        tokio::fs::create_dir_all(dir.join(ty)).await?;
    }
    for i in 0..=255u8 {
        tokio::fs::create_dir_all(dir.join("data").join(format!("{i:02x}"))).await?;
    }
    Ok(status(StatusCode::OK))
}

async fn read_file(
    state: &State,
    path: &Path,
    req: &Request<Incoming>,
    head_only: bool,
) -> std::io::Result<Response<Body>> {
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, len));
    let (start, end, partial) = match range {
        None => (0, len, false),
        Some(Some((s, e))) => (s, e, true),
        Some(None) => {
            let mut r = status(StatusCode::RANGE_NOT_SATISFIABLE);
            r.headers_mut().insert(
                "content-range",
                HeaderValue::from_str(&format!("bytes */{len}"))
                    .unwrap_or(HeaderValue::from_static("")),
            );
            return Ok(r);
        }
    };
    let count = end - start;
    let body: Body = if head_only {
        Empty::new().map_err(|never| match never {}).boxed()
    } else {
        file.seek(SeekFrom::Start(start)).await?;
        state.bytes_out.fetch_add(count, Ordering::Relaxed);
        let stream = ReaderStream::new(file.take(count)).map_ok(Frame::data);
        BodyExt::boxed(StreamBody::new(stream))
    };
    let mut resp = Response::new(body);
    if partial {
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{len}", end - 1)) {
            resp.headers_mut().insert("content-range", v);
        }
    }
    resp.headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from(count));
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok(resp)
}

/// Parse `bytes=a-b`, `bytes=a-` or `bytes=-n` into a half-open range within `len`.
fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n), len)
        }
        (s, "") => (s.parse().ok()?, len),
        (s, e) => {
            let e: u64 = e.parse().ok()?;
            (s.parse().ok()?, e.saturating_add(1).min(len))
        }
    };
    (start < end && start < len).then_some((start, end))
}

/// Stream the body to a temporary file, verify its hash, then link it into
/// place. Never overwrites: an existing file with the same content is a
/// success (a retried upload), anything else is refused.
async fn save_file(
    state: &State,
    repo: &Path,
    path: &Path,
    expect_sha256: Option<&str>,
    mut body: Incoming,
) -> std::io::Result<Response<Body>> {
    let parent = path.parent().unwrap_or(repo);
    tokio::fs::create_dir_all(parent).await?;
    let n = state.tmp_counter.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".tmp-{}-{n}", std::process::id()));
    let result = async {
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut hasher = Sha256::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(std::io::Error::other)?;
            if let Ok(data) = frame.into_data() {
                hasher.update(&data);
                state
                    .bytes_in
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                file.write_all(&data).await?;
            }
        }
        file.sync_all().await?;
        drop(file);
        if let Some(expected) = expect_sha256 {
            let actual: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if actual != expected {
                return Ok(status_msg(
                    StatusCode::BAD_REQUEST,
                    "content does not match its name",
                ));
            }
        }
        match tokio::fs::hard_link(&tmp, path).await {
            Ok(()) => {
                sync_dir(parent).await;
                Ok(status(StatusCode::OK))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if same_content(&tmp, path).await? {
                    Ok(status(StatusCode::OK))
                } else {
                    Ok(status_msg(StatusCode::FORBIDDEN, "file exists"))
                }
            }
            Err(e) => Err(e),
        }
    }
    .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

async fn same_content(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (ma, mb) = (tokio::fs::metadata(a).await?, tokio::fs::metadata(b).await?);
    if ma.len() != mb.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (
        tokio::fs::File::open(a).await?,
        tokio::fs::File::open(b).await?,
    );
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = fa.read(&mut ba).await?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut bb[..n]).await?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

async fn sync_dir(dir: &Path) {
    if let Ok(d) = tokio::fs::File::open(dir).await {
        let _ = d.sync_all().await;
    }
}

async fn delete_file(path: &Path) -> std::io::Result<Response<Body>> {
    tokio::fs::remove_file(path).await?;
    Ok(status(StatusCode::OK))
}

async fn list(dir: &Path, ty: &str, req: &Request<Incoming>) -> std::io::Result<Response<Body>> {
    let type_dir = dir.join(ty);
    if !type_dir.is_dir() {
        return Ok(status(StatusCode::NOT_FOUND));
    }
    let mut entries: Vec<(String, u64)> = Vec::new();
    let mut dirs = vec![type_dir.clone()];
    while let Some(d) = dirs.pop() {
        let mut rd = tokio::fs::read_dir(&d).await?;
        while let Some(e) = rd.next_entry().await? {
            let name = e.file_name().to_string_lossy().into_owned();
            let ft = e.file_type().await?;
            if ft.is_dir() && ty == "data" && d == type_dir && name.len() == 2 {
                dirs.push(e.path());
            } else if ft.is_file() && is_object_name(&name) {
                entries.push((name, e.metadata().await?.len()));
            }
        }
    }
    entries.sort();
    let v2 = req
        .headers()
        .get(hyper::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains(V2_MEDIA_TYPE));
    let json = if v2 {
        serde_json::Value::Array(
            entries
                .into_iter()
                .map(|(name, size)| serde_json::json!({ "name": name, "size": size }))
                .collect(),
        )
    } else {
        serde_json::Value::Array(entries.into_iter().map(|(n, _)| n.into()).collect())
    };
    let mut resp = Response::new(full(Bytes::from(json.to_string())));
    let ct = if v2 {
        V2_MEDIA_TYPE
    } else {
        "application/vnd.x.restic.rest.v1"
    };
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    Ok(resp)
}

fn full(b: Bytes) -> Body {
    Full::new(b).map_err(|never| match never {}).boxed()
}

fn status(code: StatusCode) -> Response<Body> {
    let mut r = Response::new(full(Bytes::new()));
    *r.status_mut() = code;
    r
}

fn status_msg(code: StatusCode, msg: &'static str) -> Response<Body> {
    let mut r = Response::new(full(Bytes::from_static(msg.as_bytes())));
    *r.status_mut() = code;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_paths() {
        let r = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_target("/rob/proj/config", false),
            Some(Target::Config(r(&["rob", "proj"])))
        );
        assert_eq!(parse_target("/config", false), Some(Target::Config(vec![])));
        assert_eq!(
            parse_target("/p/data/", false),
            Some(Target::List(r(&["p"]), "data"))
        );
        assert_eq!(
            parse_target(&format!("/p/keys/{ID}"), false),
            Some(Target::Object(r(&["p"]), "keys", ID.into()))
        );
        assert_eq!(
            parse_target("/rob/proj/", true),
            Some(Target::Repo(r(&["rob", "proj"])))
        );
        assert_eq!(parse_target("/../etc/config", false), None);
        assert_eq!(parse_target("/.htpasswd", false), None);
        assert_eq!(parse_target("/p/.tls/config", false), None);
        assert_eq!(parse_target("/p/data/notahexname", false), None);
        assert_eq!(parse_target("/p/other/", false), None);
        assert_eq!(parse_target("/_/config", false), None);
    }

    #[test]
    fn parses_ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 10)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 100)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 100)));
        assert_eq!(parse_range("bytes=95-200", 100), Some((95, 100)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
    }
}
