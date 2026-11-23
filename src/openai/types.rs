use serde::{Deserialize, Serialize};
use serde_json::Value;

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
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(arr)) => {
                let mut parts = Vec::new();
                for item in arr {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        parts.push(text.to_string());
                    } else if let Some(s) = item.as_str() {
                        parts.push(s.to_string());
                    }
                }
                parts.join("\n")
            }
            Some(serde_json::Value::Object(object)) => object
                .get("text")
                .and_then(|text| text.as_str())
                .unwrap_or_default()
                .to_string(),
            Some(_) => String::new(),
            None => String::new(),
        }
    }
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
        for message in &self.messages {
            match message.content.as_ref() {
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(attachment) = media_from_part(part) {
                            media.push(attachment);
                        }
                    }
                }
                Some(part @ Value::Object(_)) => {
                    if let Some(attachment) = media_from_part(part) {
                        media.push(attachment);
                    }
                }
                _ => {}
            }
        }
        media
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
