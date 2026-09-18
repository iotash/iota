//! MCP server configuration (mcp/manager.go:22-30,530-549, mcp/vars.go): the parsing half of the manager, which
//! `crate::cmd::assemble` reads `--mcp` flags and config entries through before any server is connected.

use std::collections::BTreeMap;

use crate::app::env::{Env, expand};

/// How a streamable-HTTP server is authenticated (brain page `mcp-cli-and-oauth`): nothing beyond the static
/// `headers:`, or OAuth 2.1 with the tokens `iota mcp login` stored. Spelled `auth: oauth` in the config.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// No login: the headers as written are all the server gets.
    #[default]
    None,
    /// OAuth 2.1 (discovery → PKCE authorization code); the token store supplies the bearer token.
    Oauth,
}

impl AuthMode {
    /// `serde(skip_serializing_if)`: the default is left out of a written entry.
    pub fn is_none(&self) -> bool {
        *self == Self::None
    }
}

/// One MCP server definition (config entry or `--mcp` flag).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerConfig {
    /// Server name (config key or argv0 / URL for flags).
    pub name: String,
    /// stdio transport: the command to spawn.
    pub command: String,
    /// stdio transport: command arguments.
    pub args: Vec<String>,
    /// streamable-HTTP transport: the endpoint URL.
    pub url: String,
    /// Extra environment for the child process.
    pub env: BTreeMap<String, String>,
    /// Extra HTTP headers.
    pub headers: BTreeMap<String, String>,
    /// How an HTTP server is authenticated.
    pub auth: AuthMode,
    /// `auth: oauth`: a client id registered with the authorization server out of band (`""` = none: dynamic
    /// registration, else the Client ID Metadata Document).
    pub client_id: String,
    /// `auth: oauth`: the secret paired with `client_id`, when the registration has one (`""` = a public
    /// client). Written as a `${env:VAR}` reference and expanded like every other value.
    pub client_secret: String,
}

/// manager.go:530-549 + POLICY F-02: trim; empty → `Err(EmptyFlag)`; `http(s)://` prefix → `{name: value, url:
/// value}`; else `split_whitespace` → `{name: argv0, command: argv0, args: rest}`.
pub fn parse_mcp_flag(value: &str) -> Result<ServerConfig, McpFlagError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(McpFlagError::EmptyFlag);
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return Ok(ServerConfig {
            name: value.to_owned(),
            url: value.to_owned(),
            ..ServerConfig::default()
        });
    }
    let mut parts = value.split_whitespace();
    // A trimmed, non-empty value has at least one field.
    let argv0 = parts.next().unwrap_or_default().to_owned();
    Ok(ServerConfig {
        name: argv0.clone(),
        command: argv0,
        args: parts.map(str::to_owned).collect(),
        ..ServerConfig::default()
    })
}

/// `expand` on command, url, each arg, each env VALUE, each header VALUE (never name or map keys).
pub(crate) fn expand_server_config(server_cfg: &ServerConfig, env: &Env) -> ServerConfig {
    let x = |s: &str| expand(s, env).into_owned();
    ServerConfig {
        name: server_cfg.name.clone(),
        command: x(&server_cfg.command),
        args: server_cfg.args.iter().map(|a| x(a)).collect(),
        url: x(&server_cfg.url),
        env: server_cfg
            .env
            .iter()
            .map(|(k, v)| (k.clone(), x(v)))
            .collect(),
        headers: server_cfg
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), x(v)))
            .collect(),
        auth: server_cfg.auth,
        client_id: x(&server_cfg.client_id),
        client_secret: x(&server_cfg.client_secret),
    }
}

/// url if non-empty, else command + `" "` + args joined by `" "` (of an already-expanded config).
pub(crate) fn endpoint_of(server_cfg: &ServerConfig) -> String {
    if !server_cfg.url.is_empty() {
        return server_cfg.url.clone();
    }
    let mut endpoint = server_cfg.command.clone();
    if !server_cfg.args.is_empty() {
        endpoint.push(' ');
        endpoint.push_str(&server_cfg.args.join(" "));
    }
    endpoint
}

