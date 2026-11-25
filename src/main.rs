use clap::{Parser, Subcommand};
use dragoman::accounts::{AccountPool, BalanceStrategy, session_path_for_account};
use dragoman::browser::{BrowserLauncher, CdpClient, default_profile_dir};
use dragoman::desktop_accounts::AccountStore;
use dragoman::login_batch::{BatchOptions, parse_credentials, run_batch};
use dragoman::proxy::server::{AppState, create_router};
use dragoman::session::SessionData;
use dragoman::web::GeminiWebClient;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser, Debug)]
#[command(name = "dragoman")]
#[command(author = "Khoi Pham Minh <khoipm06@gmail.com>")]
#[command(version = "0.1.0")]
#[command(about = "High-performance native OpenAI-compatible proxy for Gemini Web (Gemini 4 Argon)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the standalone OpenAI-compatible proxy server (no browser needed)
    Serve {
        /// HTTP listen port for external coding agents
        #[arg(short, long, default_value_t = 8080)]
        port: u16,

        /// Target model identifier reported to coding agents
        #[arg(short, long, default_value = "gemini-4-hydrogen")]
        model: String,

        /// Maximum attachment size in MiB for video, image and file parts
        #[arg(long, default_value_t = 200)]
        max_attachment_mb: u64,

        /// Account rotation strategy
        #[arg(long, default_value = "round-robin", value_parser = ["round-robin", "least-used"])]
        balance: String,

        #[arg(long, hide = true)]
        desktop: bool,
    },
    /// Perform one-time browser login to extract and save session credentials
    Login {
        /// Chrome DevTools Protocol (CDP) port used during login
        #[arg(long, default_value_t = 9222)]
        cdp_port: u16,

        /// Account name used for the saved session file
        #[arg(long, default_value = "main")]
        account: String,

        /// Chrome profile directory used for this login
        #[arg(long)]
        profile_dir: Option<std::path::PathBuf>,

        /// Egress proxy for this account, for example http://127.0.0.1:7901
        #[arg(long)]
        proxy: Option<String>,

        #[arg(long, hide = true)]
        desktop_id: Option<String>,
    },
    /// Sign in many Google accounts and save one Gemini session per account
    LoginBatch {
        /// Account file, one `email----password---` per line
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,

        /// Account lines passed directly, `\n` is accepted as a line break
        #[arg(long)]
        text: Option<String>,

        /// Egress proxy for every account, for example http://127.0.0.1:7901
        #[arg(long)]
        proxy: Option<String>,

        /// First account to log in, 1 based
        #[arg(long, default_value_t = 1)]
        start: usize,

        /// Maximum number of accounts to log in, 0 means all
        #[arg(long, default_value_t = 0)]
        limit: usize,

        /// Seconds to wait for a verification code from google111111.asia
        #[arg(long, default_value_t = 120)]
        code_timeout: u64,

        /// Leave every Chrome window open after its account finishes
        #[arg(long)]
        keep_open: bool,

        /// Parse and list the accounts without opening Chrome
        #[arg(long)]
        dry_run: bool,
    },
    /// List configured accounts and their rotation state
    Accounts,
    #[command(hide = true)]
    DesktopAccounts {
        #[command(subcommand)]
        action: AccountAction,
    },
    /// Test direct native HTTP RPC without browser
    TestRpc {
        #[arg(default_value = "Say hello and confirm you are Gemini 4 Argon in 1 sentence.")]
        prompt: String,
    },
}

