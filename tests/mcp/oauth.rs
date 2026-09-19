//! The OAuth 2.1 chain against the mock authorization server (`tests/common/oauth_mock.rs`): login → the
//! token file → a connect that carries the bearer → a refresh once the token has aged → a refresh the server
//! forces with a 401 → logout (revocation) → the "not logged in" degradation of that ONE server.

use std::time::Duration;

use iota::app::env::Env;
use iota::mcp::auth::{Browser, CLIENT_METADATA_URL, LoginRequest, LoginStep, TokenStore, login};
#[cfg(unix)]
use iota::mcp::auth::{LoginState, logout};
use iota::mcp::config::{AuthMode, ServerConfig};
use iota::mcp::{Manager, ManagerOptions};
#[cfg(unix)]
use iota::provider::model::JsonObject;
#[cfg(unix)]
use iota::tool::Dispatcher;
#[cfg(unix)]
use iota::tool::context::RunCtx;
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

use crate::common_oauth as oauth_mock;
use crate::common_project::temp_project;

/// The config of the OAuth server under test.
fn oauth_server(url: &str) -> ServerConfig {
    ServerConfig {
        name: "nb".to_owned(),
        url: url.to_owned(),
        auth: AuthMode::Oauth,
        ..ServerConfig::default()
    }
}

/// A `sh` MCP server beside it, to show the degradation is per server.
#[cfg(unix)]
fn plain_server() -> ServerConfig {
    ServerConfig {
        name: "plain".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), crate::manager::SH_SERVER.to_owned()],
        ..ServerConfig::default()
    }
}

/// Runs the login flow with a "browser" that follows the authorization URL to the loopback callback.
async fn login_via_redirect(mock: &oauth_mock::OauthMock, store: &TokenStore) -> Duration {
    let (outcome, _) = login_as(mock, store, None, None, None)
        .await
        .expect("login");
    outcome.expires_in.expect("the mock says how long")
}

/// The flow with the client identity given (or not), through the same "browser"; the steps come back with
/// the outcome, or the error when the flow did not reach the browser.
async fn login_as(
    mock: &oauth_mock::OauthMock,
    store: &TokenStore,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    redirect_port: Option<u16>,
) -> Result<(iota::mcp::auth::LoginOutcome, Vec<LoginStep>), iota::mcp::auth::LoginError> {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let browser = tokio::spawn(async move {
        let url = rx.await.expect("the auth url");
        reqwest::Client::new()
            .get(url)
            .send()
            .await
            .expect("follow the redirect")
            .text()
            .await
            .expect("the callback page")
    });
    let mut tx = Some(tx);
    let mut steps = Vec::new();
    let outcome = login(
        LoginRequest {
            name: "nb",
            url: &mock.mcp_url(),
            http: reqwest::Client::new(),
            store: store.clone(),
            browser: Browser::Print,
            paste: None,
            cancel: &CancellationToken::new(),
            client_id: client_id.map(str::to_owned),
            client_secret: client_secret.map(str::to_owned),
            redirect_port,
        },
        &mut |step| {
            if let LoginStep::AuthUrl(url) = &step
                && let Some(tx) = tx.take()
            {
                let _ = tx.send(url.clone());
            }
            steps.push(step);
        },
    )
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            browser.abort();
            return Err(e);
        }
    };
    let page = browser.await.expect("browser task");
    assert!(
        page.contains("Logged in to nb. You can close this window."),
        "{page}"
    );
    assert!(
        matches!(
            steps.as_slice(),
            [
                LoginStep::Redirect(_),
                LoginStep::Identity { .. },
                LoginStep::AuthUrl(_),
                LoginStep::Waiting { paste: false }
            ]
        ),
        "{steps:?}"
    );
    assert_eq!(outcome.path, store.path());
    Ok((outcome, steps))
}

