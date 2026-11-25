use dragoman::proxy::server::{AppState, create_router};
use dragoman::session::SessionData;
use dragoman::web::GeminiWebClient;
use reqwest::{Client, StatusCode};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn proxy_routes_do_not_require_or_check_an_api_key() {
    let web_client = GeminiWebClient::new(SessionData {
        snlm0e: String::new(),
        f_sid: String::new(),
        bl: String::new(),
        cookie: String::new(),
        model: "Argon4p0".to_string(),
        conversation_id: None,
        response_id: None,
        choice_id: None,
        base_url: None,
        proxy: None,
        path: None,
    });
    let state = Arc::new(AppState {
        web_client: Arc::new(web_client),
        default_model: "gemini-4-argon".to_string(),
        max_attachment_bytes: 200 * 1024 * 1024,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async { axum::serve(listener, create_router(state)).await });
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    for key in [None, Some("dragoman-local"), Some("arbitrary-placeholder")] {
        let models = client.get(format!("{base}/v1/models"));
        let chat = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&serde_json::json!({}));
        let with_key = |request: reqwest::RequestBuilder| match key {
            Some(key) => request.bearer_auth(key),
            None => request,
        };
        assert_eq!(
            with_key(models).send().await.unwrap().status(),
            StatusCode::OK
        );
        // An invalid body reaches JSON validation without making a request to Gemini.
        assert_eq!(
            with_key(chat).send().await.unwrap().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    server.abort();
}
