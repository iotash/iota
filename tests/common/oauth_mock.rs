//! A mock OAuth 2.1 authorization server WITH a protected MCP endpoint, on wiremock: what `iota mcp login`,
//! the runtime transport and the `/mcp` panel are pinned against (`tests/mcp/oauth.rs`, `tests/cmd/mcp.rs`,
//! the tmux scenario). One process-wide state shared by every endpoint, so a test can read back what the
//! server saw (which bearer token, which grant, what was revoked).
//!
//! The endpoints, all under one base URL `http://127.0.0.1:<port>`:
//!
//! | endpoint | behaviour |
//! |---|---|
//! | `GET /.well-known/oauth-protected-resource/mcp` | RFC 9728: the resource and this server as its AS |
//! | `GET /.well-known/oauth-authorization-server` | RFC 8414: the endpoints, S256, `code` |
//! | `POST /register` | RFC 7591: `client_id` `cid-1`, the redirect URIs echoed |
//! | `GET /authorize` | records the PKCE challenge, redirects (302) to `redirect_uri?code=…&state=…` |
//! | `POST /token` | `authorization_code` (S256 verified) → `at-1`/`rt-1`; `refresh_token` → the next pair |
//! | `POST /revoke` | RFC 7009: records the token |
//! | `POST /mcp` | the MCP server (initialize, tools/list with `echo`, tools/call) behind a bearer check |
//! | `GET /mcp` | 401 with the `WWW-Authenticate` challenge (unauthenticated), 405 (authenticated) |
//!
//! A token stops being accepted when it is revoked; an unknown or revoked refresh token is
//! `invalid_grant`.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers};

/// What the server has seen and issued.
#[derive(Debug, Default)]
pub struct State {
    /// Every bearer token presented to `POST /mcp`, in order.
    pub bearers: Vec<Option<String>>,
    /// Every `grant_type` presented to `/token`, in order.
    pub grants: Vec<String>,
    /// Tokens presented to `/revoke`.
    pub revoked: Vec<String>,
    /// Redirect URIs registered through `/register`.
    pub registered: Vec<String>,
    /// Access tokens the MCP endpoint currently accepts.
    valid_access: Vec<String>,
    /// Refresh tokens `/token` currently accepts.
    valid_refresh: Vec<String>,
    /// Codes issued by `/authorize` with their S256 challenge.
    codes: Vec<(String, String)>,
    /// The token pair counter (`at-N`/`rt-N`).
    issued: u32,
    /// The `expires_in` the token endpoint reports.
    pub expires_in: u64,
}

/// The running server.
pub struct OauthMock {
    pub server: MockServer,
    pub state: Arc<Mutex<State>>,
}

impl OauthMock {
    /// The base URL (`http://127.0.0.1:<port>`).
    pub fn base(&self) -> String {
        self.server.uri()
    }

    /// The MCP endpoint.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.server.uri())
    }

    /// A snapshot of the state.
    pub fn state(&self) -> State {
        let st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        State {
            bearers: st.bearers.clone(),
            grants: st.grants.clone(),
            revoked: st.revoked.clone(),
            registered: st.registered.clone(),
            valid_access: st.valid_access.clone(),
            valid_refresh: st.valid_refresh.clone(),
            codes: st.codes.clone(),
            issued: st.issued,
            expires_in: st.expires_in,
        }
    }

    /// Stops accepting `token` at the MCP endpoint (a server-side expiry / revocation).
    pub fn invalidate_access(&self, token: &str) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.valid_access.retain(|t| t != token);
    }

    /// Stops accepting `token` at the token endpoint.
    pub fn invalidate_refresh(&self, token: &str) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.valid_refresh.retain(|t| t != token);
    }
}

