#![cfg(feature = "webhooks")]
use cronharbor::{
    config::{AlertConfig, SecretRef},
    server,
};
use std::net::IpAddr;
fn config(endpoint: &str) -> AlertConfig {
    AlertConfig {
        endpoint: endpoint.into(),
        token_ref: SecretRef::Env {
            name: "CRONHARBOR_WEBHOOK_TEST_TOKEN".into(),
        },
        allowed_private_addresses: Vec::new(),
        ca_file: None,
    }
}
#[test]
fn destination_policy_requires_https_and_exact_private_approval() {
    for endpoint in [
        "http://example.com/alerts",
        "https://user@example.com/alerts",
        "https://example.com/alerts?token=x",
        "https://example.com/alerts#x",
        "https://127.0.0.1/alerts",
        "https://169.254.1.1/alerts",
        "https://255.255.255.255/alerts",
        "https://[::ffff:127.0.0.1]/alerts",
    ] {
        assert!(
            server::validate_alert_config(Some(&config(endpoint))).is_err(),
            "{endpoint}"
        );
    }
    assert!(server::validate_alert_config(Some(&config("https://example.com/alerts"))).is_ok());
    let mut local = config("https://127.0.0.1/alerts");
    local
        .allowed_private_addresses
        .push("127.0.0.1".parse::<IpAddr>().unwrap());
    assert!(server::validate_alert_config(Some(&local)).is_ok());
    for address in [
        "169.254.1.1",
        "255.255.255.255",
        "::ffff:127.0.0.1",
        "0.0.0.0",
        "224.0.0.1",
        "2001:db8::1",
    ] {
        let mut invalid = config("https://example.com/alerts");
        invalid
            .allowed_private_addresses
            .push(address.parse().unwrap());
        assert!(
            server::validate_alert_config(Some(&invalid)).is_err(),
            "{address}"
        );
    }
}

