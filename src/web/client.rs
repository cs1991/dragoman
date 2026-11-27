use futures::StreamExt;
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, ORIGIN, REFERER, USER_AGENT};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tracing::info;

use crate::accounts::{Account, AccountPool, BalanceStrategy, FailureKind, RequestKind};
use crate::openai::types::MediaAttachment;
use crate::session::SessionData;
use crate::web::models::{self, ModelSpec};
use crate::web::upload::{UploadError, UploadedFile, upload_attachment};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamChunkPayload {
    pub conversation_id: Option<String>,
    pub response_id: Option<String>,
    pub choice_id: Option<String>,
    pub candidate_text: Option<String>,
    /// Set when the chunk is an upstream queue heartbeat (`{"37":[n]}`).
    ///
    /// Gemini Web emits these only while a request is parked before the
    /// candidate phase. A normal generation never produces them, including
    /// the long silence while a video is analyzed.
    pub parked: bool,
}

pub struct GeminiWebClient {
    pool: Arc<AccountPool>,
}

struct StreamAttempt {
    text: String,
    error: Option<String>,
    kind: Option<FailureKind>,
    chunks: usize,
}

impl StreamAttempt {
    fn failed(error: String, kind: FailureKind) -> Self {
        Self {
            text: String::new(),
            error: Some(error),
            kind: Some(kind),
            chunks: 0,
        }
    }
}

struct TurnError {
    message: String,
    kind: FailureKind,
    streamed: bool,
}

fn upload_failure_kind(error: &UploadError) -> Option<FailureKind> {
    match error {
        UploadError::Invalid(_) | UploadError::TooLarge(_) => None,
        UploadError::Connection(_) => Some(FailureKind::Network),
        UploadError::Upstream(message) => {
            if message.contains("HTTP 401") || message.contains("HTTP 403") {
                Some(FailureKind::Auth)
            } else if message.contains("HTTP 429") {
                Some(FailureKind::Quota)
            } else {
                Some(FailureKind::Transient)
            }
        }
    }
}

/// Classify a request that never produced an HTTP response.
///
/// A refused TCP connection, a failed TLS handshake or a connect timeout is an
/// egress problem shared by every account, so it must not move the per-account
/// failure counter.
fn transport_failure_kind(error: &reqwest::Error) -> FailureKind {
    if error.is_connect() || error.is_request() || error.is_timeout() {
        FailureKind::Network
    } else {
        FailureKind::Transient
    }
}

fn idle_timeout() -> Duration {
    std::env::var("DRAGOMAN_STREAM_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(120))
}

/// Heartbeats Gemini Web emits while a request sits in the upstream queue.
///
/// A served request never receives one, so a few consecutive heartbeats with
/// no candidate text identify a parked model. Waiting for the 120 second
/// stream idle timeout instead makes the caller hang on a request the backend
/// has already shelved.
fn parked_tick_limit() -> usize {
    std::env::var("DRAGOMAN_PARKED_TICKS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|ticks| *ticks > 0)
        .unwrap_or(3)
}

/// Marker prefixed to the message of a parked-model failure.
///
/// The proxy strips it and answers with a 503 instead of a generic 500.
pub const MODEL_PARKED_MARKER: &str = "model_parked: ";

fn transport_error(action: &str, error: &reqwest::Error) -> String {
    let detail = if error.is_timeout() {
        "The connection or response timed out. Check your network and proxy settings."
    } else if error.is_connect() {
        "The connection failed. Check your network and system HTTPS proxy."
    } else if error.is_redirect() {
        "Gemini Web returned an unexpected redirect. Check the proxy or sign in again."
    } else {
        "The HTTPS request did not complete."
    };
    format!("{action}: {detail}")
}

/// Decode a byte stream that can split one UTF-8 character across chunks.
///
/// Decoding each chunk on its own turns a split three-byte Chinese character
/// into three replacement characters, so an incomplete tail waits in `pending`
/// for the next chunk. Bytes that are genuinely invalid still become U+FFFD.
fn decode_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    let mut output = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                output.push_str(text);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0
                    && let Ok(text) = std::str::from_utf8(&pending[..valid])
                {
                    output.push_str(text);
                }
                match error.error_len() {
                    Some(length) => {
                        output.push('\u{FFFD}');
                        pending.drain(..valid + length);
                    }
                    None => {
                        // Incomplete character at the end of this chunk.
                        pending.drain(..valid);
                        break;
                    }
                }
            }
        }
    }
    output
}

