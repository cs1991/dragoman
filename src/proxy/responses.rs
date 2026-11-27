//! OpenAI Responses API adapter (`POST /v1/responses`).
//!
//! The proxy speaks one upstream protocol: a prompt built from chat messages
//! sent to Gemini Web. Responses clients send a different envelope, so this
//! module translates in both directions.
//!
//! Requests: `instructions` becomes a system message, `input` items become
//! chat messages, and flat Responses tool definitions become the nested chat
//! shape the prompt translator already understands. `previous_response_id`
//! replays a bounded in-memory copy of that response's messages when the
//! caller does not resend the history itself.
//!
//! Responses: `output` carries `message` items with `output_text` parts and
//! one `function_call` item per tool call. Streaming uses the named events the
//! Responses clients subscribe to instead of chat.completion chunks.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{error, info};
use uuid::Uuid;

use crate::accounts::RequestKind;
use crate::openai::types::{
    ChatCompletionRequest, ChatMessage, FunctionCall, FunctionDefinition, MediaKind, ToolCall,
    ToolDefinition,
};
use crate::proxy::server::{AppState, classify_generation_error, openai_error};
use crate::proxy::translator::PromptTranslator;
use crate::web::models;

const MAX_HISTORY_ENTRIES: usize = 128;
const MAX_HISTORY_MESSAGES: usize = 200;
const STORED_MESSAGE_CHAR_LIMIT: usize = 32_000;

/// Incoming `/v1/responses` body. Unknown fields are ignored on purpose, so a
/// newer client with extra knobs still reaches the model instead of failing
/// deserialization.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub instructions: Option<Value>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    #[serde(default)]
    pub store: Option<bool>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub truncation: Option<String>,
    #[serde(default)]
    pub text: Option<Value>,
    #[serde(default)]
    pub reasoning: Option<Value>,
    #[serde(default)]
    pub user: Option<String>,
}

/// Values echoed on every response object, captured before the request body is
/// consumed by the streaming task.
#[derive(Debug, Clone)]
struct ResponseMeta {
    model: String,
    instructions: Value,
    tools: Value,
    tool_choice: Value,
    max_output_tokens: Option<u32>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    parallel_tool_calls: bool,
    previous_response_id: Value,
    store: bool,
    metadata: Value,
    text: Value,
    reasoning: Value,
    user: Value,
    truncation: Value,
}

impl ResponseMeta {
    fn from_request(request: &ResponsesRequest, model: &str) -> Self {
        Self {
            model: model.to_string(),
            instructions: request.instructions.clone().unwrap_or(Value::Null),
            tools: request
                .tools
                .as_ref()
                .map(|tools| Value::Array(tools.clone()))
                .unwrap_or_else(|| Value::Array(Vec::new())),
            tool_choice: request
                .tool_choice
                .clone()
                .unwrap_or_else(|| Value::String("auto".to_string())),
            max_output_tokens: request.max_output_tokens,
            temperature: request.temperature,
            top_p: request.top_p,
            parallel_tool_calls: request.parallel_tool_calls.unwrap_or(true),
            previous_response_id: request
                .previous_response_id
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
            store: request.store.unwrap_or(true),
            metadata: request.metadata.clone().unwrap_or_else(|| json!({})),
            text: request
                .text
                .clone()
                .unwrap_or_else(|| json!({"format": {"type": "text"}})),
            reasoning: request
                .reasoning
                .clone()
                .unwrap_or_else(|| json!({"effort": null, "summary": null})),
            user: request
                .user
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
            truncation: request
                .truncation
                .clone()
                .map(Value::String)
                .unwrap_or_else(|| Value::String("disabled".to_string())),
        }
    }
}

