# Web authentication and browser security

This document describes the Web UI authentication and browser-security architecture introduced to support multiple Kiliax servers through different ports on the same browser host.

It covers the current implementation, wire protocols, security properties, migration impact, and verification procedures.

## Problem statement

The previous Web UI authentication flow stored the server bearer token in a fixed HTTP cookie:

```http
Set-Cookie: kiliax_token=<token>; Path=/; HttpOnly; SameSite=Strict
```

The browser cookie model does not isolate cookies by port. For example, these URLs share the same host cookie namespace:

```text
http://127.0.0.1:8123
http://127.0.0.1:8124
```

This caused a conflict when one port served a local Kiliax instance and another port forwarded a remote Kiliax instance:

1. Opening the remote URL stored the remote token in `kiliax_token`.
2. Opening the local URL replaced it with the local token.
3. Subsequent remote API requests sent the local token.
4. The remote server returned `401 Unauthorized`.

The old design also automatically sent the Kiliax cookie to unrelated HTTP services on other ports of the same host.

## Current architecture

Authentication is now split by transport:

| Surface | Authentication |
|---|---|
| Web UI static assets | Public |
| Swagger UI `/docs` shell | Public |
| `/v1` HTTP API, OpenAPI JSON/YAML, and SSE | `Authorization: Bearer <token>` |
| Session event WebSocket | First client message containing the token |

The Web UI stores the token in `sessionStorage`. Browser storage is isolated by Origin:

```text
Origin = scheme + host + port
```

Therefore, the following pages have independent token storage:

```text
http://127.0.0.1:8123
http://127.0.0.1:8124
https://127.0.0.1:8123
```

`sessionStorage` is also scoped to a browser tab. Refreshing the tab preserves the token, but closing it clears the token. A newly opened tab must use a Kiliax URL containing the token.

## Browser bootstrap flow

The CLI continues to print and open a URL in this form:

```text
http://127.0.0.1:8123/?token=<server-token>
```

On initial page load, the Web UI:

1. Reads the first `token` query parameter.
2. Trims and stores a non-empty value in `sessionStorage["kiliax_token"]`.
3. Removes `token` from the visible URL with `history.replaceState`.
4. Uses the stored token for API and WebSocket authentication.

If a URL contains a new non-empty token, it replaces the current tab's stored token.

The query parameter is only a bootstrap transport. It is never accepted by the server as API authentication and is removed from the address bar before normal operation.

Implementation:

- `web/src/lib/auth.ts`
- `web/src/app.tsx`

## HTTP API authentication

All protected `/v1` HTTP requests use:

```http
Authorization: Bearer <server-token>
```

The shared Web API client reads the current tab token and injects the header into every request.

The server authentication middleware applies these rules:

- Non-`/v1` routes are public.
- `/v1/sessions/{id}/events/ws` is allowed through to perform WebSocket first-message authentication.
- Every other `/v1` route requires an exact bearer-token match.
- Cookies and query parameters do not authenticate API requests.

This includes:

- REST APIs
- SSE event streams
- `/v1/openapi.json`
- `/v1/openapi.yaml`
- admin endpoints

The `/docs` Swagger UI shell is public, but API calls made from it still require a bearer token entered through Swagger's Authorize control.

Implementation:

- `web/src/lib/api.ts`
- `crates/kiliax-server/src/http/middleware.rs`

## WebSocket authentication protocol

Browsers cannot set an arbitrary `Authorization` header when constructing a native `WebSocket`. Kiliax therefore authenticates using the first client message.

### Client flow

After the socket opens, the Web UI immediately sends:

```json
{"type":"auth","token":"<server-token>"}
```

No token is placed in the WebSocket URL.

### Server flow

For a token-protected server, the event WebSocket:

1. Completes the HTTP WebSocket upgrade without accessing session data.
2. Waits up to five seconds for the first client message.
3. Requires the first message to be text JSON with exactly the `type` and `token` fields.
4. Requires `type` to equal `auth` and `token` to exactly match the configured server token.
5. Only after authentication loads the live session, reads backlog events, subscribes to events, and sends data.

Invalid JSON, unknown fields, incorrect message type, incorrect token, non-text first messages, and timeout all close the socket with:

```text
close code: 1008
reason: unauthorized
```

The Web UI treats any `1008` close as an authentication/policy failure. It clears the token, stops reconnecting, closes the current socket, and displays the Unauthorized screen.

Normal network closures retain the existing exponential reconnect behavior.

Implementation:

- `web/src/lib/use-ws-events.ts`
- `crates/kiliax-server/src/http/handlers/events.rs`

## Unauthorized behavior

The Web UI enters the Unauthorized state when:

- No bootstrap or stored token exists.
- Any HTTP API request returns `401`.
- The WebSocket closes with code `1008`.

Entering this state:

1. Removes `sessionStorage["kiliax_token"]`.
2. Closes the current WebSocket.
3. Stops authenticated session polling because no token remains.
4. Displays a prompt to reopen the URL printed by `kiliax server start`.

This prevents a stale or invalid token from causing repeated API and WebSocket authentication attempts.

## Browser security headers

The server adds these headers to responses:

```http
X-Content-Type-Options: nosniff
Referrer-Policy: no-referrer
Content-Security-Policy: ...
```

The enforced Content Security Policy is:

```text
default-src 'none';
script-src 'self';
style-src 'self' 'unsafe-inline';
img-src 'self' data: blob:;
font-src 'self' data:;
connect-src 'self' ws://<request-host> wss://<request-host>;
object-src 'none';
base-uri 'none';
frame-ancestors 'none';
form-action 'none'
```

Security effects:

- Scripts may only load from the current Kiliax Origin.
- API and WebSocket connections are limited to the current request host.
- Objects/plugins, base URL replacement, framing, and form submission are disabled.
- Referrer data is not sent to linked sites.
- MIME-type sniffing is disabled.

The request `Host` value is only added to CSP when it contains valid hostname/IP/port characters. Invalid values fall back to `connect-src 'self'`.

`style-src 'unsafe-inline'` remains necessary for the current UI libraries and generated Mermaid SVG styles. It permits inline CSS, not inline JavaScript.

Implementation:

- `crates/kiliax-server/src/http/middleware.rs`
- `crates/kiliax-server/src/http/mod.rs`

## Mermaid hardening

Mermaid was previously loaded at runtime from jsDelivr and its generated SVG was inserted into the page. This created an external-script dependency and a sensitive HTML/SVG insertion point.

The current implementation:

1. Bundles Mermaid into the Vite Web application.
2. Initializes Mermaid with `securityLevel: "strict"`.
3. Sanitizes generated SVG with DOMPurify's SVG profiles.
4. Explicitly forbids `script` and `foreignObject`.
5. Removes attributes beginning with `on`, such as `onload` and `onerror`.
6. Removes `href` and `xlink:href` values that do not reference an internal `#fragment`.
7. Rejects SVG that fails XML parsing.
8. Inserts only the sanitized SVG into the page.

This removes runtime CDN trust and reduces the risk of model-generated Mermaid content executing scripts or loading external resources.

Implementation:

- `web/src/components/code-block.tsx`
- `web/package.json`
- `web/bun.lock`

## Security model and residual risks

### Improvements

- Different ports no longer overwrite or receive each other's tokens.
- The token is not automatically attached to unrelated same-host services.
- API authentication uses a standard explicit bearer header.
- WebSocket session data is inaccessible before authentication.
- Tokens are removed from the visible URL after bootstrap.
- The token is cleared when the browser tab closes.
- CSP limits script execution and outbound connections.
- Mermaid no longer executes a runtime CDN script and sanitizes generated SVG.

### Residual risks

`sessionStorage` is readable by JavaScript running in the Kiliax Origin. A successful XSS vulnerability could read the token and invoke Kiliax APIs. CSP, React escaping, restricted Markdown links, and Mermaid sanitization reduce this risk but do not remove the need for secure rendering practices.

The bootstrap token may briefly appear in:

- browser history before `replaceState` executes
- browser automation traces
- upstream proxy logs
- screenshots or copied URLs

Kiliax access logs strip the `token` query parameter before logging request targets, but external proxies must be configured separately.

The server token remains a bearer credential. Anyone who obtains it has the configured server permissions until the server token changes.

## Compatibility and migration

This change intentionally does not preserve the old Cookie authentication interface.

After upgrading:

- Existing `kiliax_token` cookies are ignored.
- Existing tabs authenticated through the old cookie will receive Unauthorized.
- Users must reopen the URL printed by `ki` or `kiliax server start`.
- API clients must use the `Authorization` header.
- WebSocket clients must implement the first-message authentication protocol.

No server-side data migration is required.

## Remote and port-forwarded usage

With a remote server forwarded to local port `8124` and a local server on `8123`, open each server's printed URL:

```text
http://127.0.0.1:8123/?token=<local-token>
http://127.0.0.1:8124/?token=<remote-token>
```

The browser stores them independently because the Origins differ by port:

```text
http://127.0.0.1:8123 -> local token
http://127.0.0.1:8124 -> remote token
```

Opening or using one instance does not invalidate the other.

## Verification

Run the server tests:

```bash
cargo test -p kiliax-server
```

Run the Web build:

```bash
cd web
bun run build
```

Run Web UI E2E tests:

```bash
cd web
bun run test:e2e
```

The relevant test coverage verifies:

- Public Web UI and docs responses do not set cookies.
- Web responses contain CSP and security headers.
- Query tokens and cookies cannot authenticate `/v1`.
- Bearer tokens authenticate HTTP APIs.
- WebSocket auth messages require the exact type/token shape.
- URL tokens move into `sessionStorage` and disappear from the address bar.
- HTTP requests and WebSocket auth messages use the stored token.
- Unauthorized responses clear the stored token.
- Mermaid SVG output contains no scripts, `foreignObject`, event handlers, or external references.

## Operational troubleshooting

### Web UI shows Unauthorized after upgrade

Reopen the full URL printed by:

```bash
ki
# or
ki server start
```

An old tab or cookie cannot authenticate the new flow.

### HTTP API returns 401

Verify the bearer header:

```bash
curl \
  -H "Authorization: Bearer <server-token>" \
  http://127.0.0.1:8123/v1/capabilities
```

Using `?token=...` or a `kiliax_token` cookie will not authenticate the API.

### WebSocket immediately closes with 1008

Verify that the first client message is sent within five seconds and exactly matches:

```json
{"type":"auth","token":"<server-token>"}
```

Do not send subscription, resume, ping, or application messages before authentication.

### CSP blocks a new Web feature

Do not broadly weaken CSP. Identify the required resource type and Origin, then update the narrowest applicable directive. External runtime scripts should remain disallowed; dependencies should be bundled into the Web application.
