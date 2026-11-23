use crate::openai::types::{MediaAttachment, MediaKind, MediaSource, mime_from_filename};
use futures::StreamExt;
use reqwest::header::{COOKIE, HeaderMap, HeaderValue, ORIGIN, REFERER, USER_AGENT};
use reqwest::multipart::{Form, Part};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub const UPLOAD_ENDPOINT: &str = "https://content-push.googleapis.com/upload";
const UPLOAD_PUSH_ID: &str = "feeds/mcudyrk2a4khkz";
const UPLOAD_TENANT_ID: &str = "bard-storage";
const UPLOAD_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedFile {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadError {
    Invalid(String),
    TooLarge(String),
    Upstream(String),
}

impl UploadError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::TooLarge(_) => 413,
            Self::Upstream(_) => 502,
        }
    }
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::TooLarge(message) | Self::Upstream(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for UploadError {}

enum SourceData {
    Bytes {
        bytes: Vec<u8>,
        mime_hint: Option<String>,
    },
    File {
        path: PathBuf,
        length: u64,
    },
    Remote(String),
    AlreadyUploaded(String),
}

/// Upload one attachment to Google's push endpoint and return the file reference
/// that `StreamGenerate` expects in payload slot 0 index 3.
pub async fn upload_attachment(
    http: &reqwest::Client,
    cookie: &str,
    media: &MediaAttachment,
    max_bytes: u64,
) -> Result<UploadedFile, UploadError> {
    upload_to_endpoint(http, UPLOAD_ENDPOINT, cookie, media, max_bytes).await
}

pub async fn upload_to_endpoint(
    http: &reqwest::Client,
    endpoint: &str,
    cookie: &str,
    media: &MediaAttachment,
    max_bytes: u64,
) -> Result<UploadedFile, UploadError> {
    let source = resolve_source(media, max_bytes).await?;

    let mime_hint = match &source {
        SourceData::Bytes { mime_hint, .. } => mime_hint.clone(),
        _ => None,
    };

    let mime_type = media
        .mime_type
        .clone()
        .or_else(|| media.filename.as_deref().and_then(mime_from_filename))
        .or(mime_hint)
        .or_else(|| default_mime(media.kind).map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let filename = resolve_filename(media, &mime_type);

    if let SourceData::AlreadyUploaded(path) = source {
        return Ok(UploadedFile {
            path,
            name: filename,
        });
    }

    let part = build_part(http, source, &filename, &mime_type, cookie, max_bytes).await?;

    let mut headers = HeaderMap::new();
    headers.insert("x-tenant-id", HeaderValue::from_static(UPLOAD_TENANT_ID));
    headers.insert("push-id", HeaderValue::from_static(UPLOAD_PUSH_ID));
    headers.insert(
        ORIGIN,
        HeaderValue::from_static("https://gemini.google.com"),
    );
    headers.insert(
        REFERER,
        HeaderValue::from_static("https://gemini.google.com/"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(UPLOAD_USER_AGENT));
    if let Ok(mut cookie_value) = HeaderValue::from_str(cookie) {
        cookie_value.set_sensitive(true);
        headers.insert(COOKIE, cookie_value);
    }

    let form = Form::new().part("file", part);
    let response = http
        .post(endpoint)
        .headers(headers)
        .multipart(form)
        .timeout(UPLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            let detail = if error.is_timeout() {
                "Attachment upload timed out"
            } else if error.is_connect() {
                "Attachment upload could not connect to Google"
            } else {
                "Attachment upload failed"
            };
            UploadError::Upstream(detail.to_string())
        })?;

    let status = response.status();
    let body = response.text().await.map_err(|_| {
        UploadError::Upstream("Attachment upload response was unreadable".to_string())
    })?;

    if !status.is_success() {
        return Err(UploadError::Upstream(format!(
            "Attachment upload returned HTTP {}",
            status.as_u16()
        )));
    }

    let path = body.trim();
    if !path.starts_with("/contrib_service/")
        || path.len() > 2048
        || path.chars().any(char::is_whitespace)
    {
        return Err(UploadError::Upstream(
            "Attachment upload returned an unexpected file reference".to_string(),
        ));
    }

    Ok(UploadedFile {
        path: path.to_string(),
        name: filename,
    })
}

async fn resolve_source(
    media: &MediaAttachment,
    max_bytes: u64,
) -> Result<SourceData, UploadError> {
    match &media.source {
        MediaSource::DataUrl(url) => {
            let (mime, bytes) = decode_data_url(url, max_bytes)?;
            Ok(SourceData::Bytes {
                bytes,
                mime_hint: if mime.is_empty() { None } else { Some(mime) },
            })
        }
        MediaSource::FilePath(path) => {
            let path = normalize_local_path(path);
            let metadata = tokio::fs::metadata(&path).await.map_err(|_| {
                UploadError::Invalid(format!(
                    "Attachment file could not be read: {}",
                    path.display()
                ))
            })?;
            if !metadata.is_file() {
                return Err(UploadError::Invalid(format!(
                    "Attachment path is not a file: {}",
                    path.display()
                )));
            }
            if metadata.len() > max_bytes {
                return Err(too_large(metadata.len(), max_bytes));
            }
            Ok(SourceData::File {
                path,
                length: metadata.len(),
            })
        }
        MediaSource::Url(url) => Ok(SourceData::Remote(url.clone())),
        MediaSource::UploadedPath(path) => {
            let normalized = if path.starts_with('/') {
                path.clone()
            } else {
                format!("/{path}")
            };
            Ok(SourceData::AlreadyUploaded(normalized))
        }
    }
}

async fn build_part(
    http: &reqwest::Client,
    source: SourceData,
    filename: &str,
    mime_type: &str,
    cookie: &str,
    max_bytes: u64,
) -> Result<Part, UploadError> {
    match source {
        SourceData::Bytes { bytes, .. } => {
            if bytes.len() as u64 > max_bytes {
                return Err(too_large(bytes.len() as u64, max_bytes));
            }
            Part::bytes(bytes)
                .file_name(filename.to_string())
                .mime_str(mime_type)
                .map_err(|_| UploadError::Invalid("Attachment MIME type is invalid".to_string()))
        }
        SourceData::File { path, length } => {
            if length > max_bytes {
                return Err(too_large(length, max_bytes));
            }
            let mut file = tokio::fs::File::open(&path).await.map_err(|_| {
                UploadError::Invalid(format!(
                    "Attachment file could not be opened: {}",
                    path.display()
                ))
            })?;
            let stream = async_stream::stream! {
                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    match file.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(read) => yield Ok::<Vec<u8>, std::io::Error>(buffer[..read].to_vec()),
                        Err(error) => {
                            yield Err(error);
                            break;
                        }
                    }
                }
            };
            Part::stream_with_length(reqwest::Body::wrap_stream(stream), length)
                .file_name(filename.to_string())
                .mime_str(mime_type)
                .map_err(|_| UploadError::Invalid("Attachment MIME type is invalid".to_string()))
        }
        SourceData::Remote(url) => {
            let request = http_get_with_cookie(http, cookie, &url);
            let response = request
                .timeout(UPLOAD_TIMEOUT)
                .send()
                .await
                .map_err(|_| UploadError::Upstream("Attachment download failed".to_string()))?;

            if !response.status().is_success() {
                return Err(UploadError::Upstream(format!(
                    "Attachment download returned HTTP {}",
                    response.status().as_u16()
                )));
            }

            if let Some(length) = response.content_length() {
                if length > max_bytes {
                    return Err(too_large(length, max_bytes));
                }
                let body = reqwest::Body::wrap_stream(response.bytes_stream());
                Part::stream_with_length(body, length)
                    .file_name(filename.to_string())
                    .mime_str(mime_type)
                    .map_err(|_| {
                        UploadError::Invalid("Attachment MIME type is invalid".to_string())
                    })
            } else {
                let mut collected = Vec::new();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| {
                        UploadError::Upstream("Attachment download failed".to_string())
                    })?;
                    if collected.len() as u64 + chunk.len() as u64 > max_bytes {
                        return Err(too_large(
                            collected.len() as u64 + chunk.len() as u64,
                            max_bytes,
                        ));
                    }
                    collected.extend_from_slice(&chunk);
                }
                Part::bytes(collected)
                    .file_name(filename.to_string())
                    .mime_str(mime_type)
                    .map_err(|_| {
                        UploadError::Invalid("Attachment MIME type is invalid".to_string())
                    })
            }
        }
        SourceData::AlreadyUploaded(_) => Err(UploadError::Invalid(
            "Attachment source is already uploaded".to_string(),
        )),
    }
}

