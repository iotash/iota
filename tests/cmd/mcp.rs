//! End-to-end tests of `iota mcp add|list|get|remove`: the two scopes and the `-c` file, what each command
//! writes and prints, and every refusal's text (brain page `mcp-cli-and-oauth`).
//!
//! Every child runs with a CLEARED environment (`common::cleared_env`) in a temp project whose home is the
//! fixture's, so the user tier is `<home>/.iota.yaml` and the project tier `<cwd>/.iota.yaml`.

use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::common::{cleared_env, temp_project};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

/// A project the child runs in: a temp cwd plus a temp `HOME`, neither holding a config file.
fn project() -> (TempDir, std::path::PathBuf) {
    let (dir, dirs) = temp_project(&[]);
    let home = dirs.home.expect("fixture home");
    (dir, home)
}

/// `iota mcp …` with a cleared environment.
fn mcp(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    mcp_with(cwd, home, &[], args)
}

/// `iota mcp …` with a cleared environment plus `extra_env` (`$BROWSER`, a secret's variable).
fn mcp_with(cwd: &Path, home: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .arg("mcp")
        .args(args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("run iota")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Asserts exit 0 and returns stdout.
fn ok(o: &Output) -> String {
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(o));
    assert_eq!(err(o), "", "a clean command prints nothing on stderr");
    out(o)
}

/// The project file as the CHILD names it: `<its cwd>/.iota.yaml`, where its cwd is what the OS reports for
/// the directory it was started in. On unix that is the physical path — `getcwd` resolves symlinks, so a
/// macOS temp dir comes back as `/private/var/…` — which is what `canonicalize` gives. On Windows
/// `GetCurrentDirectory` returns the string the child was started with, verbatim, and `canonicalize`
/// would instead add a `\\?\` prefix and expand an 8.3 name (`RUNNER~1` → `runneradmin`): there the path
/// handed to the command IS the expectation.
fn child_project_file(cwd: &Path) -> std::path::PathBuf {
    let cwd = if cfg!(windows) {
        cwd.to_path_buf()
    } else {
        cwd.canonicalize().expect("the temp dir resolves")
    };
    cwd.join(".iota.yaml")
}

/// Asserts the command failed with exit 1 and printed exactly `Error: {message}`.
fn assert_error(o: &Output, message: &str) {
    assert_eq!(o.status.code(), Some(1), "stderr was: {}", err(o));
    assert_eq!(err(o), format!("Error: {message}\n"));
    assert!(out(o).is_empty(), "a failed command printed to stdout");
}

/// `add` writes the user file by default — creating it — and `list`/`get`/`remove` read it back; the file
/// keeps everything else the user wrote.
#[test]
fn mcp_add_list_get_remove_in_the_user_scope() {
    let (dir, home) = project();
    let user = home.join(".iota.yaml");
    let cwd = dir.path();

    // A fresh home: the file is created with the block alone.
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "fs",
            "-e",
            "LOG=info",
            "--defer",
            "file tools",
            "--",
            "npx",
            "-y",
            "server-fs",
            "/tmp",
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added fs (stdio: npx -y server-fs /tmp) to {}\n",
            user.display()
        )
    );
    assert_eq!(
        fs::read_to_string(&user).unwrap(),
        "mcp_servers:\n  fs:\n    command: npx\n    args:\n    - -y\n    - server-fs\n    - /tmp\n    env:\n      LOG: info\n    defer: file tools\n"
    );

    // Hand-written content around the block survives the next add.
    fs::write(
        &user,
        format!(
            "# mine\nproviders:\n  openai: {{key: k}}\n\n{}",
            fs::read_to_string(&user).unwrap()
        ),
    )
    .unwrap();
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "gh",
            "--url",
            "https://gh.example/mcp",
            "--header",
            "Authorization: Bearer ${env:GH}",
            "--header",
            "X-Client:iota",
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added gh (http: https://gh.example/mcp) to {}\n",
            user.display()
        )
    );
    assert_eq!(
        fs::read_to_string(&user).unwrap(),
        "# mine\nproviders:\n  openai: {key: k}\n\nmcp_servers:\n  fs:\n    command: npx\n    args:\n    - -y\n    - server-fs\n    - /tmp\n    env:\n      LOG: info\n    defer: file tools\n  gh:\n    url: https://gh.example/mcp\n    headers:\n      Authorization: Bearer ${env:GH}\n      X-Client: iota\n"
    );

    // list: name, transport, file, auth — aligned.
    let o = mcp(cwd, &home, &["list"]);
    assert_eq!(
        ok(&o),
        format!(
            "MCP servers:\n  fs  stdio  {u}  [auth: none]\n  gh  http   {u}  [auth: header]\n",
            u = user.display()
        )
    );
    // --scope project sees nothing: the project has no file.
    let o = mcp(cwd, &home, &["list", "--scope", "project"]);
    assert_eq!(
        ok(&o),
        "No MCP servers configured. Add one with `iota mcp add <name> -- <command>` or `iota mcp add <name> --url <url>`.\n"
    );
    // --json: one object per server, every field present.
    let o = mcp(cwd, &home, &["list", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&ok(&o)).expect("json");
    assert_eq!(
        rows,
        serde_json::json!([
            {"name": "fs", "transport": "stdio", "file": user.display().to_string(), "command": "npx",
             "args": ["-y", "server-fs", "/tmp"], "url": "", "env": {"LOG": "info"}, "headers": {},
             "defer": "file tools", "client_id": "", "client_secret": "", "redirect_port": null, "redirect_uri": null, "auth": "none", "login": null},
            {"name": "gh", "transport": "http", "file": user.display().to_string(), "command": "",
             "args": [], "url": "https://gh.example/mcp", "env": {},
             "headers": {"Authorization": "Bearer ${env:GH}", "X-Client": "iota"}, "defer": null, "client_id": "", "client_secret": "", "redirect_port": null, "redirect_uri": null, "auth": "header", "login": null}
        ])
    );

    // get: the entry as declared.
    let o = mcp(cwd, &home, &["get", "gh"]);
    assert_eq!(
        ok(&o),
        format!(
            "gh:\n  file: {}\n  transport: http\n  url: https://gh.example/mcp\n  headers:\n    Authorization: Bearer ${{env:GH}}\n    X-Client: iota\n  auth: header\n",
            user.display()
        )
    );
    let o = mcp(cwd, &home, &["get", "fs"]);
    assert_eq!(
        ok(&o),
        format!(
            "fs:\n  file: {}\n  transport: stdio\n  command: npx\n  args:\n    - -y\n    - server-fs\n    - /tmp\n  env:\n    LOG: info\n  defer: file tools\n  auth: none\n",
            user.display()
        )
    );

    // remove: the entry leaves; the last one takes the block with it.
    let o = mcp(cwd, &home, &["remove", "fs"]);
    assert_eq!(ok(&o), format!("Removed fs from {}\n", user.display()));
    let o = mcp(cwd, &home, &["remove", "gh"]);
    assert_eq!(ok(&o), format!("Removed gh from {}\n", user.display()));
    assert_eq!(
        fs::read_to_string(&user).unwrap(),
        "# mine\nproviders:\n  openai: {key: k}\n"
    );
}

