use dragoman::accounts::Account;
use dragoman::session::SessionData;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn session(proxy: Option<String>) -> SessionData {
    SessionData {
        snlm0e: "at".to_string(),
        f_sid: "sid".to_string(),
        bl: "bl".to_string(),
        cookie: "cookie".to_string(),
        model: "Argon4p0".to_string(),
        conversation_id: None,
        response_id: None,
        choice_id: None,
        base_url: None,
        proxy,
        path: None,
    }
}

/// A per-account proxy must carry the request itself. An HTTP target without
/// TLS keeps the fixture small: the proxy sees an absolute-form request line.
#[tokio::test]
async fn account_proxy_carries_requests_through_the_listener() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let proxy_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = vec![0u8; 4096];
        let read = socket.read(&mut buffer).await.unwrap();
        let head = String::from_utf8_lossy(&buffer[..read]).to_string();
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await;
        head
    });

    // A bare host:port is accepted and treated as an HTTP proxy.
    let account = Account::new("proxied", session(Some(format!("127.0.0.1:{port}"))));
    let response = account
        .http
        .get("http://example.test/ping")
        .send()
        .await
        .expect("proxied request should answer");
    assert_eq!(response.status().as_u16(), 200);

    let head = proxy_task.await.unwrap();
    assert!(
        head.starts_with("GET http://example.test/ping"),
        "proxy did not see the absolute request line: {head}"
    );
}

#[tokio::test]
async fn account_without_proxy_keeps_a_working_client() {
    let account = Account::new("direct", session(None));
    // The client builds without a proxy. Reaching the network is not part of
    // this check, so only construction and local request routing matter here.
    let request = account
        .http
        .get("http://127.0.0.1:1/health")
        .build()
        .expect("request should build");
    assert_eq!(request.url().host_str(), Some("127.0.0.1"));
}