pub async fn responses(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<ResponsesRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return openai_error(
                rejection.status(),
                "invalid_request_error",
                rejection.body_text(),
            );
        }
    };

    let requested_model = request
        .model
        .clone()
        .unwrap_or_else(|| state.default_model.clone());
    info!("Received responses request for model: {}", requested_model);

    let (mut chat, custom_tools) = match request.to_chat_request(&requested_model) {
        Ok(value) => value,
        Err(message) => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                message,
            );
        }
    };

    if let Some(previous) = request.previous_response_id.as_deref() {
        let stored = load_history(previous);
        if !stored.is_empty() {
            let before = chat.messages.len();
            chat.messages = merge_history(&stored, chat.messages);
            info!(
                "Responses request continues {}: {} stored + {} new = {} message(s)",
                previous,
                stored.len(),
                before,
                chat.messages.len()
            );
        }
    }

    let unsupported = chat.unsupported_media_parts();
    if !unsupported.is_empty() {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "Unsupported attachment content part(s): {}. Use url, data, file_data, base64, file_id, or source.",
                unsupported.join("; ")
            ),
        );
    }

    let media = chat.collect_media();
    let kind = if media.iter().any(|item| item.kind == MediaKind::Video) {
        RequestKind::Video
    } else {
        RequestKind::Chat
    };

    let mut prompt = PromptTranslator::build_gemini_prompt(&chat);
    let has_user_text = chat
        .messages
        .iter()
        .any(|message| message.role == "user" && !message.content_as_str().trim().is_empty());
    if !has_user_text && !media.is_empty() {
        if prompt.trim().is_empty() {
            prompt = "Please describe the attached file.".to_string();
        } else {
            prompt.push_str("\n\nPlease describe the attached file.");
        }
    }

    if prompt.trim().is_empty() && media.is_empty() {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "The request contains no text or supported attachment.",
        );
    }

    info!(
        "Responses prompt characters: {}, attachments: {}",
        prompt.chars().count(),
        media.len()
    );

    let model_spec = models::resolve(&chat.model);
    if !model_spec.available {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "model_not_available",
            "Carbon requires a Google AI Ultra subscription.",
        );
    }

    let meta = ResponseMeta::from_request(&request, &chat.model);
    let is_stream = request.stream.unwrap_or(false);
    let history_messages = if meta.store {
        Some(chat.messages.clone())
    } else {
        None
    };
    let prompt_chars = prompt.chars().count();
    let (delta_tx, mut delta_rx) = mpsc::channel::<Result<String, String>>(100);

    if is_stream {
        let client = state.web_client.clone();
        let prompt_clone = prompt.clone();
        let media_clone = media.clone();
        let max_attachment_bytes = state.max_attachment_bytes;
        tokio::spawn(async move {
            if let Err(e) = client
                .send_prompt_stream_with_media_and_model(
                    &prompt_clone,
                    &media_clone,
                    model_spec,
                    max_attachment_bytes,
                    kind,
                    delta_tx,
                )
                .await
            {
                error!("Responses stream execution error: {}", e);
            }
        });

        let sse_stream = async_stream::stream! {
            let response_id = format!("resp_{}", Uuid::new_v4().simple());
            let message_item_id = format!("msg_{}", Uuid::new_v4().simple());
            let created_at = now_seconds();
            let mut sequence = 0u64;

            let created = response_json(
                &response_id,
                created_at,
                "in_progress",
                Vec::new(),
                None,
                &meta,
            );
            yield Ok::<Event, Infallible>(sse_event(
                &mut sequence,
                "response.created",
                json!({ "response": created }),
            ));
            let progress = response_json(
                &response_id,
                created_at,
                "in_progress",
                Vec::new(),
                None,
                &meta,
            );
            yield Ok::<Event, Infallible>(sse_event(
                &mut sequence,
                "response.in_progress",
                json!({ "response": progress }),
            ));

            let mut filter = TagFilter::new();
            let mut accumulated = String::new();
            let mut visible = String::new();
            let mut message_started = false;
            let mut failure: Option<String> = None;

            while let Some(item) = delta_rx.recv().await {
                match item {
                    Ok(delta) => {
                        accumulated.push_str(&delta);
                        let clean = filter.push(&delta);
                        if clean.is_empty() {
                            continue;
                        }
                        if !message_started {
                            message_started = true;
                            yield Ok::<Event, Infallible>(sse_event(
                                &mut sequence,
                                "response.output_item.added",
                                json!({
                                    "output_index": 0,
                                    "item": {
                                        "type": "message",
                                        "id": message_item_id.clone(),
                                        "status": "in_progress",
                                        "role": "assistant",
                                        "content": []
                                    }
                                }),
                            ));
                            yield Ok::<Event, Infallible>(sse_event(
                                &mut sequence,
                                "response.content_part.added",
                                json!({
                                    "item_id": message_item_id.clone(),
                                    "output_index": 0,
                                    "content_index": 0,
                                    "part": {"type": "output_text", "text": "", "annotations": []}
                                }),
                            ));
                        }
                        visible.push_str(&clean);
                        yield Ok::<Event, Infallible>(sse_event(
                            &mut sequence,
                            "response.output_text.delta",
                            json!({
                                "item_id": message_item_id.clone(),
                                "output_index": 0,
                                "content_index": 0,
                                "delta": clean
                            }),
                        ));
                    }
                    Err(err) => {
                        failure = Some(err);
                        break;
                    }
                }
            }

            if let Some(err) = failure {
                let (message, _, _) = classify_generation_error(err);
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.failed",
                    json!({
                        "response": {
                            "id": response_id.clone(),
                            "object": "response",
                            "created_at": created_at,
                            "status": "failed",
                            "model": meta.model.clone(),
                            "error": {"code": "upstream_error", "message": message}
                        }
                    }),
                ));
                return;
            }

            let tail = filter.finish();
            if !tail.is_empty() {
                if !message_started {
                    message_started = true;
                    yield Ok::<Event, Infallible>(sse_event(
                        &mut sequence,
                        "response.output_item.added",
                        json!({
                            "output_index": 0,
                            "item": {
                                "type": "message",
                                "id": message_item_id.clone(),
                                "status": "in_progress",
                                "role": "assistant",
                                "content": []
                            }
                        }),
                    ));
                    yield Ok::<Event, Infallible>(sse_event(
                        &mut sequence,
                        "response.content_part.added",
                        json!({
                            "item_id": message_item_id.clone(),
                            "output_index": 0,
                            "content_index": 0,
                            "part": {"type": "output_text", "text": "", "annotations": []}
                        }),
                    ));
                }
                visible.push_str(&tail);
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.output_text.delta",
                    json!({
                        "item_id": message_item_id.clone(),
                        "output_index": 0,
                        "content_index": 0,
                        "delta": tail
                    }),
                ));
            }

            let (_, parsed_tools) = PromptTranslator::parse_response(&accumulated);
            let tool_calls = parsed_tools.unwrap_or_default();
            let mut output_items: Vec<Value> = Vec::new();
            let mut output_index = 0usize;

            if message_started {
                let item = message_item(&message_item_id, &visible, "completed");
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.output_text.done",
                    json!({
                        "item_id": message_item_id.clone(),
                        "output_index": 0,
                        "content_index": 0,
                        "text": visible.clone()
                    }),
                ));
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.content_part.done",
                    json!({
                        "item_id": message_item_id.clone(),
                        "output_index": 0,
                        "content_index": 0,
                        "part": {"type": "output_text", "text": visible.clone(), "annotations": []}
                    }),
                ));
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.output_item.done",
                    json!({ "output_index": 0, "item": item }),
                ));
                output_items.push(item);
                output_index = 1;
            }

            for call in &tool_calls {
                let item_id = format!("fc_{}", Uuid::new_v4().simple());
                let custom = custom_tools.iter().any(|name| name == &call.function.name);
                let item = tool_call_item(&item_id, call, custom);
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.output_item.added",
                    json!({
                        "output_index": output_index,
                        "item": {
                            "type": item["type"].clone(),
                            "id": item_id.clone(),
                            "status": "in_progress",
                            "call_id": call.id.clone(),
                            "name": call.function.name.clone(),
                            "arguments": ""
                        }
                    }),
                ));
                if !custom {
                    yield Ok::<Event, Infallible>(sse_event(
                        &mut sequence,
                        "response.function_call_arguments.delta",
                        json!({
                            "item_id": item_id.clone(),
                            "output_index": output_index,
                            "delta": call.function.arguments.clone()
                        }),
                    ));
                }
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.function_call_arguments.done",
                    json!({
                        "item_id": item_id.clone(),
                        "output_index": output_index,
                        "arguments": call.function.arguments.clone()
                    }),
                ));
                yield Ok::<Event, Infallible>(sse_event(
                    &mut sequence,
                    "response.output_item.done",
                    json!({ "output_index": output_index, "item": item }),
                ));
                output_items.push(item);
                output_index += 1;
            }

            let usage = usage_json(prompt_chars, accumulated.chars().count());
            let completed = response_json(
                &response_id,
                created_at,
                "completed",
                output_items,
                Some(usage),
                &meta,
            );

            if let Some(mut messages) = history_messages.clone() {
                messages.push(assistant_message(
                    if visible.trim().is_empty() {
                        None
                    } else {
                        Some(visible.clone())
                    },
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls.clone())
                    },
                ));
                store_history(response_id.clone(), slim_messages(messages));
            }

            yield Ok::<Event, Infallible>(sse_event(
                &mut sequence,
                "response.completed",
                json!({ "response": completed }),
            ));
        };

        Sse::new(sse_stream)
            .keep_alive(KeepAlive::default())
            .into_response()
    } else {
        let drain = tokio::spawn(async move { while delta_rx.recv().await.is_some() {} });
        match state
            .web_client
            .send_prompt_stream_with_media_and_model(
                &prompt,
                &media,
                model_spec,
                state.max_attachment_bytes,
                kind,
                delta_tx,
            )
            .await
        {
            Ok(full_output) => {
                drain.abort();
                let (text, tool_calls) = PromptTranslator::parse_response(&full_output);
                let tool_calls = tool_calls.unwrap_or_default();
                let output_items =
                    build_output_items(&text, &tool_calls, &custom_tools);
                let response_id = format!("resp_{}", Uuid::new_v4().simple());
                let created_at = now_seconds();
                let usage = usage_json(prompt_chars, full_output.chars().count());
                let body = response_json(
                    &response_id,
                    created_at,
                    "completed",
                    output_items,
                    Some(usage),
                    &meta,
                );

                if let Some(mut messages) = history_messages {
                    messages.push(assistant_message(
                        text,
                        if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                    ));
                    store_history(response_id, slim_messages(messages));
                }

                Json(body).into_response()
            }
            Err(e) => {
                drain.abort();
                error!("Responses generation failed: {}", e);
                let (message, code, error_type) = classify_generation_error(e);
                openai_error(
                    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    error_type,
                    message,
                )
            }
        }
    }
}

