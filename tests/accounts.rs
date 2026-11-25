use dragoman::accounts::{AccountPool, BalanceStrategy, FailureKind};
use dragoman::session::SessionData;
use std::sync::Arc;

fn session(cookie: &str) -> SessionData {
    SessionData {
        snlm0e: "at".to_string(),
        f_sid: "sid".to_string(),
        bl: "bl".to_string(),
        cookie: cookie.to_string(),
        model: "Argon4p0".to_string(),
        conversation_id: None,
        response_id: None,
        choice_id: None,
        base_url: None,
        proxy: None,
        path: None,
    }
}

fn pool(entries: &[(&str, &str)], strategy: BalanceStrategy) -> AccountPool {
    AccountPool::from_sessions(
        entries
            .iter()
            .map(|(name, cookie)| ((*name).to_string(), session(cookie)))
            .collect(),
        strategy,
    )
    .unwrap()
}

#[tokio::test]
async fn round_robin_rotates_across_accounts() {
    let pool = pool(
        &[("alpha", "c1"), ("beta", "c2"), ("gamma", "c3")],
        BalanceStrategy::RoundRobin,
    );

    let mut order = Vec::new();
    for _ in 0..4 {
        let lease = pool.lease().await;
        order.push(lease.account.name.clone());
    }

    assert_eq!(order, vec!["alpha", "beta", "gamma", "alpha"]);
}

#[tokio::test]
async fn cooling_account_is_skipped() {
    let pool = pool(
        &[("alpha", "c1"), ("beta", "c2")],
        BalanceStrategy::RoundRobin,
    );
    let alpha = pool.accounts()[0].clone();
    alpha.note_failure(FailureKind::Quota, "HTTP 429 quota exhausted");

    for _ in 0..3 {
        let lease = pool.lease().await;
        assert_eq!(lease.account.name, "beta");
    }

    let status = pool.status().await;
    let alpha_status = status
        .accounts
        .iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    assert!(alpha_status.cooldown_seconds > 0);
    assert!(
        alpha_status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("quota"))
    );
}

#[tokio::test]
async fn least_used_prefers_the_idle_account() {
    let pool = pool(
        &[("alpha", "c1"), ("beta", "c2")],
        BalanceStrategy::LeastUsed,
    );
    let alpha = pool.accounts()[0].clone();
    pool.note_success(&alpha);

    let lease = pool.lease().await;
    assert_eq!(lease.account.name, "beta");
    drop(lease);

    let status = pool.status().await;
    let beta = status
        .accounts
        .iter()
        .find(|account| account.name == "beta")
        .unwrap();
    assert_eq!(beta.served, 0);
}

#[tokio::test]
async fn single_account_pool_is_always_leased() {
    let pool = pool(&[("only", "c1")], BalanceStrategy::RoundRobin);
    let first = pool.lease().await;
    assert_eq!(first.account.name, "only");

    let handle = tokio::spawn({
        let pool = Arc::new(pool);
        async move {
            let lease = pool.lease().await;
            lease.account.name.clone()
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    drop(first);
    assert_eq!(handle.await.unwrap(), "only");
}

#[test]
fn account_names_are_restricted_to_safe_characters() {
    assert_eq!(
        dragoman::accounts::file_name_for_account("work-2").unwrap(),
        "work-2"
    );
    assert!(dragoman::accounts::file_name_for_account("../evil").is_err());
    assert!(dragoman::accounts::file_name_for_account("").is_err());
}