/// The project scope writes `./.iota.yaml`, refuses a secret in clear, and wins a name the user file also
/// declares; `remove` of a name in both tiers wants `--scope`.
#[test]
fn mcp_project_scope_and_the_two_tiers() {
    let (dir, home) = project();
    let cwd = dir.path();
    let user = home.join(".iota.yaml");
    let proj = child_project_file(cwd);

    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "gh",
            "--scope",
            "project",
            "--url",
            "https://gh.example/mcp",
            "--header",
            "Authorization: Bearer sk-secret",
        ],
    );
    assert_error(
        &o,
        "mcp: a project-scope value must reference an environment variable (${NAME}), not the secret itself: headers.Authorization",
    );
    assert!(!proj.exists(), "nothing was written");
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "srv",
            "--scope",
            "project",
            "-e",
            "TOKEN=abc",
            "--",
            "srv-bin",
        ],
    );
    assert_error(
        &o,
        "mcp: a project-scope value must reference an environment variable (${NAME}), not the secret itself: env.TOKEN",
    );

    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "gh",
            "--scope",
            "project",
            "--url",
            "https://gh.example/mcp",
            "--header",
            "Authorization: Bearer ${env:GH}",
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added gh (http: https://gh.example/mcp) to {}\n",
            proj.display()
        )
    );
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "gh",
            "--url",
            "https://user.example/mcp",
            "--no-login",
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added gh (http: https://user.example/mcp) to {}\nif the server asks for a login: iota mcp login gh\n",
            user.display()
        )
    );

    // The project entry wins the merged listing; each scope lists its own.
    let o = mcp(cwd, &home, &["list"]);
    assert_eq!(
        ok(&o),
        format!(
            "MCP servers:\n  gh  http   {}  [auth: header]\n",
            proj.display()
        )
    );
    let o = mcp(cwd, &home, &["list", "--scope", "user"]);
    assert_eq!(
        ok(&o),
        format!(
            "MCP servers:\n  gh  http   {}  [auth: auto]\n",
            user.display()
        )
    );
    let o = mcp(cwd, &home, &["list", "--scope", "all"]);
    assert_eq!(
        ok(&o),
        format!(
            "MCP servers:\n  gh  http   {}  [auth: header]\n",
            proj.display()
        )
    );

    // Adding the same name to the same file is refused.
    let o = mcp(
        cwd,
        &home,
        &["add", "gh", "--scope", "project", "--url", "https://x/mcp"],
    );
    assert_error(
        &o,
        &format!(
            "mcp: a server named \"gh\" already exists in {} (remove it first, or pick another name)",
            proj.display()
        ),
    );

    // remove without a scope cannot choose between the two files.
    let o = mcp(cwd, &home, &["remove", "gh"]);
    assert_error(
        &o,
        &format!(
            "mcp: \"gh\" is declared in more than one file; say which with --scope:\n  {}\n  {}",
            user.display(),
            proj.display()
        ),
    );
    let o = mcp(cwd, &home, &["remove", "gh", "--scope", "project"]);
    assert_eq!(ok(&o), format!("Removed gh from {}\n", proj.display()));
    assert_eq!(fs::read_to_string(&proj).unwrap(), "");
    // …and a scope that does not hold the name says so.
    let o = mcp(cwd, &home, &["remove", "gh", "--scope", "project"]);
    assert_error(
        &o,
        &format!("mcp: no server named \"gh\" in {}", proj.display()),
    );
    let o = mcp(cwd, &home, &["remove", "nope"]);
    assert_error(
        &o,
        "mcp: no server named \"nope\"\n  configured servers: gh",
    );
    let o = mcp(cwd, &home, &["get", "nope"]);
    assert_error(
        &o,
        "mcp: no server named \"nope\"\n  configured servers: gh",
    );
    let o = mcp(cwd, &home, &["remove", "gh", "--scope", "user"]);
    assert_eq!(ok(&o), format!("Removed gh from {}\n", user.display()));
    let o = mcp(cwd, &home, &["get", "gh"]);
    assert_error(&o, "mcp: no server named \"gh\" (none are configured)");
}

/// `-c <file>` is the only scope, from either side of the verb; `--scope` beside it is refused.
#[test]
fn mcp_explicit_config_file_is_the_only_scope() {
    let (dir, home) = project();
    let cwd = dir.path();
    let alt = cwd.join("alt.yaml");
    fs::write(&alt, "providers:\n  p: {key: k}\n").unwrap();

    let o = mcp(
        cwd,
        &home,
        &["-c", alt.to_str().unwrap(), "add", "s", "--", "srv"],
    );
    assert_eq!(
        ok(&o),
        format!("Added s (stdio: srv) to {}\n", alt.display())
    );
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "t",
            "--url",
            "http://127.0.0.1:1/mcp",
            "-c",
            alt.to_str().unwrap(),
            "--no-login",
        ],
    );
    // No `--auth`: nothing is written for it, and the hint (`--no-login`: the endpoint is not asked) is
    // conditional — the server, not the entry, says whether there is a login.
    assert_eq!(
        ok(&o),
        format!(
            "Added t (http: http://127.0.0.1:1/mcp) to {}\nif the server asks for a login: iota mcp login t\n",
            alt.display()
        )
    );
    assert_eq!(
        fs::read_to_string(&alt).unwrap(),
        "providers:\n  p: {key: k}\n\nmcp_servers:\n  s:\n    command: srv\n  t:\n    url: http://127.0.0.1:1/mcp\n"
    );
    assert!(!home.join(".iota.yaml").exists() && !cwd.join(".iota.yaml").exists());
    let o = mcp(cwd, &home, &["list", "-c", alt.to_str().unwrap()]);
    assert_eq!(
        ok(&o),
        format!(
            "MCP servers:\n  s  stdio  {a}  [auth: none]\n  t  http   {a}  [auth: auto]\n",
            a = alt.display()
        )
    );
    for args in [
        &[
            "add",
            "u",
            "--scope",
            "user",
            "-c",
            alt.to_str().unwrap(),
            "--",
            "x",
        ][..],
        &["list", "--scope", "user", "-c", alt.to_str().unwrap()][..],
        &[
            "remove",
            "s",
            "--scope",
            "user",
            "-c",
            alt.to_str().unwrap(),
        ][..],
    ] {
        let o = mcp(cwd, &home, args);
        assert_error(
            &o,
            "--scope does not apply with -c: the file given is the only scope",
        );
    }
    let o = mcp(cwd, &home, &["remove", "s", "-c", alt.to_str().unwrap()]);
    assert_eq!(ok(&o), format!("Removed s from {}\n", alt.display()));

    // A file iota would refuse to load is not rewritten, and the refusal names the coordinate.
    fs::write(&alt, "providers:\n  p: {kye: k}\n").unwrap();
    let o = mcp(
        cwd,
        &home,
        &["add", "v", "-c", alt.to_str().unwrap(), "--", "x"],
    );
    assert_error(
        &o,
        &format!(
            "config {}: providers.p.kye: unknown key (want type, key, url)",
            alt.display()
        ),
    );
}

