//! Batch Google sign-in for Gemini Web accounts.
//!
//! One isolated Chrome profile is used per account. The sign-in page is driven
//! through the DevTools Protocol with plain DOM events, then the Gemini session
//! token and cookies are captured the same way `dragoman login` does it.
//! Verification codes are read from the configured mailbox site.

use crate::browser::{BrowserLauncher, CdpClient};
use crate::session::SessionData;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

const GOOGLE_LOGIN: &str = "https://accounts.google.com/signin/v2/identifier?service=accountsettings&continue=https%3A%2F%2Fgemini.google.com%2Fapp";
const GEMINI_APP: &str = "https://gemini.google.com/app";
const MAIL_LOGIN: &str = "https://google111111.asia/login?lang=zh";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const ACCOUNT_DEADLINE: Duration = Duration::from_secs(360);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub email: String,
    pub password: String,
    /// Third field of the line. A 32 character base32 authenticator secret is
    /// used for the code challenge, other values are kept only for reporting.
    pub extra: Option<String>,
}

impl Credential {
    /// Authenticator secret from the third field when it is valid base32.
    pub fn totp_secret(&self) -> Option<String> {
        self.extra
            .as_deref()
            .and_then(crate::totp::normalize_secret)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchResult {
    pub email: String,
    pub account: String,
    pub ok: bool,
    pub message: String,
    pub session_path: Option<String>,
}

pub struct BatchOptions {
    pub proxy: Option<String>,
    pub keep_open: bool,
    pub code_timeout: Duration,
    /// Skip accounts whose session file already holds usable tokens.
    pub skip_logged_in: bool,
}

/// Read an account file and decode it as UTF-8 or UTF-16.
///
/// Windows exports are frequently UTF-16 with a byte order mark while plain
/// text exports are UTF-8. Byte order marked files are decoded first, then a
/// UTF-16 layout without a mark is detected from the zero byte pattern.
pub fn read_account_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取账号文件失败 {}: {}", path.display(), error))?;
    Ok(decode_account_bytes(&bytes))
}

fn decode_account_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, false);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let pairs = bytes.len() / 2;
    if pairs >= 2 {
        let even_zero = (0..pairs).filter(|i| bytes[i * 2] == 0).count();
        let odd_zero = (0..pairs).filter(|i| bytes[i * 2 + 1] == 0).count();
        if odd_zero * 2 >= pairs {
            return decode_utf16(&bytes[..pairs * 2], true);
        }
        if even_zero * 2 >= pairs {
            return decode_utf16(&bytes[..pairs * 2], false);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks(2)
        .filter_map(|chunk| match chunk {
            [low, high] => Some(if little_endian {
                u16::from_le_bytes([*low, *high])
            } else {
                u16::from_be_bytes([*low, *high])
            }),
            _ => None,
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Split account input into logical lines.
///
/// Files produced by spreadsheet exports, clipboard dumps and older editors
/// use LF, CRLF or a bare CR as the record separator. Some exports also strip
/// the line breaks and keep every account on one physical line. All of those
/// shapes are normalized here. A line that holds more than one mailbox is cut
/// at each mailbox start, which restores the original records even when the
/// separator between them was lost.
pub fn split_account_lines(raw: &str) -> Vec<String> {
    let text = raw.trim_start_matches('\u{feff}');
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                normalized.push('\n');
            }
            '\u{2028}' | '\u{2029}' | '\u{0085}' => normalized.push('\n'),
            other => normalized.push(other),
        }
    }
    let mut text = normalized;
    // Some chat clients serialize line breaks as a literal backslash plus n.
    if !text.contains('\n') && text.contains("\\n") {
        text = text.replace("\\n", "\n");
    }

    let mut lines = Vec::new();
    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let anchors = mailbox_starts(line);
        if anchors.len() < 2 {
            lines.push(line.trim_start_matches('\u{feff}').to_string());
            continue;
        }
        for (position, start) in anchors.iter().enumerate() {
            let end = anchors.get(position + 1).copied().unwrap_or(line.len());
            let piece = line[*start..end].trim();
            if !piece.is_empty() {
                lines.push(piece.to_string());
            }
        }
    }
    lines
}

/// Byte offsets where a mailbox address begins.
///
/// A candidate needs an `@` with a non-empty local part, a dotted domain and
/// an alphabetic top level label. The local part scan stops at a doubled
/// dash, so the `----` separator that follows a password never becomes part of
/// the next mailbox name.
fn mailbox_starts(line: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut starts = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'@' {
            index += 1;
            continue;
        }
        let mut start = index;
        while start > 0 && is_mailbox_local(bytes[start - 1]) {
            if bytes[start - 1] == b'-' && start >= 2 && bytes[start - 2] == b'-' {
                break;
            }
            start -= 1;
        }
        if start == index {
            index += 1;
            continue;
        }
        let leading_ok =
            start == 0 || bytes[start - 1] == b'-' || bytes[start - 1].is_ascii_whitespace();
        if !leading_ok {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        let mut last_dot = None;
        while end < bytes.len() && is_mailbox_domain(bytes[end]) {
            // A doubled dash is the field separator used by every export
            // shape, so the domain must not run past it into the password.
            if bytes[end] == b'-' && end + 1 < bytes.len() && bytes[end + 1] == b'-' {
                break;
            }
            if bytes[end] == b'.' {
                last_dot = Some(end);
            }
            end += 1;
        }
        if let Some(dot) = last_dot {
            let top_level = &bytes[dot + 1..end];
            if top_level.len() >= 2 && top_level.iter().all(u8::is_ascii_alphabetic) {
                if starts.last() != Some(&start) {
                    starts.push(start);
                }
                index = end;
                continue;
            }
        }
        index += 1;
    }
    starts
}

fn is_mailbox_local(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_mailbox_domain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')
}