/// The token file, as JSON.
#[cfg(unix)]
fn token_json(store: &TokenStore) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(store.path()).expect("the token file")).expect("json")
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_stores_connects_refreshes_and_logs_out() {
    let mock = oauth_mock::start(3600).await;
    let (_dir, dirs) = temp_project(&[]);
    let env = Env::fixed(&[]).with_dirs(dirs.clone());
    let store = TokenStore::for_server(&dirs, "nb", &mock.mcp_url()).expect("a home");
    assert_eq!(store.status(), LoginState::NotLoggedIn);
    assert_eq!(
        store.path(),
        dirs.home
            .as_ref()
            .expect("home")
            .join(".iota/mcp/auth/nb.json")
    );

    // ---- login: discovery, registration, the redirect, the exchange, the file.
    let expires_in = login_via_redirect(&mock, &store).await;
    assert!(expires_in > Duration::from_secs(3500), "{expires_in:?}");
    let st = mock.state();
    assert_eq!(st.grants, ["authorization_code"]);
    assert_eq!(st.registered.len(), 1, "one dynamic registration");
    // RFC 8707: the resource on the authorization request and on the exchange; the scopes the resource
    // names (its metadata, its challenge) plus `offline_access` because the server lists it.
    assert_eq!(st.authorizations.len(), 1);
    assert_eq!(st.authorizations[0].client_id, "cid-1");
    assert_eq!(
        st.authorizations[0].scope.as_deref(),
        Some("mcp offline_access")
    );
    assert_eq!(
        st.authorizations[0].resource.as_deref(),
        Some(mock.mcp_url().as_str())
    );
    assert_eq!(
        st.token_requests[0].resource.as_deref(),
        Some(mock.mcp_url().as_str())
    );
    assert_eq!(st.token_requests[0].client_id.as_deref(), Some("cid-1"));
    assert_eq!(st.token_requests[0].client_secret, None, "a public client");
    assert!(
        st.registered[0].starts_with("http://127.0.0.1:")
            && st.registered[0].ends_with("/callback"),
        "{}",
        st.registered[0]
    );
    let file = token_json(&store);
    assert_eq!(file["url"], mock.mcp_url());
    assert_eq!(file["credentials"]["client_id"], "cid-1");
    assert_eq!(
        file["credentials"]["token_response"]["access_token"],
        "at-1"
    );
    assert_eq!(
        file["credentials"]["token_response"]["refresh_token"],
        "rt-1"
    );
    assert_eq!(
        file["metadata"]["token_endpoint"],
        format!("{}/token", mock.base())
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(store.path())
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the token file is the owner's alone");
    }
    assert_eq!(store.status(), LoginState::LoggedIn);

    // ---- a run connects with the stored token, and the tool call carries it.
    let opts = ManagerOptions::new(reqwest::Client::new(), env.clone());
    let m = Manager::new(
        vec![oauth_server(&mock.mcp_url()), plain_server()],
        opts.clone(),
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(statuses[0].connected(), "nb: {:?}", statuses[0].state);
    assert_eq!(statuses[0].tools, ["echo"]);
    assert_eq!(statuses[0].auth, AuthMode::Oauth);
    assert_eq!(statuses[0].login, Some(LoginState::LoggedIn));
    assert!(statuses[1].connected(), "plain: {:?}", statuses[1].state);
    assert_eq!(statuses[1].login, None, "a plain server has no login state");
    let cx = RunCtx::new(CancellationToken::new());
    let out = m
        .call_tool(&cx, "mcp__nb__echo", JsonObject::new())
        .await
        .expect("call");
    assert_eq!(out.text, "pong via at-1");
    assert_eq!(mock.state().bearers.last(), Some(&Some("at-1".to_owned())));
    m.close().await;

    // ---- the token ages past its lifetime: the next run refreshes before its first request.
    let mut file = token_json(&store);
    file["credentials"]["token_received_at"] = serde_json::json!(1_000_000);
    std::fs::write(
        store.path(),
        serde_json::to_vec_pretty(&file).expect("json"),
    )
    .expect("write");
    let m = Manager::new(vec![oauth_server(&mock.mcp_url())], opts.clone());
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(
        statuses[0].connected(),
        "after refresh: {:?}",
        statuses[0].state
    );
    let st = mock.state();
    assert_eq!(st.grants, ["authorization_code", "refresh_token"]);
    assert_eq!(st.bearers.last(), Some(&Some("at-2".to_owned())));
    assert_eq!(
        token_json(&store)["credentials"]["token_response"]["access_token"],
        "at-2",
        "the refreshed token is written back"
    );
    m.close().await;

    // ---- the server stops accepting the token: one 401 → refresh → retry, invisibly.
    mock.invalidate_access("at-2");
    let m = Manager::new(vec![oauth_server(&mock.mcp_url())], opts.clone());
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(
        statuses[0].connected(),
        "after 401: {:?}",
        statuses[0].state
    );
    assert_eq!(mock.state().bearers.last(), Some(&Some("at-3".to_owned())));
    m.close().await;

    // ---- logout: revoked at the server, the file gone, the next run degrades that one server.
    assert!(
        logout(&reqwest::Client::new(), &store)
            .await
            .expect("logout")
    );
    assert_eq!(
        mock.state().revoked,
        ["rt-3"],
        "the refresh token is what gets revoked"
    );
    assert!(!store.path().exists());
    assert_eq!(store.status(), LoginState::NotLoggedIn);
    assert!(
        !logout(&reqwest::Client::new(), &store)
            .await
            .expect("logout again"),
        "nothing to forget"
    );
    let requests_before = mock.state().bearers.len();
    let m = Manager::new(vec![oauth_server(&mock.mcp_url()), plain_server()], opts);
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert_eq!(
        statuses[0].error(),
        Some("not logged in: run iota mcp login nb")
    );
    assert_eq!(statuses[0].login, Some(LoginState::NotLoggedIn));
    assert!(statuses[1].connected(), "the plain server is untouched");
    assert_eq!(
        m.tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["mcp__plain__echo"]
    );
    assert_eq!(
        mock.state().bearers.len(),
        requests_before,
        "no request went out: with nothing stored there is nothing to send"
    );
    m.close().await;
}

/// A token the server will neither accept nor refresh is "not logged in" too — the run does not stall on
/// a challenge it cannot answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_refresh_is_not_logged_in() {
    let mock = oauth_mock::start(3600).await;
    let (_dir, dirs) = temp_project(&[]);
    let env = Env::fixed(&[]).with_dirs(dirs.clone());
    let store = TokenStore::for_server(&dirs, "nb", &mock.mcp_url()).expect("a home");
    login_via_redirect(&mock, &store).await;
    mock.invalidate_access("at-1");
    mock.invalidate_refresh("rt-1");
    let m = Manager::new(
        vec![oauth_server(&mock.mcp_url())],
        ManagerOptions::new(reqwest::Client::new(), env),
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert_eq!(
        statuses[0].error(),
        Some("not logged in: run iota mcp login nb")
    );
    assert_eq!(
        mock.state().grants,
        ["authorization_code", "refresh_token"],
        "one refresh was attempted"
    );
    m.close().await;
}

/// `Manager::login` from a run — the REPL's `/mcp login` — drives the same flow through `$BROWSER` and
/// reconnects the server; `Manager::logout` takes it down again.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manager_login_reconnects_and_logout_disconnects() {
    let mock = oauth_mock::start(3600).await;
    let (dir, dirs) = temp_project(&[]);
    // The "browser": a script that follows the authorization URL (through the redirect) to the callback.
    let script = dir.path().join("browser.sh");
    std::fs::write(&script, "#!/bin/sh\ncurl -sL \"$1\" >/dev/null 2>&1 &\n").expect("write");
    let env =
        Env::fixed(&[("BROWSER", &format!("sh {}", script.display()))]).with_dirs(dirs.clone());
    let m = Manager::new(
        vec![oauth_server(&mock.mcp_url()), plain_server()],
        ManagerOptions::new(reqwest::Client::new(), env),
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert_eq!(
        statuses[0].error(),
        Some("not logged in: run iota mcp login nb")
    );
    assert_eq!(
        m.tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["mcp__plain__echo"]
    );

    let mut lines = Vec::new();
    let status = m
        .login(
            "nb",
            &mut |line| lines.push(line),
            &CancellationToken::new(),
        )
        .await
        .expect("login through the browser script");
    assert!(status.connected(), "{:?}", status.state);
    assert_eq!(status.tools, ["echo"]);
    assert!(
        lines[0].starts_with("Redirect: http://127.0.0.1:") && lines[0].ends_with("/callback"),
        "{}",
        lines[0]
    );
    assert_eq!(lines[1], "Client: cid-1 (dynamic registration)");
    assert!(
        lines[2].starts_with("Open this URL to log in:\n  http://127.0.0.1:"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].starts_with("Logged in to nb; the token expires in "),
        "{}",
        lines[3]
    );
    assert_eq!(
        m.tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["mcp__plain__echo", "mcp__nb__echo"],
        "the reconnected server's tools join the live set"
    );
    assert_eq!(m.servers()[0].login, Some(LoginState::LoggedIn));
    let cx = RunCtx::new(CancellationToken::new());
    let out = m
        .call_tool(&cx, "mcp__nb__echo", JsonObject::new())
        .await
        .expect("call after login");
    assert_eq!(out.text, "pong via at-1");

    // A second login re-registers, replaces the session, and the tools are still there once.
    let status = m
        .login("nb", &mut |_| {}, &CancellationToken::new())
        .await
        .expect("login again");
    assert!(status.connected());
    assert_eq!(
        m.tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["mcp__plain__echo", "mcp__nb__echo"]
    );
    assert_eq!(mock.state().registered.len(), 2);

    // Not an OAuth server, or not in this run: the text says so.
    assert_eq!(
        m.login("plain", &mut |_| {}, &CancellationToken::new())
            .await
            .expect_err("plain has nothing to log in to"),
        "plain is not an OAuth server (set `auth: oauth` on it, or add it with --auth oauth)"
    );
    assert_eq!(
        m.login("nope", &mut |_| {}, &CancellationToken::new())
            .await
            .expect_err("unknown"),
        "no MCP server named \"nope\" in this run"
    );

    // Logout: the file goes, the session with it, the other server stays.
    let line = m.logout("nb").await.expect("logout");
    assert!(line.starts_with("logged out of nb (forgot "), "{line}");
    assert_eq!(
        m.servers()[0].error(),
        Some("not logged in: run iota mcp login nb")
    );
    assert_eq!(m.servers()[0].login, Some(LoginState::NotLoggedIn));
    assert_eq!(
        m.tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["mcp__plain__echo"]
    );
    assert_eq!(mock.state().revoked, ["rt-2"]);
    assert_eq!(
        m.logout("nb").await.expect("logout again"),
        "not logged in to nb (nothing to forget)"
    );
    m.close().await;
}

/// The authorization server registers nobody but takes a Client ID Metadata Document (Logto's shape, with
/// its discovery document appended to the issuer's path): the login identifies itself as iota's document
/// URL, asks for the resource's scopes plus `offline_access`, names the resource, and comes back with a
/// refresh token; a run then connects with the token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_metadata_document_identifies_the_client_when_nobody_registers() {
    let mock = oauth_mock::start_with(oauth_mock::Options {
        identity: oauth_mock::Identity::Cimd,
        as_path: "/oidc",
        ..oauth_mock::Options::default()
    })
    .await;
    let (_dir, dirs) = temp_project(&[]);
    let env = Env::fixed(&[]).with_dirs(dirs.clone());
    let store = TokenStore::for_server(&dirs, "nb", &mock.mcp_url()).expect("a home");

    let (_, steps) = login_as(&mock, &store, None, None, None)
        .await
        .expect("login through the metadata document");
    assert_eq!(
        steps[1],
        LoginStep::Identity {
            client_id: CLIENT_METADATA_URL.to_owned(),
            source: "client id metadata document",
        }
    );
    // Not a pre-registered client: the port is whatever was free, and the server was told it.
    let LoginStep::Redirect(redirect) = &steps[0] else {
        panic!("{:?}", steps[0]);
    };
    assert!(
        redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"),
        "{redirect}"
    );
    assert_eq!(&mock.state().authorizations[0].redirect_uri, redirect);
    let st = mock.state();
    assert!(st.registered.is_empty(), "no registration was attempted");
    assert_eq!(st.authorizations.len(), 1);
    assert_eq!(st.authorizations[0].client_id, CLIENT_METADATA_URL);
    assert_eq!(
        st.authorizations[0].scope.as_deref(),
        Some("mcp offline_access")
    );
    assert_eq!(
        st.authorizations[0].resource.as_deref(),
        Some(mock.mcp_url().as_str())
    );
    assert!(
        st.authorizations[0]
            .redirect_uri
            .starts_with("http://127.0.0.1:")
            && st.authorizations[0].redirect_uri.ends_with("/callback"),
        "{}",
        st.authorizations[0].redirect_uri
    );
    assert_eq!(
        st.token_requests[0].client_id.as_deref(),
        Some(CLIENT_METADATA_URL)
    );
    assert_eq!(
        st.token_requests[0].resource.as_deref(),
        Some(mock.mcp_url().as_str())
    );
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path()).expect("file")).expect("json");
    assert_eq!(file["credentials"]["client_id"], CLIENT_METADATA_URL);
    assert_eq!(
        file["credentials"]["token_response"]["refresh_token"],
        "rt-1"
    );
    assert_eq!(file["metadata"]["issuer"], format!("{}/oidc", mock.base()));

    let m = Manager::new(
        vec![oauth_server(&mock.mcp_url())],
        ManagerOptions::new(reqwest::Client::new(), env),
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(statuses[0].connected(), "{:?}", statuses[0].state);
    m.close().await;
}