use cronharbor::{
    alerts::{AlertDispatch, AlertReceipt},
    contracts::AlertErrorCode,
    webhooks::{WebhookWorker, destination_hash},
};
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct TlsFixture {
    directory: tempfile::TempDir,
    child: Child,
    port: u16,
}
impl Drop for TlsFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl TlsFixture {
    fn new() -> Self {
        Self::with_san("IP:127.0.0.1,DNS:localhost")
    }
    fn with_san(san: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let openssl = |arguments: &[&str]| {
            let output = Command::new("openssl")
                .args(arguments)
                .current_dir(directory.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "openssl fixture certificate creation failed"
            );
        };
        openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=CronHarbor test CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
        ]);
        openssl(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
        ]);
        fs::write(directory.path().join("extensions"),format!("subjectAltName={san}\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")).unwrap();
        openssl(&[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "1",
            "-extfile",
            "extensions",
            "-out",
            "server.pem",
        ]);
        fs::write(directory.path().join("token"), "first-token\n").unwrap();
        fs::write(directory.path().join("mode"), "204").unwrap();
        for entry in fs::read_dir(directory.path()).unwrap() {
            fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let code = r#"
import http.server,ssl,sys,pathlib,time,json,socket
root=pathlib.Path(sys.argv[1])
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_POST(self):
  body=self.rfile.read(int(self.headers.get('Content-Length','0')))
  with (root/'requests').open('a') as out: out.write(json.dumps({'path':self.path,'body':body.decode(),'token':self.headers.get('Authorization'),'key':self.headers.get('Idempotency-Key'),'host':self.headers.get('Host'),'user_agent':self.headers.get('User-Agent')})+'\n')
  mode=(root/'mode').read_text()
  if mode=='slow': time.sleep(20); return
  if mode=='drop': self.connection.shutdown(socket.SHUT_RDWR); return
  if mode in ('oversized','above_nominal'):
   prefix=b'HTTP/1.1 204 No Content\r\nX-Large: '
   size=600000 if mode=='oversized' else 417793
   try: self.connection.sendall(prefix+b'x'*(size-len(prefix)-4)+b'\r\n\r\n')
   except OSError: pass
   return
  self.send_response(int(mode))
  if mode=='302': self.send_header('Location','https://example.com/must-not-follow')
  if mode=='429': self.send_header('Retry-After','7200')
  self.send_header('Content-Length','0'); self.end_headers()
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
server.daemon_threads=True
context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(root/'server.pem',root/'server.key')
server.socket=context.wrap_socket(server.socket,server_side=True)
print(server.server_port,flush=True);server.serve_forever(poll_interval=.01)
"#;
        let mut child = Command::new("python3")
            .args(["-u", "-c", code])
            .arg(directory.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().parse().unwrap();
        Self {
            directory,
            child,
            port,
        }
    }
    fn config(&self) -> AlertConfig {
        AlertConfig {
            endpoint: format!("https://127.0.0.1:{}/alerts", self.port),
            token_ref: SecretRef::File {
                path: self
                    .directory
                    .path()
                    .join("token")
                    .to_string_lossy()
                    .into_owned(),
            },
            allowed_private_addresses: vec!["127.0.0.1".parse().unwrap()],
            ca_file: Some(self.directory.path().join("ca.pem")),
        }
    }
    fn mode(&self, mode: &str) {
        fs::write(self.directory.path().join("mode"), mode).unwrap();
    }
    fn records(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.directory.path().join("requests"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
fn dispatch(config: &AlertConfig) -> AlertDispatch {
    AlertDispatch {
        alert_id: uuid::Uuid::new_v4().to_string(),
        nonce: uuid::Uuid::new_v4().to_string(),
        destination_hash: destination_hash(config).unwrap(),
        payload_json: "{\"metadata\":\"fixture\"}".into(),
    }
}
fn send(worker: &mut WebhookWorker, config: &AlertConfig) -> AlertReceipt {
    worker.try_send(dispatch(config)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(result) = worker.try_result() {
            return result.receipt;
        }
        assert!(
            Instant::now() < deadline,
            "bounded webhook result did not arrive"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn real_https_validates_tls_rotates_credentials_and_obeys_http_outcomes() {
    let fixture = TlsFixture::new();
    let config = fixture.config();
    let mut worker = WebhookWorker::spawn(config.clone()).unwrap();
    assert!(matches!(
        send(&mut worker, &config),
        AlertReceipt::Accepted { http_status: 204 }
    ));
    let records = fixture.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["token"], "Bearer first-token");
    assert_eq!(records[0]["user_agent"], "CronHarbor/1");
    assert_eq!(records[0]["body"], "{\"metadata\":\"fixture\"}");
    assert!(
        records[0]["key"]
            .as_str()
            .is_some_and(|key| uuid::Uuid::parse_str(key).is_ok())
    );
    fs::write(fixture.directory.path().join("token"), "rotated-token\r\n").unwrap();
    fixture.mode("429");
    assert!(matches!(
        send(&mut worker, &config),
        AlertReceipt::Retry {
            http_status: Some(429),
            retry_after_seconds: Some(3600),
            ..
        }
    ));
    assert_eq!(fixture.records()[1]["token"], "Bearer rotated-token");
    for status in [408, 425, 500, 503] {
        fixture.mode(&status.to_string());
        assert!(
            matches!(send(&mut worker,&config),AlertReceipt::Retry{http_status:Some(code),..} if code==status)
        );
    }
    for status in [302, 400, 401, 404] {
        fixture.mode(&status.to_string());
        assert!(
            matches!(send(&mut worker,&config),AlertReceipt::Failed{http_status:Some(code),..} if code==status)
        );
    }
    fixture.mode("700");
    assert!(matches!(
        send(&mut worker, &config),
        AlertReceipt::Failed {
            code: AlertErrorCode::HttpRejected,
            http_status: None
        }
    ));
    fixture.mode("drop");
    assert!(matches!(
        send(&mut worker, &config),
        AlertReceipt::Retry {
            code: AlertErrorCode::TransportUnavailable,
            ..
        }
    ));
    let mut untrusted = config.clone();
    untrusted.ca_file = None;
    let mut other = WebhookWorker::spawn(untrusted.clone()).unwrap();
    assert!(matches!(
        send(&mut other, &untrusted),
        AlertReceipt::Blocked {
            code: AlertErrorCode::TlsUnavailable
        }
    ));
    let wrong_host_fixture = TlsFixture::with_san("DNS:localhost");
    let wrong_host = wrong_host_fixture.config();
    let mut other = WebhookWorker::spawn(wrong_host.clone()).unwrap();
    assert!(matches!(
        send(&mut other, &wrong_host),
        AlertReceipt::Blocked {
            code: AlertErrorCode::TlsUnavailable
        }
    ));
    worker.shutdown();
}
#[test]
fn oversized_aggregate_response_headers_are_bounded_and_slow_receiver_cancels_promptly() {
    let fixture = TlsFixture::new();
    let config = fixture.config();
    let mut worker = WebhookWorker::spawn(config.clone()).unwrap();
    fixture.mode("above_nominal");
    let nominal = send(&mut worker, &config);
    // Hyper checks incomplete headers before reading again, but can accept a completed overshoot.
    assert!(matches!(
        nominal,
        AlertReceipt::Accepted { http_status: 204 }
    ));
    fixture.mode("oversized");
    let started = Instant::now();
    assert!(matches!(
        send(&mut worker, &config),
        AlertReceipt::Retry {
            code: AlertErrorCode::TransportUnavailable,
            ..
        }
    ));
    assert!(started.elapsed() < Duration::from_secs(3));
    fixture.mode("slow");
    worker.try_send(dispatch(&config)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while fixture.records().len() < 3 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let stopped = Instant::now();
    worker.shutdown();
    assert!(stopped.elapsed() < Duration::from_millis(700));
    assert!(!worker.available());
}
#[test]
fn private_secret_files_are_checked_without_following_symlinks_or_accepting_extra_lines() {
    let fixture = TlsFixture::new();
    let config = fixture.config();
    let token = fixture.directory.path().join("token");
    let mut worker = WebhookWorker::spawn(config.clone()).unwrap();
    for bytes in [b"token\nsecond".as_slice(), b"token ", b"", &[b'x'; 515]] {
        fs::write(&token, bytes).unwrap();
        assert!(matches!(
            send(&mut worker, &config),
            AlertReceipt::Blocked {
                code: AlertErrorCode::CredentialUnavailable
            }
        ));
    }
    fs::write(&token, "token").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        send(&mut worker, &config),
        AlertReceipt::Blocked {
            code: AlertErrorCode::CredentialUnavailable
        }
    ));
    fs::remove_file(&token).unwrap();
    std::os::unix::fs::symlink(Path::new("ca.pem"), &token).unwrap();
    assert!(matches!(
        send(&mut worker, &config),
        AlertReceipt::Blocked {
            code: AlertErrorCode::CredentialUnavailable
        }
    ));
    assert!(fixture.records().is_empty());
}

#[test]
fn ambient_proxy_is_ignored_and_hostname_resolution_preserves_tls_host() {
    const CHILD: &str = "CRONHARBOR_PROXY_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let fixture = TlsFixture::with_san("DNS:localhost");
        let mut config = fixture.config();
        config.endpoint = format!("https://localhost:{}/alerts", fixture.port);
        config.allowed_private_addresses =
            vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()];
        let mut worker = WebhookWorker::spawn(config.clone()).unwrap();
        assert!(matches!(
            send(&mut worker, &config),
            AlertReceipt::Accepted { http_status: 204 }
        ));
        assert_eq!(
            fixture.records()[0]["host"],
            format!("localhost:{}", fixture.port)
        );
        return;
    }
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ambient_proxy_is_ignored_and_hostname_resolution_preserves_tls_host",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("https_proxy", "http://127.0.0.1:1")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "proxy-isolation child failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