impl ResponsesRequest {
    fn to_chat_request(
        &self,
        model: &str,
    ) -> Result<(ChatCompletionRequest, Vec<String>), String> {
        let mut messages: Vec<ChatMessage> = Vec::new();

        if let Some(instructions) = &self.instructions {
            let text = string_or_text(instructions);
            if !text.trim().is_empty() {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(Value::String(text)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        let input = self
            .input
            .as_ref()
            .ok_or_else(|| "The request is missing the `input` field.".to_string())?;
        for item in input_items(input) {
            if let Some(message) = item_to_message(&item) {
                messages.push(message);
            }
        }

        if let Some(instruction) = format_instruction(self.text.as_ref()) {
            messages.insert(
                0,
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(Value::String(instruction)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            );
        }

        let (tools, custom_tools) =
            convert_tools(self.tools.as_deref().unwrap_or(&[]));

        Ok((
            ChatCompletionRequest {
                model: model.to_string(),
                messages,
                tools: if tools.is_empty() { None } else { Some(tools) },
                tool_choice: self.tool_choice.clone(),
                stream: self.stream,
                temperature: self.temperature,
                max_tokens: self.max_output_tokens,
            },
            custom_tools,
        ))
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn string_or_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Normalize `input` into a list of items. A bare string is one user message.
fn input_items(input: &Value) -> Vec<Value> {
    match input {
        Value::String(text) => vec![json!({"role": "user", "content": text})],
        Value::Array(items) => items.clone(),
        Value::Object(_) => vec![input.clone()],
        _ => Vec::new(),
    }
}

fn item_to_message(item: &Value) -> Option<ChatMessage> {
    let object = item.as_object()?;
    let item_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match item_type {
        "function_call" => {
            let call_id = object
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| object.get("id").and_then(Value::as_str))
                .unwrap_or("call_unknown");
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let arguments = match object.get("arguments") {
                Some(Value::String(text)) => text.clone(),
                Some(value) => value.to_string(),
                None => "{}".to_string(),
            };
            Some(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                name: None,
                tool_calls: Some(vec![ToolCall {
                    id: call_id.to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: name.to_string(),
                        arguments,
                    },
                }]),
                tool_call_id: None,
            })
        }
        "custom_tool_call" => {
            let call_id = object
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| object.get("id").and_then(Value::as_str))
                .unwrap_or("call_unknown");
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let input = object.get("input").and_then(Value::as_str).unwrap_or("");
            Some(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                name: None,
                tool_calls: Some(vec![ToolCall {
                    id: call_id.to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: name.to_string(),
                        arguments: json!({ "input": input }).to_string(),
                    },
                }]),
                tool_call_id: None,
            })
        }
        "function_call_output" | "custom_tool_call_output" => {
            let call_id = object
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or("call_unknown");
            let output = object
                .get("output")
                .map(render_tool_output)
                .unwrap_or_default();
            Some(ChatMessage {
                role: "tool".to_string(),
                content: Some(Value::String(output)),
                name: None,
                tool_calls: None,
                tool_call_id: Some(call_id.to_string()),
            })
        }
        "message" => Some(message_from_role(object)),
        "input_text" | "output_text" | "text" => {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(ChatMessage {
                role: "user".to_string(),
                content: Some(Value::String(text.to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            })
        }
        "input_image" | "input_file" | "input_video" | "input_audio" => {
            Some(ChatMessage {
                role: "user".to_string(),
                content: Some(Value::Array(vec![item.clone()])),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            })
        }
        _ => {
            if object.contains_key("role") {
                Some(message_from_role(object))
            } else {
                None
            }
        }
    }
}

fn message_from_role(object: &serde_json::Map<String, Value>) -> ChatMessage {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let content = object.get("content").map(convert_content);
    ChatMessage {
        role: role.to_string(),
        content,
        name: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

fn render_tool_output(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn convert_content(content: &Value) -> Value {
    match content {
        Value::Array(parts) => Value::Array(parts.iter().map(convert_part).collect()),
        other => other.clone(),
    }
}

/// Responses content parts carry `input_text` / `output_text` names. The
/// prompt translator reads `text`, and media parts are already accepted under
/// their Responses names.
fn convert_part(part: &Value) -> Value {
    let Some(object) = part.as_object() else {
        return part.clone();
    };
    let part_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match part_type {
        "input_text" | "output_text" | "text" | "summary_text" | "" => {
            match object.get("text").and_then(Value::as_str) {
                Some(text) => json!({"type": "text", "text": text}),
                None => part.clone(),
            }
        }
        "refusal" => {
            let text = object
                .get("refusal")
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!({"type": "text", "text": text})
        }
        _ => part.clone(),
    }
}

/// Convert Responses tool definitions into the nested chat shape. A `custom`
/// tool (freeform input, for example a patch editor) becomes a function with a
/// single `input` string parameter, and its call is returned as
/// `custom_tool_call` again.
fn convert_tools(tools: &[Value]) -> (Vec<ToolDefinition>, Vec<String>) {
    let mut definitions = Vec::new();
    let mut custom_tools = Vec::new();

    for tool in tools {
        let tool_type = tool
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        match tool_type {
            "function" => {
                let source = tool.get("function").unwrap_or(tool);
                let name = source
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                definitions.push(ToolDefinition {
                    r#type: "function".to_string(),
                    function: FunctionDefinition {
                        name: name.to_string(),
                        description: source
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        parameters: source.get("parameters").cloned(),
                    },
                });
            }
            "custom" => {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                custom_tools.push(name.to_string());
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("Freeform tool.");
                definitions.push(ToolDefinition {
                    r#type: "function".to_string(),
                    function: FunctionDefinition {
                        name: name.to_string(),
                        description: Some(format!(
                            "{} Put the complete raw tool input in the `input` argument.",
                            description
                        )),
                        parameters: Some(json!({
                            "type": "object",
                            "properties": {
                                "input": {
                                    "type": "string",
                                    "description": "The complete raw tool input."
                                }
                            },
                            "required": ["input"]
                        })),
                    },
                });
            }
            _ => {}
        }
    }

    (definitions, custom_tools)
}

/// `text.format` structured output is enforced by prompt instruction because
/// the Gemini Web endpoint has no response-format field.
fn format_instruction(text: Option<&Value>) -> Option<String> {
    let format = text?.get("format")?;
    let format_type = format.get("type").and_then(Value::as_str)?;
    match format_type {
        "json_schema" => {
            let schema = format.get("schema")?;
            let name = format
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("response");
            Some(format!(
                "Return only a single JSON document named `{}` that validates against this JSON Schema. No prose, no code fences.\n{}",
                name,
                serde_json::to_string(schema).unwrap_or_default()
            ))
        }
        "json_object" => Some(
            "Return only a single JSON object. No prose, no code fences.".to_string(),
        ),
        _ => None,
    }
}

fn message_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "status": status,
        "role": "assistant",
        "content": [{"type": "output_text", "text": text, "annotations": []}]
    })
}

fn tool_call_item(id: &str, call: &ToolCall, custom: bool) -> Value {
    if custom {
        let input = serde_json::from_str::<Value>(&call.function.arguments)
            .ok()
            .and_then(|value| {
                value
                    .get("input")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| call.function.arguments.clone());
        json!({
            "type": "custom_tool_call",
            "id": id,
            "status": "completed",
            "call_id": call.id,
            "name": call.function.name,
            "input": input
        })
    } else {
        json!({
            "type": "function_call",
            "id": id,
            "status": "completed",
            "call_id": call.id,
            "name": call.function.name,
            "arguments": call.function.arguments
        })
    }
}

fn build_output_items(
    text: &Option<String>,
    tool_calls: &[ToolCall],
    custom_tools: &[String],
) -> Vec<Value> {
    let mut items = Vec::new();
    if let Some(text) = text
        && !text.trim().is_empty()
    {
        items.push(message_item(
            &format!("msg_{}", Uuid::new_v4().simple()),
            text,
            "completed",
        ));
    }
    for call in tool_calls {
        let custom = custom_tools.iter().any(|name| name == &call.function.name);
        items.push(tool_call_item(
            &format!("fc_{}", Uuid::new_v4().simple()),
            call,
            custom,
        ));
    }
    items
}

fn assistant_message(text: Option<String>, tool_calls: Option<Vec<ToolCall>>) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: text.map(Value::String),
        name: None,
        tool_calls,
        tool_call_id: None,
    }
}

fn usage_json(input_chars: usize, output_chars: usize) -> Value {
    let input_tokens = (input_chars / 4) as u32;
    let output_tokens = (output_chars / 4) as u32;
    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": output_tokens,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": input_tokens.saturating_add(output_tokens)
    })
}

