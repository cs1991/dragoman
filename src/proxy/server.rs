use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};
use uuid::Uuid;

use crate::openai::types::*;
use crate::proxy::translator::PromptTranslator;
use crate::web::GeminiWebClient;

pub struct AppState {
    pub web_client: Arc<GeminiWebClient>,
    pub default_model: String,
    pub max_attachment_bytes: u64,
}

const MAX_ATTACHMENTS_PER_REQUEST: usize = 10;
const MAX_PROMPT_BYTES: usize = 8 * 1024 * 1024;

pub fn create_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let body_limit = usize::try_from(
        state
            .max_attachment_bytes
            .saturating_mul(4)
            .saturating_div(3)
            .saturating_add(8 * 1024 * 1024),
    )
    .unwrap_or(usize::MAX);

    // The client API key is optional. Gemini authentication uses the saved session.
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/health", get(health_check))
        .route("/", get(root_handler))
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(cors)
        .with_state(state)
}

async fn root_handler() -> &'static str {
    "Dragoman Native OpenAI-compatible bridge for Gemini Web (Gemini 4 Argon) is running."
}

async fn health_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "dragoman",
        "engine": "native-http2-streamgenerate",
        "model": state.default_model
    }))
}

async fn list_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let models = vec![
        ModelObject {
            id: state.default_model.clone(),
            object: "model".to_string(),
            created: now,
            owned_by: "google".to_string(),
        },
        ModelObject {
            id: "gemini-4-argon".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "google".to_string(),
        },
        ModelObject {
            id: "gemini-4-argon-thinking".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "google".to_string(),
        },
    ];

    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<ChatCompletionRequest>, JsonRejection>,
) -> Response {
    let Json(req) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return openai_error(
                rejection.status(),
                "invalid_request_error",
                rejection.body_text(),
            );
        }
    };

    info!("Received chat completion request for model: {}", req.model);

    let unsupported = req.unsupported_media_parts();
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

    let media = req.collect_media();
    info!("Request attachments: {}", media.len());
    if media.len() > MAX_ATTACHMENTS_PER_REQUEST {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "A request can contain at most {MAX_ATTACHMENTS_PER_REQUEST} video, image or file attachments"
            ),
        );
    }

    let mut files = Vec::with_capacity(media.len());
    for attachment in &media {
        match state
            .web_client
            .upload_media(attachment, state.max_attachment_bytes)
            .await
        {
            Ok(file) => {
                info!("Uploaded attachment: {}", file.name);
                files.push(file);
            }
            Err(error) => {
                let status =
                    StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::BAD_GATEWAY);
                let error_type = if status == StatusCode::BAD_GATEWAY {
                    "upstream_error"
                } else {
                    "invalid_request_error"
                };
                return openai_error(status, error_type, error.to_string());
            }
        }
    }

    let mut prompt = PromptTranslator::build_gemini_prompt(&req);
    let has_user_text = req
        .messages
        .iter()
        .any(|message| message.role == "user" && !message.content_as_str().trim().is_empty());
    if !has_user_text && !files.is_empty() {
        if prompt.trim().is_empty() {
            prompt = "Please describe the attached file.".to_string();
        } else {
            prompt.push_str("\n\nPlease describe the attached file.");
        }
    }

    if prompt.trim().is_empty() && files.is_empty() {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "The request contains no text or supported attachment.",
        );
    }

    if prompt.len() > MAX_PROMPT_BYTES {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "The text prompt is too large. Send video, image or file bytes as attachment content parts.",
        );
    }

    info!(
        "Prompt characters: {}, uploaded attachments: {}",
        prompt.chars().count(),
        files.len()
    );

    let is_stream = req.stream.unwrap_or(false);
    let request_model = req.model.clone();

    let (delta_tx, mut delta_rx) = mpsc::channel::<Result<String, String>>(100);

    if is_stream {
        let stream_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Spawn prompt submission and streaming task via direct native HTTP RPC
        let client = state.web_client.clone();
        let prompt_clone = prompt.clone();
        let files_clone = files.clone();
        tokio::spawn(async move {
            if let Err(e) = client
                .send_prompt_stream(&prompt_clone, &files_clone, delta_tx)
                .await
            {
                error!("Prompt stream execution error: {}", e);
            }
        });

        let sse_stream = async_stream::stream! {
            let initial_chunk = ChatCompletionChunk {
                id: stream_id.clone(),
                object: "chat.completion.chunk".to_string(),
                created: created_at,
                model: request_model.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: ChunkDelta {
                        role: Some("assistant".to_string()),
                        content: Some("".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
            };
            if let Ok(json_str) = serde_json::to_string(&initial_chunk) {
                yield Ok::<Event, Infallible>(Event::default().data(json_str));
            }

            let mut accumulated = String::new();
            let mut stream_error = None;

            while let Some(res) = delta_rx.recv().await {
                match res {
                    Ok(delta) => {
                        accumulated.push_str(&delta);

                        let chunk = ChatCompletionChunk {
                            id: stream_id.clone(),
                            object: "chat.completion.chunk".to_string(),
                            created: created_at,
                            model: request_model.clone(),
                            choices: vec![ChunkChoice {
                                index: 0,
                                delta: ChunkDelta {
                                    role: None,
                                    content: Some(delta),
                                    tool_calls: None,
                                },
                                finish_reason: None,
                            }],
                        };
                        if let Ok(json_str) = serde_json::to_string(&chunk) {
                            yield Ok::<Event, Infallible>(Event::default().data(json_str));
                        }
                    }
                    Err(err) => {
                        stream_error = Some(err);
                        break;
                    }
                }
            }

            if let Some(err) = stream_error {
                let err_payload = serde_json::json!({
                    "error": {
                        "message": err,
                        "type": "upstream_error",
                        "code": 500
                    }
                });
                yield Ok::<Event, Infallible>(Event::default().event("error").data(err_payload.to_string()));
                yield Ok::<Event, Infallible>(Event::default().data("[DONE]"));
                return;
            }

            let (_, tool_calls) = PromptTranslator::parse_response(&accumulated);

            if let Some(calls) = tool_calls {
                let tool_chunk = ChatCompletionChunk {
                    id: stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: created_at,
                    model: request_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta {
                            role: None,
                            content: None,
                            tool_calls: Some(calls),
                        },
                        finish_reason: Some("tool_calls".to_string()),
                    }],
                };
                if let Ok(json_str) = serde_json::to_string(&tool_chunk) {
                    yield Ok::<Event, Infallible>(Event::default().data(json_str));
                }
            } else {
                let finish_chunk = ChatCompletionChunk {
                    id: stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: created_at,
                    model: request_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta::default(),
                        finish_reason: Some("stop".to_string()),
                    }],
                };
                if let Ok(json_str) = serde_json::to_string(&finish_chunk) {
                    yield Ok::<Event, Infallible>(Event::default().data(json_str));
                }
            }

            yield Ok::<Event, Infallible>(Event::default().data("[DONE]"));
        };

        Sse::new(sse_stream)
            .keep_alive(KeepAlive::default())
            .into_response()
    } else {
        match state
            .web_client
            .send_prompt_stream(&prompt, &files, delta_tx)
            .await
        {
            Ok(full_output) => {
                let (text_opt, tool_calls_opt) = PromptTranslator::parse_response(&full_output);
                let finish_reason = if tool_calls_opt.is_some() {
                    "tool_calls".to_string()
                } else {
                    "stop".to_string()
                };

                let response = ChatCompletionResponse {
                    id: format!("chatcmpl-{}", Uuid::new_v4().simple()),
                    object: "chat.completion".to_string(),
                    created: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    model: request_model,
                    choices: vec![ChatChoice {
                        index: 0,
                        message: ChatMessage {
                            role: "assistant".to_string(),
                            content: text_opt.map(serde_json::Value::String),
                            name: None,
                            tool_calls: tool_calls_opt,
                            tool_call_id: None,
                        },
                        finish_reason: Some(finish_reason),
                    }],
                    usage: Some(Usage {
                        prompt_tokens: (prompt.len() / 4) as u32,
                        completion_tokens: (full_output.len() / 4) as u32,
                        total_tokens: ((prompt.len() + full_output.len()) / 4) as u32,
                    }),
                };

                Json(response).into_response()
            }
            Err(e) => {
                error!("Generation failed: {}", e);
                openai_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "upstream_error",
                    format!("Generation failed: {}", e),
                )
            }
        }
    }
}

fn openai_error(status: StatusCode, error_type: &str, message: impl Into<String>) -> Response {
    let payload = serde_json::json!({
        "error": {
            "message": message.into(),
            "type": error_type,
            "code": status.as_u16()
        }
    });
    (status, Json(payload)).into_response()
}