/// What `add` refuses about its own flags, each with its text.
#[test]
fn mcp_add_refusals() {
    let (dir, home) = project();
    let cwd = dir.path();
    let cases: &[(&[&str], &str)] = &[
        (
            &["add", "x"],
            "mcp add: a server needs a command after `--`, or --url",
        ),
        (
            &["add", "x", "--url", "https://x/mcp", "--", "srv"],
            "mcp add: give a command after `--` or --url, not both",
        ),
        (
            &["add", "x", "--url", "ftp://x"],
            "mcp add: --url wants http:// or https://, got \"ftp://x\"",
        ),
        (
            &[
                "add",
                "x",
                "--header",
                "Authorization Bearer t",
                "--url",
                "https://x/mcp",
            ],
            "mcp add: --header wants 'Name: value', got \"Authorization Bearer t\"",
        ),
        (
            &["add", "x", "--header", ": v", "--url", "https://x/mcp"],
            "mcp add: --header wants 'Name: value', got \": v\"",
        ),
        (
            &["add", "x", "-e", "NOVALUE", "--", "srv"],
            "mcp add: -e wants NAME=value, got \"NOVALUE\"",
        ),
        (
            &["add", "x", "--header", "A: b", "--", "srv"],
            "mcp add: --header applies to --url servers only",
        ),
        (
            &["add", "x", "--auth", "oauth", "--", "srv"],
            "mcp add: --auth applies to --url servers only",
        ),
        (
            &["add", "x", "-e", "A=b", "--url", "https://x/mcp"],
            "mcp add: -e/--env applies to command servers only",
        ),
        (
            &["add", "bad name", "--", "srv"],
            "mcp add: a server name is letters, digits, `_`, `-` and `.`: \"bad name\"",
        ),
        (
            &["add", "a:b", "--", "srv"],
            "mcp add: a server name is letters, digits, `_`, `-` and `.`: \"a:b\"",
        ),
    ];
    for (args, want) in cases {
        let o = mcp(cwd, &home, args);
        assert_error(&o, want);
    }
    assert!(
        !home.join(".iota.yaml").exists(),
        "a refused add writes nothing"
    );
}

/// `--auth oauth` is recorded as `auth: oauth`, listed as such, and followed by the login hint — and so is
/// `--auth none`, now that not writing `auth:` means "the server says"; the agent that runs by default is
/// told when it lists its servers explicitly and left the new one out.
#[test]
fn mcp_add_oauth_and_the_agent_subset_hint() {
    let (dir, home) = project();
    let cwd = dir.path();
    let user = home.join(".iota.yaml");
    fs::write(
        &user,
        "providers:\n  p: {key: k}\nagents:\n  default:\n    models: [\"p:m\"]\n    mcp_servers: [fs]\n",
    )
    .unwrap();
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "nb",
            "--url",
            "https://nb.example/api/mcp",
            "--auth",
            "oauth",
            "--no-login",
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added nb (http: https://nb.example/api/mcp) to {}\nNext: iota mcp login nb\nagent default lists its servers explicitly; add nb there to use it\n",
            user.display()
        )
    );
    assert!(
        fs::read_to_string(&user).unwrap().ends_with(
            "mcp_servers:\n  nb:\n    url: https://nb.example/api/mcp\n    auth: oauth\n"
        ),
        "{}",
        fs::read_to_string(&user).unwrap()
    );
    let o = mcp(cwd, &home, &["list"]);
    assert_eq!(
        ok(&o),
        format!(
            "MCP servers:\n  nb  http   {}  [auth: oauth: not logged in]\n",
            user.display()
        )
    );
    // `--auth none` is written (it is not the default), draws no login hint, and a name the agent lists
    // draws no subset hint.
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "fs",
            "--url",
            "https://fs.example/mcp",
            "--auth",
            "none",
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added fs (http: https://fs.example/mcp) to {}\n",
            user.display()
        )
    );
    assert!(
        fs::read_to_string(&user).unwrap().contains(
            "mcp_servers:\n  fs:\n    url: https://fs.example/mcp\n    auth: none\n  nb:\n"
        ),
        "{}",
        fs::read_to_string(&user).unwrap()
    );
    let o = mcp(cwd, &home, &["list"]);
    assert_eq!(
        ok(&o),
        format!(
            "MCP servers:\n  fs  http   {u}  [auth: none]\n  nb  http   {u}  [auth: oauth: not logged in]\n",
            u = user.display()
        )
    );
    assert_eq!(
        ok(&mcp(cwd, &home, &["get", "fs"])),
        format!(
            "fs:\n  file: {}\n  transport: http\n  url: https://fs.example/mcp\n  auth: none\n",
            user.display()
        )
    );

    // The config the run loads refuses `auth: oauth` on a stdio server where it is written.
    fs::write(
        &user,
        "providers:\n  p: {key: k}\nmcp_servers:\n  s: {command: srv, auth: oauth}\n",
    )
    .unwrap();
    let o = mcp(cwd, &home, &["list"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "the listing reads the block as written"
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, &home)
        .current_dir(cwd)
        .args(["config", "check"]);
    let o = cmd.output().expect("run");
    assert_error(
        &o,
        "mcp_servers.s: auth: oauth needs a url (a stdio server has nothing to log in to)",
    );
}