#[derive(Subcommand, Debug)]
enum AccountAction {
    List,
    Enable {
        #[arg(long)]
        id: String,
    },
    Disable {
        #[arg(long)]
        id: String,
    },
    Remove {
        #[arg(long)]
        id: String,
        #[arg(long)]
        disabled: bool,
    },
    SetProxy {
        #[arg(long)]
        id: String,
        /// Proxy URL. An empty value clears the proxy.
        #[arg(long)]
        proxy: Option<String>,
        /// Local source address, for example 127.0.0.7. An empty value clears it.
        #[arg(long)]
        local_ip: Option<String>,
    },
    AssignProxies {
        /// First loopback host octet, so 2 means 127.0.0.2.
        #[arg(long, default_value_t = 2)]
        start_ip: u8,
        /// Shared Clash Verge mixed port.
        #[arg(long, default_value_t = 7897)]
        proxy_port: u16,
        /// Accounts that share one source address and one node.
        #[arg(long, default_value_t = 1)]
        per_ip: u16,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    fmt().with_env_filter(filter).init();

    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Serve {
        port: 8080,
        model: "gemini-4-hydrogen".to_string(),
        max_attachment_mb: 200,
        balance: "round-robin".to_string(),
        desktop: false,
    }) {
        Commands::Serve {
            port,
            model,
            max_attachment_mb,
            balance,
            desktop,
        } => {
            let strategy = BalanceStrategy::parse(&balance).unwrap_or(BalanceStrategy::RoundRobin);
            println!("=======================================================");
            println!(" Dragoman - Native Gemini Web OpenAI Bridge");
            println!(" Target Model: {}", model);
            println!(" Max Attachment: {} MiB", max_attachment_mb);
            println!(" Balance Strategy: {}", strategy.label());
            println!(" Mode: Standalone HTTP/2 Direct RPC (Zero Browser Runtime)");
            println!(
                " Proxy Endpoint: http://127.0.0.1:{}/v1/chat/completions",
                port
            );
            println!("=======================================================\n");

            let pool = match AccountPool::load(strategy) {
                Ok(pool) => pool,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    eprintln!(
                        "Please run 'dragoman login --account NAME' first to authenticate a Google AI Pro account."
                    );
                    std::process::exit(1);
                }
            };

            println!(
                " Loaded {} account session(s) from {}",
                pool.len(),
                SessionData::config_dir().display()
            );
            let web_client = Arc::new(GeminiWebClient::with_pool(Arc::new(pool)));
            let app_state = Arc::new(AppState {
                web_client,
                default_model: model,
                max_attachment_bytes: max_attachment_mb.saturating_mul(1024 * 1024),
            });

            let router = create_router(app_state);
            let ip = if desktop {
                [127, 0, 0, 1]
            } else {
                [0, 0, 0, 0]
            };
            let addr = SocketAddr::from((ip, port));

            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => listener,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    eprintln!(
                        "Port {} is already in use. Stop the process using it or choose another port with --port.",
                        port
                    );
                    std::process::exit(2);
                }
                Err(error) => return Err(error.into()),
            };
            info!("Dragoman listening on http://{}", listener.local_addr()?);
            if desktop {
                println!("DESKTOP_READY");
                axum::serve(listener, router)
                    .with_graceful_shutdown(async {
                        let _ = tokio::task::spawn_blocking(|| {
                            std::io::copy(&mut std::io::stdin(), &mut std::io::sink())
                        })
                        .await;
                    })
                    .await?;
            } else {
                axum::serve(listener, router).await?;
            }
        }
        Commands::Login {
            cdp_port,
            account,
            profile_dir,
            proxy,
            desktop_id,
        } => {
            if let Some(id) = desktop_id {
                let store = AccountStore::new(SessionData::config_dir());
                let path = store.login_path(&id)?;
                let profile = AccountStore::profile_dir(&id, &std::env::current_exe()?)?;
                let launch_error =
                    "Chrome 启动失败。请确认已安装 Chrome，并关闭占用了该账号配置目录的进程。";

                // One Chrome window stays open for the whole login. The monitor
                // polls the profile for a completed Google sign-in and attaches
                // DevTools only after the account cookies exist, so the sign-in
                // page itself never sees an active debugger.
                let had_session = BrowserLauncher::profile_has_google_session(&profile);
                let known_accounts = BrowserLauncher::profile_google_accounts(&profile);

                let (port, _chrome) =
                    BrowserLauncher::start_or_reuse_login(profile.clone(), proxy.as_deref())
                        .await
                        .map_err(|_| launch_error)?;
                println!("Chrome 已打开，正在监听登录状态。登录成功后自动添加账号，窗口保持打开。");

                let mut session: Option<SessionData> = None;
                if had_session {
                    session = capture_with_timeout(port, path.clone(), 15).await;
                }

                if session.is_none() {
                    session =
                        watch_login_window(port, path.clone(), &profile, &known_accounts).await;
                }

                let Some(mut session) = session else {
                    return Err(
                        "Chrome 已关闭。重新打开登录窗口，完成登录后账号会自动添加。".into(),
                    );
                };

                if let Ok(previous) = SessionData::load_from(&path) {
                    session.local_ip = previous.local_ip;
                    if proxy.is_none() {
                        session.proxy = previous.proxy;
                    }
                }

                if let Some(value) = proxy.filter(|value| !value.trim().is_empty()) {
                    session.proxy = Some(value);
                }
                session
                    .save()
                    .map_err(|_| "保存会话失败。请检查配置目录权限。")?;
                println!("SUCCESS: 账号已保存，Chrome 窗口保持打开。");
                return Ok(());
            }
            let session_path = match session_path_for_account(&account) {
                Ok(path) => path,
                Err(error) => {
                    eprintln!("Error: {}", error);
                    std::process::exit(1);
                }
            };
            println!("=== Dragoman Initial Login & Credential Extraction ===");
            println!("Account: {}", account);
            println!("Session file: {}", session_path.display());
            println!(
                "Ensuring Chrome is running with remote debugging on port {}...",
                cdp_port
            );

            let profile = match profile_dir.clone() {
                Some(dir) => dir,
                None => default_profile_dir()?,
            };
            let known_accounts = BrowserLauncher::profile_google_accounts(&profile);
            BrowserLauncher::ensure_chrome_running(cdp_port, profile_dir, proxy.as_deref())
                .await
                .map_err(|e| format!("Failed to launch browser: {}", e))?;
            println!("Chrome 已打开，正在监听登录状态。登录成功后自动保存，窗口保持打开。");

            let Some(mut session) =
                watch_login_window(cdp_port, session_path.clone(), &profile, &known_accounts).await
            else {
                return Err("Chrome 已关闭。重新运行 login，完成登录后会话会自动保存。".into());
            };
            if let Some(value) = proxy.filter(|value| !value.trim().is_empty()) {
                session.proxy = Some(value);
            }
            session.save()?;
            println!(
                "\nSUCCESS: Saved authentication session to {}",
                session_path.display()
            );
            println!("Chrome 窗口保持打开，关闭与否都不影响使用。");
            println!("Run 'dragoman serve' to launch the proxy without any browser running.");
        }
        Commands::LoginBatch {
            file,
            text,
            proxy,
            start,
            limit,
            code_timeout,
            keep_open,
            dry_run,
        } => {
            let raw = match (file, text) {
                (Some(path), _) => std::fs::read_to_string(&path)
                    .map_err(|error| format!("读取账号文件失败 {}: {}", path.display(), error))?,
                (None, Some(text)) => text.replace("\\n", "\n"),
                (None, None) => {
                    println!("粘贴账号，每行一个，格式：email----password---");
                    println!("粘贴完成后按 Ctrl+Z 再回车：");
                    let mut buffer = String::new();
                    std::io::stdin().read_to_string(&mut buffer)?;
                    buffer
                }
            };
            let mut credentials = parse_credentials(&raw)?;
            let skip = start.saturating_sub(1);
            if skip >= credentials.len() {
                return Err(format!("--start 超出范围：共有 {} 个账号", credentials.len()).into());
            }
            credentials.drain(..skip);
            if limit > 0 && credentials.len() > limit {
                credentials.truncate(limit);
            }

            println!("待登录账号：{} 个", credentials.len());
            if dry_run {
                for (index, credential) in credentials.iter().enumerate() {
                    println!(
                        "{}. {}  密码长度 {}",
                        index + 1,
                        credential.email,
                        credential.password.chars().count()
                    );
                }
                return Ok(());
            }

            let options = BatchOptions {
                proxy,
                keep_open,
                code_timeout: std::time::Duration::from_secs(code_timeout.max(10)),
            };
            let results = run_batch(&credentials, &options).await;
            let succeeded = results.iter().filter(|result| result.ok).count();
            let failed = results.len() - succeeded;
            let report_path = SessionData::config_dir().join("batch-login-report.json");
            if let Some(parent) = report_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&results) {
                let _ = std::fs::write(&report_path, json);
            }
            println!();
            println!("批量登录完成：成功 {}，失败 {}", succeeded, failed);
            println!("报告：{}", report_path.display());
            println!("会话目录：{}", SessionData::sessions_dir().display());
            if failed > 0 {
                std::process::exit(1);
            }
        }
        Commands::DesktopAccounts { action } => {
            let store = AccountStore::new(SessionData::config_dir());
            match action {
                AccountAction::List => println!("{}", serde_json::to_string(&store.list()?)?),
                AccountAction::Enable { id } => store.set_enabled(&id, true)?,
                AccountAction::Disable { id } => store.set_enabled(&id, false)?,
                AccountAction::Remove { id, disabled } => store.remove(&id, !disabled)?,
                AccountAction::SetProxy {
                    id,
                    proxy,
                    local_ip,
                } => {
                    store.set_route(&id, proxy.as_deref(), local_ip.as_deref())?;
                    println!("{}", serde_json::to_string(&store.list()?)?);
                }
                AccountAction::AssignProxies {
                    start_ip,
                    proxy_port,
                    per_ip,
                } => {
                    let assignments = store.assign_proxies(start_ip, proxy_port, per_ip)?;
                    println!("{}", serde_json::to_string(&assignments)?);
                }
            }
        }
        Commands::Accounts => {
            let pool = match AccountPool::load(BalanceStrategy::RoundRobin) {
                Ok(pool) => pool,
                Err(error) => {
                    eprintln!("Error: {}", error);
                    std::process::exit(1);
                }
            };
            let status = pool.status().await;
            println!("Config directory: {}", SessionData::config_dir().display());
            println!("Strategy: {}", status.strategy);
            println!("Total served: {}", status.total_served);
            println!();
            println!(
                "{:<20} {:<16} {:<22} {:<12} {:>7} {:>6} {:>9}  LAST ERROR",
                "NAME", "MODEL", "PROXY", "LOCAL IP", "SERVED", "FAILS", "COOLDOWN"
            );
            for account in &status.accounts {
                println!(
                    "{:<20} {:<16} {:<22} {:<12} {:>7} {:>6} {:>8}s  {}",
                    account.name,
                    account.model,
                    account.proxy.as_deref().unwrap_or("-"),
                    account.local_ip.as_deref().unwrap_or("-"),
                    account.served,
                    account.failures,
                    account.cooldown_seconds,
                    account.last_error.as_deref().unwrap_or("-")
                );
            }
        }
        Commands::TestRpc { prompt } => {
            let pool = AccountPool::load(BalanceStrategy::RoundRobin)
                .map_err(|e| format!("Could not load account sessions: {}", e))?;
            println!("Loaded {} account session(s)", pool.len());
            let client = GeminiWebClient::with_pool(Arc::new(pool));
            let (tx, mut rx) = tokio::sync::mpsc::channel(100);

            println!("Sending prompt: \"{}\"", prompt);
            tokio::spawn(async move {
                while let Some(res) = rx.recv().await {
                    match res {
                        Ok(delta) => {
                            print!("{}", delta);
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        }
                        Err(e) => {
                            eprintln!("\n[Stream Error]: {}", e);
                        }
                    }
                }
            });

            match client.send_prompt_stream(&prompt, &[], tx).await {
                Ok(full) => {
                    println!("\n\n--- Direct RPC Completed successfully ---");
                    println!("Total chars received: {}", full.len());
                }
                Err(e) => {
                    eprintln!("\nDirect RPC error: {}", e);
                }
            }
        }
    }

    Ok(())
}