/// The only MCP error that aborts a run; feature-independent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpFlagError {
    /// `--mcp ""` / whitespace (Go panics).
    #[error("--mcp: empty server specification")]
    EmptyFlag,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::{McpFlagError, ServerConfig, endpoint_of, expand_server_config, parse_mcp_flag};
    use crate::app::HostDirs;
    use crate::app::env::Env;

    // Go: mcp/manager.go:530 (ParseMCPFlag has no Go test; the cases are the spec's, plus POLICY F-02).
    #[test]
    fn test_parse_mcp_flag() {
        // URL form: the URL is both the name and the endpoint.
        let url = parse_mcp_flag("  https://mcp.example.com/mcp ").expect("url");
        assert_eq!(
            url,
            ServerConfig {
                name: "https://mcp.example.com/mcp".to_owned(),
                url: "https://mcp.example.com/mcp".to_owned(),
                ..ServerConfig::default()
            }
        );
        let http = parse_mcp_flag("http://localhost:8080/x").expect("http url");
        assert_eq!(http.url, "http://localhost:8080/x");
        assert!(http.command.is_empty());
        assert!(http.args.is_empty());

        // argv form: whitespace runs split, no quoting; argv0 is both name and command.
        let argv =
            parse_mcp_flag("npx  -y\t@modelcontextprotocol/server-filesystem /tmp").expect("argv");
        assert_eq!(
            argv,
            ServerConfig {
                name: "npx".to_owned(),
                command: "npx".to_owned(),
                args: vec![
                    "-y".to_owned(),
                    "@modelcontextprotocol/server-filesystem".to_owned(),
                    "/tmp".to_owned(),
                ],
                ..ServerConfig::default()
            }
        );
        let bare = parse_mcp_flag("server-bin").expect("bare command");
        assert_eq!(bare.name, "server-bin");
        assert_eq!(bare.command, "server-bin");
        assert!(bare.args.is_empty());
        assert!(bare.url.is_empty());
        // Only the http(s) schemes select the URL transport.
        let other = parse_mcp_flag("ftp://x").expect("other scheme");
        assert_eq!(other.command, "ftp://x");
        assert!(other.url.is_empty());

        // POLICY F-02: an empty or whitespace-only value is an error, never a panic.
        for bad in ["", "  ", "\t\n"] {
            assert_eq!(parse_mcp_flag(bad), Err(McpFlagError::EmptyFlag), "{bad:?}");
        }
        assert_eq!(
            McpFlagError::EmptyFlag.to_string(),
            "--mcp: empty server specification"
        );
    }

    fn fixed() -> Env {
        Env::fixed(&[("TOKEN", "t0k")]).with_dirs(HostDirs {
            cwd: Some(PathBuf::from("/wd")),
            home: Some(PathBuf::from("/home/u")),
            ..HostDirs::default()
        })
    }

    #[test]
    fn expand_server_config_touches_values_only() {
        let server_cfg = ServerConfig {
            name: "${cwd}-srv".to_owned(),
            command: "${userHome}/bin/mcp".to_owned(),
            args: vec![
                "--root".to_owned(),
                "${cwd}".to_owned(),
                "${unknown}".to_owned(),
            ],
            url: "https://x/${env:TOKEN}".to_owned(),
            env: BTreeMap::from([
                ("${cwd}_KEY".to_owned(), "${env:TOKEN}".to_owned()),
                ("PLAIN".to_owned(), "v".to_owned()),
            ]),
            headers: BTreeMap::from([("X-${cwd}".to_owned(), "Bearer ${env:TOKEN}".to_owned())]),
            auth: super::AuthMode::Oauth,
            client_id: "cid".to_owned(),
            client_secret: "${env:TOKEN}".to_owned(),
        };
        let got = expand_server_config(&server_cfg, &fixed());
        assert_eq!(
            got.auth,
            super::AuthMode::Oauth,
            "auth rides along untouched"
        );
        assert_eq!(got.client_id, "cid");
        assert_eq!(
            got.client_secret, "t0k",
            "the secret is a reference, expanded"
        );
        assert_eq!(got.name, "${cwd}-srv", "name is never expanded");
        assert_eq!(got.command, "/home/u/bin/mcp");
        assert_eq!(got.args, vec!["--root", "/wd", "${unknown}"]);
        assert_eq!(got.url, "https://x/t0k");
        assert_eq!(
            got.env,
            BTreeMap::from([
                ("${cwd}_KEY".to_owned(), "t0k".to_owned()),
                ("PLAIN".to_owned(), "v".to_owned()),
            ]),
            "env keys stay verbatim, values expand"
        );
        assert_eq!(
            got.headers,
            BTreeMap::from([("X-${cwd}".to_owned(), "Bearer t0k".to_owned())]),
            "header keys stay verbatim, values expand"
        );
        // The input is untouched (a copy is returned).
        assert_eq!(server_cfg.command, "${userHome}/bin/mcp");

        // endpoint_of: url wins; else command joined with its args; a bare command has no trailing space.
        assert_eq!(endpoint_of(&got), "https://x/t0k");
        let stdio = ServerConfig {
            command: "npx".to_owned(),
            args: vec!["-y".to_owned(), "srv".to_owned()],
            ..ServerConfig::default()
        };
        assert_eq!(endpoint_of(&stdio), "npx -y srv");
        let bare = ServerConfig {
            command: "srv".to_owned(),
            ..ServerConfig::default()
        };
        assert_eq!(endpoint_of(&bare), "srv");
    }
}
