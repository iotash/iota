//! MCP OAuth 2.1 (brain page `mcp-cli-and-oauth`): the file token store, the login and logout flows, and the
//! runtime manager the streamable-HTTP transport is wrapped in for an `auth: oauth` server.
//!
//! The protocol is rmcp's `auth` module (discovery per RFC 9728 → RFC 8414, dynamic registration per RFC
//! 7591, the PKCE authorization code grant, the refreshing `AuthClient`); what is iota's here is where the
//! tokens live and how the browser round trip is driven:
//!
//! - `~/.iota/mcp/auth/<name>.json`, mode 0600, written atomically — one file per server, holding rmcp's
//!   `StoredCredentials` beside the endpoint and the authorization-server metadata it was issued under, so a
//!   run refreshes a token without a discovery round trip. A token never enters a config file.
//! - `login`: discovery → registration (or the stored client id when the server registers nobody) → the
//!   authorization URL → the browser (`$BROWSER`, else the platform opener; printed either way) → a loopback
//!   listener on `127.0.0.1:<random>/callback` collects the code, a pasted redirect URL is the fallback, five
//!   minutes is the deadline → the code is exchanged and the store written.
//! - `logout`: the file is removed; the token is revoked when the server publishes a revocation endpoint,
//!   and a failure there is not an error — the file is gone either way.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationMetadata, AuthorizationMetadataSource,
    AuthorizationRequest, AuthorizationSession, CredentialStore, StoredCredentials,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

use crate::app::HostDirs;
use crate::app::env::Env;

/// The directory under `~/.iota` that holds one token file per server.
pub const AUTH_DIR: &str = "mcp/auth";

/// How long `login` waits for the browser to come back.
pub const LOGIN_TIMEOUT: Duration = Duration::from_mins(5);

/// The `client_name` sent with a dynamic registration.
const CLIENT_NAME: &str = "iota";

/// The path the loopback listener answers.
const CALLBACK_PATH: &str = "/callback";

/// Mode of a token file: the owner alone.
const TOKEN_FILE_MODE: u32 = 0o600;

/// The `McpError::Connect`-level text of a server whose store holds no usable token; the CLI hint is part of
/// it so every outlet (headless stderr, the REPL notice, `/mcp`) says what to do.
pub fn not_logged_in(name: &str) -> String {
    format!("not logged in: run iota mcp login {name}")
}

// ---------------------------------------------------------------- the file

/// What `~/.iota/mcp/auth/<name>.json` holds.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TokenFile {
    /// The MCP endpoint the tokens were issued for (as expanded at login).
    pub url: String,
    /// The authorization server's metadata at login: the token endpoint a refresh posts to, the revocation
    /// endpoint a logout uses, the issuer the credentials are bound to.
    pub metadata: AuthorizationMetadata,
    /// rmcp's own record: client id, the token response, the scopes, when it was received.
    pub credentials: StoredCredentials,
}

/// Where one server's tokens stand, as `iota mcp list` and `/mcp` say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginState {
    /// No file, or a file with no token in it.
    NotLoggedIn,
    /// A token, and a refresh token to renew it with — or one that is still valid.
    LoggedIn,
    /// An access token past its lifetime with nothing to refresh it.
    Expired,
}

impl LoginState {
    /// The words the listings print.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotLoggedIn => "not logged in",
            Self::LoggedIn => "logged in",
            Self::Expired => "expired",
        }
    }
}

/// One server's token file. `Clone` because rmcp's manager takes the store by value and the flow keeps its
/// own handle to read the outcome back.
#[derive(Clone, Debug)]
pub struct TokenStore {
    path: PathBuf,
    /// What a `save` from rmcp — which hands over the credentials alone — is written beside.
    url: String,
    metadata: Arc<std::sync::Mutex<Option<AuthorizationMetadata>>>,
}