async fn capture_with_timeout(
    cdp_port: u16,
    session_path: std::path::PathBuf,
    seconds: u64,
) -> Option<SessionData> {
    tokio::time::timeout(std::time::Duration::from_secs(seconds), async {
        loop {
            if let Ok(session) = capture_session(cdp_port, session_path.clone()).await {
                return session;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
    .await
    .ok()
}

/// Watch one open login window until the Google account is usable.
///
/// The browser target is asked for its cookie list once a second, so the sign-in
/// is noticed the moment the network service stores the auth cookie. The profile
/// files act as a second signal. DevTools is attached to a page only after one of
/// those signals is present, which keeps the sign-in page free of a debugger.
async fn watch_login_window(
    cdp_port: u16,
    session_path: std::path::PathBuf,
    profile: &std::path::Path,
    known_accounts: &[String],
) -> Option<SessionData> {
    let fast_gap = std::time::Duration::from_secs(6);
    let slow_gap = std::time::Duration::from_secs(30);
    let mut had_session = BrowserLauncher::profile_has_google_session(profile);
    let mut last_attempt: Option<std::time::Instant> = None;
    let mut failures = 0u8;
    let mut closed_polls = 0u8;
    let mut browser: Option<CdpClient> = None;
    let mut announced = false;

    loop {
        let live_cookie = match browser.as_ref() {
            Some(client) => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    client.auth_cookie_present(),
                )
                .await
                {
                    Ok(Ok(present)) => {
                        closed_polls = 0;
                        present
                    }
                    _ => {
                        browser = None;
                        closed_polls += 1;
                        false
                    }
                }
            }
            None => {
                match CdpClient::connect_browser(cdp_port).await {
                    Ok(client) => {
                        browser = Some(client);
                        closed_polls = 0;
                    }
                    Err(_) => closed_polls += 1,
                }
                false
            }
        };
        if closed_polls >= 3 {
            println!("Chrome 已关闭，停止监听登录状态。");
            return None;
        }

        let accounts = BrowserLauncher::profile_google_accounts(profile);
        let new_account = accounts.iter().any(|email| !known_accounts.contains(email));
        let has_session = BrowserLauncher::profile_has_google_session(profile);
        let session_appeared = has_session && !had_session;
        had_session = has_session;

        let gap = if failures >= 5 { slow_gap } else { fast_gap };
        let interval_ok = last_attempt.is_none_or(|at| at.elapsed() >= gap);
        let should_capture =
            interval_ok && (new_account || session_appeared || live_cookie || has_session);

        if !announced && (new_account || session_appeared || live_cookie) {
            println!("已检测到登录状态，正在读取会话...");
            announced = true;
        }

        if should_capture {
            last_attempt = Some(std::time::Instant::now());
            if let Ok(captured) = capture_session(cdp_port, session_path.clone()).await {
                if let Some(email) = accounts.last() {
                    println!("已识别登录账号: {}", email);
                }
                return Some(captured);
            }
            failures = failures.saturating_add(1);
            if new_account || session_appeared {
                // The Gemini tab can still be finishing the redirect that
                // follows the Google sign-in. Retry briefly before falling
                // back to the slow poll.
                for _ in 0..4 {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    if let Ok(captured) = capture_session(cdp_port, session_path.clone()).await {
                        if let Some(email) = accounts.last() {
                            println!("已识别登录账号: {}", email);
                        }
                        return Some(captured);
                    }
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }
}

async fn capture_session(
    cdp_port: u16,
    session_path: std::path::PathBuf,
) -> Result<SessionData, Box<dyn std::error::Error>> {
    // Open a Gemini tab when the login window has none. The window stays open,
    // so the extra tab costs nothing and a closed tab cannot stall the capture.
    let client = CdpClient::connect_to_gemini_tab(cdp_port, true).await?;

    let snlm0e = client
        .evaluate_js("window.WIZ_global_data?.SNlM0e || ''")
        .await?
        .as_str()
        .unwrap_or("")
        .to_string();
    let f_sid = client
        .evaluate_js("window.WIZ_global_data?.FdrFJe || ''")
        .await?
        .as_str()
        .unwrap_or("")
        .to_string();
    let bl = client
        .evaluate_js("window.WIZ_global_data?.cfb2h || ''")
        .await?
        .as_str()
        .unwrap_or("")
        .to_string();

    if snlm0e.is_empty() {
        return Err(
            "Gemini is not ready. Finish signing in in the Chrome window, then run login again."
                .into(),
        );
    }

    // Extract cookies via CDP
    let cookies_val = client
        .call_cdp(
            "Network.getCookies",
            serde_json::json!({
                "urls": ["https://gemini.google.com"]
            }),
        )
        .await?;

    let mut cookie_header = String::new();
    let mut has_auth_cookie = false;
    if let Some(cookies) = cookies_val
        .get("result")
        .and_then(|r| r.get("cookies"))
        .and_then(|c| c.as_array())
    {
        has_auth_cookie = cookies.iter().any(|cookie| {
            cookie
                .get("name")
                .and_then(|name| name.as_str())
                .is_some_and(|name| matches!(name, "__Secure-1PSID" | "__Secure-3PSID" | "SID"))
        });
        let parts: Vec<String> = cookies
            .iter()
            .filter_map(|c| {
                let name = c.get("name")?.as_str()?;
                let value = c.get("value")?.as_str()?;
                Some(format!("{}={}", name, value))
            })
            .collect();
        cookie_header = parts.join("; ");
    }

    if !has_auth_cookie || f_sid.is_empty() || bl.is_empty() {
        return Err("Finish signing in to Gemini first.".into());
    }
    Ok(SessionData {
        snlm0e,
        f_sid,
        bl,
        cookie: cookie_header,
        model: "Argon4p0".to_string(),
        conversation_id: None,
        response_id: None,
        choice_id: None,
        base_url: None,
        proxy: None,
        local_ip: None,
        path: Some(session_path),
    })
}
