# BYOT (Bring Your Own Type) compatibility layer

Kiliax targets **OpenAI-compatible** APIs, but many providers deviate from the canonical schema (especially for tool-calls, reasoning/thinking fields, and streaming usage). To keep the runtime stable across providers, `kiliax-core` implements a small **BYOT-style** parsing layer: build a mostly-standard request, then deserialize responses/stream chunks into tolerant structs that accept provider-specific fields.

This document describes what BYOT means in Kiliax and what is currently supported.

## What “BYOT” means here

Kiliax depends on `async-openai` and enables its `byot` feature flag, but **Kiliax does not call** `async-openai`’s `*_byot(...)` endpoints directly.

Instead, Kiliax:

1. Uses `async-openai` types/builders to assemble a canonical OpenAI Chat Completions request.
2. Serializes that request to JSON.
3. Optionally patches the JSON (provider-specific fields).
4. Sends the request with `reqwest` (and `reqwest-eventsource` for streaming).
5. Deserializes responses/stream chunks into Kiliax’s own “BYOT” structs that are more tolerant to schema differences.

Code: `crates/kiliax-core/src/llm.rs`.

## Why it exists (problems it solves)

- **Provider schema drift**: some providers return `thinking` or `reasoning` instead of `reasoning_content`, or support multiple at once.
- **Tool-calls shape differences**: some providers still emit legacy `function_call` instead of `tool_calls`.
- **Streaming usage**: OpenAI can emit token usage only when `stream_options.include_usage=true`, while other providers may omit/rename fields.
- **Error envelopes**: non-2xx errors may or may not follow the canonical `{ "error": { ... } }` shape.

## Compatibility (currently implemented)

### Response parsing (`POST /chat/completions`)

Kiliax’s BYOT response structs accept:

- `choices[0].message.content` (standard)
- `choices[0].message.tool_calls` (standard-ish)
- legacy `choices[0].message.function_call` (fallback)
- optional reasoning fields on the assistant message:
  - `reasoning_content`
  - `thinking`
  - `reasoning`
- optional `usage` (token usage) at the top level

Notes:

- Only **the first choice** (`choices[0]`) is consumed.
- Tool calls are normalized into Kiliax’s internal `ToolCall { id, name, arguments }`.

Code: `chat_response_from_byot(...)` in `crates/kiliax-core/src/llm.rs`.

### Streaming parsing (`POST /chat/completions` with SSE)

Kiliax’s BYOT stream chunk structs accept:

- `choices[0].delta.content`
- reasoning/thinking deltas from any of:
  - `choices[0].delta.reasoning_content`
  - `choices[0].delta.thinking`
  - `choices[0].delta.reasoning`
- tool-call deltas from either:
  - `choices[0].delta.tool_calls[]` (preferred)
  - `choices[0].delta.function_call` (fallback)
- optional `usage` at the top level (forwarded to the runtime; typically present on the final chunk)

Notes:

- Only **the first choice** (`choices[0]`) is consumed.
- Tool-call deltas are normalized into Kiliax’s internal `ToolCallDelta { index, id, name, arguments }`.

Code: `chat_stream_chunk_from_byot(...)` in `crates/kiliax-core/src/llm.rs`.

### Request-side compatibility patches

After building the canonical request JSON, Kiliax applies a few provider-specific adjustments:

- `prompt_cache_key` (top-level field): injected when configured by the runtime (useful for providers that support prompt caching).
- Moonshot-specific tool-call reasoning passthrough:
  - when the assistant message contains tool-calls, Kiliax injects `reasoning_content` into the outbound message JSON to preserve provider expectations.
- OpenAI streaming usage:
  - when `provider == "openai"` and streaming is enabled, Kiliax injects `stream_options.include_usage=true`.

Code: `inject_prompt_cache_fields(...)`, `inject_reasoning_content_for_tool_calls(...)`, and the `stream_options` block in `chat_stream(...)` (`crates/kiliax-core/src/llm.rs`).

### Error compatibility

For non-2xx responses, Kiliax:

- reads up to 16KiB of response body
- attempts to parse the canonical OpenAI-style `{ "error": { ... } }`
- otherwise falls back to treating the body as plain text and wraps it as an API error

Code: `map_api_error_response(...)` in `crates/kiliax-core/src/llm.rs`.

## Non-goals / current limitations

- Multi-choice handling: BYOT currently ignores `choices[1..]`.
- Non-chat endpoints (embeddings, images, audio, etc.) are not implemented via Kiliax’s BYOT layer today.
- Provider-specific fields outside the set above are ignored unless explicitly added to the BYOT structs.

