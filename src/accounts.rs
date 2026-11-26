use crate::session::SessionData;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, OwnedMutexGuard};

/// Consecutive failed turns after which an account is parked as abnormal.
///
/// An abnormal account is never leased again until someone reviews it. A
/// successful sign-in for the same account name, or an explicit clear, removes
/// the mark.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 5;

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

/// Why an account was parked for manual review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbnormalState {
    pub reason: String,
    pub kind: String,
    pub error: String,
    pub since: u64,
    pub consecutive_failures: u32,
}

impl AbnormalState {
    pub fn summary(&self) -> String {
        format!(
            "{} after {} consecutive failures at unix {}: {}",
            self.kind, self.consecutive_failures, self.since, self.error
        )
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Path of the persisted abnormal-account marks.
pub fn health_path() -> PathBuf {
    SessionData::config_dir().join("account-health.json")
}

/// Small JSON map from account name to its abnormal mark.
///
/// The session files stay untouched, so a mark never collides with login state.
/// `ephemeral()` keeps the store in memory for tests and single-account clients.
pub struct HealthStore {
    path: Option<PathBuf>,
    entries: StdMutex<BTreeMap<String, AbnormalState>>,
}

impl HealthStore {
    pub fn load(path: PathBuf) -> Arc<Self> {
        let entries = read_health_map(&path);
        Arc::new(Self {
            path: Some(path),
            entries: StdMutex::new(entries),
        })
    }

    pub fn ephemeral() -> Arc<Self> {
        Arc::new(Self {
            path: None,
            entries: StdMutex::new(BTreeMap::new()),
        })
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, AbnormalState>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn get(&self, name: &str) -> Option<AbnormalState> {
        self.map().get(name).cloned()
    }

    pub fn snapshot(&self) -> BTreeMap<String, AbnormalState> {
        self.map().clone()
    }

    /// Adopt marks written or removed by another process, such as a manual
    /// sign-in or `dragoman accounts --clear` while the proxy is running.
    pub fn reload_from_disk(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        // A mark file is replaced atomically by this process and by the login
        // command. Ignore a missing or half-written file instead of dropping
        // every mark on a transient read.
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(fresh) = serde_json::from_str::<BTreeMap<String, AbnormalState>>(&text) else {
            return;
        };
        let mut entries = self.map();
        if *entries != fresh {
            *entries = fresh;
        }
    }

    pub fn mark(&self, name: &str, state: AbnormalState) {
        {
            let mut entries = self.map();
            entries.insert(name.to_string(), state);
        }
        self.persist();
    }

    pub fn clear(&self, name: &str) {
        let removed = {
            let mut entries = self.map();
            entries.remove(name).is_some()
        };
        if removed {
            self.persist();
        }
    }

    fn persist(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let snapshot = self.snapshot();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = match serde_json::to_string_pretty(&snapshot) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!("Could not serialize account health marks: {}", error);
                return;
            }
        };
        let temp = path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&temp, json) {
            tracing::warn!(
                "Could not write account health marks to {}: {}",
                temp.display(),
                error
            );
            return;
        }
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        if let Err(error) = std::fs::rename(&temp, path) {
            tracing::warn!(
                "Could not replace account health marks at {}: {}",
                path.display(),
                error
            );
        }
    }
}

fn read_health_map(path: &Path) -> BTreeMap<String, AbnormalState> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_str(&text) {
        Ok(map) => map,
        Err(error) => {
            tracing::warn!(
                "Ignoring unreadable account health marks at {}: {}",
                path.display(),
                error
            );
            BTreeMap::new()
        }
    }
}

/// Remove the abnormal mark for one account name. Returns true when a mark was
/// present. A successful sign-in calls this so a repaired account is usable on
/// the next request.
pub fn clear_account_health(name: &str) -> bool {
    let store = HealthStore::load(health_path());
    let existed = store.get(name).is_some();
    if existed {
        store.clear(name);
    }
    existed
}

#[derive(Debug, Default)]
struct AccountRuntime {
    cooldown_until: Option<Instant>,
    last_error: Option<String>,
    served: u64,
    failures: u32,
    consecutive_failures: u32,
    abnormal: Option<AbnormalState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFingerprint {
    snlm0e: String,
    f_sid: String,
    bl: String,
    cookie: String,
    proxy: Option<String>,
    local_ip: Option<String>,
    base_url: Option<String>,
}

impl SessionFingerprint {
    fn from_session(session: &SessionData) -> Self {
        Self {
            snlm0e: session.snlm0e.clone(),
            f_sid: session.f_sid.clone(),
            bl: session.bl.clone(),
            cookie: session.cookie.clone(),
            proxy: session.proxy.clone(),
            local_ip: session.local_ip.clone(),
            base_url: session.base_url.clone(),
        }
    }