impl TokenStore {
    /// `<home>/.iota/mcp/auth/<name>.json`; `None` without a home directory.
    pub fn for_server(dirs: &HostDirs, name: &str, url: &str) -> Option<Self> {
        let path = dirs.app_home()?.join(AUTH_DIR).join(format!("{name}.json"));
        Some(Self {
            path,
            url: url.to_owned(),
            metadata: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    /// The file's path (for the messages that name it).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file, decoded; `None` when it does not exist.
    pub fn load(&self) -> std::io::Result<Option<TokenFile>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let file: TokenFile = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        // A refresh writes the credentials alone; the metadata it posts to is the file's.
        *crate::sync::lock(&self.metadata) = Some(file.metadata.clone());
        Ok(Some(file))
    }

    /// Writes the file (mode 0600, atomically).
    pub fn save(&self, file: &TokenFile) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
        crate::app::fs::write_atomic(&self.path, &bytes, Some(TOKEN_FILE_MODE))
    }

    /// Removes the file; a file that is not there is not an error.
    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Where the tokens stand, read off the file: the shape rmcp serialises (`token_response.expires_in`,
    /// `token_received_at`, `token_response.refresh_token`).
    pub fn status(&self) -> LoginState {
        let Ok(Some(file)) = self.load() else {
            return LoginState::NotLoggedIn;
        };
        login_state(&file.credentials)
    }

    /// Remembers the metadata a later `save` from rmcp is written beside.
    fn set_metadata(&self, metadata: AuthorizationMetadata) {
        *crate::sync::lock(&self.metadata) = Some(metadata);
    }
}

/// [`LoginState`] of a credentials record: a refresh token keeps a session alive across expiry; without one
/// the access token's own lifetime decides.
fn login_state(credentials: &StoredCredentials) -> LoginState {
    let Ok(value) = serde_json::to_value(credentials) else {
        return LoginState::NotLoggedIn;
    };
    let Some(token) = value.get("token_response").filter(|t| !t.is_null()) else {
        return LoginState::NotLoggedIn;
    };
    if token.get("refresh_token").is_some_and(|r| !r.is_null()) {
        return LoginState::LoggedIn;
    }
    let expires_in = token.get("expires_in").and_then(serde_json::Value::as_u64);
    let received_at = value
        .get("token_received_at")
        .and_then(serde_json::Value::as_u64);
    match (expires_in, received_at) {
        (Some(expires_in), Some(received_at)) if now_epoch_secs() >= received_at + expires_in => {
            LoginState::Expired
        }
        _ => LoginState::LoggedIn,
    }
}

/// Seconds the access token has left, when the record says.
pub fn expires_in(credentials: &StoredCredentials) -> Option<Duration> {
    let value = serde_json::to_value(credentials).ok()?;
    let expires_in = value.get("token_response")?.get("expires_in")?.as_u64()?;
    let received_at = value.get("token_received_at")?.as_u64()?;
    Some(Duration::from_secs(
        (received_at + expires_in).saturating_sub(now_epoch_secs()),
    ))
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The desugared `#[async_trait]` shape rmcp declares the trait in (the crate that writes it for you is not
/// a dependency of this one, and three methods do not justify it).
type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AuthError>> + Send + 'a>>;

impl CredentialStore for TokenStore {
    fn load<'life0, 'async_trait>(
        &'life0 self,
    ) -> StoreFuture<'async_trait, Option<StoredCredentials>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            TokenStore::load(self)
                .map(|file| file.map(|f| f.credentials))
                .map_err(|e| AuthError::InternalError(format!("token store: {e}")))
        })
    }

    fn save<'life0, 'async_trait>(
        &'life0 self,
        credentials: StoredCredentials,
    ) -> StoreFuture<'async_trait, ()>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let metadata = crate::sync::lock(&self.metadata).clone().ok_or_else(|| {
                AuthError::InternalError(
                    "token store: no authorization metadata to save under".to_owned(),
                )
            })?;
            TokenStore::save(
                self,
                &TokenFile {
                    url: self.url.clone(),
                    metadata,
                    credentials,
                },
            )
            .map_err(|e| AuthError::InternalError(format!("token store: {e}")))
        })
    }

    fn clear<'life0, 'async_trait>(&'life0 self) -> StoreFuture<'async_trait, ()>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            TokenStore::clear(self)
                .map_err(|e| AuthError::InternalError(format!("token store: {e}")))
        })
    }
}

// ---------------------------------------------------------------- the runtime manager