fn response_json(
    id: &str,
    created_at: u64,
    status: &str,
    output: Vec<Value>,
    usage: Option<Value>,
    meta: &ResponseMeta,
) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "instructions": meta.instructions,
        "max_output_tokens": meta.max_output_tokens,
        "model": meta.model,
        "output": output,
        "parallel_tool_calls": meta.parallel_tool_calls,
        "previous_response_id": meta.previous_response_id,
        "reasoning": meta.reasoning,
        "store": meta.store,
        "temperature": meta.temperature,
        "text": meta.text,
        "tool_choice": meta.tool_choice,
        "tools": meta.tools,
        "top_p": meta.top_p,
        "truncation": meta.truncation,
        "usage": usage.unwrap_or(Value::Null),
        "user": meta.user,
        "metadata": meta.metadata
    })
}

fn sse_event(sequence: &mut u64, name: &str, mut payload: Value) -> Event {
    if let Value::Object(fields) = &mut payload {
        fields.insert("type".to_string(), Value::String(name.to_string()));
        fields.insert("sequence_number".to_string(), Value::from(*sequence));
    }
    *sequence += 1;
    Event::default().event(name).data(payload.to_string())
}

/// Removes `<tool_call>...</tool_call>` blocks from streamed deltas, holding
/// back a split tag prefix until the next delta arrives.
struct TagFilter {
    pending: String,
    in_tool: bool,
}

