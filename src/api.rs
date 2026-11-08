//! Optional HTTP transport. All operational requests go through the daemon policy.
use crate::{
    Error, Result,
    contracts::{AppCommand, EventDto, LogStoragePolicyInput, OperationDto, Page, Role},
    server,
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    convert::Infallible,
    fs::OpenOptions,
    future::Future,
    io::{self, Read},
    net::SocketAddr,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::services::{ServeDir, ServeFile};

const MAX_BODY: usize = 65_536;
const MAX_QUERY: usize = 2_048;
const MAX_HEADERS: usize = 8_192;
const DEFAULT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; connect-src 'self'";
type RequestRates = HashMap<String, (Instant, u32, u32)>;
#[derive(Clone)]
struct AppState {
    directory: PathBuf,
    origins: Arc<Vec<String>>,
    shutdown: Arc<AtomicBool>,
    requests: Arc<Semaphore>,
    rpc: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    rates: Arc<Mutex<RequestRates>>,
    verifications: Arc<Mutex<(Instant, u32)>>,
    csp: Arc<Result<HeaderValue>>,
}
impl AppState {
    fn new(directory: PathBuf, origins: Vec<String>, shutdown: Arc<AtomicBool>) -> Self {
        Self {
            directory,
            origins: Arc::new(
                origins
                    .into_iter()
                    .filter(|origin| valid_origin(origin))
                    .collect(),
            ),
            shutdown,
            requests: Arc::new(Semaphore::new(64)),
            rpc: Arc::new(Semaphore::new(16)),
            streams: Arc::new(Semaphore::new(16)),
            rates: Arc::new(Mutex::new(HashMap::new())),
            verifications: Arc::new(Mutex::new((Instant::now(), 0))),
            csp: Arc::new(Ok(HeaderValue::from_static(DEFAULT_CSP))),
        }
    }
}

/// Builds the same authenticated routes used by the listener, without opening TCP.
pub fn router(
    directory: PathBuf,
    static_dir: Option<PathBuf>,
    allowed_origins: Vec<String>,
) -> Router {
    let mut state = AppState::new(directory, allowed_origins, Arc::new(AtomicBool::new(false)));
    state.csp = Arc::new(content_security_policy(static_dir.as_deref()));
    build_router(state, static_dir)
}
fn content_security_policy(static_dir: Option<&std::path::Path>) -> Result<HeaderValue> {
    let invalid = || {
        Error::new(
            "API_CONFIG_INVALID",
            "Dashboard API configuration requires a bounded relative path or explicit HTTPS origin (HTTP only on loopback), without credentials, query or fragment",
        )
    };
    let Some(directory) = static_dir else {
        return Ok(HeaderValue::from_static(DEFAULT_CSP));
    };
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(directory.join("config.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HeaderValue::from_static(DEFAULT_CSP));
        }
        Err(_) => return Err(invalid()),
    };
    let metadata = file.metadata().map_err(|_| invalid())?;
    let limit = u64::try_from(MAX_BODY).map_err(|_| invalid())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > MAX_BODY {
        return Err(invalid());
    }
    #[derive(Deserialize)]
    struct DashboardConfig {
        #[serde(rename = "apiBaseUrl")]
        api_base_url: String,
    }
    let config: DashboardConfig = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let address = config.api_base_url;
    if address.is_empty()
        || address.len() > MAX_HEADERS
        || address.contains(['?', '#'])
        || address.starts_with("//")
    {
        return Err(invalid());
    }
    let uri: axum::http::Uri = address.parse().map_err(|_| invalid())?;
    if uri.scheme().is_none() && uri.authority().is_none() {
        return Ok(HeaderValue::from_static(DEFAULT_CSP));
    }
    let scheme = uri.scheme_str().ok_or_else(invalid)?;
    let authority = uri.authority().ok_or_else(invalid)?;
    let origin = format!("{scheme}://{authority}");
    let loopback = uri.host().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "[::1]" | "::1")
    });
    if !valid_origin(&origin)
        || (scheme == "http" && !loopback)
        || !authority
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_:[]".contains(&byte))
    {
        return Err(invalid());
    }
    HeaderValue::from_str(&format!("{DEFAULT_CSP} {origin}")).map_err(|_| invalid())
}
fn build_router(state: AppState, static_dir: Option<PathBuf>) -> Router {
    let mut api = Router::new();
    for route in [
        "/health/ready",
        "/status",
        "/instance/status",
        "/session",
        "/workloads",
        "/workloads/{id}",
        "/executions",
        "/executions/{id}",
        "/executions/{id}/logs",
        "/executions/{id}/logs/info",
        "/schedules/{id}/preview",
        "/operations/{id}",
        "/diagnostics/{id}",
        "/audit",
        "/alerts/status",
        "/alerts/deliveries",
    ] {
        api = api.route(route, get(dispatch));
    }
    for route in [
        "/workloads/validate",
        "/config/plan",
        "/config/apply",
        "/jobs/{id}/run",
        "/workloads/{id}/pause",
        "/services/{id}/desired-state",
        "/services/{id}/restart",
        "/executions/{id}/cancel",
        "/executions/{id}/reconcile",
        "/tokens/create",
        "/tokens/{id}/revoke",
        "/alerts/deliveries/{id}/retry",
    ] {
        api = api.route(route, post(dispatch));
    }
    api = api
        .route("/log-storage", get(dispatch).post(dispatch))
        .route("/tokens", get(dispatch).post(dispatch))
        .route("/health/live", get(|| async { Json(json!({"live":true})) }))
        .route("/events", get(events))
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found);
    let mut app = Router::new().nest("/api/v1", api);
    if let Some(dir) = static_dir {
        app = app.fallback_service(
            ServeDir::new(&dir).not_found_service(ServeFile::new(dir.join("index.html"))),
        );
    } else {
        app = app.fallback(not_found);
    }
    app.layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}
