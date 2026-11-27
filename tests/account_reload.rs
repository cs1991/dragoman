use dragoman::accounts::{AccountPool, BalanceStrategy, FailureKind, HealthStore};
use dragoman::session::SessionData;
use std::path::{Path, PathBuf};

fn temp_root(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("dragoman-{label}-{}-{nanos}", std::process::id()))
}

fn write_session(root: &Path, name: &str, cookie: &str) {
    std::fs::create_dir_all(root).unwrap();
    let path = root.join(format!("{name}.json"));
    let session = SessionData {
        snlm0e: format!("at-{cookie}"),
        f_sid: "sid".to_string(),
        bl: "bl".to_string(),
        cookie: cookie.to_string(),
        model: "Argon4p0".to_string(),
        conversation_id: None,
        response_id: None,
        choice_id: None,
        base_url: None,
        proxy: None,
        local_ip: None,
        tier: dragoman::session::AccountTier::Pro,
        path: Some(path),
    };
    session.save().unwrap();
}

fn names(pool: &AccountPool) -> Vec<String> {
    pool.accounts()
        .into_iter()
        .map(|account| account.name.clone())
        .collect()
}

#[test]
fn reload_tracks_added_disabled_and_removed_sessions() {
    let root = temp_root("reload");
    write_session(&root, "alpha", "c1");
    write_session(&root, "beta", "c2");
    let pool = AccountPool::load_dir(&root, BalanceStrategy::RoundRobin).unwrap();
    assert_eq!(names(&pool), vec!["alpha", "beta"]);

    let alpha = pool
        .accounts()
        .into_iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    pool.note_success(&alpha);

    std::thread::sleep(std::time::Duration::from_millis(20));
    write_session(&root, "gamma", "c3");
    std::fs::rename(root.join("beta.json"), root.join("beta.json.disabled")).unwrap();

    let report = pool.reload_if_changed().expect("directory changed");
    assert_eq!(report.total, 2);
    assert_eq!(report.added, 1);
    assert_eq!(report.removed, 1);
    assert_eq!(names(&pool), vec!["alpha", "gamma"]);

    let alpha_after = pool
        .accounts()
        .into_iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    assert_eq!(alpha_after.served(), 1);

    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::remove_file(root.join("gamma.json")).unwrap();
    let report = pool.reload_if_changed().expect("directory changed");
    assert_eq!(report.total, 1);
    assert_eq!(names(&pool), vec!["alpha"]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn abnormal_mark_survives_restart_and_clears_after_a_new_sign_in() {
    let root = temp_root("marks");
    write_session(&root, "alpha", "c1");
    write_session(&root, "beta", "c2");
    let pool = AccountPool::load_dir(&root, BalanceStrategy::RoundRobin).unwrap();
    let alpha = pool
        .accounts()
        .into_iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    for _ in 0..5 {
        alpha.note_failure(FailureKind::Auth, "HTTP 401");
    }
    assert!(alpha.is_abnormal());
    assert!(root.join("account-health.json").is_file());

    drop(pool);
    let restarted = AccountPool::load_dir(&root, BalanceStrategy::RoundRobin).unwrap();
    let alpha = restarted
        .accounts()
        .into_iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    assert!(alpha.is_abnormal());

    std::thread::sleep(std::time::Duration::from_millis(20));
    write_session(&root, "alpha", "c9");
    let report = restarted.reload_if_changed().expect("directory changed");
    assert_eq!(report.refreshed, 1);
    let alpha = restarted
        .accounts()
        .into_iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    assert!(!alpha.is_abnormal());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn external_health_clear_takes_effect_on_the_live_pool() {
    let root = temp_root("health");
    write_session(&root, "alpha", "c1");
    let pool = AccountPool::load_dir(&root, BalanceStrategy::RoundRobin).unwrap();
    let alpha = pool
        .accounts()
        .into_iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    for _ in 0..5 {
        alpha.note_failure(FailureKind::Auth, "HTTP 401");
    }
    assert!(alpha.is_abnormal());

    // The login command and `accounts --clear` repair an account from another
    // process. The running pool picks that up without a restart.
    HealthStore::load(root.join("account-health.json")).clear("alpha");
    let report = pool.reload_if_changed().expect("health file changed");
    assert_eq!(report.total, 1);
    let alpha = pool
        .accounts()
        .into_iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    assert!(!alpha.is_abnormal());
    assert!(alpha.is_available());

    let _ = std::fs::remove_dir_all(&root);
}