const TOOL_TAG: &str = "<tool_call>";
const TOOL_TAG_END: &str = "</tool_call>";

impl TagFilter {
    fn new() -> Self {
        Self {
            pending: String::new(),
            in_tool: false,
        }
    }

    fn push(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        let mut output = String::new();
        loop {
            if self.in_tool {
                if let Some(end) = self.pending.find(TOOL_TAG_END) {
                    self.pending.drain(..end + TOOL_TAG_END.len());
                    self.in_tool = false;
                    continue;
                }
                if self.pending.len() > 1 << 20 {
                    self.pending.clear();
                }
                break;
            }
            if let Some(start) = self.pending.find(TOOL_TAG) {
                output.push_str(&self.pending[..start]);
                self.pending.drain(..start + TOOL_TAG.len());
                self.in_tool = true;
                continue;
            }
            let keep = partial_prefix_len(&self.pending);
            let split = self.pending.len() - keep;
            output.push_str(&self.pending[..split]);
            self.pending.drain(..split);
            break;
        }
        output
    }

    fn finish(&mut self) -> String {
        if self.in_tool {
            self.pending.clear();
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

fn partial_prefix_len(text: &str) -> usize {
    let max = TOOL_TAG.len().min(text.len());
    for len in (1..=max).rev() {
        if text.ends_with(&TOOL_TAG[..len]) {
            return len;
        }
    }
    0
}

/// Bounded in-memory response store, used only to answer
/// `previous_response_id` when a client sends the new turn alone.
struct HistoryStore {
    entries: HashMap<String, (u64, Vec<ChatMessage>)>,
    clock: u64,
}

fn history_store() -> &'static Mutex<HistoryStore> {
    static STORE: OnceLock<Mutex<HistoryStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        Mutex::new(HistoryStore {
            entries: HashMap::new(),
            clock: 0,
        })
    })
}