/// Parse one `email----password---` account per line.
///
/// A third field is accepted for exports shaped like
/// `email----password----secret`. Blank lines and `#` comments are ignored. The
/// trailing separator dashes are removed from the last field, while dashes
/// inside the password are preserved.
pub fn parse_credentials(raw: &str) -> Result<Vec<Credential>, String> {
    let mut credentials = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in split_account_lines(raw).iter().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts: Vec<&str> = if line.contains("----") {
            line.split("----").map(str::trim).collect()
        } else {
            line.split('\t').map(str::trim).collect()
        };
        if parts.len() < 2 {
            return Err(format!(
                "第 {} 行缺少分隔符。格式：email----password--- 或 email----password----2FA密钥",
                index + 1
            ));
        }
        if let Some(last) = parts.last_mut() {
            *last = last.trim_end_matches('-').trim();
        }
        let email = parts[0];
        let password = parts[1];
        let extra = parts
            .get(2)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if !email.contains('@') {
            return Err(format!("第 {} 行的邮箱无效：{email}", index + 1));
        }
        if password.is_empty() {
            return Err(format!("第 {} 行的密码为空：{email}", index + 1));
        }
        if seen.insert(email.to_ascii_lowercase()) {
            credentials.push(Credential {
                email: email.to_string(),
                password: password.to_string(),
                extra,
            });
        }
    }
    if credentials.is_empty() {
        return Err(
            "没有可登录的账号。每行格式：email----password--- 或 email----password----2FA密钥"
                .to_string(),
        );
    }
    Ok(credentials)
}

/// Session file name for an account. The local part matches the existing
/// account naming in `dragoman accounts`.
pub fn account_name_for_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("account");
    let mut name = String::new();
    for character in local.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            name.push(character);
        } else {
            name.push('_');
        }
    }
    let name = name.trim_matches('.').to_string();
    let name = if name.is_empty() {
        "account".to_string()
    } else {
        name
    };
    let upper = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
    {
        format!("account-{name}")
    } else {
        name
    }
}

/// Whether a usable session file already exists for an account.
pub fn has_saved_session(account: &str) -> bool {
    let Ok(path) = crate::accounts::session_path_for_account(account) else {
        return false;
    };
    SessionData::load_from(&path).is_ok_and(|session| {
        !session.snlm0e.trim().is_empty()
            && !session.f_sid.trim().is_empty()
            && !session.bl.trim().is_empty()
            && !session.cookie.trim().is_empty()
    })
}

/// Run every account sequentially. Each account gets its own Chrome profile so
/// a failed sign-in never contaminates the next one.
pub async fn run_batch(credentials: &[Credential], options: &BatchOptions) -> Vec<BatchResult> {
    let http = match reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return credentials
                .iter()
                .map(|credential| BatchResult {
                    email: credential.email.clone(),
                    account: account_name_for_email(&credential.email),
                    ok: false,
                    message: format!("HTTP client error: {error}"),
                    session_path: None,
                })
                .collect();
        }
    };

    let mut results = Vec::with_capacity(credentials.len());
    let mut used_names = HashSet::new();
    for (index, credential) in credentials.iter().enumerate() {
        let base_name = account_name_for_email(&credential.email);
        let mut account = base_name.clone();
        let mut suffix = 2;
        while !used_names.insert(account.clone()) {
            account = format!("{base_name}-{suffix}");
            suffix += 1;
        }

        if options.skip_logged_in && has_saved_session(&account) {
            let session_path = crate::accounts::session_path_for_account(&account)
                .map(|path| path.display().to_string())
                .ok();
            println!(
                "[{}/{}] 跳过 {}（已登录）",
                index + 1,
                credentials.len(),
                credential.email
            );
            results.push(BatchResult {
                email: credential.email.clone(),
                account,
                ok: true,
                message: "已登录，跳过".to_string(),
                session_path,
            });
            continue;
        }

        println!(
            "[{}/{}] 登录 {}",
            index + 1,
            credentials.len(),
            credential.email
        );
        let result = match login_one(credential, &account, options, &http).await {
            Ok(path) => BatchResult {
                email: credential.email.clone(),
                account,
                ok: true,
                message: "已保存会话".to_string(),
                session_path: Some(path.display().to_string()),
            },
            Err(error) => BatchResult {
                email: credential.email.clone(),
                account,
                ok: false,
                message: error,
                session_path: None,
            },
        };
        if result.ok {
            println!(
                "[{}/{}] 成功 {}",
                index + 1,
                credentials.len(),
                result.email
            );
        } else {
            println!(
                "[{}/{}] 失败 {}：{}",
                index + 1,
                credentials.len(),
                result.email,
                result.message
            );
        }
        results.push(result);
    }
    results
}

async fn login_one(
    credential: &Credential,
    account: &str,
    options: &BatchOptions,
    http: &reqwest::Client,
) -> Result<PathBuf, String> {
    let profile = batch_profile_dir(account)?;
    let (port, mut chrome) =
        BrowserLauncher::start_batch_login(profile.clone(), options.proxy.as_deref()).await?;

    let outcome = async {
        let client = CdpClient::connect_first_page(port).await?;
        let _ = client.call_cdp("Page.enable", json!({})).await;
        drive_google_login(&client, credential, options, http).await?;
        ensure_gemini_app(&client).await?;
        capture_session(&client, account, options).await
    }
    .await;

    if !options.keep_open {
        stop_chrome(&mut chrome);
        // A killed Chrome does not remove its DevTools record. Leaving it
        // behind makes the next attempt for this account start without CDP.
        let _ = std::fs::remove_file(profile.join("DevToolsActivePort"));
    }
    outcome
}