/// `--probe` connects the way a run does and reports each server's outcome beside its row.
#[cfg(unix)]
#[test]
fn mcp_list_probe_reports_each_server() {
    let (dir, home) = project();
    let cwd = dir.path();
    let user = home.join(".iota.yaml");
    let script = cwd.join("server.sh");
    fs::write(&script, SH_SERVER).unwrap();
    let o = mcp(
        cwd,
        &home,
        &["add", "good", "--", "sh", script.to_str().unwrap()],
    );
    ok(&o);
    let o = mcp(cwd, &home, &["add", "bad", "--", "/nonexistent/mcp-server"]);
    ok(&o);
    let o = mcp(cwd, &home, &["list", "--probe"]);
    let text = ok(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[1].starts_with(&format!(
            "  bad   stdio  {}  [auth: none]  failed: connect failed: ",
            user.display()
        )),
        "{}",
        lines[1]
    );
    assert_eq!(
        lines[2],
        format!(
            "  good  stdio  {}  [auth: none]  connected (1 tools)",
            user.display()
        )
    );
    let o = mcp(cwd, &home, &["list", "--probe", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&ok(&o)).expect("json");
    assert_eq!(rows[0]["probe"]["state"], "failed");
    assert!(
        rows[0]["probe"]["error"]
            .as_str()
            .unwrap()
            .starts_with("connect failed: ")
    );
    assert_eq!(
        rows[1]["probe"],
        serde_json::json!({"state": "connected", "tools": ["echo"], "error": null})
    );
}

/// A minimal MCP server in POSIX `sh` (the one `tests/mcp/manager.rs` uses): `initialize`, `tools/list` with
/// one tool `echo`, `tools/call` answering `pong`; exits on stdin EOF.
#[cfg(unix)]
const SH_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.0.0"}}}' ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[{"name":"echo","description":"says pong","inputSchema":{"type":"object"}}]}}' ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"content":[{"type":"text","text":"pong"}],"isError":false}}' ;;
  esac
done
"#;

// ---------------------------------------------------------------- OAuth: login, logout, the degraded run

/// `iota mcp login <name> --no-browser` against the mock authorization server, on an entry that says
/// nothing about `auth` (the default: the server says): the URL is printed, a "browser" (this test) follows
/// it to the loopback callback, the token file lands with mode 0600, `list` and `get` say "logged in", a
/// headless run connects with the token, `logout` revokes and forgets — and a run after that degrades the
/// ONE server with the line that names the way back in, because the server's 401 asked for one.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_login_logout_through_the_cli() {
    use crate::common_oauth as oauth_mock;

    let mock = oauth_mock::start(3600).await;
    let (dir, home) = project();
    let cwd = dir.path();
    let token_file = home.join(".iota/mcp/auth/nb.json");

    // The server, declared in the user file with no `auth:` (and `--no-login`: the login under test is the
    // command's own); a plain one beside it.
    ok(&mcp(
        cwd,
        &home,
        &["add", "nb", "--url", &mock.mcp_url(), "--no-login"],
    ));
    let script = cwd.join("server.sh");
    fs::write(&script, SH_SERVER).unwrap();
    ok(&mcp(
        cwd,
        &home,
        &["add", "plain", "--", "sh", script.to_str().unwrap()],
    ));
    assert!(
        !fs::read_to_string(home.join(".iota.yaml"))
            .unwrap()
            .contains("auth:"),
        "nothing about auth is written"
    );

    // Before any login an undeclared entry is just `auto` — a listing does not connect to find out more;
    // `logout` has nothing to forget; `login` refuses what has no login: a stdio server, `auth: none`.
    let text = ok(&mcp(cwd, &home, &["list"]));
    assert!(
        text.contains("  nb     http   ") && text.contains("[auth: auto]\n"),
        "{text}"
    );
    let text = ok(&mcp(cwd, &home, &["get", "nb"]));
    assert!(text.ends_with("  auth: auto\n"), "{text}");
    let rows: serde_json::Value =
        serde_json::from_str(&ok(&mcp(cwd, &home, &["list", "--json"]))).unwrap();
    assert_eq!(rows[0]["auth"], "auto");
    assert_eq!(rows[0]["login"], serde_json::Value::Null);
    assert_eq!(
        ok(&mcp(cwd, &home, &["logout", "nb"])),
        "Not logged in to nb (nothing to forget)\n"
    );
    assert_error(
        &mcp(cwd, &home, &["login", "plain"]),
        "mcp: \"plain\" has nothing to log in to (a stdio server)",
    );
    assert_error(
        &mcp(cwd, &home, &["login", "nope"]),
        "mcp: no server named \"nope\"\n  configured servers: nb, plain",
    );
    ok(&mcp(
        cwd,
        &home,
        &["add", "off", "--url", &mock.mcp_url(), "--auth", "none"],
    ));
    for verb in ["login", "logout"] {
        assert_error(
            &mcp(cwd, &home, &[verb, "off"]),
            "mcp: \"off\" is declared auth: none; drop that (or set auth: oauth) to log in",
        );
    }
    ok(&mcp(cwd, &home, &["remove", "off"]));

    // Login: the child prints the URL and waits; this test is the browser.
    let lines = cli_login(
        cwd,
        &home,
        &[],
        &["login", "nb", "--no-browser"],
        &mock.base(),
    )
    .await;
    assert!(
        lines[0].starts_with("Redirect: http://127.0.0.1:") && lines[0].ends_with("/callback"),
        "{}",
        lines[0]
    );
    assert_eq!(lines[1], "Client: cid-1 (dynamic registration)");
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(
        lines[5].starts_with("Logged in to nb; the token expires in ")
            && lines[5].ends_with(&format!("(saved to {})", token_file.display())),
        "{}",
        lines[5]
    );
    assert!(token_file.is_file());
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&token_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // With a token file the entry reads `auto: logged in` — the file is what a run goes by.
    let text = ok(&mcp(cwd, &home, &["list"]));
    assert!(text.contains("[auth: auto: logged in]"), "{text}");
    let text = ok(&mcp(cwd, &home, &["get", "nb"]));
    assert!(
        text.ends_with(&format!(
            "  auth: auto (logged in; {})\n",
            token_file.display()
        )),
        "{text}"
    );
    let rows: serde_json::Value =
        serde_json::from_str(&ok(&mcp(cwd, &home, &["list", "--json"]))).unwrap();
    assert_eq!(rows[0]["auth"], "auto");
    assert_eq!(rows[0]["login"], "logged in");
    assert_eq!(rows[1]["auth"], "none");
    assert_eq!(rows[1]["login"], serde_json::Value::Null);

    // A probe connects with the token.
    let text = ok(&mcp(cwd, &home, &["list", "--probe"]));
    assert!(
        text.contains("[auth: auto: logged in]  connected (1 tools)"),
        "{text}"
    );
    assert_eq!(mock.state().bearers.last(), Some(&Some("at-1".to_owned())));

    // Logout: revoked, forgotten.
    assert_eq!(
        ok(&mcp(cwd, &home, &["logout", "nb"])),
        format!("Logged out of nb (forgot {})\n", token_file.display())
    );
    assert!(!token_file.exists());
    assert_eq!(mock.state().revoked, ["rt-1"]);

    // A headless run degrades that one server and says how to get it back — the bare handshake was
    // answered 401, which is the server asking for a login, not a broken connection; the plain one serves.
    let api = wiremock::MockServer::start().await;
    crate::common::transcript::openai_transcript(&api).await;
    fs::write(
        cwd.join(".iota.yaml"),
        format!(
            "providers:\n  p: {{type: openai, key: sk-x, url: {}}}\nmodels:\n  m: p:gpt-test\nagents:\n  default: {{models: [m]}}\n",
            api.uri()
        ),
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, &home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .args(["-m", "hi"]);
    let o = tokio::task::spawn_blocking(move || cmd.output().expect("run"))
        .await
        .expect("join");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(
        err(&o),
        "Warning: mcp server nb: not logged in: run iota mcp login nb\n"
    );
    assert_eq!(out(&o), format!("{}\n", crate::common::transcript::REPLY));
    // The probe says the same in its own words: a login is a step not taken, not a failure.
    let text = ok(&mcp(cwd, &home, &["list", "--probe"]));
    assert!(
        text.contains("[auth: auto]  needs login: iota mcp login nb"),
        "{text}"
    );
    let rows: serde_json::Value =
        serde_json::from_str(&ok(&mcp(cwd, &home, &["list", "--probe", "--json"]))).unwrap();
    assert_eq!(
        rows[0]["probe"],
        serde_json::json!({"state": "failed", "tools": [], "error": "not logged in: run iota mcp login nb"})
    );
}

/// `--client-id` (with `--client-secret-env`) records a client registered out of band: the id as given, the
/// secret as the `${env:VAR}` reference and never the value. The flags say "oauth" on their own — no
/// `--auth oauth` is needed and none is written — and only `--auth none` contradicts them.
#[test]
fn mcp_add_preregistered_client() {
    let (dir, home) = project();
    let cwd = dir.path();
    let user = home.join(".iota.yaml");
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "nb",
            "--url",
            "https://nb.example/api/mcp",
            "--client-id",
            "pre-1",
            "--client-secret-env",
            "NB_SECRET",
            "--no-login",
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added nb (http: https://nb.example/api/mcp) to {}\nNext: iota mcp login nb\n",
            user.display()
        ),
        "a client id means a login: the firm hint, not the conditional one"
    );
    assert_eq!(
        fs::read_to_string(&user).unwrap(),
        "mcp_servers:\n  nb:\n    url: https://nb.example/api/mcp\n    client_id: pre-1\n    client_secret: ${env:NB_SECRET}\n"
    );
    let text = ok(&mcp(cwd, &home, &["get", "nb"]));
    assert!(
        text.ends_with(
            "  client_id: pre-1\n  client_secret: ${env:NB_SECRET}\n  redirect_uri: http://127.0.0.1:17801/callback\n  auth: auto\n"
        ),
        "{text}"
    );
    let rows: serde_json::Value =
        serde_json::from_str(&ok(&mcp(cwd, &home, &["list", "--json"]))).unwrap();
    assert_eq!(rows[0]["client_id"], "pre-1");
    assert_eq!(rows[0]["client_secret"], "${env:NB_SECRET}");
    assert_eq!(rows[0]["redirect_port"], serde_json::Value::Null);
    assert_eq!(rows[0]["redirect_uri"], "http://127.0.0.1:17801/callback");
    assert_eq!(rows[0]["auth"], "auto");
    assert_eq!(rows[0]["login"], serde_json::Value::Null);
    let text = ok(&mcp(cwd, &home, &["list"]));
    assert!(
        text.contains("[auth: auto]  redirect: http://127.0.0.1:17801/callback\n"),
        "{text}"
    );
    // A port of its own is written and shown; `--redirect-port` goes with `--client-id`.
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "nb2",
            "--url",
            "https://nb.example/api/mcp",
            "--auth",
            "oauth",
            "--client-id",
            "pre-2",
            "--redirect-port",
            "18000",
            "--no-login",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert!(
        fs::read_to_string(&user)
            .unwrap()
            .ends_with("  nb2:\n    url: https://nb.example/api/mcp\n    auth: oauth\n    client_id: pre-2\n    redirect_port: 18000\n"),
        "{}",
        fs::read_to_string(&user).unwrap()
    );
    let text = ok(&mcp(cwd, &home, &["get", "nb2"]));
    assert!(
        text.contains("  client_id: pre-2\n  redirect_port: 18000\n  redirect_uri: http://127.0.0.1:18000/callback\n"),
        "{text}"
    );
    assert_error(
        &mcp(
            cwd,
            &home,
            &[
                "add",
                "x",
                "--url",
                "https://x/mcp",
                "--auth",
                "oauth",
                "--redirect-port",
                "18000",
            ],
        ),
        "mcp add: --redirect-port goes with --client-id (a pre-registered client's redirect URI must match exactly; a registered-on-the-spot or metadata-document client gets a random port)",
    );
    assert_error(
        &mcp(
            cwd,
            &home,
            &[
                "add",
                "x",
                "--url",
                "https://x/mcp",
                "--auth",
                "none",
                "--redirect-port",
                "18000",
            ],
        ),
        "mcp add: --client-id, --client-secret-env and --redirect-port describe an OAuth login, which --auth none rules out",
    );
    fs::write(
        alt_or_new(cwd),
        "mcp_servers:\n  s: {url: \"https://x/mcp\", auth: oauth, redirect_port: 18000}\n",
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, &home)
        .current_dir(cwd)
        .args(["config", "check", "-c"])
        .arg(alt_or_new(cwd));
    assert_error(
        &cmd.output().expect("run"),
        "mcp_servers.s: redirect_port needs a client_id (only a pre-registered client has a fixed redirect URI)",
    );
    // A project-scope entry carries the reference, never the secret, so it passes the secret check.
    let o = mcp(
        cwd,
        &home,
        &[
            "add",
            "nb",
            "--scope",
            "project",
            "--url",
            "https://nb.example/api/mcp",
            "--auth",
            "oauth",
            "--client-id",
            "pre-1",
            "--client-secret-env",
            "NB_SECRET",
            "--no-login",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));

    let cases: &[(&[&str], &str)] = &[
        (
            &[
                "add",
                "x",
                "--url",
                "https://x/mcp",
                "--auth",
                "none",
                "--client-id",
                "pre-1",
            ],
            "mcp add: --client-id, --client-secret-env and --redirect-port describe an OAuth login, which --auth none rules out",
        ),
        (
            &[
                "add",
                "x",
                "--url",
                "https://x/mcp",
                "--auth",
                "none",
                "--client-secret-env",
                "V",
            ],
            "mcp add: --client-id, --client-secret-env and --redirect-port describe an OAuth login, which --auth none rules out",
        ),
        (
            &["add", "x", "--client-id", "pre-1", "--", "srv"],
            "mcp add: --client-id applies to --url servers only",
        ),
        (
            &[
                "add",
                "x",
                "--url",
                "https://x/mcp",
                "--auth",
                "oauth",
                "--client-secret-env",
                "NB_SECRET",
            ],
            "mcp add: --client-secret-env wants the NAME of an environment variable, beside --client-id; got \"NB_SECRET\"",
        ),
        (
            &[
                "add",
                "x",
                "--url",
                "https://x/mcp",
                "--auth",
                "oauth",
                "--client-id",
                "pre-1",
                "--client-secret-env",
                "not a name",
            ],
            "mcp add: --client-secret-env wants the NAME of an environment variable, beside --client-id; got \"not a name\"",
        ),
    ];
    for (args, want) in cases {
        assert_error(&mcp(cwd, &home, args), want);
    }

    // The config the run loads refuses the keys where they make no sense: beside `auth: none`, or on a
    // stdio server. (A cross-layer rule of the load, so no file prefix — the same as the `auth: oauth`
    // rule above.) Without `auth:` they are fine: the login is the server's to ask for.
    let alt = cwd.join("alt.yaml");
    for (body, want) in [
        (
            "mcp_servers:\n  s: {url: \"https://x/mcp\", auth: none, client_id: c}\n",
            "mcp_servers.s: client_id/client_secret/redirect_port describe an OAuth login, which `auth: none` rules out",
        ),
        (
            "mcp_servers:\n  s: {command: srv, client_id: c}\n",
            "mcp_servers.s: client_id/client_secret/redirect_port describe an OAuth login, and a stdio server has nothing to log in to",
        ),
    ] {
        fs::write(&alt, body).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
        cleared_env(&mut cmd, &home)
            .current_dir(cwd)
            .args(["config", "check", "-c"])
            .arg(&alt);
        assert_error(&cmd.output().expect("run"), want);
    }
    fs::write(
        &alt,
        "providers:\n  p: {key: k}\nmcp_servers:\n  s: {url: \"https://x/mcp\", client_id: c}\n",
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, &home)
        .current_dir(cwd)
        .args(["config", "check", "-c"])
        .arg(&alt);
    let o = cmd.output().expect("run");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    fs::write(
        &alt,
        "mcp_servers:\n  s: {url: \"https://x/mcp\", auth: oauth, client_secret: \"${env:V}\"}\n",
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, &home)
        .current_dir(cwd)
        .args(["config", "check", "-c"])
        .arg(&alt);
    assert_error(
        &cmd.output().expect("run"),
        "mcp_servers.s: client_secret needs a client_id",
    );
}