fn store_history(id: String, messages: Vec<ChatMessage>) {
    if messages.is_empty() {
        return;
    }
    let Ok(mut store) = history_store().lock() else {
        return;
    };
    store.clock += 1;
    let clock = store.clock;
    store.entries.insert(id, (clock, messages));
    while store.entries.len() > MAX_HISTORY_ENTRIES {
        let oldest = store
            .entries
            .iter()
            .min_by_key(|(_, (tick, _))| *tick)
            .map(|(key, _)| key.clone());
        match oldest {
            Some(key) => {
                store.entries.remove(&key);
            }
            None => break,
        }
    }
}

fn load_history(id: &str) -> Vec<ChatMessage> {
    let Ok(mut store) = history_store().lock() else {
        return Vec::new();
    };
    store.clock += 1;
    let clock = store.clock;
    match store.entries.get_mut(id) {
        Some(entry) => {
            entry.0 = clock;
            entry.1.clone()
        }
        None => Vec::new(),
    }
}

/// Join a stored conversation with the messages a client just sent. A client
/// that resends the full history shares the stored prefix and only its extra
/// tail is appended, so turns are never duplicated.
fn merge_history(stored: &[ChatMessage], incoming: Vec<ChatMessage>) -> Vec<ChatMessage> {
    if stored.is_empty() {
        return incoming;
    }
    let common = stored
        .iter()
        .zip(incoming.iter())
        .take_while(|(left, right)| same_message(left, right))
        .count();
    let mut merged = stored.to_vec();
    merged.extend_from_slice(&incoming[common..]);
    if merged.len() > MAX_HISTORY_MESSAGES {
        merged.drain(..merged.len() - MAX_HISTORY_MESSAGES);
    }
    merged
}