/// Neither registration nor a metadata document: a client registered out of band — the config's
/// `client_id`/`client_secret` — identifies the login, the secret authenticates the exchange and every
/// refresh, and a resource that names no scope gets no `scope` parameter (not the server's whole list).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preregistered_client_logs_in_with_its_secret_and_no_scope() {
    let mock = oauth_mock::start_with(oauth_mock::Options {
        identity: oauth_mock::Identity::Preregistered {
            client_id: "pre-1".to_owned(),
            client_secret: Some("s3cret".to_owned()),
        },
        resource_scopes: false,
        ..oauth_mock::Options::default()
    })
    .await;
    let (_dir, dirs) = temp_project(&[]);
    let env = Env::fixed(&[("NB_SECRET", "s3cret")]).with_dirs(dirs.clone());
    let store = TokenStore::for_server(&dirs, "nb", &mock.mcp_url()).expect("a home");

    // Without an identity the flow stops before the browser, naming the ways out.
    let err = login_as(&mock, &store, None, None, None)
        .await
        .expect_err("nothing identifies the client");
    assert_eq!(
        err.to_string(),
        "the authorization server offers no dynamic client registration and does not accept a client id metadata document, and no client id is configured: register a client with the server's operator and give it to iota — `iota mcp add <name> --url <url> --auth oauth --client-id <id> [--client-secret-env VAR]`, or `iota mcp login <name> --client-id <id>` for this login"
    );
    assert!(mock.state().authorizations.is_empty());

    let (_, steps) = login_as(&mock, &store, Some("pre-1"), Some("s3cret"), None)
        .await
        .expect("login as the pre-registered client");
    assert_eq!(
        steps[0],
        LoginStep::Redirect("http://127.0.0.1:17801/callback".to_owned()),
        "a pre-registered client's redirect URI is the fixed default port"
    );
    assert_eq!(
        steps[1],
        LoginStep::Identity {
            client_id: "pre-1".to_owned(),
            source: "pre-registered",
        }
    );
    let st = mock.state();
    assert_eq!(st.authorizations.len(), 1);
    assert_eq!(st.authorizations[0].client_id, "pre-1");
    assert_eq!(
        st.authorizations[0].redirect_uri,
        "http://127.0.0.1:17801/callback"
    );
    assert_eq!(
        st.authorizations[0].scope, None,
        "nothing to ask for: no scope parameter at all"
    );
    assert_eq!(
        st.authorizations[0].resource.as_deref(),
        Some(mock.mcp_url().as_str())
    );
    assert_eq!(st.token_requests[0].client_id.as_deref(), Some("pre-1"));
    assert_eq!(
        st.token_requests[0].client_secret.as_deref(),
        Some("s3cret")
    );

    // A run with the secret in its config refreshes as the same confidential client.
    let mut file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path()).expect("file")).expect("json");
    file["credentials"]["token_received_at"] = serde_json::json!(1_000_000);
    std::fs::write(
        store.path(),
        serde_json::to_vec_pretty(&file).expect("json"),
    )
    .expect("write");
    let m = Manager::new(
        vec![ServerConfig {
            client_id: "pre-1".to_owned(),
            client_secret: "${env:NB_SECRET}".to_owned(),
            ..oauth_server(&mock.mcp_url())
        }],
        ManagerOptions::new(reqwest::Client::new(), env),
    );
    let statuses = m.connect_all(&CancellationToken::new()).await;
    assert!(statuses[0].connected(), "{:?}", statuses[0].state);
    let st = mock.state();
    assert_eq!(st.grants, ["authorization_code", "refresh_token"]);
    assert_eq!(
        st.token_requests[1].client_secret.as_deref(),
        Some("s3cret")
    );
    assert_eq!(
        st.token_requests[1].resource.as_deref(),
        Some(mock.mcp_url().as_str())
    );
    m.close().await;
}