fn http_get_with_cookie(
    http: &reqwest::Client,
    cookie: &str,
    url: &str,
) -> reqwest::RequestBuilder {
    let mut builder = http.get(url).header(USER_AGENT, UPLOAD_USER_AGENT);
    if is_google_host(url)
        && let Ok(mut cookie_value) = HeaderValue::from_str(cookie)
    {
        cookie_value.set_sensitive(true);
        builder = builder.header(COOKIE, cookie_value);
    }
    builder
}

fn is_google_host(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == "google.com"
        || host.ends_with(".google.com")
        || host == "googleapis.com"
        || host.ends_with(".googleapis.com")
        || host == "googleusercontent.com"
        || host.ends_with(".googleusercontent.com")
}

fn normalize_local_path(path: &str) -> PathBuf {
    let trimmed = path.trim();
    let without_scheme = trimmed
        .strip_prefix("file://")
        .or_else(|| trimmed.strip_prefix("file:"))
        .unwrap_or(trimmed);
    let decoded = percent_decode(without_scheme);

    #[cfg(windows)]
    {
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
            return PathBuf::from(&decoded[1..]);
        }
        if decoded.starts_with("//") {
            return PathBuf::from(decoded.trim_start_matches('/'));
        }
    }

    PathBuf::from(decoded)
}

