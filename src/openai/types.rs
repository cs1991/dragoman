use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn content_as_str(&self) -> String {
        match &self.content {
            Some(value) => rendered_text(value),
            None => String::new(),
        }
    }
}

/// Render message content as prompt text. Base64 data URLs are replaced with a
/// short placeholder because the bytes travel through the attachment upload
/// channel, not through the prompt.
fn rendered_text(content: &Value) -> String {
    match content {
        Value::String(text) => match structured_json(text) {
            Some(parsed) => rendered_text(&parsed),
            None => strip_inline_media(text),
        },
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(strip_inline_media(text));
                } else if let Some(text) = item.as_str() {
                    parts.push(strip_inline_media(text));
                }
            }
            parts.join("\n")
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(strip_inline_media)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Some clients serialize the content part array into a JSON string before
/// sending it. Parse that representation when it clearly carries content parts.
fn structured_json(text: &str) -> Option<Value> {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('[') || trimmed.starts_with('{')) || text.len() > 4 * 1024 * 1024 {
        return None;
    }
    let parsed: Value = serde_json::from_str(text).ok()?;
    let looks_structured = matches!(&parsed, Value::Array(_) | Value::Object(_));
    looks_structured.then_some(parsed)
}

const MIN_INLINE_MEDIA_CHARS: usize = 64;

/// Extract `data:<mime>;base64,<payload>` blobs embedded in plain prompt text.
/// Sizeable payloads are treated as attachments so the prompt stays small and
/// Gemini receives real media instead of base64 noise.
pub fn inline_media_from_text(text: &str) -> Vec<MediaAttachment> {
    let mut media = Vec::new();
    if text.len() < MIN_INLINE_MEDIA_CHARS + 16 || !contains_ignore_ascii_case(text, "base64") {
        return media;
    }

    let mut cursor = 0usize;
    while let Some((start, end, mime)) = find_data_url(text, cursor) {
        let url = text[start..end].to_string();
        let kind = mime
            .as_deref()
            .and_then(kind_from_mime)
            .unwrap_or(MediaKind::File);
        media.push(MediaAttachment {
            kind,
            source: MediaSource::DataUrl(url),
            filename: None,
            mime_type: mime,
        });
        cursor = end;
    }
    media
}

