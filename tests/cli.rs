use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn local_cli_validates_applies_inspects_and_never_starts_a_second_owner() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let binary = env!("CARGO_BIN_EXE_cronharbor");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/hello.yaml");
    assert!(
        Command::new(binary)
            .args(["config", "validate", "-f", fixture])
            .status()
            .unwrap()
            .success()
    );
    let mut daemon = Daemon(
        Command::new(binary)
            .arg("--state-dir")
            .arg(dir.path())
            .args(["server", "--paused"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.path().join("control.sock").exists() {
        assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited");
        assert!(Instant::now() < deadline, "daemon did not become ready");
        thread::sleep(Duration::from_millis(20));
    }
    let second = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .arg("server")
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("OWNER_UNAVAILABLE"));
    for args in [
        vec!["config", "apply", "-f", fixture],
        vec!["status"],
        vec!["executions"],
        vec!["log-storage", "status"],
        vec!["alerts", "status"],
        vec!["alerts", "list", "--limit", "10"],
    ] {
        let output = Command::new(binary)
            .arg("--state-dir")
            .arg(dir.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    }
    let output = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .arg("status")
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["paused"], true);
    assert_eq!(status["workloads"], 1);

    let updated = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .args([
            "log-storage",
            "set",
            "--max-disk-bytes",
            "536870912",
            "--max-segments",
            "2048",
            "--retention-seconds",
            "1209600",
            "--expected-revision",
            "1",
            "--idempotency-key",
            "cli-log-policy-1",
        ])
        .output()
        .unwrap();
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let output = Command::new(binary)
        .arg("--state-dir")
        .arg(dir.path())
        .args(["log-storage", "status"])
        .output()
        .unwrap();
    let policy: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(policy["policy"]["revision"], 2);
    assert_eq!(policy["policy"]["max_segments"], 2048);
    assert_eq!(policy["policy"]["retention_seconds"], 1209600);
}
