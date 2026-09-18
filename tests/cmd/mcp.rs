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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iota"));
    cleared_env(&mut cmd, home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .arg("mcp")
        .args(args);
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
             "defer": "file tools", "auth": "none"},
            {"name": "gh", "transport": "http", "file": user.display().to_string(), "command": "",
             "args": [], "url": "https://gh.example/mcp", "env": {},
             "headers": {"Authorization": "Bearer ${env:GH}", "X-Client": "iota"}, "defer": null, "auth": "header"}
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
        &["add", "gh", "--url", "https://user.example/mcp"],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added gh (http: https://user.example/mcp) to {}\n",
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
            "MCP servers:\n  gh  http   {}  [auth: none]\n",
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
        ],
    );
    assert_eq!(
        ok(&o),
        format!(
            "Added t (http: http://127.0.0.1:1/mcp) to {}\n",
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
            "MCP servers:\n  s  stdio  {a}  [auth: none]\n  t  http   {a}  [auth: none]\n",
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

/// `--auth oauth` is recorded as `auth: oauth`, listed as such, and followed by the login hint; the agent
/// that runs by default is told when it lists its servers explicitly and left the new one out.
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
            "MCP servers:\n  nb  http   {}  [auth: oauth]\n",
            user.display()
        )
    );
    // `--auth none` writes nothing, and a name the agent lists draws no hint.
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
        fs::read_to_string(&user)
            .unwrap()
            .contains("mcp_servers:\n  fs:\n    url: https://fs.example/mcp\n  nb:\n"),
        "{}",
        fs::read_to_string(&user).unwrap()
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
