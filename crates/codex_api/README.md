# codex_api

Transport-focused crate for ChatGPT Codex API HTTP + SSE parity work.

- Scope: request URL/header/payload/retry/error/stream transport behavior only.
- No login flow implementation (PKCE/callback/device/device auth are out of scope).
- Parity target: PI (`pi-mono`) and OpenCode Codex transport references.

## Adapter Contract

- Create with `CodexApiConfig::new(token)` and optional chainable modifiers.
- Token payload must include `https://api.openai.com/auth.chatgpt_account_id` for request authorization context.
- Configure and send requests through `CodexApiClient::build_request`.
- Execute request/retry behavior with `CodexApiClient::send_with_retry`.
- Consume SSE streams incrementally with `CodexApiClient::stream_with_handler`,
  which invokes the callback in parser order for non-failure
  `CodexStreamEvent` values and returns an optional terminal status at EOF.
  Stream-failure events (`response.failed`/`error`) are surfaced as
  `CodexApiError::StreamFailed` and short-circuit the stream call.
- `CodexApiClient::stream` remains available as a compatibility collector around
  `stream_with_handler`, returning buffered events plus
  `StreamResult::terminal` (absent when stream ends without a known terminal
  state).
- Function-call output items are normalized into ordered event pairs:
  - `CodexStreamEvent::OutputItemDone` for raw item completion metadata
  - `CodexStreamEvent::ToolCallRequested` for host-mediated tool execution payloads
- Tool-call payloads are never silently repaired by the parser; malformed
  `call_id`/`tool_name`/`arguments` content is preserved for explicit adapter-level
  failure handling.
- Consumers implementing host-mediated tool loops should only execute pending
  tool calls when terminal status is `completed`; non-complete or missing
  terminal states remain explicit failure conditions.
- Unknown typed SSE frames are retained as `CodexStreamEvent::Unknown`.
- Retry policy is bounded by `MAX_RETRIES` with exponential backoff and PI-parity
  retry behavior for retryable HTTP/transient failures.
- Cancellation is explicit: pass `Some(&CancellationSignal)` to
  `stream_with_handler`/`stream` and `send_with_retry`; if set, the call
  returns `CodexApiError::Cancelled`.
- Error taxonomy is represented by `CodexApiError` and includes parsed error
  payloads, retry exhaustion, SSE parse failures, and cancellation.