fn resolve_filename(media: &MediaAttachment, mime_type: &str) -> String {
    let provided = media.filename.clone().or_else(|| match &media.source {
        MediaSource::Url(url) | MediaSource::FilePath(url) => filename_from_path(url),
        MediaSource::DataUrl(_) | MediaSource::UploadedPath(_) => None,
    });

    let name = provided.unwrap_or_else(|| {
        let extension = extension_from_mime(mime_type).unwrap_or("bin");
        format!("attachment.{extension}")
    });

    sanitize_filename(&name)
}

fn filename_from_path(path: &str) -> Option<String> {
    let without_scheme = path
        .strip_prefix("file://")
        .or_else(|| path.strip_prefix("file:"))
        .unwrap_or(path);
    let without_query = without_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let name = without_query
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_query)
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(percent_decode(name))
    }
}

fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let mut sanitized: String = base
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || "._- ()[]".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        sanitized = "attachment".to_string();
    }

    if sanitized.chars().count() > 120 {
        let extension = sanitized
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_string())
            .filter(|extension| extension.len() <= 12);
        sanitized = match extension {
            Some(extension) => {
                let stem: String = sanitized.chars().take(100).collect();
                format!("{stem}.{extension}")
            }
            None => sanitized.chars().take(120).collect(),
        };
    }

    sanitized
}

fn decode_data_url(url: &str, max_bytes: u64) -> Result<(String, Vec<u8>), UploadError> {
    let trimmed = url.trim();
    if trimmed.len() < 5 || !trimmed[..5].eq_ignore_ascii_case("data:") {
        return Err(UploadError::Invalid(
            "Attachment data URL is invalid".to_string(),
        ));
    }
    let comma = trimmed
        .find(',')
        .ok_or_else(|| UploadError::Invalid("Attachment data URL has no payload".to_string()))?;
    let metadata = &trimmed[5..comma];
    let payload = &trimmed[comma + 1..];

    let is_base64 = metadata
        .split(';')
        .any(|part| part.trim().eq_ignore_ascii_case("base64"));
    if !is_base64 {
        return Err(UploadError::Invalid(
            "Attachment data URL must be base64 encoded".to_string(),
        ));
    }

    let mime = metadata
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();

    let decoded_payload = percent_decode(payload);
    let estimated = (decoded_payload.len() as u64).saturating_mul(3) / 4;
    if estimated > max_bytes.saturating_add(3) {
        return Err(too_large(estimated, max_bytes));
    }

    let bytes = decode_base64(&decoded_payload).map_err(|message| {
        UploadError::Invalid(format!("Attachment base64 is invalid: {message}"))
    })?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large(bytes.len() as u64, max_bytes));
    }

    Ok((mime, bytes))
}