/// The manager a run wraps its transport in: the cached metadata and the stored client id, no network until
/// the first request. `Ok(None)` = nothing stored (the caller reports "not logged in").
pub async fn runtime_manager(
    http: reqwest::Client,
    store: TokenStore,
) -> Result<Option<AuthorizationManager>, AuthError> {
    let Some(file) = store
        .load()
        .map_err(|e| AuthError::InternalError(format!("token store: {e}")))?
    else {
        return Ok(None);
    };
    let mut manager = AuthorizationManager::new(file.url.as_str()).await?;
    manager.with_client(http)?;
    manager.set_metadata(file.metadata);
    manager.set_credential_store(store);
    if !manager.initialize_from_store().await? {
        return Ok(None);
    }
    Ok(Some(manager))
}

// ---------------------------------------------------------------- login

/// How the authorization URL reaches the user.
#[derive(Clone, Debug)]
pub enum Browser {
    /// `$BROWSER` when set, else the platform's opener (`open` / `xdg-open` / `start`).
    Open(Env),
    /// Print the URL only (`--no-browser`, or a build that knows it has no desktop).
    Print,
}

/// What `login` says as it goes; the caller prints it where its user looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginStep {
    /// The authorization URL, for the user to open (or to copy when the browser cannot be opened).
    AuthUrl(String),
    /// The browser could not be started: the text, so the user opens the URL by hand.
    BrowserFailed(String),
    /// Waiting on the loopback listener (and on a pasted redirect URL when the caller offers one).
    Waiting {
        /// Whether the caller reads a pasted URL too.
        paste: bool,
    },
}

/// What a login needs.
pub struct LoginRequest<'a> {
    /// The server name (the token file's, and the messages').
    pub name: &'a str,
    /// The MCP endpoint, expanded.
    pub url: &'a str,
    /// The run's HTTP client (proxy, TLS).
    pub http: reqwest::Client,
    /// Where the tokens go.
    pub store: TokenStore,
    /// How the URL reaches the user.
    pub browser: Browser,
    /// A pasted redirect URL, when the caller can read one (`None` = the listener alone).
    pub paste: Option<Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>>,
    /// Cancels the wait.
    pub cancel: &'a CancellationToken,
}

/// A finished login.
#[derive(Debug)]
pub struct LoginOutcome {
    /// The access token's lifetime, when the server said.
    pub expires_in: Option<Duration>,
    /// The token file.
    pub path: PathBuf,
}

/// Why a login did not finish.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The server published neither protected-resource nor authorization-server metadata.
    #[error(
        "{0} publishes no OAuth metadata (no protected-resource or authorization-server document)"
    )]
    NoMetadata(String),
    /// No registration endpoint and no stored client id to fall back on.
    #[error(
        "the authorization server offers no dynamic client registration and no client id is stored"
    )]
    NoRegistration,
    /// The browser did not come back in time.
    #[error("no callback within {}", crate::text::go_duration(*.0))]
    Timeout(Duration),
    /// The wait was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The callback carried an `error` instead of a code.
    #[error("the authorization server refused: {0}")]
    Refused(String),
    /// The loopback listener could not be bound or read.
    #[error("loopback listener: {0}")]
    Listener(#[from] std::io::Error),
    /// rmcp's own failure text (discovery, registration, the exchange).
    #[error("{0}")]
    Auth(#[from] AuthError),
}

/// The whole flow (module doc). The store is written by the exchange; the outcome reads it back.
pub async fn login(
    req: LoginRequest<'_>,
    report: &mut dyn FnMut(LoginStep),
) -> Result<LoginOutcome, LoginError> {
    let LoginRequest {
        name,
        url,
        http,
        store,
        browser,
        paste,
        cancel,
    } = req;
    let mut manager = AuthorizationManager::new(url).await?;
    manager.with_client(http)?;
    manager.set_credential_store(store.clone());
    let resolution = manager.resolve_metadata().await?;
    if resolution.source == AuthorizationMetadataSource::LegacyEndpointFallback {
        return Err(LoginError::NoMetadata(url.to_owned()));
    }
    let metadata = resolution.metadata;
    store.set_metadata(metadata.clone());
    manager.set_metadata(metadata.clone());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let mut request = AuthorizationRequest::new(redirect_uri).with_client_name(CLIENT_NAME);
    if metadata.registration_endpoint.is_none() {
        // Nobody registers here: the client id a previous login stored (or the server's operator gave) is
        // the only one there is.
        let stored = store
            .load()
            .map_err(|e| AuthError::InternalError(format!("token store: {e}")))?;
        match stored {
            Some(file) => request = request.with_preregistered_client(file.credentials.client_id),
            None => return Err(LoginError::NoRegistration),
        }
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, e)| e)?;
    let auth_url = session.get_authorization_url().to_owned();
    report(LoginStep::AuthUrl(auth_url.clone()));
    if let Browser::Open(env) = &browser
        && let Err(e) = open_url(env, &auth_url)
    {
        report(LoginStep::BrowserFailed(e));
    }
    report(LoginStep::Waiting {
        paste: paste.is_some(),
    });

    let callback = wait_for_callback(&listener, port, name, paste, cancel).await?;
    session.handle_callback_url(&callback).await?;
    let expires_in = store
        .load()
        .ok()
        .flatten()
        .and_then(|f| expires_in(&f.credentials));
    Ok(LoginOutcome {
        expires_in,
        path: store.path().to_path_buf(),
    })
}