/// TCP is opened only by an explicit API-enabled CLI request.
pub async fn serve(
    directory: PathBuf,
    listen: SocketAddr,
    static_dir: Option<PathBuf>,
    allowed_origins: Vec<String>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    if allowed_origins.iter().any(|origin| !valid_origin(origin)) {
        return Err(Error::new(
            "API_CONFIG_INVALID",
            "Allowed origins must be explicit http or https origins",
        ));
    }
    let mut state = AppState::new(directory, allowed_origins, shutdown.clone());
    state.csp = Arc::new(Ok(content_security_policy(static_dir.as_deref())?));
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|_| Error::new("API_UNAVAILABLE", "Cannot bind API listener"))?;
    let app = build_router(state, static_dir);
    axum::serve(
        BoundedListener {
            listener,
            slots: Arc::new(Semaphore::new(128)),
        },
        app,
    )
    .with_graceful_shutdown(async move {
        while !shutdown.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| Error::new("API_UNAVAILABLE", "API listener failed"))
}

// A whole-connection deadline also bounds clients that never finish HTTP headers.
struct DeadlineIo<S> {
    stream: S,
    deadline: Pin<Box<tokio::time::Sleep>>,
    _permit: OwnedSemaphorePermit,
}
impl<S> DeadlineIo<S> {
    fn expired(&mut self, cx: &mut Context<'_>) -> bool {
        self.deadline.as_mut().poll(cx).is_ready()
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for DeadlineIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.expired(cx) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Connection deadline reached",
            )));
        }
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for DeadlineIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.expired(cx) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Connection deadline reached",
            )));
        }
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
struct BoundedListener {
    listener: tokio::net::TcpListener,
    slots: Arc<Semaphore>,
}
impl axum::serve::Listener for BoundedListener {
    type Io = DeadlineIo<tokio::net::TcpStream>;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok(permit) = self.slots.clone().acquire_owned().await else {
                continue;
            };
            match self.listener.accept().await {
                Ok((stream, address)) => {
                    return (
                        DeadlineIo {
                            stream,
                            deadline: Box::pin(tokio::time::sleep(Duration::from_secs(60))),
                            _permit: permit,
                        },
                        address,
                    );
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

#[derive(Clone)]
struct Credentials(String);
#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}
impl ApiError {
    fn input() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INPUT_INVALID",
            "Request parameters are invalid",
        )
    }
    fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }
    fn from_backend(error: Error) -> Self {
        let (status, code, message) = match error.code.as_str() {
            "UNAUTHENTICATED"
            | "AUTHENTICATION_REQUIRED"
            | "TOKEN_INVALID"
            | "TOKEN_EXPIRED"
            | "TOKEN_REVOKED" => (401, "UNAUTHENTICATED", "A valid bearer token is required"),
            "FORBIDDEN" | "NOT_AUTHORIZED" | "CAPABILITY_REQUIRED" | "NAMESPACE_FORBIDDEN" => {
                (403, "FORBIDDEN", "This token does not permit the request")
            }
            "NOT_FOUND"
            | "WORKLOAD_NOT_FOUND"
            | "EXECUTION_NOT_FOUND"
            | "OPERATION_NOT_FOUND"
            | "LOG_UNAVAILABLE" => (404, "NOT_FOUND", "Requested resource is unavailable"),
            "FILE_OWNED" | "SOURCE_CONFLICT" | "FILE_OWNERSHIP_CONFLICT" => (
                409,
                "FILE_OWNED",
                "File-owned definitions must be changed through file configuration",
            ),
            "REVISION_CONFLICT" | "STALE_REVISION" => (
                409,
                "REVISION_CONFLICT",
                "Configuration revision has changed; refresh and review the plan",
            ),
            "IDEMPOTENCY_CONFLICT" => (
                409,
                "IDEMPOTENCY_CONFLICT",
                "Idempotency key was already used for a different request",
            ),
            "CONFLICT"
            | "RESTORE_REQUIRED"
            | "STATE_CONFLICT"
            | "RECONCILIATION_REQUIRED"
            | "ALERTS_DISABLED" => (
                409,
                "STATE_CONFLICT",
                "Current state does not permit this operation",
            ),
            "EVENT_CURSOR_EXPIRED" | "EVENTS_GAP" | "CURSOR_EXPIRED" => (
                409,
                "EVENT_CURSOR_EXPIRED",
                "Event history has expired; refresh current state",
            ),
            "LOG_CURSOR_EXPIRED" => (
                409,
                "LOG_CURSOR_EXPIRED",
                "Earlier log output has expired; inspect the retained range before continuing",
            ),
            "LOG_POLICY_MOVED" => (
                422,
                "LOG_POLICY_MOVED",
                "Installation log limits are managed through the global log storage policy",
            ),
            "INPUT_INVALID" | "CONFIG_INVALID" | "CURSOR_INVALID" => {
                (400, "INPUT_INVALID", "Request parameters are invalid")
            }
            "POLICY_INVALID" | "POLICY_REJECTED" | "EXECUTOR_NOT_ALLOWED" => (
                422,
                "POLICY_REJECTED",
                "Workload does not satisfy configured policy",
            ),
            "RATE_LIMITED" | "ALERT_QUEUE_FULL" => {
                (429, "RATE_LIMITED", "Request limit reached; retry later")
            }
            "PERSISTENCE_UNAVAILABLE" | "STORAGE_UNAVAILABLE" => {
                (507, "STORAGE_UNAVAILABLE", "State storage is unavailable")
            }
            "LOCAL_TRANSPORT_UNAVAILABLE" | "EXECUTOR_UNAVAILABLE" | "WORKER_UNAVAILABLE" => (
                503,
                "SERVICE_UNAVAILABLE",
                "The local service is unavailable",
            ),
            _ => (500, "INTERNAL_ERROR", "The request could not be completed"),
        };
        Self::new(
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code,
            message,
        )
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = format!("req_{}", uuid::Uuid::new_v4());
        let mut response = (
            self.status,
            Json(json!({"error":{"code":self.code,"message":self.message},"requestId":request_id})),
        )
            .into_response();
        if let Ok(value) = HeaderValue::from_str(&request_id) {
            response.headers_mut().insert("x-request-id", value);
        }
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"cronharbor\""),
            );
        }
        if self.status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        }
        response
    }
}
async fn method_not_allowed() -> Response {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "HTTP method is not allowed for this route",
    )
    .into_response()
}
async fn not_found() -> Response {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "NOT_FOUND",
        "Requested route is unavailable",
    )
    .into_response()
}
fn bearer(headers: &HeaderMap) -> std::result::Result<String, ApiError> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let token = values
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty() && s.len() <= 512 && s.bytes().all(|b| b.is_ascii_graphic()));
    if values.next().is_some() {
        return Err(ApiError::input());
    }
    token.map(str::to_owned).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "A valid bearer token is required",
        )
    })
}
fn rate(state: &AppState, token: &str, expensive: bool) -> std::result::Result<(), ApiError> {
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    let mut rates = state.rates.lock().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "The local service is unavailable",
        )
    })?;
    let now = Instant::now();
    if rates.len() >= 1_024 && !rates.contains_key(&digest) {
        rates.retain(|_, entry| now.duration_since(entry.0) < Duration::from_secs(60));
        if rates.len() >= 1_024 {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Request limit reached; retry later",
            ));
        }
    }
    let entry = rates.entry(digest).or_insert((now, 0, 0));
    if now.duration_since(entry.0) >= Duration::from_secs(60) {
        *entry = (now, 0, 0);
    }
    entry.1 += 1;
    if expensive {
        entry.2 += 1;
    }
    if entry.1 > 240 || entry.2 > 30 {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Request limit reached; retry later",
        ));
    }
    Ok(())
}
async fn admit(
    state: &AppState,
    token: &str,
    expensive: bool,
) -> std::result::Result<(), ApiError> {
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    let known = state
        .rates
        .lock()
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                "The local service is unavailable",
            )
        })?
        .contains_key(&digest);
    if !known {
        {
            let mut window = state.verifications.lock().map_err(|_| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "SERVICE_UNAVAILABLE",
                    "The local service is unavailable",
                )
            })?;
            if window.0.elapsed() >= Duration::from_secs(60) {
                *window = (Instant::now(), 0);
            }
            if window.1 >= 240 {
                return Err(ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "RATE_LIMITED",
                    "Authentication attempt limit reached; retry later",
                ));
            }
            window.1 += 1;
        }
        let identity = rpc(state, token.to_owned(), AppCommand::Identity).await?;
        serde_json::from_value::<crate::contracts::PrincipalDto>(identity).map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                "The local service returned an invalid identity",
            )
        })?;
    }
    rate(state, token, expensive)
}
fn origin_allowed(state: &AppState, headers: &HeaderMap, origin: &str) -> bool {
    if !valid_origin(origin) {
        return false;
    }
    if state.origins.iter().any(|candidate| candidate == origin) {
        return true;
    }
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    matches!(uri.scheme_str(), Some("http" | "https"))
        && uri.path_and_query().is_none_or(|p| p.as_str() == "/")
        && uri.authority().is_some_and(|authority| {
            headers.get(header::HOST).and_then(|h| h.to_str().ok()) == Some(authority.as_str())
        })
}
fn valid_origin(origin: &str) -> bool {
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    matches!(uri.scheme_str(), Some("http" | "https"))
        && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
        && uri.path_and_query().is_none_or(|p| p.as_str() == "/")
}
async fn guard(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let allowed_origin = request.headers().get_all(header::ORIGIN).iter().count() == 1
        && origin.as_ref().is_some_and(|o| {
            o.to_str()
                .is_ok_and(|v| origin_allowed(&state, request.headers(), v))
        });
    let result = async {
        if state.csp.is_err() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "API_CONFIG_INVALID",
                "Dashboard API configuration is invalid",
            ));
        }
        let _permit = state.requests.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Request limit reached; retry later",
            )
        })?;
        if [
            "origin",
            "host",
            "content-type",
            "last-event-id",
            "access-control-request-method",
            "access-control-request-headers",
        ]
        .iter()
        .any(|name| request.headers().get_all(*name).iter().count() > 1)
            || request.headers().len() > 64
            || request
                .headers()
                .iter()
                .map(|(n, v)| n.as_str().len() + v.len())
                .sum::<usize>()
                > MAX_HEADERS
            || request.uri().query().is_some_and(|q| q.len() > MAX_QUERY)
            || request.uri().path().len() > 1_024
        {
            return Err(ApiError::input());
        }
        if let Some(origin) = &origin
            && !origin
                .to_str()
                .is_ok_and(|o| origin_allowed(&state, request.headers(), o))
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "ORIGIN_FORBIDDEN",
                "Browser origin is not allowed",
            ));
        }
        if request.method() == Method::OPTIONS {
            let method = request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_METHOD)
                .and_then(|v| v.to_str().ok());
            if origin.is_none() || !matches!(method, Some("GET" | "POST")) {
                return Err(ApiError::input());
            }
            if request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|value| {
                    value.split(',').any(|h| {
                        !matches!(
                            h.trim().to_ascii_lowercase().as_str(),
                            "authorization" | "content-type" | "idempotency-key" | "last-event-id"
                        )
                    })
                })
            {
                return Err(ApiError::input());
            }
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        if request.uri().path().starts_with("/api/")
            && request.uri().path() != "/api/v1/health/live"
        {
            let token = bearer(request.headers())?;
            request.extensions_mut().insert(Credentials(token));
        }
        tokio::time::timeout(Duration::from_secs(12), next.run(request))
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::REQUEST_TIMEOUT,
                    "REQUEST_TIMEOUT",
                    "Request time limit exceeded",
                )
            })
    }
    .await;
    let mut response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    if !response.headers().contains_key("x-request-id")
        && let Ok(value) = HeaderValue::from_str(&format!("req_{}", uuid::Uuid::new_v4()))
    {
        response.headers_mut().insert("x-request-id", value);
    }
    if let Some(origin) = origin.filter(|_| allowed_origin) {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type, Idempotency-Key, Last-Event-ID"),
        );
    }
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
    ] {
        response.headers_mut().insert(
            header::HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        state
            .csp
            .as_ref()
            .as_ref()
            .map_or_else(|_| HeaderValue::from_static(DEFAULT_CSP), Clone::clone),
    );
    response
}
async fn rpc(
    state: &AppState,
    token: String,
    command: AppCommand,
) -> std::result::Result<Value, ApiError> {
    let permit = state.rpc.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Request limit reached; retry later",
        )
    })?;
    let directory = state.directory.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        server::client(
            &directory,
            server::Request::Execute {
                command: Box::new(command),
                token: Some(token),
            },
        )
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "The request could not be completed",
        )
    })?
    .map_err(ApiError::from_backend)
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    namespace: Option<String>,
    workload_id: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewQuery {
    count: Option<u32>,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogsQuery {
    offset: Option<u64>,
    max_bytes: Option<u32>,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    definition: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Apply {
    definition: String,
    expected_revision: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pause {
    paused: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Desired {
    desired_state: String,
    expected_revision: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    expected_revision: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reconcile {
    state: String,
    evidence: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogStorageUpdate {
    policy: LogStoragePolicyInput,
    expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateToken {
    role: Role,
    namespaces: Vec<String>,
    capabilities: Vec<String>,
    ttl_seconds: u64,
}
fn query<T: DeserializeOwned>(request: &Request) -> std::result::Result<T, ApiError> {
    Query::<T>::try_from_uri(request.uri())
        .map(|q| q.0)
        .map_err(|_| ApiError::input())
}
fn bounded(value: &str, max: usize) -> std::result::Result<(), ApiError> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        Err(ApiError::input())
    } else {
        Ok(())
    }
}
fn list(request: &Request, kind: &str) -> std::result::Result<ListQuery, ApiError> {
    let q: ListQuery = query(request)?;
    if !matches!(q.limit.unwrap_or(50), 1..=100)
        || q.namespace
            .as_ref()
            .is_some_and(|n| bounded(n, 128).is_err())
        || q.workload_id
            .as_ref()
            .is_some_and(|n| bounded(n, 256).is_err())
        || q.cursor.as_ref().is_some_and(|n| bounded(n, 512).is_err())
        || (kind != "workloads" && q.namespace.is_some())
        || (kind != "executions" && q.workload_id.is_some())
    {
        return Err(ApiError::input());
    }
    Ok(q)
}
async fn body<T: DeserializeOwned>(request: Request) -> std::result::Result<T, ApiError> {
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.split(';').next() != Some("application/json"))
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "CONTENT_TYPE_INVALID",
            "Use application/json",
        ));
    }
    let bytes = tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), MAX_BODY),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "REQUEST_TIMEOUT",
            "Request time limit exceeded",
        )
    })?
    .map_err(|_| {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "Request body exceeds the size limit",
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::input())
}
fn idempotency(headers: &HeaderMap) -> std::result::Result<String, ApiError> {
    if headers.get_all("idempotency-key").iter().count() != 1 {
        return Err(ApiError::input());
    }
    let value = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(ApiError::input)?;
    bounded(value, 128)?;
    Ok(value.into())
}
async fn dispatch(
    State(state): State<AppState>,
    matched: MatchedPath,
    path: Option<Path<HashMap<String, String>>>,
    request: Request,
) -> std::result::Result<Response, ApiError> {
    let token = request
        .extensions()
        .get::<Credentials>()
        .ok_or_else(ApiError::input)?
        .0
        .clone();
    let id = path
        .and_then(|p| p.0.get("id").cloned())
        .unwrap_or_default();
    if !id.is_empty() {
        bounded(&id, 256)?;
    }
    let route = matched
        .as_str()
        .strip_prefix("/api/v1")
        .unwrap_or(matched.as_str());
    let mut accepted = false;
    let mut admitted = false;
    let expensive = route.ends_with("/logs")
        || route.ends_with("/logs/info")
        || route.starts_with("/diagnostics/");
    let command = match (request.method().clone(), route) {
        (Method::GET, "/status" | "/instance/status" | "/health/ready") => {
            let _: Empty = query(&request)?;
            AppCommand::Status
        }
        (Method::GET, "/session") => {
            let _: Empty = query(&request)?;
            AppCommand::Identity
        }
        (Method::GET, "/log-storage") => {
            let _: Empty = query(&request)?;
            AppCommand::LogStorage
        }
        (Method::GET, "/alerts/status") => {
            let _: Empty = query(&request)?;
            AppCommand::AlertStatus
        }
        (Method::GET, "/alerts/deliveries") => {
            let q = list(&request, "alerts")?;
            AppCommand::AlertDeliveries {
                cursor: q.cursor,
                limit: q.limit.unwrap_or(50),
            }
        }
        (Method::POST, "/alerts/deliveries/{id}/retry") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let _: Empty = body(request).await?;
            accepted = true;
            AppCommand::RetryAlert {
                id,
                idempotency_key: key,
            }
        }
        (Method::POST, "/log-storage") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let update: LogStorageUpdate = body(request).await?;
            accepted = true;
            AppCommand::SetLogStorage {
                policy: update.policy,
                expected_revision: update.expected_revision,
                idempotency_key: key,
            }
        }
        (Method::GET, "/workloads") => {
            let q = list(&request, "workloads")?;
            AppCommand::Workloads {
                namespace: q.namespace,
                cursor: q.cursor,
                limit: q.limit.unwrap_or(50),
            }
        }
        (Method::GET, "/workloads/{id}") => {
            let _: Empty = query(&request)?;
            AppCommand::Workload { id }
        }
        (Method::GET, "/executions") => {
            let q = list(&request, "executions")?;
            AppCommand::Executions {
                workload_id: q.workload_id,
                cursor: q.cursor,
                limit: q.limit.unwrap_or(50),
            }
        }
        (Method::GET, "/executions/{id}") => {
            let _: Empty = query(&request)?;
            AppCommand::Execution { id }
        }
        (Method::GET, "/executions/{id}/logs" | "/executions/{id}/logs/info") => {
            let info = route.ends_with("/info");
            let q: LogsQuery = if info {
                let _: Empty = query(&request)?;
                LogsQuery::default()
            } else {
                query(&request)?
            };
            let max_bytes = q.max_bytes.unwrap_or(16_384);
            if !(1..=65_536).contains(&max_bytes) {
                return Err(ApiError::input());
            }
            admit(&state, &token, true).await?;
            admitted = true;
            let execution = rpc(&state, token.clone(), AppCommand::Execution { id }).await?;
            let attempt_id = execution
                .get("attempt_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL_ERROR",
                        "Execution response is invalid",
                    )
                })?
                .to_owned();
            if info {
                AppCommand::LogInfo { attempt_id }
            } else if let Some(offset) = q.offset {
                AppCommand::Logs {
                    attempt_id,
                    offset,
                    max_bytes,
                }
            } else {
                AppCommand::LatestLogs {
                    attempt_id,
                    max_bytes,
                }
            }
        }
        (Method::GET, "/schedules/{id}/preview") => {
            let q: PreviewQuery = query(&request)?;
            let count = q.count.unwrap_or(10);
            if !(1..=100).contains(&count) {
                return Err(ApiError::input());
            }
            AppCommand::Preview { id, count }
        }
        (Method::GET, "/operations/{id}") => {
            let _: Empty = query(&request)?;
            AppCommand::Operation { id }
        }
        (Method::GET, "/diagnostics/{id}") => {
            let _: Empty = query(&request)?;
            AppCommand::Diagnostics { execution_id: id }
        }
        (Method::GET, "/tokens") => {
            let q = list(&request, "tokens")?;
            AppCommand::Tokens {
                cursor: q.cursor,
                limit: q.limit.unwrap_or(50),
            }
        }
        (Method::GET, "/audit") => {
            let q = list(&request, "audit")?;
            AppCommand::Audit {
                cursor: q.cursor,
                limit: q.limit.unwrap_or(50),
            }
        }
        (Method::POST, "/workloads/validate") => {
            let _: Empty = query(&request)?;
            let b: Definition = body(request).await?;
            AppCommand::Validate {
                definition: b.definition,
            }
        }
        (Method::POST, "/config/plan") => {
            let _: Empty = query(&request)?;
            let b: Definition = body(request).await?;
            AppCommand::Plan {
                definition: b.definition,
            }
        }
        (Method::POST, "/config/apply") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let b: Apply = body(request).await?;
            accepted = true;
            AppCommand::Apply {
                definition: b.definition,
                expected_revision: b.expected_revision,
                source_kind: "API".into(),
                idempotency_key: key,
            }
        }
        (Method::POST, "/services/{id}/restart") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let b: Revision = body(request).await?;
            if let Some(revision) = &b.expected_revision {
                bounded(revision, 256)?;
            }
            accepted = true;
            AppCommand::ServiceRestart {
                id,
                expected_revision: b.expected_revision,
                idempotency_key: key,
            }
        }
        (Method::POST, "/jobs/{id}/run" | "/executions/{id}/cancel") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let _: Empty = body(request).await?;
            accepted = true;
            match route {
                "/jobs/{id}/run" => AppCommand::Run {
                    id,
                    idempotency_key: key,
                },
                _ => AppCommand::Cancel {
                    execution_id: id,
                    idempotency_key: key,
                },
            }
        }
        (Method::POST, "/workloads/{id}/pause") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let b: Pause = body(request).await?;
            accepted = true;
            AppCommand::Pause {
                id,
                paused: b.paused,
                idempotency_key: key,
            }
        }
        (Method::POST, "/services/{id}/desired-state") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let b: Desired = body(request).await?;
            bounded(&b.desired_state, 32)?;
            if let Some(revision) = &b.expected_revision {
                bounded(revision, 256)?;
            }
            accepted = true;
            AppCommand::ServiceDesired {
                id,
                desired_state: b.desired_state,
                expected_revision: b.expected_revision,
                idempotency_key: key,
            }
        }
        (Method::POST, "/executions/{id}/reconcile") => {
            let _: Empty = query(&request)?;
            let key = idempotency(request.headers())?;
            let b: Reconcile = body(request).await?;
            bounded(&b.state, 32)?;
            if b.evidence.trim().is_empty() || b.evidence.len() > 4_096 || b.evidence.contains('\0')
            {
                return Err(ApiError::input());
            }
            accepted = true;
            AppCommand::Reconcile {
                execution_id: id,
                state: b.state,
                evidence: b.evidence,
                idempotency_key: key,
            }
        }
        (Method::POST, "/tokens" | "/tokens/create") => {
            let _: Empty = query(&request)?;
            let b: CreateToken = body(request).await?;
            if b.namespaces.len() > 100
                || b.capabilities.len() > 64
                || b.ttl_seconds == 0
                || b.ttl_seconds > 31_536_000
                || b.namespaces
                    .iter()
                    .chain(b.capabilities.iter())
                    .any(|v| bounded(v, 128).is_err())
            {
                return Err(ApiError::input());
            }
            AppCommand::CreateToken {
                role: b.role,
                namespaces: b.namespaces,
                capabilities: b.capabilities,
                ttl_seconds: b.ttl_seconds,
            }
        }
        (Method::POST, "/tokens/{id}/revoke") => {
            let _: Empty = query(&request)?;
            let _: Empty = body(request).await?;
            AppCommand::RevokeToken { id }
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Requested route is unavailable",
            ));
        }
    };
    if !admitted {
        admit(&state, &token, expensive).await?;
    }
    let value = rpc(&state, token, command).await?;
    if !accepted {
        return Ok(Json(value).into_response());
    }
    let operation: OperationDto = serde_json::from_value(value).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Operation response is invalid",
        )
    })?;
    if operation.id.is_empty()
        || operation.id.len() > 256
        || !operation
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Operation response is invalid",
        ));
    }
    let location =
        HeaderValue::from_str(&format!("/api/v1/operations/{}", operation.id)).map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Operation response is invalid",
            )
        })?;
    let mut response = (StatusCode::ACCEPTED, Json(operation)).into_response();
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsQuery {
    after: Option<i64>,
}
async fn event_page(
    state: &AppState,
    token: String,
    after: i64,
) -> std::result::Result<Page<EventDto>, ApiError> {
    serde_json::from_value(rpc(state, token, AppCommand::Events { after, limit: 100 }).await?)
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Event response is invalid",
            )
        })
}
fn sse_event(event: EventDto) -> std::result::Result<Event, ApiError> {
    Event::default()
        .event("change")
        .id(event.seq.to_string())
        .json_data(event)
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Event response is invalid",
            )
        })
}
fn stream_error(error: ApiError) -> Event {
    let name = if error.code == "EVENT_CURSOR_EXPIRED" {
        "resync"
    } else if matches!(
        error.status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        "auth_expired"
    } else {
        "stream_error"
    };
    Event::default()
        .event(name)
        .data(json!({"code":error.code}).to_string())
}
async fn events(
    State(state): State<AppState>,
    request: Request,
) -> std::result::Result<Response, ApiError> {
    let token = request
        .extensions()
        .get::<Credentials>()
        .ok_or_else(ApiError::input)?
        .0
        .clone();
    let q: EventsQuery = query(&request)?;
    let header = request
        .headers()
        .get("last-event-id")
        .map(|v| {
            v.to_str()
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .ok_or_else(ApiError::input)
        })
        .transpose()?;
    let after = header.or(q.after).unwrap_or(0);
    if after < 0 {
        return Err(ApiError::input());
    }
    admit(&state, &token, false).await?;
    let permit = state.streams.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Event stream limit reached; retry later",
        )
    })?;
    let first = event_page(&state, token.clone(), after).await;
    if let Err(error) = &first
        && error.code != "EVENT_CURSOR_EXPIRED"
    {
        return Err(first.err().unwrap_or_else(ApiError::input));
    }
    let (sender, receiver) = mpsc::channel::<std::result::Result<Event, Infallible>>(1);
    tokio::spawn(async move {
        let _permit = permit;
        let mut cursor = after;
        let mut page = first;
        let mut validated_at = Instant::now();
        loop {
            match page {
                Ok(batch) => {
                    for event in batch.items {
                        if validated_at.elapsed() >= Duration::from_millis(500) {
                            if let Err(error) =
                                rpc(&state, token.clone(), AppCommand::Identity).await
                            {
                                let _ = sender
                                    .send_timeout(
                                        Ok(stream_error(error)),
                                        Duration::from_millis(750),
                                    )
                                    .await;
                                return;
                            }
                            validated_at = Instant::now();
                        }
                        if event.seq <= cursor {
                            continue;
                        }
                        cursor = event.seq;
                        let Ok(event) = sse_event(event) else {
                            return;
                        };
                        if sender
                            .send_timeout(Ok(event), Duration::from_millis(750))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let event = stream_error(error);
                    let _ = sender
                        .send_timeout(Ok(event), Duration::from_millis(750))
                        .await;
                    return;
                }
            }
            if sender.is_closed() || state.shutdown.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            page = event_page(&state, token.clone(), cursor).await;
            validated_at = Instant::now();
        }
    });
    Ok(Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[test]
    fn invalid_bearers_are_throttled_before_any_route_daemon_work() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = crate::store::Store::open(directory.path()).unwrap();
        let created = crate::application::execute(
            &mut store,
            AppCommand::CreateToken {
                role: Role::Viewer,
                namespaces: vec!["*".into()],
                capabilities: vec![],
                ttl_seconds: 3600,
            },
            None,
        )
        .unwrap();
        let credential = created["token"].as_str().unwrap().to_owned();
        let token_id = created["metadata"]["id"].as_str().unwrap().to_owned();
        let fresh_credential = crate::application::execute(
            &mut store,
            AppCommand::CreateToken {
                role: Role::Viewer,
                namespaces: vec!["*".into()],
                capabilities: vec![],
                ttl_seconds: 3600,
            },
            None,
        )
        .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(store);
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let path = directory.path().to_owned();
        let owner = std::thread::spawn(move || server::run_with_shutdown(&path, &[], true, flag));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !directory.path().join("control.sock").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let state = AppState::new(
            directory.path().into(),
            vec![],
            Arc::new(AtomicBool::new(false)),
        );
        let app = build_router(state.clone(), None);
        let (statuses, expired_status) = tokio::runtime::Runtime::new().unwrap().block_on(async {
            let send = |uri: &str, token: &str| {
                let request = axum::http::Request::builder()
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap();
                app.clone().oneshot(request)
            };
            assert_eq!(
                send("/api/v1/status", &credential).await.unwrap().status(),
                StatusCode::OK
            );
            let mut statuses = Vec::new();
            for index in 0..300 {
                let uri = [
                    "/api/v1/status",
                    "/api/v1/executions/not-an-execution/logs",
                    "/api/v1/events",
                ][index % 3];
                let invalid = if index % 2 == 0 {
                    "same-invalid-token".into()
                } else {
                    format!("rotating-invalid-token-{index}")
                };
                statuses.push(send(uri, &invalid).await.unwrap().status());
            }
            for uri in [
                "/api/v1/status",
                "/api/v1/executions/not-an-execution/logs",
                "/api/v1/events",
            ] {
                assert_eq!(
                    send(uri, "invalid-after-budget-exhaustion")
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::TOO_MANY_REQUESTS
                );
            }
            assert_eq!(
                send("/api/v1/status", &fresh_credential)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::TOO_MANY_REQUESTS
            );
            assert_eq!(
                send("/api/v1/status", &credential).await.unwrap().status(),
                StatusCode::OK
            );
            let digest = format!("{:x}", Sha256::digest(credential.as_bytes()));
            state.rates.lock().unwrap().get_mut(&digest).unwrap().0 =
                Instant::now() - Duration::from_secs(60);
            let expired_status = send("/api/v1/status", &credential).await.unwrap().status();
            state.rates.lock().unwrap().get_mut(&digest).unwrap().0 = Instant::now();
            server::client(
                directory.path(),
                server::Request::Execute {
                    command: Box::new(AppCommand::RevokeToken { id: token_id }),
                    token: None,
                },
            )
            .unwrap();
            assert_eq!(
                send("/api/v1/status", &credential).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
            state.verifications.lock().unwrap().0 = Instant::now() - Duration::from_secs(60);
            assert_eq!(
                send("/api/v1/status", &fresh_credential)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
            (statuses, expired_status)
        });
        shutdown.store(true, Ordering::Relaxed);
        owner.join().unwrap().unwrap();
        assert_eq!(
            expired_status,
            StatusCode::OK,
            "authenticated windows must reset without rejoining the unknown-verification budget"
        );
        assert!(
            statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
            "unknown verification must be throttled"
        );
        assert!(
            statuses
                .iter()
                .all(|s| matches!(*s, StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS))
        );
        assert_eq!(state.rates.lock().unwrap().len(), 2);
        // A denied log request must not perform its preliminary execution lookup.
        let database = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
        let rejects: i64 = database
            .query_row(
                "SELECT count(*) FROM audit_events WHERE action='auth.reject'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            rejects <= 30,
            "rejection writes must be coalesced; got {rejects}"
        );
    }
    #[test]
    fn unverified_bearers_cannot_fill_authenticated_rate_capacity() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = crate::store::Store::open(directory.path()).unwrap();
        let credential = crate::application::execute(
            &mut store,
            AppCommand::CreateToken {
                role: Role::Viewer,
                namespaces: vec!["*".into()],
                capabilities: vec![],
                ttl_seconds: 3600,
            },
            None,
        )
        .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(store);
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let path = directory.path().to_owned();
        let owner = std::thread::spawn(move || server::run_with_shutdown(&path, &[], false, flag));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !directory.path().join("control.sock").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let state = AppState::new(
            directory.path().to_owned(),
            vec![],
            Arc::new(AtomicBool::new(false)),
        );
        let app = build_router(state.clone(), None);
        let outcomes = tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut statuses = Vec::new();
            for index in 0..1024 {
                let request = axum::http::Request::builder()
                    .uri("/api/v1/nonexistent")
                    .header("authorization", format!("Bearer invented-{index}"))
                    .body(axum::body::Body::empty())
                    .unwrap();
                statuses.push(app.clone().oneshot(request).await.unwrap().status());
            }
            for token in ["invalid-a", "invalid-b", &credential] {
                let request = axum::http::Request::builder()
                    .uri("/api/v1/status")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap();
                statuses.push(app.clone().oneshot(request).await.unwrap().status());
            }
            statuses
        });
        shutdown.store(true, Ordering::Relaxed);
        owner.join().unwrap().unwrap();
        assert!(
            outcomes[..1024]
                .iter()
                .all(|status| *status == StatusCode::NOT_FOUND)
        );
        assert_eq!(
            outcomes[1024..],
            [
                StatusCode::UNAUTHORIZED,
                StatusCode::UNAUTHORIZED,
                StatusCode::OK
            ]
        );
        assert_eq!(state.rates.lock().unwrap().len(), 1);
    }
    #[test]
    fn verified_rate_windows_reset_without_unbounded_hash_retention() {
        let state = AppState::new(PathBuf::new(), vec![], Arc::new(AtomicBool::new(false)));
        for index in 0..1024 {
            assert!(rate(&state, &format!("verified-{index}"), false).is_ok());
        }
        assert_eq!(
            rate(&state, "new-verified", false).unwrap_err().status,
            StatusCode::TOO_MANY_REQUESTS
        );
        for entry in state.rates.lock().unwrap().values_mut() {
            entry.0 = Instant::now() - Duration::from_secs(60);
        }
        assert!(rate(&state, "verified-0", false).is_ok());
        assert_eq!(state.rates.lock().unwrap().len(), 1024);
        assert!(rate(&state, "new-verified", false).is_ok());
        assert_eq!(state.rates.lock().unwrap().len(), 2);
    }
    #[test]
    fn socket_deadline_bounds_a_client_that_never_sends_headers() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (stream, _peer) = tokio::io::duplex(128);
            let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
            let mut stream = DeadlineIo {
                stream,
                deadline: Box::pin(tokio::time::sleep(Duration::from_millis(1))),
                _permit: permit,
            };
            let mut bytes = [0; 1];
            let result = tokio::time::timeout(
                Duration::from_millis(200),
                tokio::io::AsyncReadExt::read(&mut stream, &mut bytes),
            )
            .await
            .unwrap();
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        });
    }
    #[test]
    fn expensive_queries_are_rate_limited_without_storing_credentials() {
        let state = AppState::new(PathBuf::new(), vec![], Arc::new(AtomicBool::new(false)));
        for _ in 0..30 {
            assert!(rate(&state, "sensitive-token", true).is_ok());
        }
        assert_eq!(
            rate(&state, "sensitive-token", true).unwrap_err().status,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert!(
            state
                .rates
                .lock()
                .unwrap()
                .keys()
                .all(|key| key != "sensitive-token")
        );
        assert!(!valid_origin("*"));
        assert!(!valid_origin("null"));
        assert!(!valid_origin("https://user@host"));
        assert!(!valid_origin("https://example.com/path"));
        assert!(valid_origin("https://example.com"));
    }
}