/// The server refuses at the browser: the callback's `error`, `error_description` and `error_uri` all reach the
/// error — a refusal with nothing but a code left the user guessing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_carries_the_description_and_the_uri() {
    let mock = oauth_mock::start_with(oauth_mock::Options {
        refusal: Some(oauth_mock::Refusal {
            error: "access_denied".to_owned(),
            description: Some("the user declined the consent screen".to_owned()),
            uri: Some("https://as.example/errors/access_denied".to_owned()),
        }),
        ..oauth_mock::Options::default()
    })
    .await;
    let (_dir, dirs) = temp_project(&[]);
    let store = TokenStore::for_server(&dirs, "nb", &mock.mcp_url()).expect("a home");
    let err = login_as(&mock, &store, None, None, None)
        .await
        .expect_err("the server refused");
    assert_eq!(
        err.to_string(),
        "the authorization server refused: access_denied — the user declined the consent screen (https://as.example/errors/access_denied)"
    );
    assert!(
        matches!(&err, iota::mcp::auth::LoginError::Refused { error, description: Some(d), uri: Some(u), hint: None }
            if error == "access_denied" && d == "the user declined the consent screen" && u.ends_with("/access_denied")),
        "{err:?}"
    );
    assert!(!store.path().exists(), "nothing is stored");
}