/// Runs `iota mcp <args>` — a `login nb`, or an `add nb --url …` that starts one — with `extra_env` on top of
/// the cleared environment and acts as the browser:
/// the child's stdout is read until the wait line, the URL it printed is followed (through the redirect to
/// the loopback callback), and the child's whole stdout comes back once it has exited on its own. Its stdin
/// is a pipe held OPEN and never written to — a terminal nobody types into — so the exit also proves the
/// paste reader does not keep the process alive once the browser has come back.
#[cfg(unix)]
async fn cli_login(
    cwd: &Path,
    home: &Path,
    extra_env: &[(&str, &str)],
    args: &[&str],
    mock_base: &str,
) -> Vec<String> {
    use std::io::BufRead as _;

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("mcp")
        .args(args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn iota mcp login");
    let stdin_kept_open = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = tokio::sync::oneshot::channel::<Vec<String>>();
    let reader = tokio::task::spawn_blocking(move || {
        let mut lines = Vec::new();
        let mut tx = Some(tx);
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.expect("read stdout");
            lines.push(line);
            // The URL line is indented under "Open this URL to log in:"; the wait line follows it.
            if lines
                .iter()
                .any(|l| l.starts_with("Waiting for the browser"))
                && let Some(tx) = tx.take()
            {
                let _ = tx.send(lines.clone());
            }
        }
        lines
    });
    let Ok(Ok(head)) = tokio::time::timeout(std::time::Duration::from_secs(30), rx).await else {
        let _ = child.kill();
        let lines = reader.await.expect("reader");
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut e, &mut stderr);
        }
        panic!("the login never reached the wait line; stdout: {lines:?}; stderr: {stderr}");
    };
    let at = head
        .iter()
        .position(|l| l == "Open this URL to log in:")
        .expect("the URL line");
    let url = head[at + 1].trim().to_owned();
    assert!(url.starts_with(&format!("{mock_base}/authorize?")), "{url}");
    assert_eq!(
        head[at + 2],
        "Waiting for the browser to come back (5m), or paste the redirect URL here:"
    );
    let page = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .expect("follow the redirect")
        .text()
        .await
        .expect("callback page");
    assert!(
        page.contains("Logged in to nb. You can close this window."),
        "{page}"
    );
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || child.wait().expect("wait")),
    )
    .await
    .expect("the login exits on its own with stdin still open")
    .expect("join");
    drop(stdin_kept_open);
    let lines = reader.await.expect("reader");
    assert!(
        status.success(),
        "login exit: {status:?}; stdout: {lines:?}"
    );
    lines
}

