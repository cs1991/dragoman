use axum::{Router, body::Bytes, extract::State, http::HeaderMap, routing::post};
use dragoman::openai::types::{ChatCompletionRequest, MediaAttachment, MediaKind, MediaSource};
use dragoman::web::GeminiWebClient;
use dragoman::web::upload::{UploadError, UploadedFile, upload_to_endpoint};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Default)]
struct CapturedRequest {
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn capture_upload(
    State(captured): State<Arc<Mutex<CapturedRequest>>>,
    headers: HeaderMap,
    body: Bytes,
) -> String {
    let mut captured = captured.lock().await;
    captured.headers = headers;
    captured.body = body.to_vec();
    "/contrib_service/ttl_1d/fixture_ref".to_string()
}

#[test]
fn payload_carries_uploaded_files_in_slot_zero() {
    let files = vec![
        UploadedFile {
            path: "/contrib_service/ttl_1d/first".to_string(),
            name: "first.mp4".to_string(),
        },
        UploadedFile {
            path: "/contrib_service/ttl_1d/second".to_string(),
            name: "second.webm".to_string(),
        },
    ];

    let payload = GeminiWebClient::build_stream_generate_payload_with_files(
        "Describe the clips",
        Some("c_conv"),
        Some("r_resp"),
        &files,
    );

    assert_eq!(payload.len(), 99);
    assert_eq!(
        payload[0],
        json!([
            "Describe the clips",
            0,
            null,
            [
                [["/contrib_service/ttl_1d/first"], "first.mp4"],
                [["/contrib_service/ttl_1d/second"], "second.webm"]
            ],
            null,
            null,
            0
        ])
    );
    assert_eq!(payload[3], json!("r_resp"));

    let envelope = GeminiWebClient::build_form_envelope_with_files(
        "Describe the clips",
        Some("c_conv"),
        Some("r_resp"),
        &files,
    );
    assert!(envelope.contains("/contrib_service/ttl_1d/first"));
    assert!(envelope.contains("second.webm"));
}

#[test]
fn extracts_video_image_and_file_content_parts() {
    let request: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "gemini-4-argon",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Describe these attachments"},
                {
                    "type": "video_url",
                    "video_url": {"url": "data:video/mp4;base64,RFJBR09NQU4="}
                },
                {
                    "type": "input_video",
                    "input_video": {
                        "data": "QUJD",
                        "format": "mp4",
                        "filename": "raw.mp4"
                    }
                },
                {
                    "type": "image_url",
                    "image_url": {"url": "https://example.com/frame.png"}
                },
                {
                    "type": "file",
                    "file": {
                        "filename": "report.pdf",
                        "file_data": "data:application/pdf;base64,JVBERi0="
                    }
                }
            ]
        }]
    }))
    .unwrap();

    let media = request.collect_media();
    assert_eq!(media.len(), 4);
    assert_eq!(media[0].kind, MediaKind::Video);
    assert!(matches!(media[0].source, MediaSource::DataUrl(_)));
    assert_eq!(media[1].kind, MediaKind::Video);
    assert_eq!(media[1].filename.as_deref(), Some("raw.mp4"));
    assert_eq!(media[1].mime_type.as_deref(), Some("video/mp4"));
    assert_eq!(
        media[1].source,
        MediaSource::DataUrl("data:video/mp4;base64,QUJD".to_string())
    );
    assert_eq!(media[2].kind, MediaKind::Image);
    assert_eq!(
        media[2].source,
        MediaSource::Url("https://example.com/frame.png".to_string())
    );
    assert_eq!(media[3].kind, MediaKind::File);
    assert_eq!(media[3].filename.as_deref(), Some("report.pdf"));
}

#[test]
fn accepts_preuploaded_contrib_paths() {
    let request: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "gemini-4-argon",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "file",
                "file": {
                    "file_id": "/contrib_service/ttl_1d/already_uploaded",
                    "filename": "cached.mp4"
                }
            }]
        }]
    }))
    .unwrap();

    let media = request.collect_media();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].kind, MediaKind::Video);
    assert_eq!(
        media[0].source,
        MediaSource::UploadedPath("/contrib_service/ttl_1d/already_uploaded".to_string())
    );
}

