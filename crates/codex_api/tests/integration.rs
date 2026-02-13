use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use codex_api::events::{CodexResponseStatus, CodexStreamEvent};
use codex_api::{CodexApiClient, CodexApiConfig, CodexApiError, CodexRequest};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Duration};

fn allow_local_integration() -> bool {
    std::env::var("CODEX_API_ALLOW_LOCAL_INTEGRATION")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

#[derive(Clone)]
struct ResponseChunk {
    delay_ms: u64,
    bytes: Vec<u8>,
}

#[derive(Clone)]
enum ScriptedResponse {
    Respond {
        status: u16,
        content_type: &'static str,
        chunks: Vec<ResponseChunk>,
    },
    Reset,
}

struct ScriptedServer {
    base_url: String,
    request_count: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl ScriptedServer {
    async fn new(scripts: Vec<ScriptedResponse>) -> Self {
        let scripts = Arc::new(scripts);
        let request_count = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local TCP listener should bind");
        let addr = listener
            .local_addr()
            .expect("resolved local listener address");
        let base_url = format!("http://{addr}");

        let handle = tokio::spawn({
            let scripts = Arc::clone(&scripts);
            let request_count = Arc::clone(&request_count);

            async move {
                loop {
                    let (socket, _) = match listener.accept().await {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    let scripts = Arc::clone(&scripts);
                    let request_count = Arc::clone(&request_count);
                    tokio::spawn(async move {
                        serve_one(socket, scripts, request_count).await;
                    });
                }
            }
        });

        Self {
            base_url,
            request_count,
            handle,
        }
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::Acquire)
    }

    fn shutdown(&self) {
        self.handle.abort();
    }
}

fn response_sse(status: u16, frames: &[&str]) -> ScriptedResponse {
    ScriptedResponse::Respond {
        status,
        content_type: "text/event-stream",
        chunks: vec![ResponseChunk {
            delay_ms: 0,
            bytes: sse_frames(frames),
        }],
    }
}

fn response_json(status: u16, body: &str) -> ScriptedResponse {
    ScriptedResponse::Respond {
        status,
        content_type: "application/json",
        chunks: vec![ResponseChunk {
            delay_ms: 0,
            bytes: body.as_bytes().to_vec(),
        }],
    }
}

fn sse_frames(frames: &[&str]) -> Vec<u8> {
    let mut body = String::new();

    for frame in frames {
        body.push_str("data: ");
        body.push_str(frame);
        body.push_str("\n\n");
    }

    body.into_bytes()
}

#[tokio::test]
async fn stream_integration_successful_completion() {
    if !allow_local_integration() {
        return;
    }

    let server = ScriptedServer::new(vec![response_sse(
        200,
        &[
            r##"{"type":"response.output_text.delta","delta":"hello"}"##,
            r##"{"type":"response.completed","response":{"status":"completed"}}"##,
        ],
    )])
    .await;

    let request = CodexRequest::new("gpt-codex", json!("hi"), None);
    let config = CodexApiConfig::new("tok", "acct").with_base_url(&server.base_url);
    let client = CodexApiClient::new(config).expect("client");

    let result = client
        .stream(&request, None)
        .await
        .expect("stream should succeed");

    assert_eq!(result.terminal, Some(CodexResponseStatus::Completed));
    assert_eq!(result.events.len(), 2);
    assert!(matches!(
        result.events[0],
        CodexStreamEvent::OutputTextDelta { .. }
    ));

    server.shutdown();
}

#[tokio::test]
async fn stream_integration_done_alias_maps_to_completed_status() {
    if !allow_local_integration() {
        return;
    }

    let server = ScriptedServer::new(vec![response_sse(
        200,
        &[r##"{"type":"response.done","response":{"status":"in_progress"}}"##],
    )])
    .await;

    let request = CodexRequest::new("gpt-codex", json!("hi"), None);
    let config = CodexApiConfig::new("tok", "acct").with_base_url(&server.base_url);
    let client = CodexApiClient::new(config).expect("client");

    let result = client
        .stream(&request, None)
        .await
        .expect("stream should succeed");

    assert_eq!(result.terminal, Some(CodexResponseStatus::InProgress));
    assert_eq!(result.events.len(), 1);

    server.shutdown();
}

#[tokio::test]
async fn stream_integration_failed_and_error_events_fail_terminal() {
    if !allow_local_integration() {
        return;
    }

    let server = ScriptedServer::new(vec![response_sse(
        200,
        &[
            r##"{"type":"response.failed","response":{"error":{"message":"boom"}}}"##,
            r##"{"type":"error","code":"x","message":"bad"}"##,
        ],
    )])
    .await;

    let request = CodexRequest::new("gpt-codex", json!("hi"), None);
    let config = CodexApiConfig::new("tok", "acct").with_base_url(&server.base_url);
    let client = CodexApiClient::new(config).expect("client");

    let result = client
        .stream(&request, None)
        .await
        .expect("stream should succeed");

    assert_eq!(result.terminal, Some(CodexResponseStatus::Failed));
    assert!(result
        .events
        .iter()
        .any(|event| matches!(event, CodexStreamEvent::ResponseFailed { .. })));
    assert!(result
        .events
        .iter()
        .any(|event| matches!(event, CodexStreamEvent::Error { .. })));

    server.shutdown();
}

#[tokio::test]
async fn stream_integration_retryable_then_success() {
    if !allow_local_integration() {
        return;
    }

    let server = ScriptedServer::new(vec![
        response_json(503, r##"{"error":{"message":"overloaded"}}"##),
        response_sse(
            200,
            &[r##"{"type":"response.completed","response":{"status":"completed"}}"##],
        ),
    ])
    .await;

    let request = CodexRequest::new("gpt-codex", json!("hi"), None);
    let config = CodexApiConfig::new("tok", "acct").with_base_url(&server.base_url);
    let client = CodexApiClient::new(config).expect("client");

    let result = timeout(Duration::from_secs(12), client.stream(&request, None))
        .await
        .expect("retry path should be bounded")
        .expect("stream should eventually succeed");

    assert_eq!(result.terminal, Some(CodexResponseStatus::Completed));
    assert_eq!(server.request_count(), 2);

    server.shutdown();
}

#[tokio::test]
async fn stream_integration_non_retryable_status_fails_explicitly() {
    if !allow_local_integration() {
        return;
    }

    let server = ScriptedServer::new(vec![response_json(
        400,
        r##"{"error":{"message":"invalid request"}}"##,
    )])
    .await;

    let request = CodexRequest::new("gpt-codex", json!("hi"), None);
    let config = CodexApiConfig::new("tok", "acct").with_base_url(&server.base_url);
    let client = CodexApiClient::new(config).expect("client");

    let result = client
        .stream(&request, None)
        .await
        .expect_err("stream should fail");
    assert!(matches!(result, CodexApiError::Status(code, _) if code.as_u16() == 400));

    server.shutdown();
}

#[tokio::test]
async fn stream_integration_cancellation_during_stream() {
    if !allow_local_integration() {
        return;
    }

    let server = ScriptedServer::new(vec![ScriptedResponse::Respond {
        status: 200,
        content_type: "text/event-stream",
        chunks: vec![
            ResponseChunk {
                delay_ms: 0,
                bytes: sse_frames(&[r##"{"type":"response.output_text.delta","delta":"stream"}"##]),
            },
            ResponseChunk {
                delay_ms: 200,
                bytes: sse_frames(&[
                    r##"{"type":"response.completed","response":{"status":"completed"}}"##,
                ]),
            },
        ],
    }])
    .await;

    let request = CodexRequest::new("gpt-codex", json!("hi"), None);
    let config = CodexApiConfig::new("tok", "acct").with_base_url(&server.base_url);
    let client = Arc::new(CodexApiClient::new(config).expect("client"));

    let cancellation = Arc::new(AtomicBool::new(false));
    let stream_task = tokio::spawn({
        let client = Arc::clone(&client);
        let request = request.clone();
        let cancellation = Arc::clone(&cancellation);
        async move { client.stream(&request, Some(&cancellation)).await }
    });

    sleep(Duration::from_millis(120)).await;
    cancellation.store(true, Ordering::Release);

    let result = timeout(Duration::from_secs(5), stream_task)
        .await
        .expect("stream task should resolve")
        .expect("join handle should resolve")
        .expect_err("cancellation should abort stream");

    assert!(matches!(result, CodexApiError::Cancelled));
    server.shutdown();
}

#[tokio::test]
async fn stream_integration_connection_reset_then_retry_exhausted() {
    if !allow_local_integration() {
        return;
    }

    let server = ScriptedServer::new(vec![
        ScriptedResponse::Reset,
        ScriptedResponse::Reset,
        ScriptedResponse::Reset,
        ScriptedResponse::Reset,
    ])
    .await;

    let request = CodexRequest::new("gpt-codex", json!("hi"), None);
    let config = CodexApiConfig::new("tok", "acct").with_base_url(&server.base_url);
    let client = CodexApiClient::new(config).expect("client");

    let result = timeout(Duration::from_secs(20), client.stream(&request, None))
        .await
        .expect("retry path should resolve")
        .expect_err("connection reset should surface as failure");

    assert!(matches!(
        result,
        CodexApiError::RetryExhausted { status: None, .. }
    ));
    assert!(server.request_count() >= 4);

    server.shutdown();
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

async fn serve_one(
    mut socket: TcpStream,
    scripts: Arc<Vec<ScriptedResponse>>,
    request_count: Arc<AtomicUsize>,
) {
    if read_request_headers(&mut socket).await.is_err() {
        return;
    }

    let index = request_count.fetch_add(1, Ordering::AcqRel);
    let response = scripts
        .get(index)
        .cloned()
        .unwrap_or_else(|| response_json(500, r##"{"error":"unexpected request"}"##));

    match response {
        ScriptedResponse::Reset => {}
        ScriptedResponse::Respond {
            status,
            content_type,
            chunks,
        } => {
            let headers = format!(
                "HTTP/1.1 {status} {}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                status_reason(status),
                content_type,
            );

            if socket.write_all(headers.as_bytes()).await.is_err() {
                return;
            }

            for chunk in chunks {
                if chunk.delay_ms > 0 {
                    sleep(Duration::from_millis(chunk.delay_ms)).await;
                }
                let prefix = format!("{:X}\r\n", chunk.bytes.len());
                if socket.write_all(prefix.as_bytes()).await.is_err() {
                    return;
                }
                if socket.write_all(&chunk.bytes).await.is_err() {
                    return;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
            }

            let _ = socket.write_all(b"0\r\n\r\n").await;
            let _ = socket.shutdown().await;
        }
    }
}

async fn read_request_headers(socket: &mut TcpStream) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];

    loop {
        let n = socket.read(&mut buffer).await?;
        if n == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..n]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(());
        }
    }
}