/// The namebeta shape through the CLI: an entry with `--client-id`/`--client-secret-env` logs in as that
/// client with the secret read from the environment, and a run refreshes with it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_login_as_a_preregistered_client_through_the_cli() {
    use crate::common_oauth as oauth_mock;

    let mock = oauth_mock::start_with(oauth_mock::Options {
        identity: oauth_mock::Identity::Preregistered {
            client_id: "pre-1".to_owned(),
            client_secret: Some("s3cret".to_owned()),
        },
        as_path: "/oidc",
        ..oauth_mock::Options::default()
    })
    .await;
    let (dir, home) = project();
    let cwd = dir.path();
    let token_file = home.join(".iota/mcp/auth/nb.json");
    ok(&mcp(
        cwd,
        &home,
        &[
            "add",
            "nb",
            "--url",
            &mock.mcp_url(),
            "--auth",
            "oauth",
            "--client-id",
            "pre-1",
            "--client-secret-env",
            "NB_SECRET",
            "--redirect-port",
            "17811",
            "--no-login",
        ],
    ));

    let lines = cli_login(
        cwd,
        &home,
        &[("NB_SECRET", "s3cret")],
        &["login", "nb", "--no-browser"],
        &mock.base(),
    )
    .await;
    assert_eq!(lines[0], "Redirect: http://127.0.0.1:17811/callback");
    assert_eq!(lines[1], "Client: pre-1 (pre-registered)");
    assert_eq!(
        mock.state().authorizations[0].redirect_uri,
        "http://127.0.0.1:17811/callback"
    );
    assert!(token_file.is_file());
    let st = mock.state();
    assert_eq!(st.token_requests[0].client_id.as_deref(), Some("pre-1"));
    assert_eq!(
        st.token_requests[0].client_secret.as_deref(),
        Some("s3cret")
    );
    assert_eq!(
        st.authorizations[0].scope.as_deref(),
        Some("mcp offline_access")
    );

    // The token has aged: the probe refreshes as the same confidential client.
    let mut file: serde_json::Value =
        serde_json::from_slice(&fs::read(&token_file).unwrap()).unwrap();
    file["credentials"]["token_received_at"] = serde_json::json!(1_000_000);
    fs::write(&token_file, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, &home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("NB_SECRET", "s3cret")
        .args(["mcp", "list", "--probe"]);
    let o = tokio::task::spawn_blocking(move || cmd.output().expect("run"))
        .await
        .expect("join");
    let text = ok(&o);
    assert!(
        text.contains(
            "[auth: oauth: logged in]  redirect: http://127.0.0.1:17811/callback  connected (1 tools)"
        ),
        "{text}"
    );
    let st = mock.state();
    assert_eq!(st.grants, ["authorization_code", "refresh_token"]);
    assert_eq!(
        st.token_requests[1].client_secret.as_deref(),
        Some("s3cret")
    );
}

