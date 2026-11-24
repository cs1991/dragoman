use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::info;

pub struct BrowserLauncher;

impl BrowserLauncher {
    /// Launch a normal Chrome window with no DevTools endpoint. Google rejects
    /// sign-in when the browser runs with remote debugging, so the desktop
    /// login signs in here and only starts a debug build after the account
    /// cookies exist.
    pub fn spawn_plain_login(data_dir: PathBuf) -> Result<std::process::Child, String> {
        std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        let user_data_flag = format!("--user-data-dir={}", data_dir.display());
        Self::spawn_native_chrome(&[
            "--no-first-run",
            "--no-default-browser-check",
            &user_data_flag,
            "https://gemini.google.com/app",
        ])
        .map_err(|error| error.to_string())
    }

    /// Close the Chrome instance started for sign-in. A graceful taskkill lets
    /// Chrome flush the cookie database, and a forced kill backs it up.
    pub async fn close_login_browser(mut chrome: std::process::Child) {
        let pid = chrome.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", &pid, "/T"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        for _ in 0..50 {
            if chrome.try_wait().ok().flatten().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = chrome.kill();
        let _ = chrome.wait();
    }

    /// True when the Chrome profile already holds a Google account session.
    /// Cookie names are stored as plain text, cookie values stay encrypted.
    pub fn profile_has_google_session(data_dir: &std::path::Path) -> bool {
        const NAMES: [&[u8]; 3] = [b"__Secure-1PSID", b"__Secure-3PSID", b"SAPISID"];
        let candidates = [
            data_dir.join("Default/Network/Cookies-wal"),
            data_dir.join("Default/Network/Cookies"),
            data_dir.join("Default/Cookies-wal"),
            data_dir.join("Default/Cookies"),
        ];
        for path in candidates {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if NAMES
                .iter()
                .any(|name| bytes.windows(name.len()).any(|window| window == *name))
            {
                return true;
            }
        }
        false
    }

    /// Start a dedicated profile with a Chrome-assigned CDP port. Never attach
    /// to a pre-existing browser for a desktop login.
    pub async fn start_desktop_login(
        data_dir: PathBuf,
    ) -> Result<(u16, std::process::Child), String> {
        std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        let port_file = data_dir.join("DevToolsActivePort");
        let previous = std::fs::read_to_string(&port_file).ok();
        let port_flag = "--remote-debugging-port=0".to_string();
        let user_data_flag = format!("--user-data-dir={}", data_dir.display());
        let mut chrome = Self::spawn_native_chrome(&[
            "--no-first-run",
            "--no-default-browser-check",
            &port_flag,
            &user_data_flag,
            "https://gemini.google.com/app",
        ])
        .map_err(|error| error.to_string())?;

        for _ in 0..60 {
            if chrome
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Err("Close the Chrome window for this account and try again".into());
            }
            if let Ok(current) = std::fs::read_to_string(&port_file)
                && previous.as_ref() != Some(&current)
                && let Some(port) = current.lines().next().and_then(|line| line.parse().ok())
                && Self::is_cdp_ready(port).await
            {
                return Ok((port, chrome));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err("Chrome did not open a login session in time".into())
    }

    pub async fn ensure_chrome_running(
        port: u16,
        profile_dir: Option<PathBuf>,
    ) -> Result<String, String> {
        if Self::is_cdp_ready(port).await {
            info!("Chrome CDP is already listening on port {}", port);
            return Ok(format!("http://127.0.0.1:{}", port));
        }

        info!("Launching browser for authentication on port {}...", port);

        let data_dir = match profile_dir {
            Some(dir) => dir,
            None => default_profile_dir()
                .map_err(|e| format!("Failed to locate Chrome profile dir: {e}"))?,
        };

        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create profile dir: {}", e))?;

        let port_flag = format!("--remote-debugging-port={}", port);
        let user_data_flag = format!("--user-data-dir={}", data_dir.display());

        let flatpak_status = Command::new("flatpak")
            .args(["info", "com.google.Chrome"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        let launched = if let Ok(status) = flatpak_status {
            if status.success() {
                info!("Launching Flatpak Google Chrome (com.google.Chrome)...");
                Command::new("flatpak")
                    .args([
                        "run",
                        "com.google.Chrome",
                        "--no-first-run",
                        "--no-default-browser-check",
                        &port_flag,
                        &user_data_flag,
                        "https://gemini.google.com/app",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            } else {
                Self::spawn_native_chrome(&[
                    "--no-first-run",
                    "--no-default-browser-check",
                    &port_flag,
                    &user_data_flag,
                    "https://gemini.google.com/app",
                ])
            }
        } else {
            Self::spawn_native_chrome(&[
                "--no-first-run",
                "--no-default-browser-check",
                &port_flag,
                &user_data_flag,
                "https://gemini.google.com/app",
            ])
        };

        if let Err(e) = launched {
            return Err(format!("Failed to launch Chrome: {}", e));
        }

        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if Self::is_cdp_ready(port).await {
                info!(
                    "Chrome launched and CDP is ready at http://127.0.0.1:{}",
                    port
                );
                return Ok(format!("http://127.0.0.1:{}", port));
            }
        }

        Err(format!("Timed out waiting for Chrome CDP on port {}", port))
    }

    fn spawn_native_chrome(args: &[&str]) -> std::io::Result<std::process::Child> {
        for bin in chrome_binaries() {
            if let Ok(child) = Command::new(&bin)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                info!("Launched native browser binary: {}", bin.display());
                return Ok(child);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No Chrome/Chromium binary found in the usual install directories or PATH",
        ))
    }

    pub async fn is_cdp_ready(port: u16) -> bool {
        let Ok(client) = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
        else {
            return false;
        };

        let url = format!("http://127.0.0.1:{}/json/version", port);
        match client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}

fn chrome_binaries() -> impl Iterator<Item = PathBuf> {
    #[cfg(windows)]
    let installed = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("Google/Chrome/Application/chrome.exe"))
        .chain([PathBuf::from("chrome.exe")]);
    #[cfg(not(windows))]
    let installed = std::iter::empty();

    installed.chain(
        [
            "google-chrome-stable",
            "google-chrome",
            "chromium-browser",
            "chromium",
        ]
        .into_iter()
        .map(PathBuf::from),
    )
}

#[cfg(windows)]
fn default_profile_dir() -> std::io::Result<PathBuf> {
    std::env::current_exe()?
        .parent()
        .map(|dir| dir.join("chrome_profile"))
        .ok_or_else(|| std::io::Error::other("The executable has no parent directory"))
}

#[cfg(not(windows))]
fn default_profile_dir() -> std::io::Result<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        Ok(PathBuf::from(home).join(".config/dragoman/chrome_profile"))
    } else {
        Ok(PathBuf::from(".dragoman_profile"))
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn profile_is_next_to_the_running_executable() -> std::io::Result<()> {
        let profile = super::default_profile_dir()?;
        assert_eq!(
            profile.file_name(),
            Some(std::ffi::OsStr::new("chrome_profile"))
        );
        assert_eq!(profile.parent(), std::env::current_exe()?.parent());
        Ok(())
    }
}