    /// True when the sign-in identity changed, which means a re-login happened.
    fn credentials_changed(&self, other: &Self) -> bool {
        self.cookie != other.cookie
            || self.snlm0e != other.snlm0e
            || self.f_sid != other.f_sid
            || self.bl != other.bl
    }
}

pub struct Account {
    pub name: String,
    pub session: Arc<Mutex<SessionData>>,
    /// HTTP client for this account. It carries the account proxy when one is
    /// configured, so uploads and StreamGenerate calls leave from the same
    /// address.
    pub http: reqwest::Client,
    turn: Arc<Mutex<()>>,
    runtime: StdMutex<AccountRuntime>,
    health: Arc<HealthStore>,
    fingerprint: SessionFingerprint,
}

impl Account {
    pub fn new(name: impl Into<String>, session: SessionData) -> Self {
        Self::with_health(name, session, HealthStore::ephemeral(), None)
    }

    pub fn with_health(
        name: impl Into<String>,
        session: SessionData,
        health: Arc<HealthStore>,
        abnormal: Option<AbnormalState>,
    ) -> Self {
        let http = build_http_client(&session);
        let fingerprint = SessionFingerprint::from_session(&session);
        let mut runtime = AccountRuntime::default();
        if let Some(state) = abnormal {
            runtime.consecutive_failures = state.consecutive_failures;
            runtime.abnormal = Some(state);
        }
        Self {
            name: name.into(),
            http,
            session: Arc::new(Mutex::new(session)),
            turn: Arc::new(Mutex::new(())),
            runtime: StdMutex::new(runtime),
            health,
            fingerprint,
        }
    }

    fn runtime(&self) -> std::sync::MutexGuard<'_, AccountRuntime> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn is_available(&self) -> bool {
        let runtime = self.runtime();
        if runtime.abnormal.is_some() {
            return false;
        }
        runtime
            .cooldown_until
            .map(|until| until <= Instant::now())
            .unwrap_or(true)
    }

    pub fn is_abnormal(&self) -> bool {
        self.runtime().abnormal.is_some()
    }