/// Starts the mock; `expires_in` is what its tokens claim.
pub async fn start(expires_in: u64) -> OauthMock {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(State {
        expires_in,
        ..State::default()
    }));
    let base = server.uri();
    let r = |f: fn(&Request, &str, &Arc<Mutex<State>>) -> ResponseTemplate| Responder {
        f,
        base: base.clone(),
        state: Arc::clone(&state),
    };
    Mock::given(matchers::method("GET"))
        .and(matchers::path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(r(resource_metadata))
        .mount(&server)
        .await;
    Mock::given(matchers::method("GET"))
        .and(matchers::path("/.well-known/oauth-authorization-server"))
        .respond_with(r(as_metadata))
        .mount(&server)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/register"))
        .respond_with(r(register))
        .mount(&server)
        .await;
    Mock::given(matchers::method("GET"))
        .and(matchers::path("/authorize"))
        .respond_with(r(authorize))
        .mount(&server)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/token"))
        .respond_with(r(token))
        .mount(&server)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/revoke"))
        .respond_with(r(revoke))
        .mount(&server)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/mcp"))
        .respond_with(r(mcp_post))
        .mount(&server)
        .await;
    Mock::given(matchers::method("GET"))
        .and(matchers::path("/mcp"))
        .respond_with(r(mcp_get))
        .mount(&server)
        .await;
    Mock::given(matchers::method("DELETE"))
        .and(matchers::path("/mcp"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    OauthMock { server, state }
}

/// One endpoint: a function over the request, the base URL and the shared state.
struct Responder {
    f: fn(&Request, &str, &Arc<Mutex<State>>) -> ResponseTemplate,
    base: String,
    state: Arc<Mutex<State>>,
}

impl wiremock::Respond for Responder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        (self.f)(request, &self.base, &self.state)
    }
}

fn lock(state: &Arc<Mutex<State>>) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn json(status: u16, body: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

fn resource_metadata(_: &Request, base: &str, _: &Arc<Mutex<State>>) -> ResponseTemplate {
    json(
        200,
        serde_json::json!({
            "resource": format!("{base}/mcp"),
            "authorization_servers": [base],
            "scopes_supported": ["mcp"],
            "bearer_methods_supported": ["header"],
        }),
    )
}

fn as_metadata(_: &Request, base: &str, _: &Arc<Mutex<State>>) -> ResponseTemplate {
    json(
        200,
        serde_json::json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "registration_endpoint": format!("{base}/register"),
            "revocation_endpoint": format!("{base}/revoke"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": ["mcp"],
        }),
    )
}

fn register(req: &Request, _: &str, state: &Arc<Mutex<State>>) -> ResponseTemplate {
    let body: serde_json::Value = req.body_json().unwrap_or_default();
    let uris: Vec<String> = body["redirect_uris"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    lock(state).registered.extend(uris.iter().cloned());
    json(
        201,
        serde_json::json!({
            "client_id": "cid-1",
            "client_name": body["client_name"],
            "redirect_uris": uris,
            "token_endpoint_auth_method": "none",
        }),
    )
}

/// The value of query parameter `key`.
fn query(req: &Request, key: &str) -> Option<String> {
    req.url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn authorize(req: &Request, _: &str, state: &Arc<Mutex<State>>) -> ResponseTemplate {
    let (Some(redirect), Some(st), Some(challenge)) = (
        query(req, "redirect_uri"),
        query(req, "state"),
        query(req, "code_challenge"),
    ) else {
        return ResponseTemplate::new(400)
            .set_body_string("missing redirect_uri/state/code_challenge");
    };
    if query(req, "code_challenge_method").as_deref() != Some("S256")
        || query(req, "response_type").as_deref() != Some("code")
        || query(req, "client_id").as_deref() != Some("cid-1")
    {
        return ResponseTemplate::new(400).set_body_string("bad authorize request");
    }
    let mut guard = lock(state);
    let code = format!("code-{}", guard.codes.len() + 1);
    guard.codes.push((code.clone(), challenge));
    let sep = if redirect.contains('?') { '&' } else { '?' };
    ResponseTemplate::new(302)
        .insert_header("location", format!("{redirect}{sep}code={code}&state={st}"))
}

/// A form body as pairs, percent-decoded.
fn form(req: &Request) -> Vec<(String, String)> {
    let body = String::from_utf8_lossy(&req.body).into_owned();
    url_form(&body)
}

fn url_form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent(k), percent(v))
        })
        .collect()
}