/// A pre-registered client's port is the entry's when it names one, and a taken port is an error that says
/// what to do rather than a listener on some other port the server would refuse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preregistered_client_uses_its_own_port_or_reports_it_taken() {
    let mock = oauth_mock::start_with(oauth_mock::Options {
        identity: oauth_mock::Identity::Preregistered {
            client_id: "pre-1".to_owned(),
            client_secret: None,
        },
        ..oauth_mock::Options::default()
    })
    .await;
    let (_dir, dirs) = temp_project(&[]);
    let store = TokenStore::for_server(&dirs, "nb", &mock.mcp_url()).expect("a home");

    let (_, steps) = login_as(&mock, &store, Some("pre-1"), None, Some(17812))
        .await
        .expect("login on the entry's port");
    assert_eq!(
        steps[0],
        LoginStep::Redirect("http://127.0.0.1:17812/callback".to_owned())
    );
    assert_eq!(
        mock.state().authorizations[0].redirect_uri,
        "http://127.0.0.1:17812/callback"
    );

    let taken = tokio::net::TcpListener::bind(("127.0.0.1", 17813))
        .await
        .expect("bind the port first");
    let err = login_as(&mock, &store, Some("pre-1"), None, Some(17813))
        .await
        .expect_err("the port is taken");
    assert_eq!(
        err.to_string(),
        "port 17813 on 127.0.0.1 is in use, and a pre-registered client's redirect URI must match exactly: free it, or register another port and give it with --redirect-port"
    );
    drop(taken);
}