async fn drive_google_login(
    client: &CdpClient,
    credential: &Credential,
    options: &BatchOptions,
    http: &reqwest::Client,
) -> Result<(), String> {
    client.navigate(GOOGLE_LOGIN).await?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    let mailbox = mailbox_open(http, &credential.email).await.ok();
    let baseline = match mailbox.as_deref() {
        Some(url) => mailbox_codes(http, url).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let deadline = Instant::now() + ACCOUNT_DEADLINE;
    let mut email_sent = false;
    let mut password_sent = false;
    let mut email_retries = 0u8;
    let mut password_retries = 0u8;
    let mut email_reloads = 0u8;
    let mut password_reloads = 0u8;
    let mut last_email_click: Option<Instant> = None;
    let mut last_password_click: Option<Instant> = None;
    let mut code_requested = false;
    let mut totp_submitted = false;
    let mut last_url = String::new();
    let mut failures = 0u8;

    while Instant::now() < deadline {
        let state = page_state(client).await?;
        let url = state
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let text = state
            .get("text")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if url != last_url {
            println!("  页面：{url}");
            last_url = url.clone();
        }

        // The accounts.google.com URL carries continue=https://gemini.google.com,
        // so match the current host prefix instead of a substring.
        if url.starts_with("https://gemini.google.com") {
            return Ok(());
        }
        if url.starts_with("https://myaccount.google.com") {
            client.navigate(GEMINI_APP).await?;
            tokio::time::sleep(Duration::from_secs(3)).await;
            return Ok(());
        }
        if text.contains("此浏览器或应用可能不安全")
            || text.contains("This browser or app may not be secure")
            || text.contains("Couldn't sign you in")
        {
            save_failure_shot(client, account_hint(credential)).await;
            return Err("Google 拒绝了这次登录（浏览器或应用不安全）".to_string());
        }
        if text.contains("密码错误")
            || text.contains("Wrong password")
            || text.contains("密码不正确")
        {
            return Err("账号或密码错误".to_string());
        }
        if text.contains("找不到此账号")
            || text.contains("找不到该账号")
            || text.contains("找不到您的 Google 账号")
            || text.contains("Couldn't find your Google Account")
            || text.contains("输入有效的电子邮件")
            || text.contains("Enter a valid email")
        {
            return Err("账号不存在或邮箱格式无效".to_string());
        }
        if text.contains("账号已被停用")
            || text.contains("Account disabled")
            || text.contains("您的账号已停用")
        {
            return Err("账号已被停用".to_string());
        }

        let has_identifier = state
            .get("ident")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let has_password = state
            .get("pass")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let has_code = state
            .get("code")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let email_option = state
            .get("emailOption")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        if has_identifier && email_sent && !password_sent {
            let empty = client
                .evaluate_js("(document.querySelector('#identifierId')?.value || '').length === 0")
                .await?
                .as_bool()
                .unwrap_or(false);
            if empty {
                println!("  邮箱没有提交成功，重新填写");
                email_sent = false;
                email_retries = 0;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            let stalled = last_email_click.map(|at| at.elapsed()).unwrap_or_default();
            match (email_retries, stalled) {
                (0, waited) if waited >= Duration::from_secs(12) => {
                    println!("  邮箱页没有前进，改用回车提交");
                    submit_with_enter(client, "#identifierId").await.ok();
                    email_retries = 1;
                    last_email_click = Some(Instant::now());
                }
                (1, waited) if waited >= Duration::from_secs(15) => {
                    if email_reloads >= 1 {
                        save_failure_shot(client, account_hint(credential)).await;
                        return Err("邮箱提交后页面没有变化".to_string());
                    }
                    println!("  邮箱页仍然没有前进，重新加载登录页");
                    client.navigate(GOOGLE_LOGIN).await?;
                    email_reloads += 1;
                    email_sent = false;
                    email_retries = 0;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        if has_password && password_sent {
            let empty = client
                .evaluate_js(
                    "(document.querySelector('input[name=\"Passwd\"]')?.value || '').length === 0",
                )
                .await?
                .as_bool()
                .unwrap_or(false);
            if empty {
                println!("  密码没有提交成功，重新填写");
                password_sent = false;
                password_retries = 0;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            let stalled = last_password_click
                .map(|at| at.elapsed())
                .unwrap_or_default();
            match (password_retries, stalled) {
                (0, waited) if waited >= Duration::from_secs(15) => {
                    println!("  密码页没有前进，改用回车提交");
                    submit_with_enter(client, "input[name=\"Passwd\"]")
                        .await
                        .ok();
                    password_retries = 1;
                    last_password_click = Some(Instant::now());
                }
                (1, waited) if waited >= Duration::from_secs(20) => {
                    if password_reloads >= 1 {
                        save_failure_shot(client, account_hint(credential)).await;
                        return Err("密码提交后页面没有变化".to_string());
                    }
                    println!("  密码页仍然没有前进，重新加载登录页");
                    client.navigate(GOOGLE_LOGIN).await?;
                    password_reloads += 1;
                    email_sent = false;
                    password_sent = false;
                    email_retries = 0;
                    password_retries = 0;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        if has_identifier && !email_sent {
            let filled = fill_input(client, "#identifierId", &credential.email).await?;
            if !filled {
                println!("  邮箱输入框没有接受文本，重试");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            let clicked = click_texts(client, &["下一步", "Next"]).await;
            println!("  已填写邮箱，点击：{clicked}");
            email_sent = true;
            email_retries = 0;
            last_email_click = Some(Instant::now());
            tokio::time::sleep(Duration::from_secs(4)).await;
            continue;
        }
        if has_password && !password_sent {
            let filled = fill_input(client, "input[name=\"Passwd\"]", &credential.password).await?;
            if !filled {
                println!("  密码输入框没有接受文本，重试");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            let clicked = click_texts(client, &["下一步", "Next"]).await;
            println!("  已填写密码，点击：{clicked}");
            password_sent = true;
            password_retries = 0;
            last_password_click = Some(Instant::now());
            tokio::time::sleep(Duration::from_secs(6)).await;
            continue;
        }
        // After the password Google may ask how to verify the account. Pick
        // the first email code option instead of opening the other-methods
        // menu, then read the code from the mailbox site.
        if email_option && !has_code && click_first_email_option(client).await {
            println!("  选择第一个邮件验证码登录方式");
            code_requested = true;
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        if has_code {
            // A third field in the account line can hold the authenticator
            // secret. Submit a generated code first, because an authenticator
            // challenge has no mailbox fallback on many accounts.
            let mentions_authenticator = text.contains("身份验证器")
                || text.contains("验证器")
                || text.contains("Authenticator")
                || text.contains("验证码应用");
            let mentions_email = text.contains("电子邮件")
                || text.contains("邮件")
                || text.contains("发送验证码")
                || text.contains("验证码已发送")
                || text.contains("已向")
                || text.to_ascii_lowercase().contains("email");
            let mentions_email = mentions_email || text.to_ascii_lowercase().contains("sent");
            if !totp_submitted
                && (mentions_authenticator || !mentions_email)
                && let Some(secret) = credential.totp_secret()
                && let Some(code) = crate::totp::current_code(&secret)
            {
                println!("  已用账号内置 2FA 密钥生成验证码");
                fill_code_input(client, &code).await?;
                click_texts(client, &["下一步", "Next", "验证", "Verify"]).await;
                totp_submitted = true;
                tokio::time::sleep(Duration::from_secs(6)).await;
                continue;
            }
            if !code_requested {
                if mentions_email {
                    code_requested = true;
                } else {
                    let clicked = click_texts(
                        client,
                        &[
                            "尝试其他方式",
                            "使用其他方式",
                            "Try another way",
                            "获取验证码",
                            "发送验证码",
                            "发送代码",
                            "通过电子邮件发送",
                            "电子邮件",
                            "Send code",
                            "Get a verification code",
                            "Get code",
                            "Email",
                        ],
                    )
                    .await;
                    if !clicked.is_empty() {
                        println!("  已点击：{clicked}");
                        if clicked.contains("其他方式")
                            || clicked.to_ascii_lowercase().contains("another way")
                        {
                            // A method chooser opened. Re-evaluate the page so
                            // the email option can be picked on the next pass.
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    }
                    code_requested = true;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            match wait_for_new_code(
                http,
                &credential.email,
                mailbox.as_deref(),
                &baseline,
                options.code_timeout,
            )
            .await
            {
                Ok(code) => {
                    println!("  已从 google111111.asia 获取验证码");
                    fill_code_input(client, &code).await?;
                    click_texts(client, &["下一步", "Next", "验证", "Verify"]).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                Err(error) => return Err(error),
            }
            continue;
        }

        if text.contains(&credential.email) && click_account(client, &credential.email).await {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        let clicked = click_texts(
            client,
            &[
                "继续",
                "Continue",
                "我同意",
                "I agree",
                "同意",
                "接受",
                "Accept",
                "确认",
                "Confirm",
                "知道了",
                "Got it",
                "尝试其他方式",
                "Try another way",
                "获取验证码",
                "发送验证码",
                "Get a verification code",
                "Send code",
            ],
        )
        .await;
        if !clicked.is_empty() {
            println!("  已点击：{clicked}");
            if clicked.contains("验证码") || clicked.to_ascii_lowercase().contains("code") {
                code_requested = true;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        failures = failures.saturating_add(1);
        if failures > 20 {
            save_failure_shot(client, account_hint(credential)).await;
            let snippet = text.chars().take(160).collect::<String>();
            return Err(format!("登录页面无进展：{snippet}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err("登录超时".to_string())
}

fn account_hint(credential: &Credential) -> &str {
    credential.email.split('@').next().unwrap_or("account")
}

async fn ensure_gemini_app(client: &CdpClient) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut attempt = 0u8;
    while Instant::now() < deadline {
        let state = client
            .evaluate_js(
                r#"(() => ({
                    url: location.href,
                    text: ((document.body && document.body.innerText) || '').slice(0, 500),
                    app: !!document.querySelector('rich-textarea, [contenteditable="true"]'),
                    snlm0e: (window.WIZ_global_data && window.WIZ_global_data.SNlM0e) || ''
                }))()"#,
            )
            .await?;
        let app = state
            .get("app")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let token = state
            .get("snlm0e")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if attempt == 0 || attempt == 6 || attempt == 12 {
            let text = state
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>();
            println!(
                "  Gemini 状态：app={app} token={} text={text}",
                !token.is_empty()
            );
        }

        // The first-run dialog sits above the composer and exists in two
        // shapes, the signed-in overlay and the signed-out landing page. It is
        // probed before the app check so a valid token never leaves the dialog
        // covering the page.
        if let Some(label) = dismiss_gemini_onboarding(client).await {
            println!("  关闭 Gemini 引导弹窗（{label}）");
            tokio::time::sleep(Duration::from_secs(3)).await;
            attempt = attempt.saturating_add(1);
            if attempt > 12 {
                return Ok(());
            }
            continue;
        }

        if app && !token.is_empty() {
            return Ok(());
        }

        let clicked = click_texts(
            client,
            &[
                "Chat with Gemini",
                "Use Gemini",
                "使用 Gemini",
                "开始使用",
                "继续",
                "Continue",
            ],
        )
        .await;
        if !clicked.is_empty() {
            tokio::time::sleep(Duration::from_secs(4)).await;
            continue;
        }

        if attempt == 5 || attempt == 15 {
            client.navigate(GEMINI_APP).await?;
            tokio::time::sleep(Duration::from_secs(4)).await;
        }
        attempt = attempt.saturating_add(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    save_failure_shot(client, "gemini").await;
    Err("Gemini 页面没有进入可对话状态".to_string())
}

/// Click the continue control of the Gemini first-run dialog.
///
/// The control is clicked with a trusted mouse event at its center, then a
/// synthetic element click is used only when the control is still present, so
/// the dialog closes once and the background page is never clicked.
async fn dismiss_gemini_onboarding(client: &CdpClient) -> Option<String> {
    let script = r#"(() => {
        const shown = el => {
            if (!el || !el.isConnected) return false;
            const style = getComputedStyle(el);
            if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
            const rect = el.getBoundingClientRect();
            return rect.width > 0 && rect.height > 0;
        };
        const wanted = ['use gemini', '使用 gemini', '开始使用', 'get started', 'chat with gemini', 'no thanks', '不用了'];
        const nodes = Array.from(document.querySelectorAll('button, [role="button"], [role="link"], [role="menuitem"], [role="option"], a, div[jsname], div[jscontroller]'));
        const find = exact => {
            for (const w of wanted) {
                for (const el of nodes) {
                    if (!shown(el)) continue;
                    const text = ((el.innerText || el.textContent || '') + ' ' + (el.getAttribute('aria-label') || ''))
                        .replace(/\s+/g, ' ').trim();
                    if (!text || text.length > 60) continue;
                    const lower = text.toLowerCase();
                    if (exact ? lower !== w : !lower.includes(w)) continue;
                    el.scrollIntoView({ block: 'center', inline: 'center' });
                    const rect = el.getBoundingClientRect();
                    if (rect.width < 8 || rect.height < 8) continue;
                    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2, text: text.slice(0, 40) };
                }
            }
            return null;
        };
        const hit = find(true) || find(false);
        if (hit) return hit;
        // The dialog controls live in custom elements. Use the smallest
        // element whose text is exactly the wanted label.
        for (const w of wanted) {
            let best = null;
            for (const el of document.querySelectorAll('*')) {
                if (!shown(el)) continue;
                const text = (el.textContent || '').replace(/\s+/g, ' ').trim();
                if (text.toLowerCase() !== w) continue;
                const target = el.closest('button, a, [role="button"], [role="link"]') || el;
                const rect = target.getBoundingClientRect();
                if (rect.width < 8 || rect.height < 8) continue;
                if (!best || rect.width * rect.height < best.area) {
                    best = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2, text: text.slice(0, 40), area: rect.width * rect.height };
                }
            }
            if (best) {
                delete best.area;
                return best;
            }
        }
        return null;
    })()"#;
    let value = match client.evaluate_js(script).await {
        Ok(value) => value,
        Err(_) => return None,
    };
    let (Some(x), Some(y)) = (
        value.get("x").and_then(|value| value.as_f64()),
        value.get("y").and_then(|value| value.as_f64()),
    ) else {
        return None;
    };
    let label = value
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    mouse_click(client, x, y).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let fallback = r#"(() => {
        const shown = el => {
            if (!el || !el.isConnected) return false;
            const style = getComputedStyle(el);
            if (style.display === 'none' || style.visibility === 'hidden') return false;
            const rect = el.getBoundingClientRect();
            return rect.width > 0 && rect.height > 0;
        };
        const wanted = ['use gemini', '使用 gemini', '开始使用', 'get started', 'chat with gemini', 'no thanks', '不用了'];
        const nodes = Array.from(document.querySelectorAll('button, [role="button"], [role="link"], [role="menuitem"], [role="option"], a, div[jsname], div[jscontroller]'));
        for (const w of wanted) {
            for (const el of nodes) {
                if (!shown(el)) continue;
                const text = ((el.innerText || el.textContent || '') + ' ' + (el.getAttribute('aria-label') || ''))
                    .replace(/\s+/g, ' ').trim();
                if (!text || text.length > 60) continue;
                if (!text.toLowerCase().includes(w)) continue;
                el.click();
                return true;
            }
        }
        return false;
    })()"#;
    let _ = client.evaluate_js(fallback).await;
    Some(label)
}

async fn capture_session(
    client: &CdpClient,
    account: &str,
    options: &BatchOptions,
) -> Result<PathBuf, String> {
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
    if snlm0e.is_empty() || f_sid.is_empty() || bl.is_empty() {
        return Err("Gemini 页面尚未返回会话令牌".to_string());
    }

    let cookies_value = client
        .call_cdp(
            "Network.getCookies",
            json!({ "urls": ["https://gemini.google.com"] }),
        )
        .await?;
    let mut cookie_header = String::new();
    let mut has_auth_cookie = false;
    if let Some(cookies) = cookies_value
        .get("result")
        .and_then(|result| result.get("cookies"))
        .and_then(|cookies| cookies.as_array())
    {
        has_auth_cookie = cookies.iter().any(|cookie| {
            cookie
                .get("name")
                .and_then(|name| name.as_str())
                .is_some_and(|name| matches!(name, "__Secure-1PSID" | "__Secure-3PSID" | "SID"))
        });
        cookie_header = cookies
            .iter()
            .filter_map(|cookie| {
                Some(format!(
                    "{}={}",
                    cookie.get("name")?.as_str()?,
                    cookie.get("value")?.as_str()?
                ))
            })
            .collect::<Vec<_>>()
            .join("; ");
    }
    if !has_auth_cookie || cookie_header.is_empty() {
        return Err("登录后没有拿到 Google 认证 Cookie".to_string());
    }

    let base = crate::accounts::file_name_for_account(account)?;
    let path = SessionData::sessions_dir().join(format!("{base}.json"));
    let mut session = SessionData {
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
        tier: crate::session::AccountTier::default(),
        path: Some(path.clone()),
    };
    if let Ok(previous) = SessionData::load_from(&path) {
        session.local_ip = previous.local_ip;
        session.tier = previous.tier;
        if options.proxy.is_none() {
            session.proxy = previous.proxy;
        }
    }
    if let Some(proxy) = options
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        session.proxy = Some(proxy.to_string());
    }
    session.save()?;
    Ok(path)
}

async fn page_state(client: &CdpClient) -> Result<serde_json::Value, String> {
    client
        .evaluate_js(
            r#"(() => {
                const visible = el => !!el && (el.offsetWidth > 0 || el.offsetHeight > 0);
                const codeInput = Array.from(document.querySelectorAll('input')).some(el =>
                    visible(el) && (
                        /idvPin|totpPin|passcode|code|^Pin$/i.test(el.name || '') ||
                        el.autocomplete === 'one-time-code' ||
                        /验证码|verification code|enter code|one-time code/i.test((el.getAttribute('aria-label') || '') + ' ' + (el.placeholder || ''))
                    )
                );
                const emailOption = Array.from(document.querySelectorAll('button, a, li, [role="button"], [role="link"], [role="menuitem"], [role="option"], div[jsname], div[jscontroller]')).some(el => {
                    if (!visible(el)) return false;
                    const label = (el.innerText || '').replace(/\s+/g, ' ').trim();
                    return label.length > 0 && label.length < 120
                        && /接收验证码|receive (a )?(verification )?code/i.test(label)
                        && !/其他方式|another way/i.test(label);
                });
                return {
                    url: location.href,
                    text: ((document.body && document.body.innerText) || '').slice(0, 1600),
                    ident: visible(document.querySelector('#identifierId')),
                    pass: visible(document.querySelector('input[name="Passwd"]')),
                    code: codeInput,
                    emailOption
                };
            })()"#,
        )
        .await
}

async fn fill_input(client: &CdpClient, selector: &str, value: &str) -> Result<bool, String> {
    let selector_json = serde_json::to_string(selector).map_err(|error| error.to_string())?;
    let value_json = serde_json::to_string(value).map_err(|error| error.to_string())?;

    // Google's sign-in page is an Angular controller. Writing the value with
    // `Input.insertText` updates the DOM but leaves the form marked untouched,
    // so the Next button ignores the click and no request is sent. Setting the
    // value through the native prototype setter and firing input/change is the
    // path a paste or autofill takes, and it advances the wizard.
    let set_value = format!(
        r#"(() => {{
            const el = document.querySelector({selector_json});
            if (!el) return null;
            el.focus();
            if (el.select) el.select();
            const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
            setter.call(el, {value_json});
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return el.value;
        }})()"#
    );
    if client.evaluate_js(&set_value).await?.as_str() == Some(value) {
        return Ok(true);
    }

    // Trusted text insertion for pages that reject a scripted write.
    let focus = format!(
        r#"(() => {{
            const el = document.querySelector({selector_json});
            if (!el) return false;
            el.focus();
            if (el.select) el.select();
            return document.activeElement === el;
        }})()"#
    );
    if client.evaluate_js(&focus).await?.as_bool() != Some(true) {
        return Ok(false);
    }
    client.insert_text(value).await?;
    let read_value = format!(
        r#"(() => {{
            const el = document.querySelector({selector_json});
            return el ? el.value : '';
        }})()"#
    );
    Ok(client
        .evaluate_js(&read_value)
        .await?
        .as_str()
        .is_some_and(|current| current == value))
}

/// Focus a field and submit the form with Enter. Used when a click on the Next
/// button leaves the wizard on the same page.
async fn submit_with_enter(client: &CdpClient, selector: &str) -> Result<(), String> {
    let selector_json = serde_json::to_string(selector).map_err(|error| error.to_string())?;
    let focus = format!(
        r#"(() => {{
            const el = document.querySelector({selector_json});
            if (!el) return false;
            el.focus();
            if (el.select) el.select();
            return document.activeElement === el;
        }})()"#
    );
    client.evaluate_js(&focus).await?;
    client.press_enter().await
}