fn same_message(left: &ChatMessage, right: &ChatMessage) -> bool {
    left.role == right.role
        && left.content_as_str() == right.content_as_str()
        && left.tool_call_id == right.tool_call_id
        && tool_signature(&left.tool_calls) == tool_signature(&right.tool_calls)
}

fn tool_signature(calls: &Option<Vec<ToolCall>>) -> Vec<(String, String, String)> {
    calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .map(|call| {
                    (
                        call.id.clone(),
                        call.function.name.clone(),
                        call.function.arguments.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Keep stored history small. Attachments are replaced by a short marker and
/// oversized messages are truncated, so the store cannot grow without bound.
fn slim_messages(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    messages
        .into_iter()
        .map(|mut message| {
            message.content = message.content.map(|content| match content {
                Value::Array(parts) => {
                    let mut text = Vec::new();
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(value) = part.get("text").and_then(Value::as_str) {
                                    text.push(value.to_string());
                                }
                            }
                            Some("input_image") | Some("image_url") => {
                                text.push("[attached image]".to_string())
                            }
                            Some("input_video") | Some("video_url") => {
                                text.push("[attached video]".to_string())
                            }
                            _ => text.push("[attached file]".to_string()),
                        }
                    }
                    Value::String(limit_chars(text.join("\n")))
                }
                Value::String(text) => Value::String(limit_chars(text)),
                other => other,
            });
            message
        })
        .collect()
}

fn limit_chars(text: String) -> String {
    if text.chars().count() <= STORED_MESSAGE_CHAR_LIMIT {
        return text;
    }
    let mut limited: String = text.chars().take(STORED_MESSAGE_CHAR_LIMIT).collect();
    limited.push_str("\n[truncated]");
    limited
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_of(body: Value) -> ChatCompletionRequest {
        let request: ResponsesRequest = serde_json::from_value(body).unwrap();
        request.to_chat_request("gemini-4-hydrogen").unwrap().0
    }

    #[test]
    fn string_input_becomes_one_user_message() {
        let chat = chat_of(json!({"model": "gpt-4o", "input": "hello"}));
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(chat.messages[0].content_as_str(), "hello");
    }

    #[test]
    fn instructions_become_a_system_message() {
        let chat = chat_of(json!({
            "instructions": "be terse",
            "input": "hi"
        }));
        assert_eq!(chat.messages[0].role, "system");
        assert_eq!(chat.messages[0].content_as_str(), "be terse");
    }

    #[test]
    fn message_items_keep_roles_and_text_parts() {
        let chat = chat_of(json!({
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "first"}
                ]},
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "second"}
                ]}
            ]
        }));
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(chat.messages[0].content_as_str(), "first");
        assert_eq!(chat.messages[1].role, "assistant");
        assert_eq!(chat.messages[1].content_as_str(), "second");
    }

    #[test]
    fn function_call_history_round_trips() {
        let chat = chat_of(json!({
            "input": [
                {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "a.txt"}
            ]
        }));
        assert_eq!(chat.messages.len(), 2);
        let calls = chat.messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "shell");
        assert_eq!(chat.messages[1].role, "tool");
        assert_eq!(chat.messages[1].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(chat.messages[1].content_as_str(), "a.txt");
    }

    #[test]
    fn custom_tool_calls_become_input_arguments() {
        let chat = chat_of(json!({
            "input": [
                {"type": "custom_tool_call", "call_id": "call_9", "name": "apply_patch", "input": "*** Begin Patch\n*** End Patch"}
            ]
        }));
        let calls = chat.messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].function.name, "apply_patch");
        let arguments: Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(arguments["input"], "*** Begin Patch\n*** End Patch");
    }

    #[test]
    fn flat_function_tools_are_nested_for_the_prompt() {
        let chat = chat_of(json!({
            "input": "hi",
            "tools": [{
                "type": "function",
                "name": "shell",
                "description": "run a command",
                "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}}
            }]
        }));
        let tools = chat.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "shell");
        assert_eq!(
            tools[0].function.description.as_deref(),
            Some("run a command")
        );
    }

    #[test]
    fn custom_tools_get_a_single_input_parameter() {
        let request: ResponsesRequest = serde_json::from_value(json!({
            "input": "hi",
            "tools": [{"type": "custom", "name": "apply_patch", "description": "edit files"}]
        }))
        .unwrap();
        let (chat, custom) = request.to_chat_request("m").unwrap();
        assert_eq!(custom, vec!["apply_patch".to_string()]);
        let parameters = chat.tools.unwrap()[0].function.parameters.clone().unwrap();
        assert_eq!(parameters["properties"]["input"]["type"], "string");
    }

    #[test]
    fn json_schema_text_format_adds_an_instruction() {
        let chat = chat_of(json!({
            "input": "hi",
            "text": {"format": {"type": "json_schema", "name": "out", "schema": {"type": "object"}}}
        }));
        assert_eq!(chat.messages[0].role, "system");
        assert!(chat.messages[0].content_as_str().contains("JSON Schema"));
    }

    #[test]
    fn tag_filter_removes_tool_payloads() {
        let mut filter = TagFilter::new();
        let first = filter.push("before <tool_call>{\"name\":\"x\"}</tool_call> after");
        assert_eq!(first, "before  after");
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn tag_filter_keeps_text_that_arrives_later() {
        let mut filter = TagFilter::new();
        assert_eq!(filter.push("before <tool_call>{\"name\":\"x\"}"), "before ");
        assert_eq!(filter.push("</tool_call> after"), " after");
    }

    #[test]
    fn tag_filter_holds_a_split_tag_prefix() {
        let mut filter = TagFilter::new();
        assert_eq!(filter.push("hello <tool"), "hello ");
        assert_eq!(filter.push("_call>{\"name\":\"x\"}"), "");
        assert_eq!(filter.push("</tool_call>done"), "done");
    }

    #[test]
    fn history_merge_appends_a_new_turn() {
        let stored = vec![
            ChatMessage {
                role: "user".into(),
                content: Some(json!("first")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: Some(json!("answer")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let incoming = vec![ChatMessage {
            role: "user".into(),
            content: Some(json!("second")),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let merged = merge_history(&stored, incoming);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[2].content_as_str(), "second");
    }

    #[test]
    fn history_merge_does_not_duplicate_a_full_resend() {
        let stored = vec![ChatMessage {
            role: "user".into(),
            content: Some(json!("first")),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let incoming = vec![
            ChatMessage {
                role: "user".into(),
                content: Some(json!("first")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".into(),
                content: Some(json!("second")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let merged = merge_history(&stored, incoming);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn response_object_matches_the_responses_shape() {
        let request: ResponsesRequest = serde_json::from_value(json!({
            "model": "gpt-4o",
            "input": "hi"
        }))
        .unwrap();
        let meta = ResponseMeta::from_request(&request, "gpt-4o");
        let body = response_json(
            "resp_1",
            1,
            "completed",
            build_output_items(&Some("hi there".to_string()), &[], &[]),
            Some(usage_json(4, 8)),
            &meta,
        );
        assert_eq!(body["object"], "response");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0]["type"], "message");
        assert_eq!(body["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(body["output"][0]["content"][0]["text"], "hi there");
        assert_eq!(body["usage"]["output_tokens"], 2);
    }

    #[test]
    fn custom_tool_results_use_custom_tool_call_items() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            r#type: "function".into(),
            function: FunctionCall {
                name: "apply_patch".into(),
                arguments: json!({"input": "patch body"}).to_string(),
            },
        }];
        let items = build_output_items(&None, &calls, &["apply_patch".to_string()]);
        assert_eq!(items[0]["type"], "custom_tool_call");
        assert_eq!(items[0]["input"], "patch body");
    }
}
