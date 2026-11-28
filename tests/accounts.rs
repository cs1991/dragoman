use dragoman::accounts::{AccountPool, BalanceStrategy, FailureKind, RequestKind};
use dragoman::session::{AccountTier, SessionData};
use std::sync::Arc;

fn session(cookie: &str) -> SessionData {
    session_with_tier(cookie, AccountTier::Pro)
}

fn session_with_tier(cookie: &str, tier: AccountTier) -> SessionData {
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
        local_ip: None,
        tier,
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

fn pool_with_tiers(entries: &[(&str, &str, AccountTier)]) -> AccountPool {
    AccountPool::from_sessions(
        entries
            .iter()
            .map(|(name, cookie, tier)| ((*name).to_string(), session_with_tier(cookie, *tier)))
            .collect(),
        BalanceStrategy::RoundRobin,
    )
    .unwrap()
}

#[test]
fn sessions_without_a_tier_default_to_free() {
    let session: SessionData = serde_json::from_value(serde_json::json!({
        "snlm0e": "at",
        "f_sid": "sid",
        "bl": "bl",
        "cookie": "cookie"
    }))
    .unwrap();
    assert_eq!(session.tier, AccountTier::Free);

    let session: SessionData = serde_json::from_value(serde_json::json!({
        "snlm0e": "at",
        "f_sid": "sid",
        "bl": "bl",
        "cookie": "cookie",
        "tier": "pro"
    }))
    .unwrap();
    assert_eq!(session.tier, AccountTier::Pro);

    let session: SessionData = serde_json::from_value(serde_json::json!({
        "snlm0e": "at",
        "f_sid": "sid",
        "bl": "bl",
        "cookie": "cookie",
        "tier": "ultra"
    }))
    .unwrap();
    assert_eq!(session.tier, AccountTier::Ultra);
    assert_eq!(session.tier.label(), "ultra");
}

#[test]
fn ultra_tier_accepts_common_spellings() {
    for value in ["ultra", "Ultra", " ULTRA ", "ultimate", "ai-ultra"] {
        assert_eq!(AccountTier::parse(value), Some(AccountTier::Ultra));
    }
    assert_eq!(AccountTier::parse("pro"), Some(AccountTier::Pro));
    assert_eq!(AccountTier::parse("free"), Some(AccountTier::Free));
    assert_eq!(AccountTier::parse("gold"), None);
}

#[tokio::test]
async fn carbon_lease_requires_an_ultra_account() {
    let pool = pool_with_tiers(&[
        ("pro-one", "c1", AccountTier::Pro),
        ("free-one", "c2", AccountTier::Free),
    ]);
    assert!(!pool.has_tier(AccountTier::Ultra));
    assert!(pool.lease_for(RequestKind::Chat, true).await.is_none());
    assert!(pool.lease_for(RequestKind::Chat, false).await.is_some());
}

#[tokio::test]
async fn carbon_lease_picks_the_ultra_account_and_skips_free_chat() {
    let pool = pool_with_tiers(&[
        ("pro-one", "c1", AccountTier::Pro),
        ("free-one", "c2", AccountTier::Free),
        ("ultra-one", "c3", AccountTier::Ultra),
    ]);
    assert!(pool.has_tier(AccountTier::Ultra));
    for _ in 0..3 {
        let lease = pool
            .lease_for(RequestKind::Chat, true)
            .await
            .expect("ultra account");
        assert_eq!(lease.account.name, "ultra-one");
        assert_eq!(lease.account.tier(), AccountTier::Ultra);
    }

    let free_only = pool_with_tiers(&[("free-one", "c4", AccountTier::Free)]);
    free_only.set_allow_free_chat(true);
    assert!(free_only.lease_for(RequestKind::Chat, true).await.is_none());
}

#[tokio::test]
async fn round_robin_rotates_across_accounts() {
    let pool = pool(
        &[("alpha", "c1"), ("beta", "c2"), ("gamma", "c3")],
        BalanceStrategy::RoundRobin,
    );

    let mut order = Vec::new();
    for _ in 0..4 {
        let lease = pool.lease().await.expect("healthy account");
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
        let lease = pool.lease().await.expect("healthy account");
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

    let lease = pool.lease().await.expect("healthy account");
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
    let first = pool.lease().await.expect("healthy account");
    assert_eq!(first.account.name, "only");

    let handle = tokio::spawn({
        let pool = Arc::new(pool);
        async move {
            let lease = pool.lease().await.expect("healthy account");
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

#[tokio::test]
async fn five_consecutive_failures_park_the_account_for_review() {
    let pool = pool(
        &[("alpha", "c1"), ("beta", "c2")],
        BalanceStrategy::RoundRobin,
    );
    let alpha = pool.accounts()[0].clone();
    for _ in 0..4 {
        alpha.note_failure(FailureKind::Transient, "network reset");
    }
    assert_eq!(alpha.consecutive_failures(), 4);
    assert!(!alpha.is_abnormal());

    alpha.note_failure(FailureKind::Transient, "network reset");
    assert!(alpha.is_abnormal());
    assert_eq!(alpha.consecutive_failures(), 5);
    assert!(!alpha.is_available());

    for _ in 0..4 {
        let lease = pool.lease().await.expect("beta keeps serving");
        assert_eq!(lease.account.name, "beta");
    }

    let status = pool.status().await;
    let alpha_status = status
        .accounts
        .iter()
        .find(|account| account.name == "alpha")
        .unwrap();
    assert_eq!(alpha_status.state, "abnormal");
    assert_eq!(alpha_status.consecutive_failures, 5);
    assert!(alpha_status.abnormal.is_some());
}

#[tokio::test]
async fn a_success_resets_the_consecutive_failure_count() {
    let pool = pool(&[("alpha", "c1")], BalanceStrategy::RoundRobin);
    let alpha = pool.accounts()[0].clone();
    for _ in 0..4 {
        alpha.note_failure(FailureKind::Transient, "network reset");
    }
    pool.note_success(&alpha);
    assert_eq!(alpha.consecutive_failures(), 0);
    assert!(!alpha.is_abnormal());

    for _ in 0..4 {
        alpha.note_failure(FailureKind::Transient, "network reset");
    }
    assert!(!alpha.is_abnormal());
}

#[tokio::test]
async fn abnormal_account_is_not_used_as_the_cooldown_fallback() {
    let pool = pool(&[("only", "c1")], BalanceStrategy::RoundRobin);
    let only = pool.accounts()[0].clone();
    for _ in 0..5 {
        only.note_failure(FailureKind::Quota, "HTTP 429");
    }
    assert!(pool.lease().await.is_none());

    pool.clear_abnormal("only").expect("account exists");
    assert!(pool.lease().await.is_some());
}

#[tokio::test]
async fn parked_model_failures_do_not_park_the_account() {
    let pool = pool(&[("only", "c1")], BalanceStrategy::RoundRobin);
    let only = pool.accounts()[0].clone();
    for _ in 0..5 {
        only.note_failure(FailureKind::Model, "model_parked: upstream queue");
    }
    assert!(!only.is_abnormal());
}

#[tokio::test]
async fn network_failures_do_not_park_the_account() {
    let pool = pool(&[("only", "c1")], BalanceStrategy::RoundRobin);
    let only = pool.accounts()[0].clone();
    for _ in 0..20 {
        only.note_failure(
            FailureKind::Network,
            "Gemini Web request failed: The connection failed.",
        );
    }
    assert_eq!(only.consecutive_failures(), 0);
    assert!(!only.is_abnormal());
}

#[tokio::test]
async fn free_accounts_only_serve_video_until_the_switch_is_on() {
    let mut free_session = session("c1");
    free_session.tier = AccountTier::Free;
    let pool = AccountPool::from_sessions(
        vec![("free-one".to_string(), free_session)],
        BalanceStrategy::RoundRobin,
    )
    .unwrap();

    assert!(pool.lease_for(RequestKind::Chat, false).await.is_none());
    let video = pool.lease_for(RequestKind::Video, false).await.expect("video allowed");
    assert_eq!(video.account.name, "free-one");
    drop(video);

    pool.set_allow_free_chat(true);
    let chat = pool.lease_for(RequestKind::Chat, false).await.expect("free chat on");
    assert_eq!(chat.account.tier(), AccountTier::Free);
}

#[tokio::test]
async fn pro_accounts_serve_chat_and_video() {
    let pool = pool(&[("pro-one", "c1")], BalanceStrategy::RoundRobin);
    let chat = pool.lease_for(RequestKind::Chat, false).await.expect("chat allowed");
    assert_eq!(chat.account.tier(), AccountTier::Pro);
    drop(chat);
    assert!(pool.lease_for(RequestKind::Video, false).await.is_some());
}