async fn fill_code_input(client: &CdpClient, code: &str) -> Result<(), String> {
    let script = format!(
        r#"(() => {{
            const visible = el => !!el && (el.offsetWidth > 0 || el.offsetHeight > 0);
            const el = Array.from(document.querySelectorAll('input')).find(el =>
                visible(el) && (
                    /idvPin|totpPin|passcode|code|^Pin$/i.test(el.name || '') ||
                    el.autocomplete === 'one-time-code' ||
                    /验证码|verification code|enter code|one-time code/i.test((el.getAttribute('aria-label') || '') + ' ' + (el.placeholder || ''))
                )
            );
            if (!el) return false;
            el.focus();
            const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
            setter.call(el, {code});
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return true;
        }})()"#,
        code = serde_json::to_string(code).map_err(|error| error.to_string())?,
    );
    if client.evaluate_js(&script).await?.as_bool() != Some(true) {
        return Err("找不到验证码输入框".to_string());
    }
    Ok(())
}

async fn click_texts(client: &CdpClient, texts: &[&str]) -> String {
    let script = format!(
        r#"(() => {{
            const visible = el => !!el && (el.offsetWidth > 0 || el.offsetHeight > 0);
            const wanted = {wanted};
            for (const el of document.querySelectorAll('button, [role="button"], a, div[jsname]')) {{
                if (!visible(el)) continue;
                const label = ((el.innerText || '') + ' ' + (el.getAttribute('aria-label') || '')).trim();
                if (!label) continue;
                const hit = wanted.find(w => label === w || (w.length >= 4 && label.includes(w)));
                if (hit) {{
                    el.scrollIntoView({{ block: 'center', inline: 'center' }});
                    const rect = el.getBoundingClientRect();
                    return {{
                        text: hit,
                        x: rect.left + rect.width / 2,
                        y: rect.top + rect.height / 2
                    }};
                }}
            }}
            return null;
        }})()"#,
        wanted = serde_json::to_string(texts).unwrap_or_else(|_| "[]".to_string()),
    );
    let value = match client.evaluate_js(&script).await {
        Ok(value) => value,
        Err(_) => return String::new(),
    };
    let x = value.get("x").and_then(|value| value.as_f64());
    let y = value.get("y").and_then(|value| value.as_f64());
    let text = value
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if let (Some(x), Some(y)) = (x, y) {
        mouse_click(client, x, y).await;
        text
    } else {
        String::new()
    }
}

