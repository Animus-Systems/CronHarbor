#![cfg(feature = "api")]
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use cronharbor::api;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    thread,
};
use tower::ServiceExt;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}
fn accept_rpc(socket: &UnixListener) -> (UnixStream, Value) {
    let read = || {
        let (stream, _) = socket.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        (stream, request)
    };
    let (mut stream, request) = read();
    if request["app_command"]["action"] == "identity" {
        writeln!(stream, "{}", json!({"data":{"actor_id":"fixture","role":"admin","namespaces":["*"],"capabilities":["*"]},"error":null})).unwrap();
        read()
    } else {
        (stream, request)
    }
}
fn fake(reply: Value) -> (tempfile::TempDir, thread::JoinHandle<Value>) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let socket = UnixListener::bind(dir.path().join("control.sock")).unwrap();
    let thread = thread::spawn(move || {
        let (mut stream, request) = accept_rpc(&socket);
        writeln!(stream, "{}", reply).unwrap();
        request
    });
    (dir, thread)
}
fn request(method: &str, path: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request.body(Body::from(body.to_string())).unwrap()
}
async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap()).unwrap()
}
#[test]
fn operational_routes_require_bearer_and_liveness_is_minimal() {
    runtime().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let app = api::router(dir.path().into(), None, vec![]);
        let response = app
            .clone()
            .oneshot(request("GET", "/api/v1/workloads", None, json!(null)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "UNAUTHENTICATED");
        assert!(value["requestId"].is_string());
        let response = app
            .oneshot(request("GET", "/api/v1/health/live", None, json!(null)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await, json!({"live":true}));
    });
}
#[test]
fn forwards_bounded_pagination_and_token_without_local_owner_fallback() {
    runtime().block_on(async {
        let (dir, server) = fake(json!({"data":{"items":[],"next_cursor":"next"},"error":null}));
        let response = api::router(dir.path().into(), None, vec![])
            .oneshot(request(
                "GET",
                "/api/v1/workloads?namespace=prod&limit=7&cursor=abc",
                Some("scoped"),
                json!(null),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["next_cursor"], "next");
        let wire = server.join().unwrap();
        assert_eq!(wire["command"], "execute");
        assert_eq!(wire["token"], "scoped");
        assert_eq!(wire["app_command"]["action"], "workloads");
        assert!(wire.to_string().contains("\"limit\":7"));
    });
}
#[test]
fn mutations_require_idempotency_and_strict_json() {
    runtime().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let app = api::router(dir.path().into(), None, vec![]);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/jobs/job/run",
                Some("scoped"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/config/plan",
                Some("scoped"),
                json!({"definition":"x","unknown":true}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .oneshot(request(
                "GET",
                "/api/v1/workloads?limit=100000",
                Some("scoped"),
                json!(null),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    });
}
#[test]
fn backend_policy_is_preserved_and_internal_errors_are_hidden() {
    runtime().block_on(async {
    for (code,status) in [("UNAUTHENTICATED",401),("FORBIDDEN",403),("FILE_OWNED",409),("REVISION_CONFLICT",409),("INTERNAL_DETAIL",500)] {
        let (dir,server)=fake(json!({"data":null,"error":{"code":code,"message":"secret /etc/sensitive token=abc"}}));
        let response=api::router(dir.path().into(),None,vec![]).oneshot(request("GET","/api/v1/status",Some("scoped"),json!(null))).await.unwrap();assert_eq!(response.status().as_u16(),status);
        assert!(!json_body(response).await.to_string().contains("sensitive"));server.join().unwrap();
    }
});
}
#[test]
fn denies_unconfigured_origins_and_caps_body() {
    runtime().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let app = api::router(
            dir.path().into(),
            None,
            vec!["https://console.example".into()],
        );
        let req = Request::builder()
            .uri("/api/v1/status")
            .header("origin", "https://evil.example")
            .header("authorization", "Bearer scoped")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-origin")
        );
        let req = Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/status")
            .header("origin", "https://console.example")
            .header("access-control-request-method", "GET")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "https://console.example"
        );
        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/workloads/validate",
                Some("scoped"),
                json!({"definition":"x".repeat(100_000)}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    });
}
#[test]
fn apply_is_api_owned_and_returns_durable_operation() {
    runtime().block_on(async {
    let operation=json!({"id":"op_1","state":"ACCEPTED","target_id":"cfg","kind":"apply","created_at_us":1,"error_code":null});
    let (dir,server)=fake(json!({"data":operation,"error":null}));
    let mut req=request("POST","/api/v1/config/apply",Some("editor"),json!({"definition":"schema_version: 1","expected_revision":"r1"}));req.headers_mut().insert("idempotency-key","request-1".parse().unwrap());
    let response=api::router(dir.path().into(),None,vec![]).oneshot(req).await.unwrap();assert_eq!(response.status(),StatusCode::ACCEPTED);assert_eq!(response.headers()["location"],"/api/v1/operations/op_1");assert_eq!(json_body(response).await["state"],"ACCEPTED");
    let wire=server.join().unwrap();assert_eq!(wire["app_command"]["source_kind"],"API");assert_eq!(wire["app_command"]["idempotency_key"],"request-1");assert_eq!(wire["app_command"]["expected_revision"],"r1");
});
}
#[test]
fn sse_replays_last_id_then_disconnects_on_revoke() {
    runtime().block_on(async {
    let dir=tempfile::tempdir_in("/tmp").unwrap();let socket=UnixListener::bind(dir.path().join("control.sock")).unwrap();
    let server=thread::spawn(move || {let mut requests=Vec::new();for reply in [json!({"data":{"items":[{"seq":42,"event_kind":"execution_finished","workload_id":"w","execution_id":"e","payload_json":"{}","created_at_us":1}],"next_cursor":null},"error":null}),json!({"data":null,"error":{"code":"TOKEN_INVALID","message":"revoked"}})] {
        let(mut stream,request)=accept_rpc(&socket);requests.push(request);writeln!(stream,"{reply}").unwrap();}requests});
    let mut req=request("GET","/api/v1/events",Some("observer"),json!(null));req.headers_mut().insert("last-event-id","41".parse().unwrap());
    let response=api::router(dir.path().into(),None,vec![]).oneshot(req).await.unwrap();assert_eq!(response.status(),StatusCode::OK);
    let bytes=tokio::time::timeout(std::time::Duration::from_secs(3),to_bytes(response.into_body(),10000)).await.unwrap().unwrap();let text=String::from_utf8(bytes.to_vec()).unwrap();assert!(text.contains("id: 42"));assert!(text.contains("event: change"));assert!(text.contains("event: auth_expired"));
    let requests=server.join().unwrap();assert_eq!(requests[0]["app_command"]["after"],41);assert_eq!(requests[1]["app_command"]["after"],42);assert!(requests.iter().all(|v|v["token"]=="observer"));
});
}
#[test]
fn sse_retention_gap_explicitly_requests_resync() {
    runtime().block_on(async {
        let (dir, server) =
            fake(json!({"data":null,"error":{"code":"EVENT_CURSOR_EXPIRED","message":"old"}}));
        let response = api::router(dir.path().into(), None, vec![])
            .oneshot(request(
                "GET",
                "/api/v1/events?after=2",
                Some("observer"),
                json!(null),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 10000).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("event: resync"));
        assert!(text.contains("EVENT_CURSOR_EXPIRED"));
        server.join().unwrap();
    });
}
#[test]
fn viewer_write_denial_and_revision_conflicts_remain_backend_decisions() {
    runtime().block_on(async {
        for (code, status) in [
            ("NOT_AUTHORIZED", StatusCode::FORBIDDEN),
            ("SOURCE_CONFLICT", StatusCode::CONFLICT),
            ("REVISION_CONFLICT", StatusCode::CONFLICT),
        ] {
            let (dir, server) =
                fake(json!({"data":null,"error":{"code":code,"message":"internal detail"}}));
            let mut req = request(
                "POST",
                "/api/v1/config/apply",
                Some("viewer-credential"),
                json!({"definition":"anything","expected_revision":"old"}),
            );
            req.headers_mut()
                .insert("idempotency-key", "key".parse().unwrap());
            let response = api::router(dir.path().into(), None, vec![])
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            let wire = server.join().unwrap();
            assert_eq!(wire["token"], "viewer-credential");
            assert_eq!(wire["app_command"]["action"], "apply");
        }
    });
}
#[test]
fn static_routes_do_not_expose_parent_files_or_replace_api_errors() {
    runtime().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let ui = dir.path().join("ui");
        std::fs::create_dir(&ui).unwrap();
        std::fs::write(ui.join("index.html"), "<h1>Dashboard</h1>").unwrap();
        std::fs::write(dir.path().join("secret"), "PRIVATE DATA").unwrap();
        let app = api::router(dir.path().into(), Some(ui), vec![]);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/%2e%2e/secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), 10000).await.unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE DATA"));
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/missing",
                Some("scoped"),
                json!(null),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["error"]["code"], "NOT_FOUND");
        let response = app
            .oneshot(request(
                "DELETE",
                "/api/v1/workloads",
                Some("scoped"),
                json!(null),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            json_body(response).await["error"]["code"],
            "METHOD_NOT_ALLOWED"
        );
    });
}