fn too_large(actual: u64, limit: u64) -> UploadError {
    UploadError::TooLarge(format!(
        "Attachment is {actual} bytes, above the {limit} byte limit"
    ))
}

fn default_mime(kind: MediaKind) -> Option<&'static str> {
    match kind {
        MediaKind::Video => Some("video/mp4"),
        MediaKind::Image => Some("image/jpeg"),
        MediaKind::File => None,
    }
}

fn extension_from_mime(mime: &str) -> Option<&'static str> {
    let normalized = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "video/mp4" => Some("mp4"),
        "video/quicktime" => Some("mov"),
        "video/webm" => Some("webm"),
        "video/x-matroska" => Some("mkv"),
        "video/x-msvideo" => Some("avi"),
        "video/mpeg" => Some("mpeg"),
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "application/pdf" => Some("pdf"),
        "text/plain" => Some("txt"),
        "application/json" => Some("json"),
        _ => None,
    }
}

fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut accumulator: u32 = 0;
    let mut bits: u8 = 0;
    let mut characters = 0usize;
    let mut padding = 0usize;

    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return Err("padding appears before the end".to_string());
        }
        let value = base64_value(byte).ok_or_else(|| "invalid character".to_string())?;
        accumulator = (accumulator << 6) | value as u32;
        bits += 6;
        characters += 1;
        if bits >= 8 {
            bits -= 8;
            output.push(((accumulator >> bits) & 0xFF) as u8);
        }
    }

    let total = characters + padding;
    if !total.is_multiple_of(4) {
        if padding == 0 && matches!(characters % 4, 2 | 3) {
            return Ok(output);
        }
        return Err("invalid length".to_string());
    }
    if matches!(characters % 4, 1) {
        return Err("invalid length".to_string());
    }

    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_padded_and_unpadded_base64() {
        assert_eq!(decode_base64("TQ==").unwrap(), b"M");
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma");
        assert_eq!(decode_base64("TWFu").unwrap(), b"Man");
        assert_eq!(decode_base64("TQ").unwrap(), b"M");
        assert_eq!(decode_base64("TWE").unwrap(), b"Ma");
        assert_eq!(decode_base64("T W\nF u").unwrap(), b"Man");
    }

    #[test]
    fn decodes_url_safe_alphabet() {
        assert_eq!(decode_base64("_w==").unwrap(), vec![0xFF]);
        assert_eq!(decode_base64("-w==").unwrap(), vec![0xFB]);
    }

    #[test]
    fn rejects_invalid_base64() {
        assert!(decode_base64("T").is_err());
        assert!(decode_base64("TQ=A").is_err());
        assert!(decode_base64("!!!!").is_err());
    }

    #[test]
    fn parses_data_url_metadata() {
        let (mime, bytes) = decode_data_url("data:video/mp4;base64,AAAA", 1024).unwrap();
        assert_eq!(mime, "video/mp4");
        assert_eq!(bytes, vec![0, 0, 0]);
    }

    #[test]
    fn rejects_oversized_data_url() {
        let error = decode_data_url("data:video/mp4;base64,AAAA", 2).unwrap_err();
        assert!(matches!(error, UploadError::TooLarge(_)));
    }

    #[test]
    fn normalizes_file_scheme_paths() {
        let path = normalize_local_path("file:///D:/clips/demo.mp4");
        #[cfg(windows)]
        assert_eq!(path, PathBuf::from("D:/clips/demo.mp4"));
        #[cfg(not(windows))]
        assert_eq!(path, PathBuf::from("/D:/clips/demo.mp4"));
    }
}