/// The line a [`LoginStep`] prints, the same in the CLI and the REPL; `None` for a step that says nothing on
/// its own (the wait is announced by the caller, which knows whether it reads a pasted URL).
pub fn step_line(step: &LoginStep) -> Option<String> {
    match step {
        LoginStep::AuthUrl(url) => Some(format!("Open this URL to log in:\n  {url}")),
        LoginStep::BrowserFailed(e) => Some(format!("(the browser could not be started: {e})")),
        LoginStep::Waiting { .. } => None,
    }
}

/// The line a finished login prints.
pub fn logged_in_line(name: &str, outcome: &LoginOutcome) -> String {
    let lifetime = outcome
        .expires_in
        .map(|d| format!("; the token expires in {}", crate::text::go_duration(d)))
        .unwrap_or_default();
    format!(
        "Logged in to {name}{lifetime} (saved to {})",
        outcome.path.display()
    )
}

/// The redirect URL, from whichever source answers first: the loopback listener, the pasted line, the deadline,
/// the cancel token.
async fn wait_for_callback(
    listener: &tokio::net::TcpListener,
    port: u16,
    name: &str,
    paste: Option<Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>>,
    cancel: &CancellationToken,
) -> Result<String, LoginError> {
    let deadline = tokio::time::sleep(LOGIN_TIMEOUT);
    tokio::pin!(deadline);
    // A source that yields `None` (stdin at EOF) is out of the race, not the end of it.
    let mut paste = paste;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if let Some(url) = serve_callback(stream, port, name).await? {
                    return Ok(url);
                }
            }
            pasted = async { match paste.as_mut() { Some(p) => p.await, None => std::future::pending().await } } => {
                match pasted {
                    Some(line) if !line.trim().is_empty() => return Ok(line.trim().to_owned()),
                    _ => paste = None,
                }
            }
            () = &mut deadline => return Err(LoginError::Timeout(LOGIN_TIMEOUT)),
            () = cancel.cancelled() => return Err(LoginError::Cancelled),
        }
    }
}

/// One connection on the loopback listener: the request line's target is the redirect; anything but
/// `/callback` (a favicon probe) is answered 404 and ignored. Returns the full redirect URL to hand to rmcp,
/// or `Err(Refused)` when the server sent `error=` instead of a code.
async fn serve_callback(
    mut stream: tokio::net::TcpStream,
    port: u16,
    name: &str,
) -> Result<Option<String>, LoginError> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    // The head alone: a browser's GET is well under the cap, and a client that sends more is not one.
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    if path != CALLBACK_PATH {
        respond(&mut stream, "404 Not Found", "Not found.").await;
        return Ok(None);
    }
    if let Some(error) = query_param(query, "error") {
        let description = query_param(query, "error_description").unwrap_or_default();
        let text = if description.is_empty() {
            error
        } else {
            format!("{error}: {description}")
        };
        respond(
            &mut stream,
            "400 Bad Request",
            &format!("Login failed: {text}"),
        )
        .await;
        return Err(LoginError::Refused(text));
    }
    respond(
        &mut stream,
        "200 OK",
        &format!("Logged in to {name}. You can close this window."),
    )
    .await;
    Ok(Some(format!("http://127.0.0.1:{port}{target}")))
}