#[test]
fn static_host_connect_policy_uses_only_the_trusted_api_origin() {
    runtime().block_on(async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("index.html"), "dashboard").unwrap();
        std::fs::write(
            directory.path().join("config.json"),
            r#"{"apiBaseUrl":"https://scheduler.example.com:8443/api/v1"}"#,
        )
        .unwrap();
        let app = api::router(
            directory.path().into(),
            Some(directory.path().into()),
            vec![],
        );
        let response = app
            .oneshot(request("GET", "/", None, json!(null)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let policy = response.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert!(policy.contains("connect-src 'self' https://scheduler.example.com:8443"));
        assert!(!policy.contains("/api/v1"));
        assert!(!policy.contains("unsafe-inline"));
        assert!(policy.contains("object-src 'none'"));
        for address in [
            "http://remote.example/api",
            "javascript:alert(1)",
            "https://user:password@example.com/api",
            "https://example.com/api?token=secret",
            "https://example.com/api#fragment",
            "//remote.example/api",
        ] {
            std::fs::write(
                directory.path().join("config.json"),
                json!({"apiBaseUrl":address}).to_string(),
            )
            .unwrap();
            let app = api::router(
                directory.path().into(),
                Some(directory.path().into()),
                vec![],
            );
            assert_eq!(
                app.oneshot(request("GET", "/", None, json!(null)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{address}"
            );
        }
    });
}
#[test]
fn service_control_forwards_current_revision_for_cas() {
    runtime().block_on(async {
        for (route, body) in [
            (
                "/api/v1/services/service/desired-state",
                json!({"desired_state":"RUNNING","expected_revision":"revision-current"}),
            ),
            (
                "/api/v1/services/service/restart",
                json!({"expected_revision":"revision-current"}),
            ),
        ] {
            let (dir, server) =
                fake(json!({"data":{"id":"op_service","state":"ACCEPTED","target_id":"service","kind":"service_restart","created_at_us":1,"error_code":null},"error":null}));
            let mut req = request("POST", route, Some("operator"), body);
            req.headers_mut()
                .insert("idempotency-key", "service-change".parse().unwrap());
            let response = api::router(dir.path().into(), None, vec![])
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            let wire = server.join().unwrap();
            assert_eq!(wire["app_command"]["expected_revision"], "revision-current");
        }
    });
}
#[test]
fn malformed_daemon_acknowledgement_cannot_claim_an_accepted_operation() {
    runtime().block_on(async {
        let (dir, server) = fake(json!({"data":{"state":"ACCEPTED"},"error":null}));
        let mut req = request("POST", "/api/v1/jobs/job/run", Some("operator"), json!({}));
        req.headers_mut()
            .insert("idempotency-key", "request".parse().unwrap());
        let response = api::router(dir.path().into(), None, vec![])
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json_body(response).await["error"]["code"], "INTERNAL_ERROR");
        server.join().unwrap();
    });
}
#[test]
fn token_inventory_uses_bounded_pagination() {
    runtime().block_on(async {
        let (dir, server) =
            fake(json!({"data":{"items":[],"next_cursor":"token-next"},"error":null}));
        let response = api::router(dir.path().into(), None, vec![])
            .oneshot(request(
                "GET",
                "/api/v1/tokens?limit=3&cursor=token-current",
                Some("administrator"),
                json!(null),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["next_cursor"], "token-next");
        let wire = server.join().unwrap();
        assert_eq!(wire["app_command"]["action"], "tokens");
        assert_eq!(wire["app_command"]["cursor"], "token-current");
        assert_eq!(wire["app_command"]["limit"], 3);
    });
}
#[test]
fn reconciliation_preserves_bounded_multiline_operator_evidence() {
    runtime().block_on(async {
    let (dir,server)=fake(json!({"data":{"id":"op_reconcile","state":"SUCCEEDED","target_id":"execution","kind":"reconcile","created_at_us":1,"error_code":null},"error":null}));let evidence="Executor receipt reviewed\nConfirmed terminal exit with owner";let mut req=request("POST","/api/v1/executions/execution/reconcile",Some("operator"),json!({"state":"SUCCEEDED","evidence":evidence}));req.headers_mut().insert("idempotency-key","reconcile-1".parse().unwrap());let response=api::router(dir.path().into(),None,vec![]).oneshot(req).await.unwrap();assert_eq!(response.status(),StatusCode::ACCEPTED);let wire=server.join().unwrap();assert_eq!(wire["app_command"]["evidence"],evidence);
});
}

#[test]
fn ambiguous_security_headers_are_rejected_before_daemon_dispatch() {
    runtime().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let app = api::router(dir.path().into(), None, vec![]);
        for (name, first, second, method, path) in [
            (
                "authorization",
                "Bearer first",
                "Bearer second",
                "GET",
                "/api/v1/status",
            ),
            (
                "origin",
                "https://console.example",
                "https://evil.example",
                "GET",
                "/api/v1/status",
            ),
            (
                "host",
                "console.example",
                "evil.example",
                "GET",
                "/api/v1/status",
            ),
            (
                "access-control-request-method",
                "GET",
                "DELETE",
                "OPTIONS",
                "/api/v1/status",
            ),
            (
                "access-control-request-headers",
                "authorization",
                "x-unapproved",
                "OPTIONS",
                "/api/v1/status",
            ),
            (
                "content-type",
                "application/json",
                "text/plain",
                "POST",
                "/api/v1/workloads/validate",
            ),
            ("last-event-id", "41", "99", "GET", "/api/v1/events"),
            (
                "idempotency-key",
                "first",
                "second",
                "POST",
                "/api/v1/jobs/job/run",
            ),
        ] {
            let mut req = request(method, path, Some("observer"), json!({"definition":"x"}));
            req.headers_mut()
                .insert("host", "console.example".parse().unwrap());
            req.headers_mut()
                .insert("origin", "https://console.example".parse().unwrap());
            req.headers_mut()
                .insert("access-control-request-method", "GET".parse().unwrap());
            req.headers_mut().remove(name);
            req.headers_mut().append(name, first.parse().unwrap());
            req.headers_mut().append(name, second.parse().unwrap());
            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
            if name == "origin" {
                assert!(
                    !response
                        .headers()
                        .contains_key("access-control-allow-origin")
                );
            }
            assert_eq!(
                json_body(response).await["error"]["code"],
                "INPUT_INVALID",
                "{name}"
            );
        }
    });
}

