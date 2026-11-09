#![cfg(feature = "http-probes")]
use cronharbor::{
    config::Manifest,
    external::{ExecutorPolicy, ExternalHandle},
    http_readiness::{self, HttpReadiness},
};
use serde_json::{Value, json};

fn manifest(probe: Value) -> Value {
    let mut value: Value = serde_json::from_str(
        &serde_json::to_string(&Manifest::parse(include_str!("../examples/hello.yaml")).unwrap())
            .unwrap(),
    )
    .unwrap();
    value["kind"] = json!("Service");
    value["spec"]["execution"]["adapter"] = json!("systemd");
    value["spec"]["execution"]["command"] = json!("");
    value["spec"]["execution"]["args"] = json!([]);
    value["spec"]["execution"]["systemd"] = json!({"unit":"app.service"});
    value["spec"]["service"]["httpReadiness"] = probe;
    value
}

fn probe() -> Value {
    json!({"url":"http://127.0.0.1:4001/auth/health","status":200,
        "check":{"type":"json","equals":{"ok":true},"nonemptyArrays":[]},
        "intervalSeconds":15,"timeoutSeconds":3,"failureThreshold":3,"successThreshold":2})
}

#[test]
fn systemd_service_accepts_bounded_http_readiness_without_exec_authority() {
    let value = manifest(probe());
    let parsed = Manifest::parse(&value.to_string());
    assert!(parsed.is_ok(), "valid HTTP readiness rejected: {parsed:?}");
    let encoded = serde_json::to_value(parsed.unwrap()).unwrap();
    assert_eq!(
        encoded["spec"]["service"]["httpReadiness"]["check"]["equals"]["ok"],
        true
    );
}

#[test]
fn invalid_probe_destinations_and_unbounded_checks_are_rejected() {
    for url in [
        "http://example.com:80/",
        "http://localhost:4001/",
        "http://192.168.5.61:4001/",
        "https://127.0.0.1:4001/",
        "http://127.0.0.1/",
        "http://user@127.0.0.1:4001/",
        "http://127.0.0.1:4001/#fragment",
        "http://[::ffff:127.0.0.1]:4001/",
    ] {
        let mut p = probe();
        p["url"] = json!(url);
        assert!(
            Manifest::parse(&manifest(p).to_string()).is_err(),
            "accepted {url}"
        );
    }
    for (key, value) in [
        ("timeoutSeconds", 0),
        ("timeoutSeconds", 11),
        ("intervalSeconds", 0),
        ("failureThreshold", 0),
        ("successThreshold", 0),
        ("status", 99),
        ("status", 600),
    ] {
        let mut p = probe();
        p[key] = json!(value);
        assert!(
            Manifest::parse(&manifest(p).to_string()).is_err(),
            "accepted {key}={value}"
        );
    }
    let mut p = probe();
    p["check"]["equals"] = json!({"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8,"i":9});
    assert!(Manifest::parse(&manifest(p).to_string()).is_err());
}

#[test]
fn http_readiness_is_not_silently_ignored_on_other_kinds_or_adapters() {
    for (kind, adapter) in [("ScheduledJob", "systemd"), ("Service", "process")] {
        let mut v = manifest(probe());
        v["kind"] = json!(kind);
        v["spec"]["execution"]["adapter"] = json!(adapter);
        if kind == "ScheduledJob" {
            v["spec"]["schedule"] =
                json!({"type":"cron","expression":"* * * * *","timezone":"UTC"});
        }
        if adapter == "process" {
            v["spec"]["execution"]["command"] = json!("/bin/true");
            v["spec"]["execution"]["systemd"] = Value::Null;
        }
        assert!(Manifest::parse(&v.to_string()).is_err());
    }
    let mut v = manifest(probe());
    v["spec"]["service"]["readiness"] =
        json!({"command":"/bin/true","args":[],"intervalSeconds":1,"timeoutSeconds":1});
    assert!(Manifest::parse(&v.to_string()).is_err());
}

fn authorized(p: &HttpReadiness) -> (ExecutorPolicy, ExternalHandle) {
    let policy=serde_json::from_value(json!({"systemdEnabled":true,"scopedUnits":[{"unit":"app.service","scope":"user"}],
        "systemdHttpReadiness":[{"unit":"app.service","scope":"user","url":p.url,"status":p.status,"check":p.check}]})).unwrap();
    let handle=serde_json::from_value(json!({"kind":"systemd_v2","unit":"app.service","scope":"user","since_us":1,"service":true,"invocation_id":"11111111111111111111111111111111"})).unwrap();
    (policy, handle)
}

#[test]
fn owner_policy_binds_scope_endpoint_and_required_response() {
    let p: HttpReadiness = serde_json::from_value(probe()).unwrap();
    let (policy, handle) = authorized(&p);
    assert!(http_readiness::authorize(&policy, &handle, &p).is_ok());
    for field in ["url", "status", "check"] {
        let mut v = probe();
        v[field] = match field {
            "url" => json!("http://127.0.0.1:4001/other"),
            "status" => json!(201),
            _ => json!({"type":"status"}),
        };
        let changed = serde_json::from_value(v).unwrap();
        assert_eq!(
            http_readiness::authorize(&policy, &handle, &changed)
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
    }
    let mut v = serde_json::to_value(&handle).unwrap();
    v["scope"] = json!("system");
    assert!(http_readiness::authorize(&policy, &serde_json::from_value(v).unwrap(), &p).is_err());
    let mut duplicate = serde_json::to_value(policy).unwrap();
    let row = duplicate["systemdHttpReadiness"][0].clone();
    duplicate["systemdHttpReadiness"]
        .as_array_mut()
        .unwrap()
        .push(row);
    assert!(http_readiness::validate_policy(&serde_json::from_value(duplicate).unwrap()).is_err());
    let (mut policy, handle) = authorized(&p);
    policy.units.push("app.service".into());
    assert!(
        http_readiness::authorize(&policy, &handle, &p).is_err(),
        "ambiguous unit policy authorized a probe"
    );
}

fn response_check(response: Vec<u8>, check: Value, expected: bool) {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    server.set_nonblocking(true).unwrap();
    let task = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match server.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = [0; 4096];
                    let n = stream.read(&mut request).unwrap();
                    assert!(
                        std::str::from_utf8(&request[..n])
                            .unwrap()
                            .starts_with("GET /check HTTP/1.1")
                    );
                    let _ = stream.write_all(&response);
                    break;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("test server accept failed: {e}"),
            }
        }
    });
    let mut raw = probe();
    raw["url"] = json!(format!("http://127.0.0.1:{port}/check"));
    raw["check"] = check;
    let p = serde_json::from_value(raw).unwrap();
    let (policy, handle) = authorized(&p);
    assert_eq!(
        http_readiness::probe(&policy, &handle, &p).unwrap(),
        expected
    );
    task.join().unwrap();
}