async fn click_account(client: &CdpClient, email: &str) -> bool {
    let script = format!(
        r#"(() => {{
            const visible = el => !!el && (el.offsetWidth > 0 || el.offsetHeight > 0);
            const email = {email};
            const candidates = Array.from(document.querySelectorAll('div[role="link"], a, button, li, div[role="button"]'));
            const el = candidates.find(el => visible(el) && (el.innerText || '').includes(email) && (el.innerText || '').length < 300);
            if (!el) return null;
            el.scrollIntoView({{ block: 'center', inline: 'center' }});
            const rect = el.getBoundingClientRect();
            return {{ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }};
        }})()"#,
        email = serde_json::to_string(email).unwrap_or_else(|_| "\"\"".to_string()),
    );
    let value = match client.evaluate_js(&script).await {
        Ok(value) => value,
        Err(_) => return false,
    };
    match (
        value.get("x").and_then(|value| value.as_f64()),
        value.get("y").and_then(|value| value.as_f64()),
    ) {
        (Some(x), Some(y)) => {
            mouse_click(client, x, y).await;
            true
        }
        _ => false,
    }
}

/// Click the first email verification option on the account chooser page.
///
/// Google may list the same mailbox twice, for example a primary and a
/// recovery mailbox. The first entry is used. The other-methods control is
/// excluded so the method chooser is never reopened.
async fn click_first_email_option(client: &CdpClient) -> bool {
    let script = r#"(() => {
        const shown = el => {
            if (!el || !el.isConnected) return false;
            const style = getComputedStyle(el);
            if (style.display === 'none' || style.visibility === 'hidden') return false;
            const rect = el.getBoundingClientRect();
            return rect.width > 0 && rect.height > 0;
        };
        for (const el of document.querySelectorAll('button, a, li, [role="button"], [role="link"], [role="menuitem"], [role="option"], div[jsname], div[jscontroller]')) {
            if (!shown(el)) continue;
            const text = (el.innerText || el.textContent || '').replace(/\s+/g, ' ').trim();
            if (!text || text.length > 120) continue;
            if (!/接收验证码|receive (a )?(verification )?code/i.test(text)) continue;
            if (/其他方式|another way/i.test(text)) continue;
            el.scrollIntoView({ block: 'center', inline: 'center' });
            const rect = el.getBoundingClientRect();
            if (rect.width < 8 || rect.height < 8) continue;
            return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
        }
        return null;
    })()"#;
    let value = match client.evaluate_js(script).await {
        Ok(value) => value,
        Err(_) => return false,
    };
    let (Some(x), Some(y)) = (
        value.get("x").and_then(|value| value.as_f64()),
        value.get("y").and_then(|value| value.as_f64()),
    ) else {
        return false;
    };
    mouse_click(client, x, y).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    // When the option is still present after the mouse click, use a synthetic
    // element click so the chooser still advances.
    let fallback = r#"(() => {
        const shown = el => {
            if (!el || !el.isConnected) return false;
            const style = getComputedStyle(el);
            if (style.display === 'none' || style.visibility === 'hidden') return false;
            const rect = el.getBoundingClientRect();
            return rect.width > 0 && rect.height > 0;
        };
        for (const el of document.querySelectorAll('button, a, li, [role="button"], [role="link"], [role="menuitem"], [role="option"], div[jsname], div[jscontroller]')) {
            if (!shown(el)) continue;
            const text = (el.innerText || el.textContent || '').replace(/\s+/g, ' ').trim();
            if (!text || text.length > 120) continue;
            if (!/接收验证码|receive (a )?(verification )?code/i.test(text)) continue;
            if (/其他方式|another way/i.test(text)) continue;
            el.click();
            return true;
        }
        return false;
    })()"#;
    let _ = client.evaluate_js(fallback).await;
    true
}