/// Message used when every account is parked or cooling down.
///
/// Retrying an abnormal account only burns one more failure, so the caller gets
/// one actionable sentence instead.
fn no_healthy_account_message(pool: &AccountPool, kind: RequestKind) -> String {
    if pool.is_empty() {
        "No Gemini account is loaded right now. Sign in one; the proxy picks it up automatically."
            .to_string()
    } else if kind == RequestKind::Chat
        && !pool.allow_free_chat()
        && pool
            .accounts()
            .iter()
            .all(|account| account.tier() == crate::session::AccountTier::Free)
    {
        "Every loaded account is a free account and free chat is off. Mark one account as pro or enable allow-free-chat in the GUI.".to_string()
    } else {
        "No Gemini account is available right now. Every account is cooling down or marked abnormal. Sign in again to repair one, or clear its mark with 'dragoman accounts --clear NAME'.".to_string()
    }
}

impl GeminiWebClient {
    pub fn new(session: SessionData) -> Self {
        Self::with_pool(Arc::new(AccountPool::single(session)))
    }

    pub fn with_pool(pool: Arc<AccountPool>) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> Arc<AccountPool> {
        self.pool.clone()
    }

    pub fn session(&self) -> Option<Arc<Mutex<SessionData>>> {
        let accounts = self.pool.accounts();
        accounts.first().map(|account| account.session.clone())
    }

    pub async fn current_session(&self) -> Option<SessionData> {
        let accounts = self.pool.accounts();
        match accounts.first() {
            Some(account) => Some(account.session.lock().await.clone()),
            None => None,
        }
    }

    pub fn strategy(&self) -> BalanceStrategy {
        self.pool.strategy()
    }

    /// Build verified Gemini 4 Argon payload array (99 elements).
    /// Slot 71 holds the conversation identifier array when present.
    /// Slot 3 holds the previous response identifier when present.
    pub fn build_stream_generate_payload(
        prompt: &str,
        conv_id: Option<&str>,
        resp_id: Option<&str>,
    ) -> Vec<serde_json::Value> {
        Self::build_stream_generate_payload_with_files(prompt, conv_id, resp_id, &[])
    }

    pub fn build_stream_generate_payload_with_files(
        prompt: &str,
        conv_id: Option<&str>,
        resp_id: Option<&str>,
        files: &[UploadedFile],
    ) -> Vec<serde_json::Value> {
        Self::build_stream_generate_payload_with_files_and_model(
            prompt,
            conv_id,
            resp_id,
            files,
            models::default_model(),
        )
    }

