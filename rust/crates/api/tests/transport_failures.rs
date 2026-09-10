use std::time::Duration;

use api::{ApiClient, ApiError, InputMessage, MessageRequest};
use tokio::net::TcpListener;

#[tokio::test]
async fn refused_connection_is_retryable_and_does_not_panic() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let address = listener.local_addr().expect("listener address");
    drop(listener);

    let client = ApiClient::new("test-key")
        .with_base_url(format!("http://{address}"))
        .with_retry_policy(0, Duration::ZERO, Duration::ZERO);

    let error = client
        .send_message(&MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![InputMessage::user_text("transport failure")],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        })
        .await
        .expect_err("closed listener should produce a transport error");

    match error {
        ApiError::RetriesExhausted {
            attempts,
            last_error,
        } => {
            assert_eq!(attempts, 1);
            assert!(matches!(*last_error, ApiError::Http(_)));
            assert!(last_error.is_retryable());
        }
        other => panic!("expected retry exhaustion, got {other:?}"),
    }
}