#[test]
fn malformed_json_and_invalid_query_bounds_fail_with_public_errors() {
    runtime().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let app = api::router(dir.path().into(), None, vec![]);
        for body in [
            "{".to_owned(),
            r#"{"definition":"first","definition":"second"}"#.to_owned(),
            r#"{"definition":"x"} {"definition":"y"}"#.to_owned(),
            format!("{{\"definition\":{}0{}}}", "[".repeat(128), "]".repeat(128)),
        ] {
            let req = Request::builder()
                .method("POST")
                .uri("/api/v1/workloads/validate")
                .header("authorization", "Bearer observer")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(json_body(response).await["error"]["code"], "INPUT_INVALID");
        }
        for path in [
            "/api/v1/workloads?limit=0",
            "/api/v1/workloads?limit=1&limit=2",
            "/api/v1/workloads?namespace=prod%00other",
            "/api/v1/workloads?cursor=",
            "/api/v1/executions/e/logs?offset=-1",
            "/api/v1/executions/e/logs?max_bytes=65537",
            "/api/v1/events?after=-1",
        ] {
            let response = app
                .clone()
                .oneshot(request("GET", path, Some("observer"), json!(null)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(
                json_body(response).await["error"]["code"],
                "INPUT_INVALID",
                "{path}"
            );
        }
    });
}