    pub fn build_stream_generate_payload_with_files_and_model(
        prompt: &str,
        conv_id: Option<&str>,
        resp_id: Option<&str>,
        files: &[UploadedFile],
        model: ModelSpec,
    ) -> Vec<serde_json::Value> {
        let mut inner: Vec<serde_json::Value> = vec![serde_json::Value::Null; 99];
        let attachments = if files.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Array(
                files
                    .iter()
                    .map(|file| json!([[file.path.clone()], file.name.clone()]))
                    .collect(),
            )
        };
        inner[0] = json!([prompt, 0, null, attachments, null, null, 0]);
        inner[1] = json!(["en"]);
        if let Some(rid) = resp_id {
            inner[3] = json!(rid);
        }
        inner[6] = json!([1]);
        inner[7] = json!(1);
        inner[10] = json!(1);
        inner[11] = json!(0);
        inner[17] = json!([[0]]);
        inner[18] = json!(0);
        inner[27] = json!(1);
        inner[30] = json!([4, 16, 19]);
        inner[41] = json!([1]);
        inner[49] = json!(0);
        inner[53] = json!(0);
        inner[61] = json!([]);
        inner[67] = json!(0);
        inner[68] = json!(2);
        if let Some(cid) = conv_id {
            inner[71] = json!([cid, null, null, null, null, null, null, null]);
        }
        inner[59] = json!(uuid::Uuid::new_v4().to_string().to_uppercase());
        inner[79] = json!(model.number);
        inner[80] = json!(1);
        inner[91] = json!(1);
        inner[96] = json!(0);
        inner[98] = json!(1);
        inner
    }

    /// Build the f.req form envelope string containing the JSON-serialized payload array.
    pub fn build_form_envelope(
        prompt: &str,
        conv_id: Option<&str>,
        resp_id: Option<&str>,
    ) -> String {
        Self::build_form_envelope_with_files(prompt, conv_id, resp_id, &[])
    }

    pub fn build_form_envelope_with_files(
        prompt: &str,
        conv_id: Option<&str>,
        resp_id: Option<&str>,
        files: &[UploadedFile],
    ) -> String {
        Self::build_form_envelope_with_files_and_model(
            prompt,
            conv_id,
            resp_id,
            files,
            models::default_model(),
        )
    }

    pub fn build_form_envelope_with_files_and_model(
        prompt: &str,
        conv_id: Option<&str>,
        resp_id: Option<&str>,
        files: &[UploadedFile],
        model: ModelSpec,
    ) -> String {
        let inner = Self::build_stream_generate_payload_with_files_and_model(
            prompt, conv_id, resp_id, files, model,
        );
        let inner_json = serde_json::to_string(&inner).unwrap_or_default();
        json!([null, inner_json]).to_string()
    }

    /// Parse a single line from an upstream StreamGenerate chunk.
    /// Extracts (conversation_id, response_id, choice_id, candidate_text) if present.
    pub fn parse_wrb_fr_line(line: &str) -> Option<StreamChunkPayload> {
        let trimmed = line.trim();
        let stripped = trimmed.strip_prefix(")]}'").unwrap_or(trimmed).trim();
        if stripped.is_empty() || stripped.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }

        let val = serde_json::from_str::<serde_json::Value>(stripped).ok()?;

        let mut conv_id = None;
        let mut resp_id = None;
        let mut choice_id = None;
        let mut candidate_text = None;
        let mut parked = false;

        let mut entries = Vec::new();
        Self::collect_wrb_fr_entries(&val, &mut entries);

        for entry in entries {
            if let Some(payload_str) = entry.get(2).and_then(|v| v.as_str())
                && let Ok(inner_obj) = serde_json::from_str::<serde_json::Value>(payload_str)
            {
                if let Some(meta) = inner_obj.get(1).and_then(|m| m.as_array()) {
                    if let Some(cid) = meta.first().and_then(|s| s.as_str()) {
                        conv_id = Some(cid.to_string());
                    }
                    if let Some(rid) = meta.get(1).and_then(|s| s.as_str()) {
                        resp_id = Some(rid.to_string());
                    }
                }

                if let Some(candidates) = inner_obj.get(4).and_then(|c| c.as_array())
                    && let Some(first_candidate) = candidates.first()
                {
                    if let Some(chid) = first_candidate.get(0).and_then(|c| c.as_str()) {
                        choice_id = Some(chid.to_string());
                    }
                    if let Some(text_entry) = first_candidate.get(1).and_then(|t| t.as_array())
                        && let Some(content) = text_entry.first().and_then(|s| s.as_str())
                    {
                        candidate_text = Some(content.to_string());
                    }
                }

                if let Some(first) = inner_obj.as_array().and_then(|arr| arr.first())
                    && first.get("37").is_some()
                {
                    parked = true;
                }
            }
        }

        if conv_id.is_some()
            || resp_id.is_some()
            || choice_id.is_some()
            || candidate_text.is_some()
            || parked
        {
            Some(StreamChunkPayload {
                conversation_id: conv_id,
                response_id: resp_id,
                choice_id,
                candidate_text,
                parked,
            })
        } else {
            None
        }
    }

    fn collect_wrb_fr_entries<'a>(
        val: &'a serde_json::Value,
        out: &mut Vec<&'a Vec<serde_json::Value>>,
    ) {
        if let Some(arr) = val.as_array() {
            if arr.first().and_then(|v| v.as_str()) == Some("wrb.fr") {
                out.push(arr);
            } else {
                for item in arr {
                    Self::collect_wrb_fr_entries(item, out);
                }
            }
        }
    }

    pub async fn send_prompt_stream(
        &self,
        prompt: &str,
        files: &[UploadedFile],
        delta_tx: mpsc::Sender<Result<String, String>>,
    ) -> Result<String, String> {
        self.send_prompt_stream_with_model(
            prompt,
            files,
            models::default_model(),
            RequestKind::Chat,
            delta_tx,
        )
        .await
    }

    pub async fn send_prompt_stream_with_model(
        &self,
        prompt: &str,
        files: &[UploadedFile],
        model: ModelSpec,
        kind: RequestKind,
        delta_tx: mpsc::Sender<Result<String, String>>,
    ) -> Result<String, String> {
        let Some(lease) = self.pool.lease_for(kind).await else {
            let message = no_healthy_account_message(&self.pool, kind);
            let _ = delta_tx.send(Err(message.clone())).await;
            return Err(message);
        };
        let account = lease.account.clone();
        let result = self
            .run_turn(&account, prompt, files, model, delta_tx.clone())
            .await;
        drop(lease);

        match result {
            Ok(text) => {
                self.pool.note_success(&account);
                Ok(text)
            }
            Err(error) => {
                account.note_failure(error.kind, &error.message);
                let _ = delta_tx.send(Err(error.message.clone())).await;
                Err(error.message)
            }
        }
    }

    /// Upload media with the account that will answer, then send the prompt.
    /// A failed account is parked for its cooldown and the next account is
    /// tried with a fresh upload, so uploads never cross account boundaries.
    pub async fn send_prompt_stream_with_media(
        &self,
        prompt: &str,
        media: &[MediaAttachment],
        max_attachment_bytes: u64,
        kind: RequestKind,
        delta_tx: mpsc::Sender<Result<String, String>>,
    ) -> Result<String, String> {
        self.send_prompt_stream_with_media_and_model(
            prompt,
            media,
            models::default_model(),
            max_attachment_bytes,
            kind,
            delta_tx,
        )
        .await
    }

    pub async fn send_prompt_stream_with_media_and_model(
        &self,
        prompt: &str,
        media: &[MediaAttachment],
        model: ModelSpec,
        max_attachment_bytes: u64,
        kind: RequestKind,
        delta_tx: mpsc::Sender<Result<String, String>>,
    ) -> Result<String, String> {
        let attempt_cap = (self.pool.len().saturating_mul(2)).clamp(1, 4);
        let mut last_error = "All Gemini accounts failed".to_string();
        let mut tried: Vec<String> = Vec::new();

        for attempt in 0..attempt_cap {
            let Some(lease) = self.pool.lease_for(kind).await else {
                last_error = no_healthy_account_message(&self.pool, kind);
                break;
            };
            let account = lease.account.clone();
            if tried.contains(&account.name) && tried.len() >= self.pool.len() {
                drop(lease);
                break;
            }
            tried.push(account.name.clone());
            info!(
                "Account {} handling request (attempt {}/{}, balance {})",
                account.name,
                attempt + 1,
                attempt_cap,
                self.pool.strategy().label()
            );

            let mut files = Vec::with_capacity(media.len());
            let mut upload_error = None;
            if !media.is_empty() {
                let cookie = account.session.lock().await.cookie.clone();
                for attachment in media {
                    match upload_attachment(
                        &account.http,
                        &cookie,
                        attachment,
                        max_attachment_bytes,
                    )
                    .await
                    {
                        Ok(file) => {
                            info!("[{}] Uploaded attachment: {}", account.name, file.name);
                            files.push(file);
                        }
                        Err(error) => {
                            upload_error = Some(error);
                            break;
                        }
                    }
                }
            }

            if let Some(error) = upload_error {
                let message = error.to_string();
                match upload_failure_kind(&error) {
                    Some(kind) => {
                        info!(
                            "Account {} upload failed ({}): {}",
                            account.name,
                            kind.label(),
                            message
                        );
                        account.note_failure(kind, &message);
                        last_error = message;
                        drop(lease);
                        continue;
                    }
                    None => {
                        drop(lease);
                        let _ = delta_tx.send(Err(message.clone())).await;
                        return Err(message);
                    }
                }
            }

            let result = self
                .run_turn(&account, prompt, &files, model, delta_tx.clone())
                .await;
            drop(lease);

            match result {
                Ok(text) => {
                    self.pool.note_success(&account);
                    return Ok(text);
                }
                Err(error) => {
                    info!(
                        "Account {} failed ({}): {}",
                        account.name,
                        error.kind.label(),
                        error.message
                    );
                    account.note_failure(error.kind, &error.message);
                    last_error = error.message;
                    if error.kind == FailureKind::Model {
                        let _ = delta_tx.send(Err(last_error.clone())).await;
                        return Err(last_error);
                    }
                    if error.streamed {
                        let _ = delta_tx.send(Err(last_error.clone())).await;
                        return Err(last_error);
                    }
                }
            }
        }

        let _ = delta_tx.send(Err(last_error.clone())).await;
        Err(last_error)
    }

    async fn run_turn(
        &self,
        account: &Account,
        prompt: &str,
        files: &[UploadedFile],
        model: ModelSpec,
        delta_tx: mpsc::Sender<Result<String, String>>,
    ) -> Result<String, TurnError> {
        let had_context = {
            let session = account.session.lock().await;
            session.conversation_id.is_some() || session.response_id.is_some()
        };

        let first = self
            .stream_attempt(account, prompt, files, model, delta_tx.clone())
            .await;
        if first.error.is_none() && !first.text.trim().is_empty() {
            return Ok(first.text);
        }

        let streamed = !first.text.is_empty();
        let first_error = first
            .error
            .clone()
            .unwrap_or_else(|| "Gemini Web returned an empty response".to_string());
        let first_kind = first.kind.unwrap_or(FailureKind::Context);
        info!(
            "[{}] Gemini Web attempt ended with {} chunks and {} characters: {}",
            account.name,
            first.chunks,
            first.text.chars().count(),
            first_error
        );
        self.reset_conversation(account, "failed Gemini Web turn")
            .await;

        if had_context && !streamed && first_kind == FailureKind::Context {
            info!(
                "[{}] Retrying once with a fresh conversation after: {}",
                account.name, first_error
            );
            let second = self
                .stream_attempt(account, prompt, files, model, delta_tx)
                .await;
            if second.error.is_none() && !second.text.trim().is_empty() {
                return Ok(second.text);
            }
            self.reset_conversation(account, "failed Gemini Web retry")
                .await;
            return Err(TurnError {
                message: second
                    .error
                    .unwrap_or_else(|| "Gemini Web returned an empty response".to_string()),
                kind: second.kind.unwrap_or(FailureKind::Context),
                streamed: !second.text.is_empty(),
            });
        }

        Err(TurnError {
            message: first_error,
            kind: first_kind,
            streamed,
        })
    }

    async fn reset_conversation(&self, account: &Account, reason: &str) {
        let mut session = account.session.lock().await;
        if session.conversation_id.is_none()
            && session.response_id.is_none()
            && session.choice_id.is_none()
        {
            return;
        }
        info!(
            "[{}] Clearing Gemini conversation context: {}",
            account.name, reason
        );
        session.conversation_id = None;
        session.response_id = None;
        session.choice_id = None;
        if let Err(error) = session.save() {
            tracing::error!("Could not persist cleared Gemini conversation: {}", error);
        }
    }

    async fn stream_attempt(
        &self,
        account: &Account,
        prompt: &str,
        files: &[UploadedFile],
        model: ModelSpec,
        delta_tx: mpsc::Sender<Result<String, String>>,
    ) -> StreamAttempt {
        let (snlm0e, f_sid, bl, cookie, conv_id, resp_id, base_url) = {
            let s = account.session.lock().await;
            (
                s.snlm0e.clone(),
                s.f_sid.clone(),
                s.bl.clone(),
                s.cookie.clone(),
                s.conversation_id.clone(),
                s.response_id.clone(),
                s.base_url
                    .clone()
                    .unwrap_or_else(|| "https://gemini.google.com".to_string()),
            )
        };

        let req_id = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 10_000_000) as u64;

        let url = format!(
            "{}/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate?bl={}&f.sid={}&hl=en&_reqid={}&rt=c",
            base_url.trim_end_matches('/'),
            bl,
            f_sid,
            req_id
        );

        let f_req = Self::build_form_envelope_with_files_and_model(
            prompt,
            conv_id.as_deref(),
            resp_id.as_deref(),
            files,
            model,
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36"),
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded;charset=UTF-8"),
        );
        headers.insert("X-Same-Domain", HeaderValue::from_static("1"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://gemini.google.com"),
        );
        headers.insert(
            REFERER,
            HeaderValue::from_static("https://gemini.google.com/"),
        );
        if let Ok(mut cookie_val) = HeaderValue::from_str(&cookie) {
            cookie_val.set_sensitive(true);
            headers.insert(COOKIE, cookie_val);
        }
        let model_session = uuid::Uuid::new_v4().to_string().to_uppercase();
        if let Ok(value) = HeaderValue::from_str(&models::model_header(
            model,
            models::capacity(),
            false,
            &model_session,
        )) {
            headers.insert("x-goog-ext-525001261-jspb", value);
        }
        headers.insert("x-goog-ext-73010989-jspb", HeaderValue::from_static("[0]"));
        headers.insert(
            "x-goog-ext-73010990-jspb",
            HeaderValue::from_static("[0,0,0]"),
        );
        if !files.is_empty() {
            let jspb = format!(
                "[\"{}\",1]",
                uuid::Uuid::new_v4().to_string().to_uppercase()
            );
            if let Ok(value) = HeaderValue::from_str(&jspb) {
                headers.insert("x-goog-ext-525005358-jspb", value);
            }
        }

        info!(
            "[{}] Sending native direct RPC to Gemini Web (StreamGenerate, {})...",
            account.name, model.label
        );

        let response = match account
            .http
            .post(&url)
            .headers(headers)
            .form(&[("f.req", &f_req), ("at", &snlm0e)])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let err_msg = transport_error("Gemini Web request failed", &e);
                return StreamAttempt::failed(err_msg, transport_failure_kind(&e));
            }
        };

        let status = response.status();
        if !status.is_success() {
            let err_msg = format!("Gemini Web returned HTTP {}", status.as_u16());
            let kind = match status.as_u16() {
                429 => FailureKind::Quota,
                401 | 403 => FailureKind::Auth,
                _ => FailureKind::Transient,
            };
            return StreamAttempt::failed(err_msg, kind);
        }
        info!(
            "[{}] Gemini Web response status: {}",
            account.name,
            status.as_u16()
        );

        let mut stream = response.bytes_stream();
        let mut full_text = String::new();
        let mut line_buffer = String::new();
        let mut pending_bytes: Vec<u8> = Vec::new();
        let mut first_chunk = true;
        let mut chunks = 0usize;
        let mut parked_ticks = 0usize;
        let idle = idle_timeout();

        let mut new_conv_id: Option<String> = None;
        let mut new_resp_id: Option<String> = None;
        let mut new_choice_id: Option<String> = None;

        loop {
            if delta_tx.is_closed() {
                let err_msg = "Client closed the response stream".to_string();
                info!("{}", err_msg);
                return StreamAttempt {
                    text: full_text,
                    error: Some(err_msg),
                    kind: Some(FailureKind::Context),
                    chunks,
                };
            }

            let next = match tokio::time::timeout(idle, stream.next()).await {
                Ok(next) => next,
                Err(_) => {
                    let err_msg = format!(
                        "Gemini Web stream stalled: no data for {} seconds after {} chunks",
                        idle.as_secs(),
                        chunks
                    );
                    return StreamAttempt {
                        text: full_text,
                        error: Some(err_msg),
                        kind: Some(FailureKind::Context),
                        chunks,
                    };
                }
            };

            let Some(chunk_res) = next else {
                if !pending_bytes.is_empty() {
                    line_buffer.push_str(&String::from_utf8_lossy(&pending_bytes));
                    pending_bytes.clear();
                }
                break;
            };
            chunks += 1;
            let chunk = match chunk_res {
                Ok(c) => {
                    if first_chunk {
                        info!(
                            "[{}] Gemini Web stream delivered its first chunk",
                            account.name
                        );
                        first_chunk = false;
                    }
                    c
                }
                Err(e) => {
                    let err_msg = transport_error("Gemini Web stream read failed", &e);
                    return StreamAttempt {
                        text: full_text,
                        error: Some(err_msg),
                        kind: Some(FailureKind::Transient),
                        chunks,
                    };
                }
            };
            let chunk_str = decode_utf8_chunk(&mut pending_bytes, &chunk);
            line_buffer.push_str(&chunk_str);

            while let Some(newline_pos) = line_buffer.find('\n') {
                let line: String = line_buffer[..newline_pos].trim().to_string();
                line_buffer.drain(..=newline_pos);
                Self::process_stream_line(
                    &line,
                    &mut full_text,
                    &mut new_conv_id,
                    &mut new_resp_id,
                    &mut new_choice_id,
                    &mut parked_ticks,
                    &delta_tx,
                )
                .await;
            }

            if parked_ticks >= parked_tick_limit() && full_text.trim().is_empty() {
                let err_msg = format!(
                    "{}Gemini Web parked the request for model {} after {} queue heartbeats and never started generating. The signed-in accounts are not being served this model right now.",
                    MODEL_PARKED_MARKER, model.label, parked_ticks
                );
                info!("[{}] {}", account.name, err_msg);
                return StreamAttempt {
                    text: full_text,
                    error: Some(err_msg),
                    kind: Some(FailureKind::Model),
                    chunks,
                };
            }
        }

        let remaining = line_buffer.trim().to_string();
        if !remaining.is_empty() {
            Self::process_stream_line(
                &remaining,
                &mut full_text,
                &mut new_conv_id,
                &mut new_resp_id,
                &mut new_choice_id,
                &mut parked_ticks,
                &delta_tx,
            )
            .await;
        }

        // Update session with conversation context if established
        if new_conv_id.is_some() || new_resp_id.is_some() || new_choice_id.is_some() {
            let mut s = account.session.lock().await;
            if let Some(cid) = new_conv_id {
                s.conversation_id = Some(cid);
            }
            if let Some(rid) = new_resp_id {
                s.response_id = Some(rid);
            }
            if let Some(chid) = new_choice_id {
                s.choice_id = Some(chid);
            }
            let _ = s.save();
        }

        info!(
            "[{}] Gemini Web stream completed with {} characters in {} chunks",
            account.name,
            full_text.chars().count(),
            chunks
        );
        StreamAttempt {
            text: full_text,
            error: None,
            kind: None,
            chunks,
        }
    }

    pub async fn upload_media(
        &self,
        media: &MediaAttachment,
        max_bytes: u64,
    ) -> Result<UploadedFile, UploadError> {
        let accounts = self.pool.accounts();
        let Some(account) = accounts.first() else {
            return Err(UploadError::Invalid(
                "No Gemini account is configured".to_string(),
            ));
        };
        let cookie = account.session.lock().await.cookie.clone();
        upload_attachment(&account.http, &cookie, media, max_bytes).await
    }

    async fn process_stream_line(
        line: &str,
        full_text: &mut String,
        new_conv_id: &mut Option<String>,
        new_resp_id: &mut Option<String>,
        new_choice_id: &mut Option<String>,
        parked_ticks: &mut usize,
        delta_tx: &mpsc::Sender<Result<String, String>>,
    ) {
        if let Some(parsed) = Self::parse_wrb_fr_line(line) {
            if parsed.parked && parsed.candidate_text.is_none() {
                *parked_ticks += 1;
            }
            if let Some(cid) = parsed.conversation_id {
                *new_conv_id = Some(cid);
            }
            if let Some(rid) = parsed.response_id {
                *new_resp_id = Some(rid);
            }
            if let Some(chid) = parsed.choice_id {
                *new_choice_id = Some(chid);
            }
            if let Some(new_content) = parsed.candidate_text {
                let delta = if new_content.starts_with(full_text.as_str()) {
                    new_content[full_text.len()..].to_string()
                } else if full_text.is_empty() {
                    new_content.clone()
                } else {
                    let common_len = full_text
                        .chars()
                        .zip(new_content.chars())
                        .take_while(|(a, b)| a == b)
                        .map(|(c, _)| c.len_utf8())
                        .sum::<usize>();
                    if new_content.len() > common_len {
                        new_content[common_len..].to_string()
                    } else {
                        String::new()
                    }
                };
                if !delta.is_empty() {
                    let _ = delta_tx.send(Ok(delta)).await;
                    *full_text = new_content;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn queue_heartbeat_is_reported_as_parked() {
        let line = r#"[["wrb.fr",null,"[{\"37\":[2]}]"]]"#;
        let parsed = super::GeminiWebClient::parse_wrb_fr_line(line).expect("parked frame");
        assert!(parsed.parked);
        assert!(parsed.candidate_text.is_none());
        assert!(parsed.conversation_id.is_none());
    }

    #[test]
    fn candidate_frame_is_not_parked() {
        let line =
            r#"[["wrb.fr",null,"[null,[\"c_1\",\"r_1\"],null,null,[[\"rc_1\",[\"PING\"]]]]"]]"#;
        let parsed = super::GeminiWebClient::parse_wrb_fr_line(line).expect("candidate frame");
        assert!(!parsed.parked);
        assert_eq!(parsed.candidate_text.as_deref(), Some("PING"));
    }

    #[tokio::test]
    async fn transport_errors_do_not_expose_request_urls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/rpc?private=fixture",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { drop(listener.accept().await.unwrap()) });
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap_err();
        assert!(error.url().is_some_and(|url| url.query().is_some()));
        let message = super::transport_error("Gemini Web request failed", &error);
        assert!(message.starts_with("Gemini Web request failed:"));
        assert!(!message.contains("http") && !message.contains("fixture"));
    }

    #[tokio::test]
    async fn refused_connections_are_classified_as_network_failures() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap()
            .get(format!("http://{address}/rpc"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(
            super::transport_failure_kind(&error),
            crate::accounts::FailureKind::Network
        );
    }

    #[test]
    fn utf8_split_across_chunks_is_reassembled() {
        let text = "凑近一步";
        let mut pending = Vec::new();
        let mut output = String::new();
        for byte in text.as_bytes() {
            output.push_str(&super::decode_utf8_chunk(&mut pending, &[*byte]));
        }
        assert_eq!(output, text);
        assert!(pending.is_empty());
    }

    #[test]
    fn incomplete_utf8_tail_waits_for_the_next_chunk() {
        let bytes = "中".as_bytes();
        let mut pending = Vec::new();
        assert_eq!(super::decode_utf8_chunk(&mut pending, &bytes[..2]), "");
        assert_eq!(pending.len(), 2);
        assert_eq!(super::decode_utf8_chunk(&mut pending, &bytes[2..]), "中");
        assert!(pending.is_empty());
    }

    #[test]
    fn invalid_utf8_becomes_a_replacement_character() {
        let mut pending = Vec::new();
        assert_eq!(super::decode_utf8_chunk(&mut pending, &[0xFF, b'a']), "\u{FFFD}a");
        assert!(pending.is_empty());
    }
}