fn strip_inline_media(text: &str) -> String {
    if text.len() < MIN_INLINE_MEDIA_CHARS + 16 || !contains_ignore_ascii_case(text, "base64") {
        return text.to_string();
    }

    let mut output = String::with_capacity(text.len().min(64 * 1024));
    let mut cursor = 0usize;
    while let Some((start, end, mime)) = find_data_url(text, cursor) {
        output.push_str(&text[cursor..start]);
        let kind = mime
            .as_deref()
            .and_then(kind_from_mime)
            .unwrap_or(MediaKind::File);
        output.push_str(match kind {
            MediaKind::Image => "[attached image]",
            MediaKind::Video => "[attached video]",
            MediaKind::File => "[attached file]",
        });
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    output
}

fn contains_ignore_ascii_case(text: &str, needle: &str) -> bool {
    text.as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Locate the next inline data URL. Returns the byte range covering the whole
/// URL plus the declared MIME type.
fn find_data_url(text: &str, from: usize) -> Option<(usize, usize, Option<String>)> {
    let bytes = text.as_bytes();
    let mut index = from;
    while index + 5 <= bytes.len() {
        if bytes[index..index + 5].eq_ignore_ascii_case(b"data:")
            && let Some(found) = parse_data_url_at(text, index)
        {
            return Some(found);
        }
        index += 1;
    }
    None
}

fn parse_data_url_at(text: &str, start: usize) -> Option<(usize, usize, Option<String>)> {
    let bytes = text.as_bytes();
    let metadata_limit = (start + 240).min(bytes.len());
    let comma = bytes[start + 5..metadata_limit]
        .iter()
        .position(|byte| *byte == b',')
        .map(|offset| start + 5 + offset)?;

    let metadata = &text[start + 5..comma];
    let mut segments = metadata.split(';');
    let mime_segment = segments.next().unwrap_or_default().trim();
    let is_base64 = segments.any(|segment| segment.trim().eq_ignore_ascii_case("base64"));
    if !is_base64 {
        return None;
    }

    let payload_start = comma + 1;
    let mut end = payload_start;
    while end < bytes.len() {
        let byte = bytes[end];
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_' | b'%')
            || byte.is_ascii_whitespace()
        {
            end += 1;
        } else {
            break;
        }
    }

    let payload_chars = bytes[payload_start..end]
        .iter()
        .filter(|byte| !byte.is_ascii_whitespace())
        .count();
    if payload_chars < MIN_INLINE_MEDIA_CHARS {
        return None;
    }

    let mime = if mime_segment.is_empty() {
        None
    } else {
        Some(mime_segment.to_ascii_lowercase())
    };
    Some((start, end, mime))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    DataUrl(String),
    Url(String),
    FilePath(String),
    UploadedPath(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAttachment {
    pub kind: MediaKind,
    pub source: MediaSource,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
}

impl ChatCompletionRequest {
    /// Collect every video, image and file part from the message history.
    pub fn collect_media(&self) -> Vec<MediaAttachment> {
        let mut media = Vec::new();
        let mut seen = HashSet::new();
        for message in &self.messages {
            collect_media_from_content(message.content.as_ref(), &mut media, &mut seen);
        }
        media
    }

    /// Compact structural summary used for request diagnostics. Base64 payloads
    /// and cookie values never appear in this string.
    pub fn describe_shape(&self) -> String {
        let mut messages = Vec::with_capacity(self.messages.len());
        for (index, message) in self.messages.iter().enumerate() {
            messages.push(format!(
                "#{index} {} {}",
                message.role,
                describe_content(message.content.as_ref())
            ));
        }
        messages.join(" | ")
    }

    /// Return media-like content parts that do not expose a usable source.
    pub fn unsupported_media_parts(&self) -> Vec<String> {
        let mut unsupported = Vec::new();
        for message in &self.messages {
            for part in content_parts(message.content.as_ref()) {
                let Some(object) = part.as_object() else {
                    continue;
                };
                let part_type = object
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if is_media_part_type(&part_type) && media_from_part(part).is_none() {
                    let keys = object
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    unsupported.push(format!("{part_type} ({keys})"));
                }
            }
        }
        unsupported
    }
}

fn collect_media_from_content(
    content: Option<&Value>,
    media: &mut Vec<MediaAttachment>,
    seen: &mut HashSet<String>,
) {
    match content {
        Some(Value::String(text)) => {
            if let Some(parsed) = structured_json(text) {
                collect_media_from_content(Some(&parsed), media, seen);
            } else {
                push_inline_media(text, media, seen);
            }
        }
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(attachment) = media_from_part(part) {
                    push_media(attachment, media, seen);
                    continue;
                }
                if let Some(text) = part_text(part) {
                    push_inline_media(text, media, seen);
                }
            }
        }
        Some(value @ Value::Object(_)) => {
            if let Some(attachment) = media_from_part(value) {
                push_media(attachment, media, seen);
            } else if let Some(text) = part_text(value) {
                push_inline_media(text, media, seen);
            }
        }
        _ => {}
    }
}

fn push_inline_media(text: &str, media: &mut Vec<MediaAttachment>, seen: &mut HashSet<String>) {
    for attachment in inline_media_from_text(text) {
        push_media(attachment, media, seen);
    }
}

fn push_media(
    attachment: MediaAttachment,
    media: &mut Vec<MediaAttachment>,
    seen: &mut HashSet<String>,
) {
    let key = match &attachment.source {
        MediaSource::DataUrl(url) => format!("data:{}", short_hash(url.as_bytes())),
        MediaSource::Url(url) => format!("url:{url}"),
        MediaSource::FilePath(path) => format!("path:{path}"),
        MediaSource::UploadedPath(path) => format!("uploaded:{path}"),
    };
    if seen.insert(key) {
        media.push(attachment);
    }
}

fn short_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn part_text(part: &Value) -> Option<&str> {
    match part {
        Value::String(text) => Some(text),
        Value::Object(object) => object.get("text").and_then(Value::as_str),
        _ => None,
    }
}

fn describe_content(content: Option<&Value>) -> String {
    match content {
        None | Some(Value::Null) => "null".to_string(),
        Some(Value::String(text)) => format!(
            "string({} chars, {} inline media)",
            text.chars().count(),
            inline_media_from_text(text).len()
        ),
        Some(Value::Array(parts)) => {
            let shapes: Vec<String> = parts.iter().take(12).map(describe_part).collect();
            format!("array({}; {})", parts.len(), shapes.join(" "))
        }
        Some(value @ Value::Object(_)) => describe_part(value),
        Some(_) => "scalar".to_string(),
    }
}

fn describe_part(part: &Value) -> String {
    match part {
        Value::Object(object) => {
            let part_type = object.get("type").and_then(Value::as_str).unwrap_or("-");
            let keys: Vec<&str> = object.keys().take(6).map(String::as_str).collect();
            let text_len = object
                .get("text")
                .and_then(Value::as_str)
                .map(|text| text.chars().count())
                .unwrap_or(0);
            format!("{part_type}[{}]text={text_len}", keys.join(","))
        }
        Value::String(text) => format!("string({})", text.chars().count()),
        _ => "scalar".to_string(),
    }
}

fn content_parts(content: Option<&Value>) -> Vec<&Value> {
    match content {
        Some(Value::Array(parts)) => parts.iter().collect(),
        Some(value @ Value::Object(_)) => vec![value],
        _ => Vec::new(),
    }
}

fn is_media_part_type(part_type: &str) -> bool {
    matches!(
        part_type,
        "video_url"
            | "video"
            | "input_video"
            | "video_input"
            | "image_url"
            | "image"
            | "input_image"
            | "image_input"
            | "file"
            | "input_file"
            | "file_url"
            | "audio"
            | "input_audio"
    )
}

fn media_from_part(part: &Value) -> Option<MediaAttachment> {
    let object = part.as_object()?;
    let part_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let (default_kind, explicit_kind, container) = match part_type.as_str() {
        "video_url" | "video" | "input_video" | "video_input" => (
            MediaKind::Video,
            Some(MediaKind::Video),
            object
                .get("video_url")
                .or_else(|| object.get("video"))
                .or_else(|| object.get("input_video")),
        ),
        "image_url" | "image" | "input_image" | "image_input" => (
            MediaKind::Image,
            Some(MediaKind::Image),
            object
                .get("image_url")
                .or_else(|| object.get("image"))
                .or_else(|| object.get("input_image")),
        ),
        "file" | "input_file" | "file_url" => (
            MediaKind::File,
            None,
            object.get("file").or_else(|| object.get("input_file")),
        ),
        "" => {
            // Clients that omit `type` still identify media by field name.
            if let Some(container) = object
                .get("image_url")
                .or_else(|| object.get("input_image"))
                .or_else(|| object.get("image"))
            {
                (MediaKind::Image, Some(MediaKind::Image), Some(container))
            } else if let Some(container) = object
                .get("video_url")
                .or_else(|| object.get("input_video"))
                .or_else(|| object.get("video"))
            {
                (MediaKind::Video, Some(MediaKind::Video), Some(container))
            } else if let Some(container) = object.get("file").or_else(|| object.get("input_file"))
            {
                (MediaKind::File, None, Some(container))
            } else {
                let source = object.get("source")?;
                let mime = source
                    .as_object()
                    .and_then(|fields| {
                        first_string(
                            fields,
                            &["media_type", "mime_type", "mimeType", "content_type"],
                        )
                    })
                    .or_else(|| {
                        object
                            .get("media_type")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                let kind = mime
                    .as_deref()
                    .and_then(kind_from_mime)
                    .unwrap_or(MediaKind::File);
                (kind, Some(kind), Some(source))
            }
        }
        _ => return None,
    };

    let mut filename = None;
    let mut mime_type = None;
    let mut format = None;
    let mut source = None;
    let mut raw_base64 = None;

    match container {
        Some(Value::String(value)) => {
            source = Some(classify_source(value));
        }
        Some(Value::Object(fields)) => {
            merge_media_fields(
                fields,
                &mut filename,
                &mut mime_type,
                &mut format,
                &mut source,
                &mut raw_base64,
                0,
            );
        }
        Some(Value::Array(items)) => {
            if let Some(first) = items.first() {
                match first {
                    Value::String(value) => source = Some(classify_source(value)),
                    Value::Object(fields) => merge_media_fields(
                        fields,
                        &mut filename,
                        &mut mime_type,
                        &mut format,
                        &mut source,
                        &mut raw_base64,
                        0,
                    ),
                    _ => {}
                }
            }
        }
        _ => {}
    }

    merge_media_fields(
        object,
        &mut filename,
        &mut mime_type,
        &mut format,
        &mut source,
        &mut raw_base64,
        0,
    );

    if let Some(data) = raw_base64 {
        let resolved_mime = mime_type
            .clone()
            .or_else(|| format.as_deref().and_then(mime_from_format))
            .unwrap_or_else(|| "application/octet-stream".to_string());
        source = Some(MediaSource::DataUrl(format!(
            "data:{};base64,{}",
            resolved_mime,
            data.trim()
        )));
        if mime_type.is_none() {
            mime_type = Some(resolved_mime);
        }
    }

    let source = source?;
    let inferred_mime = mime_type
        .clone()
        .or_else(|| filename.as_deref().and_then(mime_from_filename));
    let kind = explicit_kind
        .or_else(|| inferred_mime.as_deref().and_then(kind_from_mime))
        .or_else(|| filename.as_deref().and_then(kind_from_filename))
        .unwrap_or(default_kind);

    Some(MediaAttachment {
        kind,
        source,
        filename,
        mime_type: inferred_mime.or(mime_type),
    })
}

fn merge_media_fields(
    fields: &serde_json::Map<String, Value>,
    filename: &mut Option<String>,
    mime_type: &mut Option<String>,
    format: &mut Option<String>,
    source: &mut Option<MediaSource>,
    raw_base64: &mut Option<String>,
    depth: usize,
) {
    if depth > 4 {
        return;
    }

    if filename.is_none() {
        *filename = first_string(fields, &["filename", "file_name", "name"]);
    }
    if mime_type.is_none() {
        *mime_type = first_string(
            fields,
            &["mime_type", "mimeType", "media_type", "content_type"],
        );
    }
    if format.is_none() {
        *format = first_string(fields, &["format"]);
    }

    if source.is_none()
        && let Some(value) = fields.get("source")
    {
        match value {
            Value::String(value) => {
                *source = Some(classify_source(value));
            }
            Value::Object(nested) => {
                merge_media_fields(
                    nested,
                    filename,
                    mime_type,
                    format,
                    source,
                    raw_base64,
                    depth + 1,
                );
            }
            _ => {}
        }
    }

    if source.is_none()
        && raw_base64.is_none()
        && let Some(url) = first_string(
            fields,
            &[
                "url",
                "file_url",
                "uri",
                "file_uri",
                "video_url",
                "image_url",
                "data_url",
                "dataUrl",
            ],
        )
    {
        *source = Some(classify_source(&url));
    }

    if source.is_none()
        && raw_base64.is_none()
        && let Some(data) = first_string(fields, &["data", "file_data", "base64", "bytes"])
    {
        if data.trim_start().to_ascii_lowercase().starts_with("data:") {
            *source = Some(MediaSource::DataUrl(data));
        } else {
            *raw_base64 = Some(data);
        }
    }

    if source.is_none()
        && raw_base64.is_none()
        && let Some(file_id) = first_string(fields, &["file_id", "fileId"])
    {
        *source = Some(classify_source(&file_id));
    }
}

fn first_string(fields: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = fields.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn classify_source(value: &str) -> MediaSource {
    let trimmed = value.trim();
    if trimmed.to_ascii_lowercase().starts_with("data:") {
        MediaSource::DataUrl(trimmed.to_string())
    } else if trimmed
        .strip_prefix('/')
        .unwrap_or(trimmed)
        .starts_with("contrib_service/")
    {
        let path = trimmed.strip_prefix('/').unwrap_or(trimmed);
        MediaSource::UploadedPath(format!("/{path}"))
    } else if trimmed.to_ascii_lowercase().starts_with("http://")
        || trimmed.to_ascii_lowercase().starts_with("https://")
    {
        MediaSource::Url(trimmed.to_string())
    } else {
        MediaSource::FilePath(trimmed.to_string())
    }
}

pub fn mime_from_format(format: &str) -> Option<String> {
    let normalized = format.trim().trim_start_matches('.').to_ascii_lowercase();
    let mime = match normalized.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "mpeg" | "mpg" => "video/mpeg",
        "3gp" => "video/3gpp",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        _ => return None,
    };
    Some(mime.to_string())
}

pub fn mime_from_filename(filename: &str) -> Option<String> {
    let extension = filename.rsplit_once('.')?.1;
    mime_from_format(extension)
}

fn kind_from_mime(mime: &str) -> Option<MediaKind> {
    if mime.starts_with("video/") {
        Some(MediaKind::Video)
    } else if mime.starts_with("image/") {
        Some(MediaKind::Image)
    } else {
        None
    }
}

fn kind_from_filename(filename: &str) -> Option<MediaKind> {
    mime_from_filename(filename)
        .as_deref()
        .and_then(kind_from_mime)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: ChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelObject {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}