/// An `access_denied` with no reason, against a client id metadata document, gets the one hint there is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unexplained_access_denied_against_the_metadata_document_hints_at_logto() {
    let mock = oauth_mock::start_with(oauth_mock::Options {
        identity: oauth_mock::Identity::Cimd,
        refusal: Some(oauth_mock::Refusal {
            error: "access_denied".to_owned(),
            description: None,
            uri: None,
        }),
        ..oauth_mock::Options::default()
    })
    .await;
    let (_dir, dirs) = temp_project(&[]);
    let store = TokenStore::for_server(&dirs, "nb", &mock.mcp_url()).expect("a home");
    let err = login_as(&mock, &store, None, None, None)
        .await
        .expect_err("refused");
    assert_eq!(
        err.to_string(),
        "the authorization server refused: access_denied; the server gave no reason; if this is a Logto tenant and the client is a client id metadata document, the tenant's Logto may still apply application access control to it — register iota as a third-party application and add it with --client-id"
    );
    // The same refusal against a pre-registered client carries no such hint.
    let mock = oauth_mock::start_with(oauth_mock::Options {
        identity: oauth_mock::Identity::Preregistered {
            client_id: "pre-1".to_owned(),
            client_secret: None,
        },
        refusal: Some(oauth_mock::Refusal {
            error: "access_denied".to_owned(),
            description: None,
            uri: None,
        }),
        ..oauth_mock::Options::default()
    })
    .await;
    let store = TokenStore::for_server(&dirs, "nb2", &mock.mcp_url()).expect("a home");
    let err = login_as(&mock, &store, Some("pre-1"), None, Some(17814))
        .await
        .expect_err("refused");
    assert_eq!(
        err.to_string(),
        "the authorization server refused: access_denied"
    );
}