#[test]
fn json_body_limit_accepts_the_boundary_and_rejects_one_extra_byte() {
    runtime().block_on(async {
        let definition = "x".repeat(65_536 - r#"{"definition":""}"#.len());
        let body = json!({"definition":definition}).to_string();
        assert_eq!(body.len(), 65_536);
        let (dir, daemon) = fake(json!({"data":{"valid":true},"error":null}));
        let app = api::router(dir.path().into(), None, vec![]);
        for (body, expected) in [
            (body.clone(), StatusCode::OK),
            (format!("{body} "), StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let req = Request::builder()
                .method("POST")
                .uri("/api/v1/workloads/validate")
                .header("authorization", "Bearer observer")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::PAYLOAD_TOO_LARGE {
                assert_eq!(json_body(response).await["error"]["code"], "BODY_TOO_LARGE");
            }
        }
        assert_eq!(
            daemon.join().unwrap()["app_command"]["definition"],
            definition
        );
    });
}

#[test]
fn sse_daemon_failure_closes_without_claiming_the_token_expired() {
    runtime().block_on(async {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let listener = UnixListener::bind(dir.path().join("control.sock")).unwrap();
        let daemon = thread::spawn(move || {
            let mut requests = Vec::new();
            for reply in [
                json!({"data":{"items":[],"next_cursor":null},"error":null}),
                json!({"data":null,"error":{"code":"LOCAL_TRANSPORT_UNAVAILABLE","message":"secret /private/db"}}),
            ] {
                let (mut stream, request) = accept_rpc(&listener);
                requests.push(request);
                writeln!(stream, "{reply}").unwrap();
            }
            requests
        });
        let response = api::router(dir.path().into(), None, vec![])
            .oneshot(request("GET", "/api/v1/events?after=7", Some("observer"), json!(null)))
            .await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(3), to_bytes(response.into_body(), 10_000)).await.unwrap().unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("event: stream_error"));
        assert!(text.contains("SERVICE_UNAVAILABLE"));
        assert!(!text.contains("auth_expired"));
        assert!(!text.contains("private"));
        let requests = daemon.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request["app_command"]["after"] == 7));
    });
}

#[test]
fn oversized_daemon_reply_is_a_service_failure_instead_of_invalid_client_input() {
    runtime().block_on(async {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let listener = UnixListener::bind(dir.path().join("control.sock")).unwrap();
        let daemon = thread::spawn(move || {
            let (mut stream, request) = accept_rpc(&listener);
            let reply = json!({"data":{"detail":"private-detail".repeat(170_000)},"error":null});
            if let Err(error) = writeln!(stream, "{reply}") {
                assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ));
            }
            request
        });
        let response = api::router(dir.path().into(), None, vec![])
            .oneshot(request(
                "GET",
                "/api/v1/status",
                Some("observer"),
                json!(null),
            ))
            .await
            .unwrap();
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(daemon.join().unwrap()["app_command"]["action"], "status");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "SERVICE_UNAVAILABLE");
        assert!(!body.to_string().contains("private-detail"));
    });
}