fn percent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'%'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn token(req: &Request, _: &str, state: &Arc<Mutex<State>>) -> ResponseTemplate {
    let form = form(req);
    let field = |k: &str| form.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
    let grant = field("grant_type").unwrap_or_default();
    let mut guard = lock(state);
    guard.grants.push(grant.clone());
    match grant.as_str() {
        "authorization_code" => {
            let (Some(code), Some(verifier)) = (field("code"), field("code_verifier")) else {
                return json(400, serde_json::json!({"error": "invalid_request"}));
            };
            let Some(at) = guard.codes.iter().position(|(c, _)| *c == code) else {
                return json(
                    400,
                    serde_json::json!({"error": "invalid_grant", "error_description": "unknown code"}),
                );
            };
            let (_, challenge) = guard.codes.remove(at);
            if s256(&verifier) != challenge {
                return json(
                    400,
                    serde_json::json!({"error": "invalid_grant", "error_description": "pkce mismatch"}),
                );
            }
        }
        "refresh_token" => {
            let Some(rt) = field("refresh_token") else {
                return json(400, serde_json::json!({"error": "invalid_request"}));
            };
            let Some(at) = guard.valid_refresh.iter().position(|t| *t == rt) else {
                return json(
                    400,
                    serde_json::json!({"error": "invalid_grant", "error_description": "refresh token rejected"}),
                );
            };
            guard.valid_refresh.remove(at);
        }
        _ => return json(400, serde_json::json!({"error": "unsupported_grant_type"})),
    }
    guard.issued += 1;
    let n = guard.issued;
    let access = format!("at-{n}");
    let refresh = format!("rt-{n}");
    guard.valid_access.push(access.clone());
    guard.valid_refresh.push(refresh.clone());
    let expires_in = guard.expires_in;
    json(
        200,
        serde_json::json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": expires_in,
            "refresh_token": refresh,
            "scope": "mcp",
        }),
    )
}

fn revoke(req: &Request, _: &str, state: &Arc<Mutex<State>>) -> ResponseTemplate {
    let form = form(req);
    let mut guard = lock(state);
    if let Some((_, token)) = form.iter().find(|(k, _)| k == "token") {
        guard.revoked.push(token.clone());
        guard.valid_refresh.retain(|t| t != token);
        guard.valid_access.retain(|t| t != token);
    }
    ResponseTemplate::new(200)
}

/// The bearer token of a request, when it carries one.
fn bearer(req: &Request) -> Option<String> {
    req.headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
}

fn challenge(base: &str) -> ResponseTemplate {
    ResponseTemplate::new(401).insert_header(
        "www-authenticate",
        format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""),
    )
}

fn mcp_get(req: &Request, base: &str, state: &Arc<Mutex<State>>) -> ResponseTemplate {
    let token = bearer(req);
    let ok = token
        .as_ref()
        .is_some_and(|t| lock(state).valid_access.contains(t));
    if ok {
        ResponseTemplate::new(405)
    } else {
        challenge(base)
    }
}

fn mcp_post(req: &Request, base: &str, state: &Arc<Mutex<State>>) -> ResponseTemplate {
    let token = bearer(req);
    let ok = {
        let mut guard = lock(state);
        guard.bearers.push(token.clone());
        token
            .as_ref()
            .is_some_and(|t| guard.valid_access.contains(t))
    };
    if !ok {
        return challenge(base);
    }
    let msg: serde_json::Value = req.body_json().unwrap_or_default();
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or_default();
    if id.is_null() {
        // A notification (`notifications/initialized`): accepted, no body.
        return ResponseTemplate::new(202);
    }
    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "oauth-mock", "version": "1.0.0"},
        }),
        "tools/list" => serde_json::json!({
            "tools": [{"name": "echo", "description": "says pong", "inputSchema": {"type": "object"}}],
        }),
        "tools/call" => serde_json::json!({
            "content": [{"type": "text", "text": format!("pong via {}", token.unwrap_or_default())}],
            "isError": false,
        }),
        _ => {
            return json(
                200,
                serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}),
            );
        }
    };
    json(
        200,
        serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
    )
}
