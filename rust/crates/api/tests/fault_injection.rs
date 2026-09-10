use std::time::Duration;

use api::{ApiClient, ApiError, InputMessage, MessageRequest, StreamEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn non_retryable_http_failures_preserve_status_and_body() {
    for (status, retryable) in [("401 Unauthorized", false), ("404 Not Found", false)] {
        let server = spawn_server(http_response(
            status,
            "application/json",
            "{\"type\":\"error\",\"error\":{\"type\":\"request_error\",\"message\":\"rejected\"}}",
        ))
        .await;

        let client = ApiClient::new("test-key")
            .with_base_url(server.base_url())
            .with_retry_policy(0, Duration::ZERO, Duration::ZERO);

        let error = client
            .send_message(&request())
            .await
            .expect_err("HTTP failure should surface");
        match error {
            ApiError::Api {
                status: actual_status,
                retryable: actual_retryable,
                body,
                ..
            } => {
                assert_eq!(actual_status.as_str(), status.split_whitespace().next().unwrap());
                assert_eq!(actual_retryable, retryable);
                assert!(body.contains("rejected"));
            }
            other => panic!("expected API error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn retryable_server_failures_are_classified_without_retry_when_disabled() {
    for status in ["429 Too Many Requests", "500 Internal Server Error"] {
        let server = spawn_server(http_response(
            status,
            "application/json",
            "{\"type\":\"error\",\"error\":{\"type\":\"server_error\",\"message\":\"retry me\"}}",
        ))
        .await;

        let client = ApiClient::new("test-key")
            .with_base_url(server.base_url())
            .with_retry_policy(0, Duration::ZERO, Duration::ZERO);

        let error = client
            .send_message(&request())
            .await
            .expect_err("retryable failure should still surface with retries disabled");
        match error {
            ApiError::Api {
                status: actual_status,
                retryable,
                ..
            } => {
                assert_eq!(actual_status.as_str(), status.split_whitespace().next().unwrap());
                assert!(retryable);
            }
            other => panic!("expected API error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn retry_budget_is_consumed_deterministically() {
    let server = spawn_server(http_response(
        "503 Service Unavailable",
        "application/json",
        "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}",
    ))
    .await;

    let client = ApiClient::new("test-key")
        .with_base_url(server.base_url())
        .with_retry_policy(1, Duration::from_millis(1), Duration::from_millis(1));

    let error = client
        .send_message(&request())
        .await
        .expect_err("persistent 503 should exhaust retries");

    match error {
        ApiError::RetriesExhausted { attempts, last_error } => {
            assert_eq!(attempts, 2);
            assert!(matches!(
                *last_error,
                ApiError::Api {
                    status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                    retryable: true,
                    ..
                }
            ));
        }
        other => panic!("expected retries exhausted, got {other:?}"),
    }
}

#[tokio::test]
async fn malformed_json_is_not_silently_accepted() {
    let server = spawn_server(http_response("200 OK", "application/json", "{not-json"))
        .await;
    let client = ApiClient::new("test-key")
        .with_base_url(server.base_url())
        .with_retry_policy(0, Duration::ZERO, Duration::ZERO);

    let error = client
        .send_message(&request())
        .await
        .expect_err("malformed JSON should fail");
    assert!(matches!(error, ApiError::Json(_)));
}

#[tokio::test]
async fn malformed_sse_payload_is_reported_at_stream_boundary() {
    let server = spawn_server(http_response(
        "200 OK",
        "text/event-stream",
        "data: {not-json}\n\n",
    ))
    .await;
    let client = ApiClient::new("test-key")
        .with_base_url(server.base_url())
        .with_retry_policy(0, Duration::ZERO, Duration::ZERO);

    let mut stream = client
        .stream_message(&request())
        .await
        .expect("stream request should be accepted");
    let error = stream
        .next_event()
        .await
        .expect_err("malformed SSE should fail when consumed");
    assert!(matches!(error, ApiError::InvalidSseFrame(_)));
}

#[tokio::test]
async fn provider_stream_survives_valid_prefix_then_detects_truncated_frame() {
    let response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: text/event-stream\r\n",
        "Connection: close\r\n",
        "\r\n",
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_partial\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4-6\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"hello\"}}\n",
    );
    let server = spawn_raw_response(response).await;
    let client = ApiClient::new("test-key")
        .with_base_url(server.base_url())
        .with_retry_policy(0, Duration::ZERO, Duration::ZERO);

    let mut stream = client
        .stream_message(&request())
        .await
        .expect("stream request should start");
    let first = stream
        .next_event()
        .await
        .expect("first event should parse")
        .expect("first event should exist");
    assert!(matches!(first, StreamEvent::MessageStart(_)));

    let second = stream
        .next_event()
        .await
        .expect("truncated final frame should not panic");
    assert!(second.is_none(), "incomplete trailing frame should be dropped");
}

fn request() -> MessageRequest {
    MessageRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 32,
        messages: vec![InputMessage::user_text("verification")],
        system: None,
        tools: None,
        tool_choice: None,
        stream: false,
    }
}

struct TestServer {
    base_url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    fn base_url(&self) -> String {
        self.base_url.clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn spawn_server(response: String) -> TestServer {
    spawn_raw_response(response_to_bytes(response)).await
}

async fn spawn_raw_response(response: String) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let address = listener.local_addr().expect("listener address");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("server should accept");
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).await;
        socket
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
        socket.shutdown().await.expect("socket should close");
    });

    TestServer {
        base_url: format!("http://{address}"),
        handle,
    }
}

fn response_to_bytes(response: String) -> String {
    response
}

fn http_response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