/// A refusal at the browser through the CLI: exit 1 with the server's `error`, `error_description` and
/// `error_uri` in the one line, and — with `IOTA_LOG` set — the whole callback query (the code it never
/// carried aside) on the developer's tap.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_login_refused_says_why_and_logs_the_callback() {
    use std::io::{BufRead as _, Read as _};

    use crate::common_oauth as oauth_mock;

    let mock = oauth_mock::start_with(oauth_mock::Options {
        refusal: Some(oauth_mock::Refusal {
            error: "access_denied".to_owned(),
            description: Some("the user declined the consent screen".to_owned()),
            uri: Some("https://as.example/errors/access_denied".to_owned()),
        }),
        ..oauth_mock::Options::default()
    })
    .await;
    let (dir, home) = project();
    let cwd = dir.path();
    ok(&mcp(
        cwd,
        &home,
        &[
            "add",
            "nb",
            "--url",
            &mock.mcp_url(),
            "--auth",
            "oauth",
            "--no-login",
        ],
    ));
    let log = cwd.join("iota.log");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, &home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("IOTA_LOG", &log)
        .args(["mcp", "login", "nb", "--no-browser"]);
    let mut child = cmd.spawn().expect("spawn iota mcp login");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let reader = tokio::task::spawn_blocking(move || {
        let mut tx = Some(tx);
        let mut lines = Vec::new();
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.expect("read stdout");
            lines.push(line);
            if let Some(at) = lines.iter().position(|l| l == "Open this URL to log in:")
                && lines.len() > at + 1
                && let Some(tx) = tx.take()
            {
                let _ = tx.send(lines[at + 1].trim().to_owned());
            }
        }
        lines
    });
    let url = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .expect("the URL in time")
        .expect("the URL");
    let page = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .expect("follow the redirect")
        .text()
        .await
        .expect("callback page");
    assert!(
        page.contains("Login failed: access_denied — the user declined the consent screen (https://as.example/errors/access_denied)"),
        "{page}"
    );
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || child.wait().expect("wait")),
    )
    .await
    .expect("the login exits")
    .expect("join");
    let _ = reader.await.expect("reader");
    assert_eq!(status.code(), Some(1));
    let mut stderr = String::new();
    stderr_pipe
        .read_to_string(&mut stderr)
        .expect("the child's stderr, all of it once it has exited");
    assert_eq!(
        stderr,
        "Error: mcp login nb: the authorization server refused: access_denied — the user declined the consent screen (https://as.example/errors/access_denied)\n"
    );
    let logged = fs::read_to_string(&log).expect("the log file");
    let refused = logged
        .lines()
        .find(|l| l.contains("oauth callback refused: "))
        .expect("the callback is logged");
    assert!(
        refused.contains(" DEBUG iota::mcp::auth: oauth callback refused: error=access_denied&state=")
            && refused.contains("&error_description=the+user+declined+the+consent+screen&error_uri=https%3A%2F%2Fas.example%2Ferrors%2Faccess_denied")
            && !refused.contains("code="),
        "{refused}"
    );
}

/// `<cwd>/alt.yaml`, for a `config check -c` of one document.
fn alt_or_new(cwd: &Path) -> std::path::PathBuf {
    cwd.join("alt.yaml")
}

/// `add --url` follows the entry up. Nothing said about `auth`, so the endpoint is probed — one bare
/// `initialize` (the mock saw a request with no credential) — and the mock's 401 starts the login on the
/// spot, through `$BROWSER`: the same steps as `iota mcp login`, and the token file is there when `add`
/// returns. `--no-browser` goes through to that login (the URL is printed, this test follows it), and the
/// line before the steps says why they are running.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_add_probes_the_endpoint_and_logs_in() {
    use crate::common_oauth as oauth_mock;

    let mock = oauth_mock::start(3600).await;
    let (dir, home) = project();
    let cwd = dir.path();
    let user = home.join(".iota.yaml");
    let token_file = home.join(".iota/mcp/auth/nb.json");
    let script = cwd.join("browser.sh");
    fs::write(&script, "#!/bin/sh\ncurl -sL \"$1\" >/dev/null 2>&1 &\n").unwrap();
    let browser = format!("sh {}", script.display());

    let o = mcp_with(
        cwd,
        &home,
        &[("BROWSER", &browser)],
        &["add", "nb", "--url", &mock.mcp_url()],
    );
    let text = ok(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        format!("Added nb (http: {}) to {}", mock.mcp_url(), user.display())
    );
    assert_eq!(lines[1], "The server asks for a login; starting it…");
    assert!(
        lines[2].starts_with("Redirect: http://127.0.0.1:") && lines[2].ends_with("/callback"),
        "{}",
        lines[2]
    );
    assert_eq!(lines[3], "Client: cid-1 (dynamic registration)");
    assert_eq!(lines[4], "Open this URL to log in:");
    assert!(
        lines[5]
            .trim()
            .starts_with(&format!("{}/authorize?", mock.base())),
        "{}",
        lines[5]
    );
    assert_eq!(
        lines[6],
        "Waiting for the browser to come back (5m), or paste the redirect URL here:"
    );
    assert!(
        lines[7].starts_with("Logged in to nb; the token expires in ")
            && lines[7].ends_with(&format!("(saved to {})", token_file.display())),
        "{}",
        lines[7]
    );
    assert_eq!(lines.len(), 8, "{text}");
    assert!(token_file.is_file());
    let seen = mock.state();
    assert_eq!(
        seen.bearers,
        [None],
        "the probe: one request, no credential"
    );
    assert_eq!(seen.authorizations.len(), 1, "one login");
    assert!(
        !fs::read_to_string(&user).unwrap().contains("auth:"),
        "the entry stays `auto`: the token file is what makes the next connect an OAuth one"
    );
    let text = ok(&mcp(cwd, &home, &["list"]));
    assert!(text.contains("[auth: auto: logged in]"), "{text}");

    // `--no-browser` reaches the login `add` starts: the URL is printed and waited on, nothing is opened.
    let (dir, home) = project();
    let cwd = dir.path();
    let lines = cli_login(
        cwd,
        &home,
        &[],
        &["add", "nb", "--url", &mock.mcp_url(), "--no-browser"],
        &mock.base(),
    )
    .await;
    assert!(
        lines[0].starts_with("Added nb (http: ")
            && lines[1] == "The server asks for a login; starting it…",
        "{lines:?}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("Logged in to nb; the token expires in ")),
        "{lines:?}"
    );
    assert!(home.join(".iota/mcp/auth/nb.json").is_file());
    assert_eq!(mock.state().authorizations.len(), 2);
}