#[test]
fn actual_http_body_and_status_determine_readiness() {
    for (status, body, check, expected) in [
        (
            200,
            "{\"ok\":true}",
            json!({"type":"json","equals":{"ok":true}}),
            true,
        ),
        (
            200,
            "{\"ok\":false}",
            json!({"type":"json","equals":{"ok":true}}),
            false,
        ),
        (
            200,
            "not json",
            json!({"type":"json","equals":{"ok":true}}),
            false,
        ),
        (
            503,
            "{\"ok\":true}",
            json!({"type":"json","equals":{"ok":true}}),
            false,
        ),
        (
            200,
            "{\"code\":\"Ok\",\"routes\":[]}",
            json!({"type":"json","equals":{"code":"Ok"},"nonemptyArrays":["routes"]}),
            false,
        ),
        (
            200,
            "{\"code\":\"Ok\",\"routes\":[{}]}",
            json!({"type":"json","equals":{"code":"Ok"},"nonemptyArrays":["routes"]}),
            true,
        ),
    ] {
        response_check(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).into_bytes(),check,expected);
    }
    response_check(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 42\r\n\r\n<!doctype html><html><body>x</body></html>".to_vec(),json!({"type":"html"}),true);
}

#[test]
fn redirect_compression_and_oversize_responses_do_not_pass() {
    for response in [
        b"HTTP/1.1 302 Found\r\nLocation: http://192.168.5.61:80/\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 65537\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\n\r\n".to_vec(),
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10001\r\n{}\r\n0\r\n\r\n",
            "x".repeat(65537)
        )
        .into_bytes(),
    ] {
        response_check(response, json!({"type":"status"}), false);
    }
}

#[test]
fn proxy_environment_does_not_redirect_local_readiness() {
    if std::env::var_os("HTTP_READINESS_PROXY_CHILD").is_some() {
        response_check(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
            json!({"type":"status"}),
            true,
        );
    } else {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "proxy_environment_does_not_redirect_local_readiness",
            ])
            .env("HTTP_READINESS_PROXY_CHILD", "1")
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("http_proxy", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[test]
fn stalled_response_has_a_total_deadline_without_retry() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let task = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut count = 0;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    count += 1;
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut buf = [0; 1024];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n");
                    thread::sleep(Duration::from_millis(1300));
                    let _ = stream.write_all(b"x");
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
        count
    });
    let mut raw = probe();
    raw["url"] = json!(format!("http://127.0.0.1:{port}/check"));
    raw["timeoutSeconds"] = json!(1);
    raw["check"] = json!({"type":"status"});
    let p = serde_json::from_value(raw).unwrap();
    let (policy, handle) = authorized(&p);
    let start = Instant::now();
    assert!(!http_readiness::probe(&policy, &handle, &p).unwrap());
    assert!(start.elapsed() < Duration::from_millis(1500));
    assert_eq!(task.join().unwrap(), 1, "readiness requests were retried");
}
