use dragoman::desktop_accounts::AccountStore;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);

#[test]
fn desktop_profiles_are_separate_and_beside_executable() {
    let fixture = Fixture::new();
    let exe = fixture.0.join("install").join("dragoman.exe");
    let profiles = fixture.0.join("install").join("desktop-profiles");
    assert_eq!(
        AccountStore::profile_dir("named:work", &exe).unwrap(),
        profiles.join("account-work.profile")
    );
    assert_eq!(
        AccountStore::profile_dir("named:work.", &exe).unwrap(),
        profiles.join("account-work..profile")
    );
    assert_eq!(
        AccountStore::profile_dir("named:backup", &exe).unwrap(),
        profiles.join("account-backup.profile")
    );
    assert_eq!(
        AccountStore::profile_dir("legacy", &exe).unwrap(),
        profiles.join("legacy.profile")
    );
    assert!(AccountStore::profile_dir("named:../work", &exe).is_err());
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("dragoman-desktop-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(home.join(".config/dragoman/sessions")).unwrap();
        Self(home)
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dragoman"));
        command.env("HOME", &self.0);
        command
    }

    fn save(&self, path: &str) {
        fs::write(
            self.0.join(".config/dragoman").join(path),
            r#"{"snlm0e":"fixture","f_sid":"fixture","bl":"fixture","cookie":"fixture-value"}"#,
        )
        .unwrap();
    }

    fn manage(&self, args: &[&str]) -> std::process::Output {
        self.cli()
            .arg("desktop-accounts")
            .args(args)
            .output()
            .unwrap()
    }

    fn list(&self) -> serde_json::Value {
        let output = self.manage(&["list"]);
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-value"));
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn manage_named_and_legacy_accounts_without_exposing_credentials() {
    let fixture = Fixture::new();
    assert_eq!(fixture.list(), serde_json::json!([]));
    fixture.save("sessions/default.json");
    fixture.save("session.json");
    let entries = fixture.list();
    assert_eq!(entries.as_array().unwrap().len(), 2);
    assert!(
        entries
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "legacy")
    );
    assert!(
        fixture
            .manage(&["disable", "--id=named:default"])
            .status
            .success()
    );
    let entries = fixture.list();
    assert!(
        entries
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "named:default" && row["enabled"] == false)
    );
    assert!(
        fixture
            .manage(&["enable", "--id=named:default"])
            .status
            .success()
    );
    assert!(fixture.manage(&["remove", "--id=legacy"]).status.success());
    assert_eq!(fixture.list().as_array().unwrap().len(), 1);
    assert!(
        !fixture
            .manage(&["remove", "--id=named:../default"])
            .status
            .success()
    );
    assert!(
        !fixture
            .manage(&["enable", "--id=named:NUL"])
            .status
            .success()
    );
}

#[test]
fn assign_proxies_groups_accounts_and_set_proxy_can_clear() {
    let fixture = Fixture::new();
    fixture.save("sessions/a.json");
    fixture.save("sessions/b.json");
    fixture.save("sessions/c.json");

    let output = fixture.manage(&["assign-proxies", "--start-port=7911", "--per-port=2"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let assignments: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(assignments.as_array().unwrap().len(), 3);

    let entries = fixture.list();
    let proxy = |name: &str| {
        entries
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .and_then(|row| row["proxy"].as_str())
            .map(str::to_string)
    };
    assert_eq!(proxy("a").as_deref(), Some("http://127.0.0.1:7911"));
    assert_eq!(proxy("b").as_deref(), Some("http://127.0.0.1:7911"));
    assert_eq!(proxy("c").as_deref(), Some("http://127.0.0.1:7912"));

    assert!(
        fixture
            .manage(&["set-proxy", "--id=named:a", "--proxy=http://127.0.0.1:7999"])
            .status
            .success()
    );
    assert!(
        fixture
            .manage(&["set-proxy", "--id=named:a", "--proxy="])
            .status
            .success()
    );
    let entries = fixture.list();
    let cleared = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "a")
        .unwrap();
    assert!(cleared["proxy"].is_null());
}

#[test]
fn desktop_proxy_skips_disabled_sessions_and_stops_on_stdin_close() {
    let fixture = Fixture::new();
    fixture.save("sessions/disabled.json.disabled");
    let no_accounts = fixture.cli().args(["serve", "--desktop"]).output().unwrap();
    assert!(!no_accounts.status.success());
    fixture.save("sessions/work.json");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let busy = fixture
        .cli()
        .args(["serve", "--desktop", "--port", &port])
        .output()
        .unwrap();
    assert_eq!(busy.status.code(), Some(2));
    drop(listener);

    let mut child = fixture
        .cli()
        .args(["serve", "--desktop", "--port", &port])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line == "DESKTOP_READY" {
                let _ = tx.send(());
            }
        }
    });
    let ready = rx.recv_timeout(Duration::from_secs(5)).is_ok();
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    let stopped = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.success();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    reader.join().unwrap();
    assert!(ready && stopped);
}