async fn mouse_click(client: &CdpClient, x: f64, y: f64) {
    let _ = client
        .call_cdp(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y }),
        )
        .await;
    let _ = client
        .call_cdp(
            "Input.dispatchMouseEvent",
            json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 }),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    let _ = client
        .call_cdp(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1 }),
        )
        .await;
}

async fn mailbox_open(client: &reqwest::Client, email: &str) -> Result<String, String> {
    let response = client
        .post(MAIL_LOGIN)
        .form(&[("email", email)])
        .send()
        .await
        .map_err(|error| format!("连接 google111111.asia 失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("google111111.asia 返回 HTTP {}", response.status()));
    }
    Ok(response.url().to_string())
}

async fn mailbox_codes(client: &reqwest::Client, inbox_url: &str) -> Result<Vec<String>, String> {
    let separator = if inbox_url.contains('?') { '&' } else { '?' };
    let refresh_url = format!("{inbox_url}{separator}refresh=1");
    let html = client
        .get(refresh_url)
        .send()
        .await
        .map_err(|error| format!("刷新验证码收件箱失败：{error}"))?
        .text()
        .await
        .map_err(|error| format!("读取验证码收件箱失败：{error}"))?;
    Ok(extract_codes(&html))
}

async fn wait_for_new_code(
    client: &reqwest::Client,
    email: &str,
    inbox_url: Option<&str>,
    baseline: &[String],
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let known: HashSet<&str> = baseline.iter().map(String::as_str).collect();
    let mut last_error = String::new();
    let mut inbox = inbox_url.map(str::to_string);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let Some(url) = inbox.clone() else {
            inbox = mailbox_open(client, email).await.ok();
            continue;
        };
        match mailbox_codes(client, &url).await {
            Ok(codes) => {
                if let Some(code) = codes.iter().find(|code| !known.contains(code.as_str())) {
                    return Ok(code.clone());
                }
            }
            Err(error) => {
                last_error = error;
                inbox = None;
            }
        }
    }
    if last_error.is_empty() {
        Err(format!(
            "{} 秒内没有从 google111111.asia 收到新验证码",
            timeout.as_secs()
        ))
    } else {
        Err(format!("获取验证码失败：{last_error}"))
    }
}