#[test]
fn extracts_object_content_and_normalizes_uploaded_file_ids() {
    let request: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "gemini-4-argon",
        "messages": [
            {
                "role": "user",
                "content": {
                    "type": "video_url",
                    "video_url": {
                        "url": "file:///D:/clips/direct.mp4"
                    }
                }
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "file",
                        "file": {
                            "file_id": "contrib_service/ttl_1d/no_leading_slash",
                            "filename": "cached.mp4"
                        }
                    }
                ]
            }
        ]
    }))
    .unwrap();

    let media = request.collect_media();
    assert_eq!(media.len(), 2);
    assert_eq!(media[0].kind, MediaKind::Video);
    assert_eq!(
        media[0].source,
        MediaSource::FilePath("file:///D:/clips/direct.mp4".to_string())
    );
    assert_eq!(media[1].kind, MediaKind::Video);
    assert_eq!(
        media[1].source,
        MediaSource::UploadedPath("/contrib_service/ttl_1d/no_leading_slash".to_string())
    );
}

#[tokio::test]
async fn uploads_local_file_with_push_headers() {
    let captured = Arc::new(Mutex::new(CapturedRequest::default()));
    let app = Router::new()
        .route("/upload", post(capture_upload))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let file_path =
        std::env::temp_dir().join(format!("dragoman-attachment-{}.mp4", uuid::Uuid::new_v4()));
    let file_bytes = b"\x00\x00\x00\x18ftypmp42DRAGOMANFIXTURE".to_vec();
    std::fs::write(&file_path, &file_bytes).unwrap();

    let media = MediaAttachment {
        kind: MediaKind::Video,
        source: MediaSource::FilePath(file_path.to_string_lossy().into_owned()),
        filename: Some("clip.mp4".to_string()),
        mime_type: Some("video/mp4".to_string()),
    };

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let uploaded = upload_to_endpoint(
        &client,
        &format!("http://{address}/upload"),
        "SID=fixture",
        &media,
        1024 * 1024,
    )
    .await
    .unwrap();

    assert_eq!(uploaded.path, "/contrib_service/ttl_1d/fixture_ref");
    assert_eq!(uploaded.name, "clip.mp4");

    let captured = captured.lock().await;
    assert_eq!(
        captured
            .headers
            .get("x-tenant-id")
            .and_then(|value| value.to_str().ok()),
        Some("bard-storage")
    );
    assert_eq!(
        captured
            .headers
            .get("push-id")
            .and_then(|value| value.to_str().ok()),
        Some("feeds/mcudyrk2a4khkz")
    );

    let position = captured
        .body
        .windows(file_bytes.len())
        .position(|window| window == file_bytes)
        .expect("multipart body must contain the file bytes");
    let headers = String::from_utf8_lossy(&captured.body[..position]);
    assert!(headers.contains("filename=\"clip.mp4\""));
    assert!(headers.contains("Content-Type: video/mp4"));
    drop(captured);

    let _ = std::fs::remove_file(&file_path);
    server.abort();
}

#[tokio::test]
async fn uploads_data_url_bytes() {
    let captured = Arc::new(Mutex::new(CapturedRequest::default()));
    let app = Router::new()
        .route("/upload", post(capture_upload))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let media = MediaAttachment {
        kind: MediaKind::Video,
        source: MediaSource::DataUrl("data:video/mp4;base64,RFJBR09NQU4=".to_string()),
        filename: None,
        mime_type: None,
    };

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let uploaded = upload_to_endpoint(
        &client,
        &format!("http://{address}/upload"),
        "SID=fixture",
        &media,
        1024 * 1024,
    )
    .await
    .unwrap();

    assert_eq!(uploaded.name, "attachment.mp4");
    let captured = captured.lock().await;
    assert!(
        captured
            .body
            .windows(b"DRAGOMAN".len())
            .any(|window| window == b"DRAGOMAN")
    );
    drop(captured);

    server.abort();
}

#[tokio::test]
async fn rejects_oversized_attachments() {
    let media = MediaAttachment {
        kind: MediaKind::Video,
        source: MediaSource::DataUrl("data:video/mp4;base64,RFJBR09NQU4=".to_string()),
        filename: None,
        mime_type: None,
    };

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let error = upload_to_endpoint(
        &client,
        "http://127.0.0.1:9/upload",
        "SID=fixture",
        &media,
        4,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, UploadError::TooLarge(_)));
}