    pub fn abnormal_state(&self) -> Option<AbnormalState> {
        self.runtime().abnormal.clone()
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.runtime().consecutive_failures
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

    pub fn served(&self) -> u64 {
        self.served_count()
    }

    /// Copy rotation counters from a replaced account. The abnormal flag is not
    /// copied, because it is owned by the health store.
    fn adopt_runtime(&self, previous: &Account) {
        let previous_runtime = previous.runtime();
        let mut runtime = self.runtime();
        runtime.served = previous_runtime.served;
        runtime.failures = previous_runtime.failures;
        runtime.consecutive_failures = previous_runtime.consecutive_failures;
        runtime.cooldown_until = previous_runtime.cooldown_until;
        runtime.last_error = previous_runtime.last_error.clone();
    }

    /// Keep the in-memory mark aligned with the shared health store.
    fn sync_abnormal(&self) {
        let stored = self.health.get(&self.name);
        let mut runtime = self.runtime();
        match stored {
            Some(state) => {
                if runtime.abnormal.as_ref() != Some(&state) {
                    runtime.consecutive_failures =
                        runtime.consecutive_failures.max(state.consecutive_failures);
                    runtime.abnormal = Some(state);
                }
            }
            None => {
                if runtime.abnormal.take().is_some() {
                    runtime.consecutive_failures = 0;
                    runtime.cooldown_until = None;
                    runtime.last_error = None;
                }
            }
        }
    }

    pub fn note_success(&self) {
        let mut runtime = self.runtime();
        runtime.served = runtime.served.saturating_add(1);
        runtime.failures = 0;
        runtime.consecutive_failures = 0;
        runtime.cooldown_until = None;
        runtime.last_error = None;
        let was_abnormal = runtime.abnormal.take().is_some();
        drop(runtime);
        if was_abnormal {
            self.health.clear(&self.name);
        }
    }

    pub fn note_failure(&self, kind: FailureKind, message: &str) {
        let mut marked = None;
        {
            let mut runtime = self.runtime();
            runtime.failures = runtime.failures.saturating_add(1);
            // A parked model is an upstream capacity problem, not a broken
            // account, so it does not move the review counter.
            if kind != FailureKind::Model {
                runtime.consecutive_failures = runtime.consecutive_failures.saturating_add(1);
            }
            runtime.cooldown_until = Some(Instant::now() + kind.cooldown());
            runtime.last_error = Some(format!("{}: {}", kind.label(), truncate(message, 200)));
            if runtime.abnormal.is_none()
                && runtime.consecutive_failures >= MAX_CONSECUTIVE_FAILURES
            {
                let state = AbnormalState {
                    reason: format!("consecutive_failures>={MAX_CONSECUTIVE_FAILURES}"),
                    kind: kind.label().to_string(),
                    error: truncate(message, 300),
                    since: now_unix(),
                    consecutive_failures: runtime.consecutive_failures,
                };
                runtime.abnormal = Some(state.clone());
                marked = Some(state);
            }
        }
        if let Some(state) = marked {
            tracing::warn!(
                "Account {} parked as abnormal after {} consecutive failures: {}",
                self.name,
                state.consecutive_failures,
                state.error
            );
            self.health.mark(&self.name, state);
        }
    }

    /// Clear the abnormal mark and every cooldown for this account.
    pub fn clear_abnormal(&self) {
        let had = {
            let mut runtime = self.runtime();
            let had = runtime.abnormal.take().is_some();
            runtime.consecutive_failures = 0;
            runtime.failures = 0;
            runtime.cooldown_until = None;
            runtime.last_error = None;
            had
        };
        if had || self.health.get(&self.name).is_some() {
            self.health.clear(&self.name);
        }
    }

    pub async fn status(&self) -> AccountStatus {
        let (model, has_conversation, proxy, local_ip) = {
            let session = self.session.lock().await;
            (
                session.model.clone(),
                session.conversation_id.is_some(),
                session.proxy.as_deref().map(redact_proxy),
                session.local_ip.clone(),
            )
        };
        let runtime = self.runtime();
        let cooldown_seconds = runtime
            .cooldown_until
            .map(|until| until.saturating_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.as_secs().saturating_add(1))
            .unwrap_or(0);
        let state = if runtime.abnormal.is_some() {
            "abnormal"
        } else if cooldown_seconds > 0 {
            "cooling"
        } else {
            "ok"
        };
        AccountStatus {
            name: self.name.clone(),
            model,
            proxy,
            local_ip,
            has_conversation,
            served: runtime.served,
            failures: runtime.failures,
            consecutive_failures: runtime.consecutive_failures,
            cooldown_seconds,
            state: state.to_string(),
            last_error: runtime.last_error.clone(),
            abnormal: runtime.abnormal.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountStatus {
    pub name: String,
    pub model: String,
    pub proxy: Option<String>,
    pub local_ip: Option<String>,
    pub has_conversation: bool,
    pub served: u64,
    pub failures: u32,
    pub consecutive_failures: u32,
    pub cooldown_seconds: u64,
    pub state: String,
    pub last_error: Option<String>,
    pub abnormal: Option<AbnormalState>,
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

#[derive(Debug, Clone)]
struct WatchPaths {
    sessions_dir: PathBuf,
    legacy: Option<PathBuf>,
    health: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirSignature(Vec<(String, u64, u128)>);

#[derive(Debug, Clone, Copy)]
pub struct PoolReload {
    pub total: usize,
    pub added: usize,
    pub removed: usize,
    pub refreshed: usize,
}

pub struct AccountPool {
    accounts: RwLock<Vec<Arc<Account>>>,
    strategy: BalanceStrategy,
    cursor: AtomicUsize,
    total_served: AtomicU64,
    health: Arc<HealthStore>,
    watch: Option<WatchPaths>,
    signature: StdMutex<Option<DirSignature>>,
    last_check: StdMutex<Option<Instant>>,
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
        let health = HealthStore::ephemeral();
        let accounts = entries
            .into_iter()
            .map(|(name, session)| Arc::new(Account::new(name, session)))
            .collect();
        Ok(Self {
            accounts: RwLock::new(accounts),
            strategy,
            cursor: AtomicUsize::new(0),
            total_served: AtomicU64::new(0),
            health,
            watch: None,
            signature: StdMutex::new(None),
            last_check: StdMutex::new(None),
        })
    }

    pub fn single(session: SessionData) -> Self {
        Self {
            accounts: RwLock::new(vec![Arc::new(Account::new("default", session))]),
            strategy: BalanceStrategy::RoundRobin,
            cursor: AtomicUsize::new(0),
            total_served: AtomicU64::new(0),
            health: HealthStore::ephemeral(),
            watch: None,
            signature: StdMutex::new(None),
            last_check: StdMutex::new(None),
        }
    }

    /// Load every `*.json` under `~/.config/dragoman/sessions` plus the legacy
    /// `session.json`. Duplicate cookies are collapsed so one Google account is
    /// not counted twice. The pool watches those paths and reloads itself when
    /// files are added, replaced, disabled or removed.
    pub fn load(strategy: BalanceStrategy) -> Result<Self, String> {
        Self::load_with(
            SessionData::sessions_dir(),
            Some(SessionData::config_path()),
            health_path(),
            strategy,
        )
    }

    /// Load one sessions directory without the legacy file. Tests use this to
    /// keep the health marks inside the same temporary directory.
    pub fn load_dir(sessions_dir: &Path, strategy: BalanceStrategy) -> Result<Self, String> {
        Self::load_with(
            sessions_dir.to_path_buf(),
            None,
            sessions_dir.join("account-health.json"),
            strategy,
        )
    }

    fn load_with(
        sessions_dir: PathBuf,
        legacy: Option<PathBuf>,
        health_file: PathBuf,
        strategy: BalanceStrategy,
    ) -> Result<Self, String> {
        let health = HealthStore::load(health_file.clone());
        let discovery = discover_sessions(&sessions_dir, legacy.as_deref());
        if discovery.entries.is_empty() {
            let mut searched = sessions_dir.display().to_string();
            if let Some(legacy) = legacy.as_ref() {
                searched.push_str(&format!(" or {}", legacy.display()));
            }
            return Err(format!(
                "No account sessions found in {searched}. Run 'dragoman login --account NAME' first."
            ));
        }
        let accounts = discovery
            .entries
            .iter()
            .map(|(name, session)| {
                let abnormal = health.get(name);
                Arc::new(Account::with_health(
                    name.clone(),
                    session.clone(),
                    health.clone(),
                    abnormal,
                ))
            })
            .collect();
        let signature = signature_of(&sessions_dir, legacy.as_deref(), Some(&health_file));
        Ok(Self {
            accounts: RwLock::new(accounts),
            strategy,
            cursor: AtomicUsize::new(0),
            total_served: AtomicU64::new(0),
            health,
            watch: Some(WatchPaths {
                sessions_dir,
                legacy,
                health: health_file,
            }),
            signature: StdMutex::new(Some(signature)),
            last_check: StdMutex::new(Some(Instant::now())),
        })
    }

    fn read_accounts(&self) -> std::sync::RwLockReadGuard<'_, Vec<Arc<Account>>> {
        self.accounts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_accounts(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Arc<Account>>> {
        self.accounts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn len(&self) -> usize {
        self.read_accounts().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read_accounts().is_empty()
    }

    pub fn strategy(&self) -> BalanceStrategy {
        self.strategy
    }

    pub fn accounts(&self) -> Vec<Arc<Account>> {
        self.read_accounts().clone()
    }

    pub fn health(&self) -> Arc<HealthStore> {
        self.health.clone()
    }

    pub fn clear_abnormal(&self, name: &str) -> Result<(), String> {
        let accounts = self.accounts();
        let account = accounts
            .iter()
            .find(|account| account.name == name)
            .ok_or_else(|| format!("Account {name} is not loaded"))?;
        account.clear_abnormal();
        Ok(())
    }

    pub fn clear_all_abnormal(&self) -> usize {
        let mut cleared = 0;
        for account in self.accounts() {
            if account.is_abnormal() || self.health.get(&account.name).is_some() {
                account.clear_abnormal();
                cleared += 1;
            }
        }
        cleared
    }

    /// Re-read the sessions directory when any file changed. The call is cheap
    /// when nothing changed and is throttled when called from `lease`.
    pub fn reload_if_changed(&self) -> Option<PoolReload> {
        let watch = self.watch.as_ref()?;
        self.reload_now(watch)
    }

    fn reload_if_changed_throttled(&self) -> Option<PoolReload> {
        let watch = self.watch.as_ref()?;
        {
            let mut last_check = self
                .last_check
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(at) = *last_check
                && at.elapsed() < Duration::from_millis(1500)
            {
                return None;
            }
            *last_check = Some(Instant::now());
        }
        self.reload_now(watch)
    }

    fn reload_now(&self, watch: &WatchPaths) -> Option<PoolReload> {
        let signature = signature_of(
            &watch.sessions_dir,
            watch.legacy.as_deref(),
            Some(&watch.health),
        );
        {
            let mut current = self
                .signature
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if current.as_ref() == Some(&signature) {
                return None;
            }
            *current = Some(signature);
        }

        self.health.reload_from_disk();
        let discovery = discover_sessions(&watch.sessions_dir, watch.legacy.as_deref());
        let mut previous: BTreeMap<String, Arc<Account>> = self
            .accounts()
            .into_iter()
            .map(|account| (account.name.clone(), account))
            .collect();
        let mut next: Vec<Arc<Account>> = Vec::new();
        let mut added = 0usize;
        let mut refreshed = 0usize;

        for (name, session) in discovery.entries {
            let fingerprint = SessionFingerprint::from_session(&session);
            if let Some(previous) = previous.remove(&name) {
                if previous.fingerprint == fingerprint {
                    next.push(previous);
                    continue;
                }
                if previous.fingerprint.credentials_changed(&fingerprint) {
                    // A new sign-in repairs the account, so the review mark is
                    // dropped as soon as the new credentials land.
                    self.health.clear(&name);
                }
                let abnormal = self.health.get(&name);
                let account = Arc::new(Account::with_health(
                    name,
                    session,
                    self.health.clone(),
                    abnormal,
                ));
                account.adopt_runtime(&previous);
                next.push(account);
                refreshed += 1;
            } else {
                let abnormal = self.health.get(&name);
                next.push(Arc::new(Account::with_health(
                    name,
                    session,
                    self.health.clone(),
                    abnormal,
                )));
                added += 1;
            }
        }

        let mut removed = 0usize;
        for (name, account) in previous {
            if discovery.seen_names.contains(&name) {
                // The file exists but is mid-write or unreadable. Keep the
                // working account and retry on the next change.
                next.push(account);
            } else {
                removed += 1;
            }
        }

        next.sort_by(|left, right| left.name.cmp(&right.name));
        for account in &next {
            account.sync_abnormal();
        }
        let total = next.len();
        *self.write_accounts() = next;

        tracing::info!(
            "Account pool reloaded: {} account(s), {} added, {} removed, {} refreshed",
            total,
            added,
            removed,
            refreshed
        );
        Some(PoolReload {
            total,
            added,
            removed,
            refreshed,
        })
    }

    /// Poll the sessions directory in the background. Accounts added, replaced,
    /// disabled or removed by another process take effect within one interval
    /// without restarting the proxy.
    pub fn spawn_watcher(
        self: &Arc<Self>,
        interval: Duration,
    ) -> Option<tokio::task::JoinHandle<()>> {
        self.watch.as_ref()?;
        let pool = Arc::clone(self);
        Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                pool.reload_if_changed();
            }
        }))
    }

    fn start_index(&self, accounts: &[Arc<Account>]) -> usize {
        let len = accounts.len();
        if len == 0 {
            return 0;
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % len;
        if self.strategy == BalanceStrategy::LeastUsed {
            let mut best = start;
            let mut best_served = u64::MAX;
            for offset in 0..len {
                let index = (start + offset) % len;
                let account = &accounts[index];
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
    /// accounts, falls back to a healthy busy account, then to the healthy
    /// account whose cooldown ends first. Returns `None` when every account is
    /// marked abnormal, so the caller can answer with a clear error instead of
    /// burning a broken account.
    pub async fn lease(&self) -> Option<Lease> {
        self.reload_if_changed_throttled();
        let accounts = self.accounts();
        let len = accounts.len();
        if len == 0 {
            return None;
        }
        let start = self.start_index(&accounts);

        for offset in 0..len {
            let index = (start + offset) % len;
            let account = &accounts[index];
            if !account.is_available() {
                continue;
            }
            if let Ok(guard) = account.turn.clone().try_lock_owned() {
                return Some(Lease {
                    account: account.clone(),
                    _guard: guard,
                });
            }
        }

        for offset in 0..len {
            let index = (start + offset) % len;
            let account = &accounts[index];
            if account.is_available() {
                let guard = account.turn.clone().lock_owned().await;
                return Some(Lease {
                    account: account.clone(),
                    _guard: guard,
                });
            }
        }

        let index = (0..len)
            .filter(|index| !accounts[*index].is_abnormal())
            .min_by_key(|index| {
                accounts[*index]
                    .cooldown_remaining()
                    .map(|remaining| remaining.as_millis())
                    .unwrap_or(0)
            })?;
        let account = accounts[index].clone();
        let guard = account.turn.clone().lock_owned().await;
        Some(Lease {
            account,
            _guard: guard,
        })
    }

    pub fn note_success(&self, account: &Account) {
        account.note_success();
        self.total_served.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn status(&self) -> PoolStatus {
        self.reload_if_changed_throttled();
        let accounts = self.accounts();
        let mut statuses = Vec::with_capacity(accounts.len());
        for account in &accounts {
            statuses.push(account.status().await);
        }
        PoolStatus {
            strategy: self.strategy.label().to_string(),
            total_served: self.total_served.load(Ordering::Relaxed),
            accounts: statuses,
        }
    }
}

struct Discovery {
    entries: Vec<(String, SessionData)>,
    seen_names: BTreeSet<String>,
}

fn session_files(sessions_dir: &Path) -> Vec<PathBuf> {
    let Ok(read_dir) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let health_file = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.eq_ignore_ascii_case("account-health.json")
                        || name.eq_ignore_ascii_case("account-health.json.tmp")
                });
            path.is_file()
                && !health_file
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        })
        .collect();
    paths.sort();
    paths
}

fn discover_sessions(sessions_dir: &Path, legacy: Option<&Path>) -> Discovery {
    let mut entries: Vec<(String, SessionData)> = Vec::new();
    let mut seen_names: BTreeSet<String> = BTreeSet::new();
    let mut cookies: HashSet<String> = HashSet::new();

    for path in session_files(sessions_dir) {
        let name = account_name_from_path(&path);
        seen_names.insert(name.clone());
        match SessionData::load_from(&path) {
            Ok(session) => {
                if cookies.insert(session.cookie.clone()) {
                    entries.push((name, session));
                }
            }
            Err(error) => tracing::warn!("Skipping account file {}: {}", path.display(), error),
        }
    }

    if let Some(legacy) = legacy
        && legacy.is_file()
    {
        seen_names.insert("default".to_string());
        match SessionData::load_from(legacy) {
            Ok(session) => {
                if cookies.insert(session.cookie.clone()) {
                    entries.push(("default".to_string(), session));
                }
            }
            Err(error) => tracing::warn!("Skipping legacy session: {}", error),
        }
    }

    Discovery {
        entries,
        seen_names,
    }
}

fn signature_of(sessions_dir: &Path, legacy: Option<&Path>, health: Option<&Path>) -> DirSignature {
    let mut paths = session_files(sessions_dir);
    if let Some(legacy) = legacy
        && legacy.is_file()
    {
        paths.push(legacy.to_path_buf());
    }
    if let Some(health) = health
        && health.is_file()
    {
        paths.push(health.to_path_buf());
    }
    let mut rows: Vec<(String, u64, u128)> = paths
        .into_iter()
        .map(|path| {
            let key = path.to_string_lossy().to_string();
            match std::fs::metadata(&path) {
                Ok(metadata) => (key, metadata.len(), modified_nanos(&metadata)),
                Err(_) => (key, 0, 0),
            }
        })
        .collect();
    rows.sort();
    DirSignature(rows)
}

fn modified_nanos(metadata: &std::fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0)
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

/// Build the per-account HTTP client.
///
/// The client keeps the long read timeout that video analysis needs, binds an
/// optional local source address and adds a router proxy when the session names
/// one. A bare `127.0.0.1:7897` is treated as an HTTP proxy, which is what a
/// Clash Verge mixed listener speaks. Source address plus `SRC-IP-CIDR` rules
/// gives every account its own node on one shared port.
pub fn build_http_client(session: &SessionData) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(900));

    if let Some(raw) = session
        .local_ip
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        match raw.parse::<std::net::IpAddr>() {
            Ok(address) => builder = builder.local_address(address),
            Err(error) => tracing::warn!("Account local_ip {raw} was ignored: {error}"),
        }
    }

    if let Some(raw) = session
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let url = normalize_proxy_url(raw);
        match reqwest::Proxy::all(&url) {
            // Disable the system proxy for this account. Without this the
            // Windows registry proxy wins and every account leaves from the
            // same address, which defeats the per-account listener.
            Ok(proxy) => builder = builder.no_proxy().proxy(proxy),
            Err(error) => {
                tracing::warn!("Account proxy {raw} was ignored: {error}");
            }
        }
    }

    builder.build().unwrap_or_default()
}

fn normalize_proxy_url(raw: &str) -> String {
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}

/// Show `host:port` for status output, so a proxy password never reaches logs.
pub fn redact_proxy(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let host_part = without_scheme
        .split_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    host_part
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(host_part)
        .to_string()
}
