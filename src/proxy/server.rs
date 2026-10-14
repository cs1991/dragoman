use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};
use uuid::Uuid;

use crate::browser::CdpClient;
use crate::openai::types::*;
use crate::proxy::translator::PromptTranslator;

pub struct AppState {
    pub cdp_client: Arc<Mutex<Option<Arc<CdpClient>>>>,
    pub cdp_port: u16,
    pub default_model: String,
}

pub fn create_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/health", get(health_check))
        .route("/", get(root_handler))
        .layer(cors)
        .with_state(state)
}

async fn root_handler() -> &'static str {
    "Dragoman OpenAI-compatible bridge for Gemini Web (Gemini 4 Argon) is running."
}

async fn health_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cdp_ready = {
        let guard = state.cdp_client.lock().await;
        guard.is_some()
    };

    Json(serde_json::json!({
        "status": "ok",
        "service": "dragoman",
        "cdp_port": state.cdp_port,
        "cdp_connected": cdp_ready,
        "model": state.default_model
    }))
}

async fn list_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

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
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    info!("Received chat completion request for model: {}", req.model);

    // Ensure CDP client is active and grab an Arc clone
    let cdp: Arc<CdpClient> = {
        let mut guard = state.cdp_client.lock().await;
        if guard.is_none() {
            info!("Connecting to Chrome CDP on port {}...", state.cdp_port);
            match CdpClient::connect_to_gemini(state.cdp_port).await {
                Ok(client) => {
                    *guard = Some(Arc::new(client));
                }
                Err(e) => {
                    error!("Failed to connect to Chrome CDP: {}", e);
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        format!("Browser connection failed: {}. Is Chrome running on port {}?", e, state.cdp_port),
                    ));
                }
            }
        }
        guard.as_ref().unwrap().clone()
    };

    let prompt = PromptTranslator::build_gemini_prompt(&req);
    let is_stream = req.stream.unwrap_or(false);
    let request_model = req.model.clone();

    let (delta_tx, mut delta_rx) = mpsc::channel::<String>(100);

    if is_stream {
        let stream_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
        let created_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        // Spawn prompt submission and streaming task
        let cdp_clone = cdp.clone();
        let prompt_clone = prompt.clone();
        tokio::spawn(async move {
            if let Err(e) = cdp_clone.submit_prompt_and_stream(&prompt_clone, delta_tx).await {
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

            while let Some(delta) = delta_rx.recv().await {
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

        Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response())
    } else {
        match cdp.submit_prompt_and_stream(&prompt, delta_tx).await {
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
                    created: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                    model: request_model,
                    choices: vec![ChatChoice {
                        index: 0,
                        message: ChatMessage {
                            role: "assistant".to_string(),
                            content: text_opt.map(|s| serde_json::Value::String(s)),
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

                Ok(Json(response).into_response())
            }
            Err(e) => {
                error!("Generation failed: {}", e);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Generation failed: {}", e),
                ))
            }
        }
    }
}