/// The follow-ups of `add --url` that make no login: `--no-login` writes the entry and the hint it earns
/// (conditional for `auto`, firm for `--auth oauth`) without a request of any kind; an endpoint that
/// answers the bare request is left at `Added …`, probed once; one that could not be reached keeps the
/// entry and the hint, exit 0; `--auth none` and an own `Authorization` header are never probed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_add_probes_or_not_without_a_login_to_make() {
    use crate::common_oauth as oauth_mock;

    let mock = oauth_mock::start(3600).await;
    let (dir, home) = project();
    let cwd = dir.path();
    let user = home.join(".iota.yaml");
    let url = mock.mcp_url();

    // (b) `--no-login`: the entry, the hint, no request — the mock sees nothing at all.
    assert_eq!(
        ok(&mcp(
            cwd,
            &home,
            &["add", "nb", "--url", &url, "--no-login"]
        )),
        format!(
            "Added nb (http: {url}) to {}\nif the server asks for a login: iota mcp login nb\n",
            user.display()
        )
    );
    assert_eq!(
        ok(&mcp(
            cwd,
            &home,
            &[
                "add",
                "forced",
                "--url",
                &url,
                "--auth",
                "oauth",
                "--no-login"
            ]
        )),
        format!(
            "Added forced (http: {url}) to {}\nNext: iota mcp login forced\n",
            user.display()
        )
    );
    // (e) `--auth none`, an own `Authorization` header: nothing to log in to, so nothing is asked.
    assert_eq!(
        ok(&mcp(
            cwd,
            &home,
            &["add", "off", "--url", &url, "--auth", "none"]
        )),
        format!("Added off (http: {url}) to {}\n", user.display())
    );
    assert_eq!(
        ok(&mcp(
            cwd,
            &home,
            &[
                "add",
                "keyed",
                "--url",
                &url,
                "--header",
                "Authorization: Bearer ${env:KEY}",
            ]
        )),
        format!("Added keyed (http: {url}) to {}\n", user.display())
    );
    let seen = mock.state();
    assert!(
        seen.bearers.is_empty(),
        "no probe was made: {:?}",
        seen.bearers
    );
    assert!(seen.authorizations.is_empty());
    assert!(
        !home.join(".iota/mcp/auth").exists(),
        "no token file of any name"
    );

    // (c) A server that takes the bare request: probed once, and `Added` is all there is to say.
    let open = oauth_mock::start_with(oauth_mock::Options {
        open: true,
        ..oauth_mock::Options::default()
    })
    .await;
    assert_eq!(
        ok(&mcp(cwd, &home, &["add", "free", "--url", &open.mcp_url()])),
        format!(
            "Added free (http: {}) to {}\n",
            open.mcp_url(),
            user.display()
        )
    );
    let seen = open.state();
    assert_eq!(seen.bearers, [None], "probed once, no credential");
    assert!(seen.authorizations.is_empty(), "no login was started");
    assert!(!home.join(".iota/mcp/auth/free.json").exists());

    // (d) An endpoint nobody answers at: the entry is written, the hint stays conditional, the reason is
    // in the line, and the command succeeds — the file write did.
    let text = ok(&mcp(
        cwd,
        &home,
        &["add", "t", "--url", "http://127.0.0.1:1/mcp"],
    ));
    assert!(
        text.starts_with(&format!(
            "Added t (http: http://127.0.0.1:1/mcp) to {}\ncould not reach http://127.0.0.1:1/mcp (",
            user.display()
        )),
        "{text}"
    );
    assert!(
        text.ends_with("); if the server asks for a login: iota mcp login t\n"),
        "{text}"
    );
    assert!(
        fs::read_to_string(&user)
            .unwrap()
            .contains("  t:\n    url: http://127.0.0.1:1/mcp\n"),
        "{}",
        fs::read_to_string(&user).unwrap()
    );

    // The two flags belong to the `--url` form.
    assert_error(
        &mcp(cwd, &home, &["add", "x", "--no-login", "--", "srv"]),
        "mcp add: --no-login applies to --url servers only",
    );
    assert_error(
        &mcp(cwd, &home, &["add", "x", "--no-browser", "--", "srv"]),
        "mcp add: --no-browser applies to --url servers only",
    );
}

/// (f) The login `add` started did not finish — the authorization server refused: the entry stays in the
/// file, the error is `login`'s own with the way to retry under it, and the exit code says the command did
/// not do all it set out to.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_add_keeps_the_entry_when_its_login_fails() {
    use crate::common_oauth as oauth_mock;

    let mock = oauth_mock::start_with(oauth_mock::Options {
        refusal: Some(oauth_mock::Refusal {
            error: "access_denied".to_owned(),
            description: Some("the user declined the consent screen".to_owned()),
            uri: None,
        }),
        ..oauth_mock::Options::default()
    })
    .await;
    let (dir, home) = project();
    let cwd = dir.path();
    let user = home.join(".iota.yaml");
    let script = cwd.join("browser.sh");
    fs::write(&script, "#!/bin/sh\ncurl -sL \"$1\" >/dev/null 2>&1 &\n").unwrap();
    let browser = format!("sh {}", script.display());

    let o = mcp_with(
        cwd,
        &home,
        &[("BROWSER", &browser)],
        &["add", "nb", "--url", &mock.mcp_url()],
    );
    assert_eq!(o.status.code(), Some(1), "stdout: {}", out(&o));
    let text = out(&o);
    assert!(
        text.starts_with(&format!(
            "Added nb (http: {}) to {}\nThe server asks for a login; starting it…\nRedirect: http://127.0.0.1:",
            mock.mcp_url(),
            user.display()
        )),
        "{text}"
    );
    assert!(text.contains("Open this URL to log in:\n"), "{text}");
    assert!(!text.contains("Logged in to"), "{text}");
    assert_eq!(
        err(&o),
        "Error: mcp login nb: the authorization server refused: access_denied — the user declined the consent screen\n  Retry with: iota mcp login nb\n"
    );
    assert!(
        fs::read_to_string(&user)
            .unwrap()
            .contains(&format!("  nb:\n    url: {}\n", mock.mcp_url())),
        "the entry stays: {}",
        fs::read_to_string(&user).unwrap()
    );
    assert!(!home.join(".iota/mcp/auth/nb.json").exists());
    assert_eq!(mock.state().authorizations.len(), 1);
    // …and the retry is the plain login, which finds the entry as written.
    let text = ok(&mcp(cwd, &home, &["get", "nb"]));
    assert!(text.ends_with("  auth: auto\n"), "{text}");
}