/// Extract verification codes from the inbox HTML. The site renders explicit
/// `.verify-code` chips and also the raw mail body inside `.code` blocks.
pub fn extract_codes(html: &str) -> Vec<String> {
    let mut codes = Vec::new();
    for marker in ["class=\"verify-code\"", "class=\"code\""] {
        let mut rest = html;
        while let Some(index) = rest.find(marker) {
            let after = &rest[index + marker.len()..];
            let Some(start) = after.find('>') else {
                break;
            };
            let body = &after[start + 1..];
            let end = body
                .find("</div>")
                .or_else(|| body.find("</span>"))
                .unwrap_or(body.len().min(4000));
            for code in six_digit_runs(&strip_tags(&body[..end])) {
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
            rest = &body[end..];
        }
    }
    codes
}

fn strip_tags(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut inside = false;
    for character in text.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => output.push(character),
            _ => {}
        }
    }
    output
}

fn six_digit_runs(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut codes = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index - start == 6 {
                codes.push(text[start..index].to_string());
            }
            continue;
        }
        index += 1;
    }
    codes
}

async fn save_failure_shot(client: &CdpClient, account: &str) {
    let directory = SessionData::config_dir().join("batch-login-shots");
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    if let Ok(png) = client.screenshot_png().await {
        let path = directory.join(format!("{account}.png"));
        if std::fs::write(&path, png).is_ok() {
            println!("  失败截图：{}", path.display());
        }
    }
}

