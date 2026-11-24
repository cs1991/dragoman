use crate::session::SessionData;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Quota,
    Auth,
    Transient,
    Context,
    Model,
}

impl FailureKind {
    pub fn cooldown(self) -> Duration {
        match self {
            Self::Quota => Duration::from_secs(300),
            Self::Auth => Duration::from_secs(1800),
            Self::Transient => Duration::from_secs(30),
            Self::Context => Duration::from_secs(60),
            Self::Model => Duration::from_secs(120),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::Auth => "auth",
            Self::Transient => "transient",
            Self::Context => "context",
            Self::Model => "model",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceStrategy {
    RoundRobin,
    LeastUsed,
}

impl BalanceStrategy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "round-robin" | "round_robin" | "rr" => Some(Self::RoundRobin),
            "least-used" | "least_used" | "least" => Some(Self::LeastUsed),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::RoundRobin => "round-robin",
            Self::LeastUsed => "least-used",
        }
    }
}

#[derive(Debug, Default)]
struct AccountRuntime {
    cooldown_until: Option<Instant>,
    last_error: Option<String>,
    served: u64,
    failures: u32,
}

pub struct Account {
    pub name: String,
    pub session: Arc<Mutex<SessionData>>,
    turn: Arc<Mutex<()>>,
    runtime: StdMutex<AccountRuntime>,
}

impl Account {
    pub fn new(name: impl Into<String>, session: SessionData) -> Self {
        Self {
            name: name.into(),
            session: Arc::new(Mutex::new(session)),
            turn: Arc::new(Mutex::new(())),
            runtime: StdMutex::new(AccountRuntime::default()),
        }
    }

