use clap::{Parser, Subcommand};
use dragoman::accounts::{AccountPool, BalanceStrategy, session_path_for_account};
use dragoman::browser::{BrowserLauncher, CdpClient};
use dragoman::desktop_accounts::AccountStore;
use dragoman::proxy::server::{AppState, create_router};
use dragoman::session::SessionData;
use dragoman::web::GeminiWebClient;
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
        #[arg(short, long, default_value = "gemini-4-argon")]
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

        #[arg(long, hide = true)]
        desktop_id: Option<String>,
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
        model: "gemini-4-argon".to_string(),
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
            desktop_id,
        } => {
            if let Some(id) = desktop_id {
                let store = AccountStore::new(SessionData::config_dir());
                let path = store.login_path(&id)?;
                let profile = AccountStore::profile_dir(&id, &std::env::current_exe()?)?;
                let launch_error =
                    "Chrome 启动失败。请确认已安装 Chrome，并关闭该账号之前打开的 Chrome 窗口。";

                // Google rejects sign-in while DevTools is enabled. An
                // interactive sign-in runs in a plain Chrome window first, then
                // a short DevTools session reads the stored cookies.
                let had_session = BrowserLauncher::profile_has_google_session(&profile);
                let mut session: Option<SessionData> = None;
                let mut capture_chrome = None;

                if had_session {
                    println!("Found an existing Google session, reading it...");
                    let (port, chrome) = BrowserLauncher::start_desktop_login(profile.clone())
                        .await
                        .map_err(|_| launch_error)?;
                    session = capture_with_timeout(port, path.clone(), 30).await;
                    if session.is_some() {
                        capture_chrome = Some(chrome);
                    } else {
                        BrowserLauncher::close_login_browser(chrome).await;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }

                if session.is_none() {
                    println!("Open Chrome, sign in to Google and open Gemini...");
                    let mut chrome = BrowserLauncher::spawn_plain_login(profile.clone())
                        .map_err(|_| launch_error)?;
                    let wait_seconds = if had_session { 180 } else { 240 };
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs(wait_seconds);
                    loop {
                        if chrome.try_wait().ok().flatten().is_some() {
                            break;
                        }
                        if !had_session && BrowserLauncher::profile_has_google_session(&profile) {
                            break;
                        }
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    BrowserLauncher::close_login_browser(chrome).await;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                    let (port, chrome) = BrowserLauncher::start_desktop_login(profile)
                        .await
                        .map_err(|_| launch_error)?;
                    session = capture_with_timeout(port, path.clone(), 60).await;
                    capture_chrome = Some(chrome);
                }

                let Some(session) = session else {
                    if let Some(chrome) = capture_chrome {
                        BrowserLauncher::close_login_browser(chrome).await;
                    }
                    return Err("读取会话失败。请重新登录，并在 Gemini 页面加载完成后重试。".into());
                };

                session
                    .save()
                    .map_err(|_| "保存会话失败。请检查配置目录权限。")?;
                if let Some(chrome) = capture_chrome {
                    BrowserLauncher::close_login_browser(chrome).await;
                }
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

            BrowserLauncher::ensure_chrome_running(cdp_port, profile_dir)
                .await
                .map_err(|e| format!("Failed to launch browser: {}", e))?;

            let session = capture_session(cdp_port, session_path.clone(), false).await?;
            session.save()?;
            println!(
                "\nSUCCESS: Saved authentication session to {}",
                session_path.display()
            );
            println!("You can now close Chrome completely.");
            println!("Run 'dragoman serve' to launch the proxy without any browser running.");
        }
        Commands::DesktopAccounts { action } => {
            let store = AccountStore::new(SessionData::config_dir());
            match action {
                AccountAction::List => println!("{}", serde_json::to_string(&store.list()?)?),
                AccountAction::Enable { id } => store.set_enabled(&id, true)?,
                AccountAction::Disable { id } => store.set_enabled(&id, false)?,
                AccountAction::Remove { id, disabled } => store.remove(&id, !disabled)?,
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
                "{:<20} {:<16} {:>7} {:>6} {:>9}  LAST ERROR",
                "NAME", "MODEL", "SERVED", "FAILS", "COOLDOWN"
            );
            for account in &status.accounts {
                println!(
                    "{:<20} {:<16} {:>7} {:>6} {:>8}s  {}",
                    account.name,
                    account.model,
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
            if let Ok(session) = capture_session(cdp_port, session_path.clone(), true).await {
                return session;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
    .await
    .ok()
}

async fn capture_session(
    cdp_port: u16,
    session_path: std::path::PathBuf,
    desktop: bool,
) -> Result<SessionData, Box<dyn std::error::Error>> {
    let client = CdpClient::connect_to_gemini_tab(cdp_port, !desktop).await?;

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

    if desktop && (!has_auth_cookie || f_sid.is_empty() || bl.is_empty()) {
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
        path: Some(session_path),
    })
}