fn batch_profile_dir(account: &str) -> Result<PathBuf, String> {
    let parent = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Executable has no parent directory".to_string())?;
    Ok(parent
        .join("desktop-profiles")
        .join(format!("batch-{account}.profile")))
}

fn stop_chrome(chrome: &mut Child) {
    #[cfg(windows)]
    {
        let pid = chrome.id().to_string();
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = chrome.kill();
    let _ = chrome.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_account_per_line() {
        let raw = "\u{feff}first@example.asia----pass-one---\r\n\r\n# comment\nother@example.com----pa-ss---\n";
        let accounts = parse_credentials(raw).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].email, "first@example.asia");
        assert_eq!(accounts[0].password, "pass-one");
        assert!(accounts[0].extra.is_none());
        assert_eq!(accounts[1].password, "pa-ss");
    }

    #[test]
    fn splits_bare_cr_records() {
        let raw = "first@example.asia----pass-one---\rsecond@example.com----pass-two---\r";
        let accounts = parse_credentials(raw).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].email, "first@example.asia");
        assert_eq!(accounts[1].email, "second@example.com");
        assert_eq!(accounts[1].password, "pass-two");
    }

    #[test]
    fn splits_unicode_and_mixed_line_breaks() {
        let raw = "a@example.asia----one---\u{2028}b@example.asia----two---\r\n\
                   c@example.asia----three---\rb@example.asia----two---";
        let accounts = parse_credentials(raw).unwrap();
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].password, "one");
        assert_eq!(accounts[1].password, "two");
        assert_eq!(accounts[2].password, "three");
    }

    #[test]
    fn splits_glued_two_field_records() {
        let raw = "first@example.asia----pass-one---second@example.com----pass-two---";
        let accounts = parse_credentials(raw).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].email, "first@example.asia");
        assert_eq!(accounts[0].password, "pass-one");
        assert_eq!(accounts[1].email, "second@example.com");
        assert_eq!(accounts[1].password, "pass-two");
    }

    #[test]
    fn splits_glued_three_field_records() {
        let raw = "first@example.asia----pass#one----GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ---\
                   second@example.com----pass-two----JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP---";
        let accounts = parse_credentials(raw).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].password, "pass#one");
        assert_eq!(
            accounts[0].extra.as_deref(),
            Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")
        );
        assert_eq!(accounts[1].password, "pass-two");
        assert_eq!(
            accounts[1].extra.as_deref(),
            Some("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP")
        );
        assert!(accounts[1].totp_secret().is_some());
    }

    #[test]
    fn splits_glued_mixed_field_records() {
        let raw = "first@example.asia----pass-one---second@example.com----pass-two----SECRET---\
                   third@example.asia----pass-three---";
        let accounts = parse_credentials(raw).unwrap();
        assert_eq!(accounts.len(), 3);
        assert!(accounts[0].extra.is_none());
        assert_eq!(accounts[1].password, "pass-two");
        assert_eq!(accounts[1].extra.as_deref(), Some("SECRET"));
        assert!(accounts[2].extra.is_none());
    }

    #[test]
    fn reads_utf16_file_with_a_byte_order_mark() {
        let text = "first@example.asia----pass-one---\r\nsecond@example.com----pass-two---\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let path = std::env::temp_dir().join(format!(
            "dragoman-accounts-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, &bytes).unwrap();
        let raw = read_account_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let accounts = parse_credentials(&raw).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[1].email, "second@example.com");
    }

    #[test]
    fn parses_the_three_field_format() {
        let raw = "first@example.asia----pass#one----GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\n\
                   second@example.asia----plain----\n\
                   third@example.asia----dash-pass---- note\n";
        let accounts = parse_credentials(raw).unwrap();
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].password, "pass#one");
        assert_eq!(
            accounts[0].extra.as_deref(),
            Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")
        );
        assert!(accounts[0].totp_secret().is_some());
        assert_eq!(accounts[1].password, "plain");
        assert!(accounts[1].extra.is_none());
        assert_eq!(accounts[2].password, "dash-pass");
        assert_eq!(accounts[2].extra.as_deref(), Some("note"));
        assert!(accounts[2].totp_secret().is_none());
    }

    #[test]
    fn duplicate_emails_are_collapsed() {
        let raw = "a@b.c----one---\na@b.c----two---";
        assert_eq!(parse_credentials(raw).unwrap().len(), 1);
    }

    #[test]
    fn rejects_a_line_without_a_separator() {
        assert!(parse_credentials("broken@example.com password").is_err());
    }

    #[test]
    fn account_names_stay_portable() {
        assert_eq!(account_name_for_email("a+b@example.com"), "a_b");
        assert_eq!(account_name_for_email("CON@example.com"), "account-CON");
    }

    #[test]
    fn extracts_codes_from_the_inbox_markup() {
        let html = r#"
            <div class="verify-row"><span class="verify-code">482913</span></div>
            <div class="code">G-123456 is your Google verification code.
            Security alert https://myaccount.google.com/alert/nt/179561664079675790?rfn%3D1</div>
        "#;
        assert_eq!(extract_codes(html), vec!["482913", "123456"]);
    }
}