    fn runtime(&self) -> std::sync::MutexGuard<'_, AccountRuntime> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn is_available(&self) -> bool {
        let runtime = self.runtime();
        runtime
            .cooldown_until
            .map(|until| until <= Instant::now())
            .unwrap_or(true)
    }

    fn cooldown_remaining(&self) -> Option<Duration> {
        let runtime = self.runtime();
        runtime
            .cooldown_until
            .map(|until| until.saturating_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
    }

    fn served_count(&self) -> u64 {
        self.runtime().served
    }

    pub fn note_success(&self) {
        let mut runtime = self.runtime();
        runtime.served = runtime.served.saturating_add(1);
        runtime.failures = 0;
        runtime.cooldown_until = None;
        runtime.last_error = None;
    }

    pub fn note_failure(&self, kind: FailureKind, message: &str) {
        let mut runtime = self.runtime();
        runtime.failures = runtime.failures.saturating_add(1);
        runtime.cooldown_until = Some(Instant::now() + kind.cooldown());
        runtime.last_error = Some(format!("{}: {}", kind.label(), truncate(message, 200)));
    }

    pub async fn status(&self) -> AccountStatus {
        let (model, has_conversation) = {
            let session = self.session.lock().await;
            (session.model.clone(), session.conversation_id.is_some())
        };
        let runtime = self.runtime();
        let cooldown_seconds = runtime
            .cooldown_until
            .map(|until| until.saturating_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.as_secs().saturating_add(1))
            .unwrap_or(0);
        AccountStatus {
            name: self.name.clone(),
            model,
            has_conversation,
            served: runtime.served,
            failures: runtime.failures,
            cooldown_seconds,
            last_error: runtime.last_error.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountStatus {
    pub name: String,
    pub model: String,
    pub has_conversation: bool,
    pub served: u64,
    pub failures: u32,
    pub cooldown_seconds: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolStatus {
    pub strategy: String,
    pub total_served: u64,
    pub accounts: Vec<AccountStatus>,
}

pub struct Lease {
    pub account: Arc<Account>,
    _guard: OwnedMutexGuard<()>,
}

pub struct AccountPool {
    accounts: Vec<Arc<Account>>,
    strategy: BalanceStrategy,
    cursor: AtomicUsize,
    total_served: AtomicU64,
}

impl AccountPool {
    pub fn from_sessions(
        entries: Vec<(String, SessionData)>,
        strategy: BalanceStrategy,
    ) -> Result<Self, String> {
        if entries.is_empty() {
            return Err(
                "No Gemini account sessions configured. Run 'dragoman login --account NAME' first."
                    .to_string(),
            );
        }
        let accounts = entries
            .into_iter()
            .map(|(name, session)| Arc::new(Account::new(name, session)))
            .collect();
        Ok(Self {
            accounts,
            strategy,
            cursor: AtomicUsize::new(0),
            total_served: AtomicU64::new(0),
        })
    }

    pub fn single(session: SessionData) -> Self {
        Self {
            accounts: vec![Arc::new(Account::new("default", session))],
            strategy: BalanceStrategy::RoundRobin,
            cursor: AtomicUsize::new(0),
            total_served: AtomicU64::new(0),
        }
    }

    /// Load every `*.json` under `~/.config/dragoman/sessions` plus the legacy
    /// `session.json`. Duplicate cookies are collapsed so one Google account is
    /// not counted twice.
    pub fn load(strategy: BalanceStrategy) -> Result<Self, String> {
        let sessions_dir = SessionData::sessions_dir();
        let mut entries: Vec<(String, SessionData)> = Vec::new();
        let mut cookies: Vec<String> = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(&sessions_dir) {
            let mut paths: Vec<_> = read_dir
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                })
                .collect();
            paths.sort();

            for path in paths {
                let name = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("account")
                    .to_string();
                match SessionData::load_from(&path) {
                    Ok(session) => {
                        if cookies.contains(&session.cookie) {
                            continue;
                        }
                        cookies.push(session.cookie.clone());
                        entries.push((name, session));
                    }
                    Err(error) => {
                        tracing::warn!("Skipping account file {}: {}", path.display(), error);
                    }
                }
            }
        }

        let legacy = SessionData::config_path();
        if legacy.exists() {
            match SessionData::load_from(&legacy) {
                Ok(session) => {
                    if !cookies.contains(&session.cookie) {
                        cookies.push(session.cookie.clone());
                        entries.push(("default".to_string(), session));
                    }
                }
                Err(error) => tracing::warn!("Skipping legacy session: {}", error),
            }
        }

        if entries.is_empty() {
            return Err(format!(
                "No account sessions found in {} or {}. Run 'dragoman login --account NAME' first.",
                sessions_dir.display(),
                legacy.display()
            ));
        }

        Self::from_sessions(entries, strategy)
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn strategy(&self) -> BalanceStrategy {
        self.strategy
    }

    pub fn accounts(&self) -> &[Arc<Account>] {
        &self.accounts
    }

    fn start_index(&self) -> usize {
        let len = self.accounts.len();
        if len == 0 {
            return 0;
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % len;
        if self.strategy == BalanceStrategy::LeastUsed {
            let mut best = start;
            let mut best_served = u64::MAX;
            for offset in 0..len {
                let index = (start + offset) % len;
                let account = &self.accounts[index];
                if !account.is_available() {
                    continue;
                }
                let served = account.served_count();
                if served < best_served {
                    best_served = served;
                    best = index;
                }
            }
            return best;
        }
        start
    }

    /// Reserve one account for the whole turn. Prefers healthy and idle
    /// accounts, falls back to a healthy busy account, then to the account whose
    /// cooldown ends first.
    pub async fn lease(&self) -> Lease {
        let len = self.accounts.len();
        let start = self.start_index();

        for offset in 0..len {
            let index = (start + offset) % len;
            let account = &self.accounts[index];
            if !account.is_available() {
                continue;
            }
            if let Ok(guard) = account.turn.clone().try_lock_owned() {
                return Lease {
                    account: account.clone(),
                    _guard: guard,
                };
            }
        }

        for offset in 0..len {
            let index = (start + offset) % len;
            let account = &self.accounts[index];
            if account.is_available() {
                let guard = account.turn.clone().lock_owned().await;
                return Lease {
                    account: account.clone(),
                    _guard: guard,
                };
            }
        }

        let index = (0..len)
            .min_by_key(|index| {
                self.accounts[*index]
                    .cooldown_remaining()
                    .map(|remaining| remaining.as_millis())
                    .unwrap_or(0)
            })
            .unwrap_or(start);
        let account = self.accounts[index].clone();
        let guard = account.turn.clone().lock_owned().await;
        Lease {
            account,
            _guard: guard,
        }
    }

    pub fn note_success(&self, account: &Account) {
        account.note_success();
        self.total_served.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn status(&self) -> PoolStatus {
        let mut accounts = Vec::with_capacity(self.accounts.len());
        for account in &self.accounts {
            accounts.push(account.status().await);
        }
        PoolStatus {
            strategy: self.strategy.label().to_string(),
            total_served: self.total_served.load(Ordering::Relaxed),
            accounts,
        }
    }
}

pub fn file_name_for_account(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Account name is empty".to_string());
    }
    if !trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(format!(
            "Account name may contain letters, digits, dash, underscore and dot only: {trimmed}"
        ));
    }
    Ok(trimmed.to_string())
}

pub fn session_path_for_account(name: &str) -> Result<std::path::PathBuf, String> {
    let file_name = file_name_for_account(name)?;
    Ok(SessionData::sessions_dir().join(format!("{file_name}.json")))
}

pub fn account_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("account")
        .to_string()
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut result: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        result.push_str("...");
    }
    result
}