/// One HTML page, then the connection closes.
async fn respond(stream: &mut tokio::net::TcpStream, status: &str, text: &str) {
    let body = format!("<!doctype html><title>iota</title><p>{text}</p>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The decoded value of `key` in a query string.
fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

/// `%XX` and `+` of an `application/x-www-form-urlencoded` value.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = &value[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Starts the user's browser on `url`: `$BROWSER` (a command line; the URL is appended) when set, else the
/// platform opener. The child is detached; only a spawn failure is reported.
pub fn open_url(env: &Env, url: &str) -> Result<(), String> {
    let (program, args): (String, Vec<String>) =
        match env.var("BROWSER").filter(|b| !b.trim().is_empty()) {
            Some(browser) => {
                let mut parts = browser.split_whitespace().map(str::to_owned);
                let program = parts.next().unwrap_or_default();
                (program, parts.collect())
            }
            None if cfg!(target_os = "macos") => ("open".to_owned(), Vec::new()),
            None if cfg!(windows) => (
                "cmd".to_owned(),
                vec!["/C".to_owned(), "start".to_owned(), String::new()],
            ),
            None => ("xdg-open".to_owned(), Vec::new()),
        };
    std::process::Command::new(&program)
        .args(&args)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {program}: {e}"))
}

// ---------------------------------------------------------------- logout

/// Forgets the tokens: revokes the refresh token (else the access token) when the server publishes a
/// revocation endpoint — a failure there is ignored — and removes the file. `Ok(false)` = there was no file.
pub async fn logout(http: &reqwest::Client, store: &TokenStore) -> std::io::Result<bool> {
    let Some(file) = store.load()? else {
        return Ok(false);
    };
    if let Some(endpoint) = file
        .metadata
        .additional_fields
        .get("revocation_endpoint")
        .and_then(serde_json::Value::as_str)
        && let Some((token, hint)) = revocable_token(&file.credentials)
    {
        let body = form_encode(&[
            ("token", token.as_str()),
            ("token_type_hint", hint),
            ("client_id", file.credentials.client_id.as_str()),
        ]);
        let _ = http
            .post(endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .timeout(Duration::from_secs(10))
            .send()
            .await;
    }
    store.clear()?;
    Ok(true)
}

/// `application/x-www-form-urlencoded` of `pairs` (RFC 7009 takes a form body; reqwest's own encoder is a
/// feature this crate does not enable).
fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        form_escape(&mut out, k);
        out.push('=');
        form_escape(&mut out, v);
    }
    out
}

/// Unreserved characters as they are, a space as `+`, everything else `%XX`.
fn form_escape(out: &mut String, value: &str) {
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
}

/// The refresh token (which revokes the grant) when there is one, else the access token, with its hint.
fn revocable_token(credentials: &StoredCredentials) -> Option<(String, &'static str)> {
    let value = serde_json::to_value(credentials).ok()?;
    let token = value.get("token_response")?;
    if let Some(refresh) = token
        .get("refresh_token")
        .and_then(serde_json::Value::as_str)
    {
        return Some((refresh.to_owned(), "refresh_token"));
    }
    token
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .map(|access| (access.to_owned(), "access_token"))
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, query_param};

    #[test]
    fn query_params_decode_the_form_encoding() {
        assert_eq!(
            super::form_encode(&[("token", "a b/c~"), ("hint", "x")]),
            "token=a+b%2Fc~&hint=x"
        );
        assert_eq!(
            query_param("code=a%2Fb+c&state=s", "code").as_deref(),
            Some("a/b c")
        );
        assert_eq!(query_param("code=x&state=s", "state").as_deref(), Some("s"));
        assert_eq!(query_param("code=x", "error"), None);
        assert_eq!(query_param("error", "error").as_deref(), Some(""));
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
