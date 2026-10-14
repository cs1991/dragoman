mod browser;
mod openai;
mod proxy;

use std::net::SocketAddr;
use std::sync::Arc;
use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::{fmt, EnvFilter};

use crate::browser::{BrowserLauncher, CdpClient};
use crate::proxy::server::{create_router, AppState};

#[derive(Parser, Debug)]
#[command(name = "dragoman")]
#[command(author = "Khoi Pham Minh <khoipm06@gmail.com>")]
#[command(version = "0.1.0")]
#[command(about = "High-performance OpenAI-compatible proxy for Gemini Web (Gemini 4 Argon)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the OpenAI-compatible proxy server
    Serve {
        /// HTTP listen port for external coding agents
        #[arg(short, long, default_value_t = 8080)]
        port: u16,

        /// Chrome DevTools Protocol (CDP) port
        #[arg(long, default_value_t = 9222)]
        cdp_port: u16,

        /// Target model identifier reported to coding agents
        #[arg(short, long, default_value = "gemini-4-argon")]
        model: String,

        /// Automatically launch Chrome if not already running
        #[arg(long, default_value_t = true)]
        auto_launch: bool,
    },
    /// Launch the dedicated Chrome session for login
    Launch {
        /// Chrome DevTools Protocol (CDP) port
        #[arg(long, default_value_t = 9222)]
        cdp_port: u16,
    },
    /// Check browser connection and Gemini Web status
    Status {
        /// Chrome DevTools Protocol (CDP) port
        #[arg(long, default_value_t = 9222)]
        cdp_port: u16,
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
        cdp_port: 9222,
        model: "gemini-4-argon".to_string(),
        auto_launch: true,
    }) {
        Commands::Serve {
            port,
            cdp_port,
            model,
            auto_launch,
        } => {
            println!("=======================================================");
            println!(" Dragoman - Gemini Web OpenAI-Compatible Bridge");
            println!(" Model Target: {}", model);
            println!(" Proxy Endpoint: http://127.0.0.1:{}/v1/chat/completions", port);
            println!("=======================================================\n");

            if auto_launch {
                if let Err(e) = BrowserLauncher::ensure_chrome_running(cdp_port, None).await {
                    eprintln!("Warning: Auto-launch failed: {}. Continuing in case browser is started manually.", e);
                }
            }

            let app_state = Arc::new(AppState {
                cdp_client: Arc::new(Mutex::new(None)),
                cdp_port,
                default_model: model,
            });

            let router = create_router(app_state);
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            info!("Dragoman listening on http://{}", addr);

            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(listener, router).await?;
        }
        Commands::Launch { cdp_port } => {
            info!("Launching browser for Google Account authentication...");
            BrowserLauncher::ensure_chrome_running(cdp_port, None).await
                .map_err(|e| format!("Failed to launch browser: {}", e))?;
            println!("\nBrowser launched. Navigate to https://gemini.google.com and log in with your Google AI Pro account.");
            println!("Once logged in, run: dragoman serve");
        }
        Commands::Status { cdp_port } => {
            let ready = BrowserLauncher::is_cdp_ready(cdp_port).await;
            if ready {
                println!("Chrome CDP is RUNNING on port {}", cdp_port);
                match CdpClient::connect_to_gemini(cdp_port).await {
                    Ok(client) => {
                        let current_model = client.check_model_selected("gemini-4-argon").await?;
                        println!("Gemini Web tab CONNECTED.");
                        println!("Active Model in Web UI: {}", current_model);
                    }
                    Err(e) => {
                        println!("Connected to Chrome, but no active Gemini tab: {}", e);
                    }
                }
            } else {
                println!("Chrome CDP is NOT running on port {}.", cdp_port);
                println!("Run 'dragoman launch' or start Chrome with --remote-debugging-port={}", cdp_port);
            }
        }
    }

    Ok(())
}
