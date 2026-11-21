# Grants — one store for "the user allowed this", kept with the session and the project

Status: **implemented** (2026-11-19). Where this document and the code differ,
DIVERGENCES X-110..X-113 win: they record what shipped. The decisions it revised are
X-44, X-91, X-92 and X-95; §10 holds the rows as drafted. §9 answers the twelve open
questions one by one; §11 is the task list.

## 1. The problem

Three things the user can allow today, three unrelated memories, none of them kept:

- **A directory.** `write_roots:` is read when the tools are assembled, so "a running
  session cannot gain a root" (code-toolset.md, Approval). A session finds out where it
  must write as it goes, and the only runtime answer is the dialog's `Allow in <dir> for
  this session` — which is one tool's grant for one exact directory
  (`src/repl/turn/approval.rs:49`, keyed `(wire tool name, Option<scope dir>)`). It does
  not reach `shell`: the sandbox's writable set is fixed at assembly
  (`src/tool/builtins/shell.rs:115,168`), so after "allow this directory" `cargo build`
  still cannot write there, and a sandboxed command cannot be prompted mid-run.
- **A tool.** `Allow for this session` for `edit_file` or an unsandboxed `shell` lives
  in the same `HashSet` and dies on `/new`, on a `/session` resume and with the process
  (X-92).
- **An MCP tool.** `requires_approval` is `!server.auto_run`
  (`src/mcp/manager.rs:985-987`): all of a server's tools or none, forever, in a config
  file — and a `--mcp` server has no entry to write it in.

The user answers the same question again after every restart, and the one question that
matters most for a build (may the shell write there) cannot be asked at all.

## 2. The decision

**One grant store** (`tool::grants::Grants`), read by every enforcement point and written
through four doors:

| door | writes | level |
|---|---|---|
| the approval dialog | a directory, an MCP tool, a built-in tool | session or project |
| `/allow`, `/revoke` (chat) | a directory, an MCP tool or `server:*`, a built-in tool | session or project |
| `--allow <target>` (CLI) | a directory, a built-in tool, `server:tool`, `server:*` | the run (the static level, §5.3) |
| `write_roots:` (config) | a directory | every run (a static root, unchanged) |

Two axes, kept apart — the split every surveyed product with both has (Claude Code's
additional directories vs its permission mode, Codex's `writable_roots` vs its approval
policy, Copilot's allowed directory list vs its tool approvals):

- **WHERE** — a *directory grant* is a write root added at run time. It widens the one
  boundary the code writers' jail and the shell sandbox share, exactly as a
  `write_roots:` entry does, and changes nothing else: inside it a code write is asked
  or not by the writer's own tool grant (`allow:`, `--allow`, `/allow`), like a write in the
  project root.
- **WHETHER** — a *tool grant* removes the question for one tool: a built-in
  (`edit_file`, `write_file`, an unsandboxed `shell`), an MCP tool (`server:tool`) or
  every tool of an MCP server (`server:*`).

Grants persist at two levels: **session** (the bundle's `meta.json`) and **project**
(`grants.json` in the project's bucket, beside its bundles). What is in effect is the
union of both, plus the static roots.

What does NOT change: `outside_writes:` stays a config key and `refuse` still means no
prompt; the protected table still beats every root and every grant; an unverifiable
location is still refused; `auto_write`/`auto_run` keep their meaning (until X-117
retires them for `allow:`); nothing new is trusted (§9 Q11).

## 3. The grant model

### 3.1 Levels

| level | lifetime | where | who reads it |
|---|---|---|---|
| static | the process | `write_roots:` into `ToolEnv::write_roots`; the agent's `allow:` then `--allow` into the store's static level (docs/design/config-allow.md §4.1), each resolved once | as today |
| project | until revoked | `~/.iota/sessions/projects/<slug>/grants.json` | every run rooted at that project, `-m` included |
| session | the session | `meta.json` `grants` | that session: now, and every resume of it |

There is no precedence to define: levels only add. Revoking at one level leaves the
other alone.

### 3.2 Keys

```rust
// src/tool/grants.rs
pub enum Level { Session, Project }

pub enum ToolKey {
    /// A built-in tool, by wire name. Either level.
    Builtin(String),
    /// One MCP tool, or every tool of the server.
    Mcp { server: ServerId, tool: McpTool },
}
pub enum ServerId {
    /// An `mcp_servers:` entry: its CONFIGURED name, never the sanitized wire segment.
    Config(String),
    /// A `--mcp` server: the flag's value, trimmed, verbatim.
    Flag(String),
}
pub enum McpTool { Named(String) /* the server's raw tool name */, Any }

/// One level's content — the persisted shape.
pub struct GrantSet { pub dirs: Vec<PathBuf>, pub tools: Vec<ToolKey> }
```

> **Amended 2026-11-20 (grants-builtin-project, DIVERGENCES X-110).** The rule is now
> symmetric: every kind of grant has both levels.

Every kind of grant has a session and a project level, and each also has a permanent,
global form that is not a grant: a directory's is `write_roots:` or `allow:` (or `--allow` for
one run), an MCP tool's is `mcp_servers.<name>.auto_run`, a built-in's is the agent's
`allow: [edit_file, write_file, shell]` (X-117). Those keys live in the home or agent
config and apply to EVERY project, and the repository's own `.iota.yaml` is shared and
trust-gated, so none of them can say "in this repository, stop asking". In every case
the project level is that per-repository answer. A built-in grant is still keyed by its
wire name and still never answers a call bound to a location (§3.4).

### 3.3 A directory grant

- **It is a real location.** The path is resolved when the grant is made
  (`app::fs::physical`: symlinks resolved, the nearest existing ancestor canonicalized)
  and that absolute real path is what is stored, shown and matched. `/allow ~/link`
  where `link → /data/x` grants `/data/x` and says so.
- **It covers the subtree.** A grant for `D` admits `D/sub/file`. The sandbox can
  express nothing else — Seatbelt's `subpath` and a bwrap bind are both trees — and a
  grant that meant one thing to `write_file` and another to `cargo build` is the trap
  X-91 exists to avoid. (X-92's "only the exact directory" is revised, §10.)
- **It is matched on real locations, at call time.** The code jail compares the
  target's real location, resolved at the call, with the stored path
  (`CodeSet::admits_real`). A link inside `D` that leads out of every root is OUTSIDE,
  as under a `write_roots:` entry.
- **It does not follow the name.** If `D` is later replaced by a symlink (or one of its
  ancestors is), no real location lies under the stored path any more, so the jail
  admits nothing through it. The sandbox must reach the same answer and would not by
  itself (Seatbelt adds a root's canonical form, bwrap binds through a link), so a
  grant is **live** only while `physical(D) == D`; a stale grant is left out of the
  sandbox's roots, shown `(stale)` by `/allow`, and kept on disk — it is live again if
  the directory comes back.
- **It never opens a protected path.** A granted directory that is a repository gets
  the git rows a `write_roots:` entry gets (`app::protected::table`'s `extra`), and the
  table is still classified before inside/outside.

### 3.4 Matching a tool grant

A gated call that is NOT bound to a location (`Tool::approval_subject` is `None`) runs
unasked when the store holds its key at either level:

- built-in: `Builtin(<wire name>)`;
- MCP: `Mcp { server, Named(<raw name>) }` or `Mcp { server, Any }`. `Any` is evaluated
  at call time by server identity, so it covers a tool the server starts exposing later
  (§9 Q8).

A call bound to a location (an OUTSIDE or PROTECTED write) is never answered by a tool
grant — an inside grant still never covers an outside write.

## 4. Where each level is stored

### 4.1 Session: `meta.json`

```jsonc
{ "v": 2, "id": "…", …,
  "grants": {                                   // omitted when empty
    "dirs":  ["/Users/x/scratch"],
    "tools": [ {"builtin": "edit_file"},
               {"server": "github", "tool": "search_issues"},
               {"flag": "npx -y @acme/mcp", "tool": "*"} ] } }
```

`SESSION_SCHEMA_VERSION` stays 2. The version exists to tell two MEANINGS of one key
apart (v1 `provider` is a type, v2 a name); an optional key that is absent in every
older bundle changes no meaning, and "absent" already reads as "no grants".

Written through `SessionWriter::update_meta` (`src/session/writer.rs:352`), i.e. under
the bundle's single-writer lock and by the existing whole-file meta write; a pending
bundle carries the value in memory until its first write, like every other meta field.

### 4.2 Project: `grants.json` in the bucket

`~/.iota/sessions/projects/<slug(root)>/grants.json`, mode 0600, written whole with
`app::fs::write_atomic` (the trust record's writer):

```jsonc
{ "v": 1,
  "roots": {
    "/Users/x/proj": {                           // the project root's REAL path
      "dirs":  ["/Users/x/shared-lib"],
      "tools": [ {"server": "github", "tool": "*"} ] } } }
```

The bucket name is lossy (`/a-b/c` and `/a/b/c` share one), so the file is a map keyed
by the canonical real project root and a run reads only its own entry: two projects
that collide on the slug share a file and nothing else (§9 Q1). `list_bucket` reads
subdirectories only and `find_dir` needs `<ulid>/meta.json`, so the file is invisible
to every session listing. The bucket is `projects/<slug(ToolEnv::root())>`, created on
the first project-level write.

> **Rewritten 2026-11-20 (grants-mode, DIVERGENCES X-110).** As first shipped (0.12.0) the
> project level existed "in every mode … a chat whose sessions use the flat layout has
> project grants too". That was wrong twice: `mode: chat` has no project by its own
> contract, and its key fell back to the working directory, so `/allow project .` in `~`
> wrote a bucket for the home directory that every later run under it inherited.

**Which runs have a project level.** Only a run that HAS a project, decided once at
assembly (`cmd::project_file`):

1. the agent has a workspace — `mode: agent` or `mode: bot`
   (`AgentMode::has_workspace`). A `mode: chat` agent has no `AGENTS.md` overlay, no
   skills set and a flat session; it has no project either, whatever directory it runs
   in;
2. and its root is one a project may stand on: never `/`, the home directory or an
   ancestor of it. `agents::project_root` is the nearest `.git`, else the working
   directory itself, so an agent started in `~` (or above it) with no repository would
   otherwise key a bucket by the home directory. The predicate is the dialog's own
   (`code::offers_root`, §5.1), applied to the root's real location — one rule, one
   function.

A run without one has NO project level anywhere: no `grants.json` is read at the start
(one left there by 0.12.0 is ignored, not deleted), none is written, the dialog offers no
`…for this project` item (§5.1), `/allow … project` is refused saying why (§5.2), and the
`Grants` tab and `/status` — which read the store — have no project row to show. The
session level, the directory, built-in and MCP grants at it, `write_roots:`, `allow:` and
`--allow` are the same in every mode. A `-m` run of an agent with a project still
inherits its grants; a `-m` run of a `mode: chat` agent inherits none.

A file that does not parse as `v: 1` grants nothing and warns once (`⚠ grants: cannot
read <path> (<error>); no project grants apply`), and a project-level write then
refuses instead of replacing it. A write is read–modify–write with no lock: the window
is one human granting in two terminals within the same millisecond, and the loser is
asked again.

## 5. The doors

### 5.1 The dialog

The title is unchanged (`<tool> <detail> — allow?`). The items by what the call is:

| the call | items |
|---|---|
| a code write OUTSIDE every root | `Allow once` · `Allow, and make <dir> writable for this session` · `Allow, and make <dir> writable for this project` · `Deny` |
| …whose `<dir>` is `/`, the home directory or an ancestor of it | `Allow once` · `Deny` |
| a PROTECTED write | `Allow once` · `Deny` (unchanged) |
| an MCP tool | `Allow once` · `Always allow <server>:<tool> in this session` · `Always allow <server>:<tool> in this project` · `Deny` |
| a built-in tool (a write inside the roots, a `shell` call outside the sandbox) | `Allow once` · `Allow for this session` · `Allow for this project` · `Deny` |

In a run with no project (§4.2: a `mode: chat` agent, or a root at the home directory or
above) the `…project` item is not there: three items — `Allow once`, the session grant,
`Deny` — for a directory, an MCP tool and a built-in tool alike.

`<dir>` is the target's real parent directory, from the one resolution X-92 already
makes (`ApprovalSubject::scope`). Choosing a "make writable" item adds `<dir>` to the
store and lets THIS call run; the next write under `<dir>` is an inside write. The
dialog never offers the wildcard `server:*` (that is `/allow`'s), and never offers to
make the home directory writable — one keypress on a write to `~/x.txt` would otherwise
open `~/.ssh` and every shell rc file; `/allow ~`, typed out, still works.

### 5.2 `/allow`

> **Rewritten 2026-11-20 (docs-readme)** to say what shipped (DIVERGENCES X-111). The plan
> had a second command, `/revoke`, and a bare `/allow` that printed a text listing; the
> allow-panel task folded both into the bare `/allow`'s surface, and grants-builtin-project
> added the bare-word form for a built-in tool. There is no `/revoke`: typed, it is an
> unregistered `/xyz`. **Amended 2026-11-21 (allow-no-browser):** the surface's `Add` tab, a
> directory browser, is gone — granting is the typed forms only; the bare `/allow` lists and
> takes back. **Amended 2026-11-21 (allow-scope-last, docs/design/allow-completion.md §5):**
> the scope is the LAST word, `/allow <target> [project]`; a line whose first word is
> `project` is refused naming the line to type, and a directory spelling that reads two
> ways is refused naming both.

The only door for the shell, since a sandboxed command cannot be prompted mid-run, and the
only one for `server:*`.

```
/allow                              list and take back: Grants (· Static)
/allow <dir>                        make <dir> writable for this session
/allow <server>:<tool>              stop asking for one MCP tool (session)
/allow <server>:*                   …for every tool of the server, those it exposes later included
/allow <tool>                       stop asking for a built-in tool (session)
/allow <…> project                  any of the four above, for this project, in every session
```

The scope is the last word: when the last whitespace-separated word is exactly `project`,
the level is the project and the target is what precedes it; otherwise the session, and the
target is the whole argument. A first word `project` — alone or before anything — is
refused before anything else is read, naming the line to type: `/allow: the scope goes
last: /allow shell project` (with nothing after it, `/allow <target> project`); a target
never starts with the scope word. A directory named `project` is `./project` (for the
project, `./project project`); one whose name ends in ` project` takes a trailing `/`
(`/allow ~/my project/`).

`/allow … project` needs a project (§4.2). In a run without one it is refused before the
target is read: `/allow: this run has no project (mode: chat); an agent with mode: agent
has one`; for an agent whose root is too wide, `/allow: this run has no project: <root>
is /, the home directory or an ancestor of it, which is never a project`; without a home,
`/allow: this run has no project (no home directory to keep its grants in)`.

- **Arguments**, read in this order. (1) A CONNECTED server's whole name, the longest that
  matches, then `:` and the tool — a `--mcp` server is named by its URL or `argv[0]`, which
  may hold a `:` and start with `/` or `.` (`/allow https://h/mcp:push`, `/allow
  ./srv.py:*`), so a server's name wins over the directory spelling. (2) A directory: it
  starts with `/`, `~` or `.` (`~` is home, a relative one hangs off the project root, as
  the tools read a path); it must exist and be a directory, and its REAL location is what
  is stored and shown. (3) Holding a `:`: `<server>:<tool>`, split at its last `:`; an
  empty half is refused. (4) Otherwise a bare word: a built-in tool by its wire name, one
  the run has and whose argument-less `requires_approval` probe asks (`shell`,
  `edit_file`, `write_file`) — a tool grant never answers
  a call bound to a location (§3.4). `<tool>` of a server is resolved against its current
  tool list (`*` excepted), so a typo is refused instead of stored. A directory is never
  guessed: when the scope was taken off a target spelled as a directory and the WHOLE
  argument is itself an existing directory (`~/my project` beside `~/my`), the line is
  refused naming both spellings.
- **The bare `/allow`** grants nothing. With no grant and no static root it opens no
  surface and prints one line: `No grants. /allow <dir> makes a directory writable for this
  session; /allow <dir> project for this project.` (in a run with no project: `No grants.
  /allow <dir> makes a directory writable for this session.`). Otherwise it opens a tabbed
  surface, Enter committing the focused tab:
  - `Grants` — a multi-select of every grant in effect, one row each, the level then the
    key: `this session  /path`, `this project  /path (stale)`, `this session  srv:tool`,
    `this project  shell`, a `--mcp` grant followed by `(--mcp <spec>)`. With nothing
    granted (a `Static` tab beside it) its prompt is the line above. Committing takes each
    checked grant back from every level that holds it (the project's file first).
  - `Static` — only when the config or the command line allows something: a read-only
    view, prompt `From the config and the command line: not grants made here, so none can
    be revoked here — edit agents.<name>.allow, or drop the flag.`, one row each,
    the agent's `allow:`, then `--allow` (each directories, then tools) — `allow:   /path`,
    `allow:   shell`, `--allow  /path` (docs/design/config-allow.md). Enter there closes the surface
    and does nothing.

  A cancel, or a commit with nothing checked, changes nothing.
- **Who may type it.** The user, at the prompt. A message from the socket never runs a
  slash command (`src/repl/inbound.rs`), and the model has no way to type one.
- **Table.** One row after `/tools`: `/allow` — `Stop asking for a tool or make a directory
  writable (type its path); bare, list and revoke grants`. X-42 kept configuration out of
  the chat; a grant is not configuration — it is the answer to a question only the chat
  asks.
- **Completion** (docs/design/allow-completion.md §3). `/allow ` opens a list of the lines
  that would grant if submitted now, right after the `/allow` row: one row per built-in
  tool that asks and is not granted at either level (`shell` — `stop asking before it
  runs`, a session grant), and after `/allow <tool> ` one `project` row (`for this project
  (<real root>), in every session`) in a run with a project, unless the project level holds
  the tool. A tool granted for this session keeps its `project` row. Then, in config order,
  one row per connected server that is one server under its name and not `auto_run`
  (`github:` — `MCP server · 12 tools`, its value the prefix `/allow github:`), and behind
  that prefix `*` (`every tool of github, those it exposes later included`) and the server's
  tools in its order, each with its own description, each followed by its `project` row; the
  server's row is there only while one of those below it is. A description is cleaned
  (ANSI stripped, other control characters dropped); a server or tool whose name holds
  whitespace or a control character has no row. Directories are never rows. The rows are
  recomputed before every read of the prompt (the first is the chat's start) and when a
  server's status resolves (a background connect finishing, a server going down, a `/tools`
  reconnect ending), and the table re-issued when they changed; they decide nothing — a
  submitted row is parsed like a typed line, and one that outlived its truth is refused by
  the command's own check.
- **`/status`** gains one row, present only when something is granted:
  `Grants   2 for this session, 3 for this project (/allow lists them)`.
- **The model is told through `<environment>`**: its `write roots:` line lists the
  static roots and every live directory grant, re-composed at the next turn. The
  sandboxed `shell` description says once: `The user can make another directory writable
  with /allow <dir>: when a command must write outside these, ask for that instead of
  working around it.` Tool descriptions otherwise name the static roots only.

Output (each one transcript notice line):

```
/allow ~/scratch            ✓ /Users/x/scratch is writable for this session
/allow ~/link               ✓ /data/x is writable for this session (~/link leads there)
/allow ../lib project       ✓ /Users/x/lib is writable for this project (/Users/x/proj)
/allow github:*             ✓ every tool of github runs without asking in this session
/allow github:search_issues project
                            ✓ github:search_issues runs without asking in this project (/Users/x/proj)
/allow shell project        ✓ shell runs without asking in this project (/Users/x/proj)
Grants tab, two checked     ✓ revoked /Users/x/scratch (this session)
                            ✓ revoked github:* (this session, this project)
```

Refusals (prefixed `/allow: `):

```
the scope goes last: /allow <target> project
"<arg>" is a directory too: write "<arg>/" for it, or "<dir>/ project" for <dir> in this project
expected a directory (starting with /, ~ or .) or <server>:<tool>
<arg> is not a directory
<dir> is protected: <why>
<dir> is already writable (the project root | allow: | --allow | this project | this session)
no MCP server named "<s>" is connected (connected: a, b)
server "<s>" has no tool "<t>"
"<s>" names <n> connected servers; add one to mcp_servers: under its own name to grant it
<s>'s tools already run without asking (auto_run)
<what> already runs without asking (this session | this project)
no built-in tool named "<t>" (tools that ask: shell, write_file)
<t> never asks (tools that ask: …)          — or (no built-in tool of this run asks)
could not save the grant (<error>)
could not save the revocation (<error>)
```

A project-level change whose file cannot be read or written is not made: the grant (or
the revocation) is refused with `could not save …`, and nothing changes. A session-level
change that cannot be written into the bundle's meta is made in memory, and its notice is
followed by `Warning: failed to save session: <error>`, as every failed meta write is said.

### 5.3 `--allow` (was `--add-dir`)

`iota run|resume … --allow <target>` (repeatable; docs/design/config-allow.md §3.4, which
replaced `--add-dir <dir>`): `allow:`'s targets, a `--mcp` server also named by its whole
value, joining the store's static level after the agent's `allow:`, in flag order — `--mcp`'s
rule. A relative directory is against the working directory (it is typed in a shell), not
the project root. `-m` honours it, and the jail, the sandbox and `<environment>` read it
through `Grants::roots`. It is not stored in the session; a resume that wants it passes it
again. The unattended refusal now ends `…; pass --allow <dir>, or list its directory in the
agent's `allow:`, to allow it`.

### 5.4 Config

`write_roots:` and `outside_writes:` are unchanged. **No new config key.** A project
grant is deliberately not writable from a config file: a repository's `.iota.yaml`
would then ship approvals.

## 6. Enforcement points

| point | today | becomes |
|---|---|---|
| the gate, `src/repl/turn/approval.rs:41-139` | owns `approved: Mutex<HashSet<…>>`; `reset` | owns no memory: asks, and reports `Answer::{Denied, Once, Granted(Level)}`; `reset` is gone |
| the chat walk, `src/repl/turn/tools.rs:245-280` | `requires_approval` → `approval_subject` → `gate.ask` → `Approved::Dir` | `tool::approval::gate(dispatch, grants, name, args)` → `Run` or `Ask { subject, key }`; a `Granted` answer goes to the store (and to disk) before the call; `RunCtx::approved` is `Once(target)` or `None` |
| the headless loop, `src/headless/run.rs:417-421` | `requires_approval` → refuse | the same `gate()`; `Ask` → refuse. A project (or resumed session) grant therefore runs in `-m` |
| the code jail, `src/tool/builtins/code/mod.rs:237,252,278,413` | `extra: Vec<Root>` fixed at assembly | the static `extra` plus `grants.live_dirs()`, read per call in `admits_real`, `protected_at` and the refusal's roots clause; `approves` loses its `Dir` arm |
| `Approved`, `src/tool/context.rs:36-41` | `Once(PathBuf)`, `Dir(PathBuf)` | `Once(PathBuf)` alone — a granted directory is a root, so a write there is INSIDE and needs no travelling approval |
| the sandbox, `src/tool/builtins/shell.rs:223-230` | `write: self.write_roots.clone()` | `write:` the static roots plus `grants.live_dirs()`, built per call (the `Sandbox` value already is) |
| the protected table, `src/app/protected.rs` | `~/.iota` entry, `trusted.json` tree | plus `~/.iota/sessions` as a tree, `Why::Grants`, with no approval path (§9 Q11) |
| MCP, `src/mcp/manager.rs:985-987` | `!auto_run` | unchanged; the manager gains `Dispatcher::grant_key(name)` → `Mcp { server, Named(raw) }` and `ServerConfig::from_flag: bool` becomes `flag: Option<String>` (the spec) |
| self-calls, `src/shell/selfcall.rs:62-74` | `config trust|untrust|init`, `mcp add|remove` stay in | also any line with `-c`, `--config[=…]`, `--mcp[=…]`, `--add-dir[=…]` (§9 Q7) |
| the harness, `src/agents/harness.rs:214-221` | `write roots:` from `ToolEnv::write_roots`, once | the static roots plus the live grants, recomposed when the set changes |

`tool::approval::gate` is the one function both walks call:

```rust
pub enum Gate { Run, Ask { subject: Option<ApprovalSubject>, key: ToolKey } }

pub fn gate(d: &dyn Dispatcher, grants: &Grants, name: &str, args: &JsonObject) -> Gate {
    if !d.requires_approval(name, Some(args)) { return Gate::Run; }
    let subject = d.approval_subject(name, args);       // the ONE resolution (X-92)
    let key = d.grant_key(name);                        // default: Builtin(name)
    if subject.is_none() && grants.allows(&key) { return Gate::Run; }
    Gate::Ask { subject, key }
}
```

`Tool::requires_approval` keeps meaning "the config gates this call". A caller that
forgets `gate()` asks too often, never too rarely.

## 7. Session lifecycle

| moment | session level | project level |
|---|---|---|
| a fresh start | empty | loaded from `grants.json` |
| `iota resume <id>`, a `/session` resume | REPLACED by the bundle's `meta.grants` | reloaded |
| `/new` | emptied | reloaded |
| a grant or a revoke | memory, then `update_meta` | read–modify–write of `grants.json`, then memory |
| `--no-save` | memory only | written (§9 Q4) |
| `/save` in an ephemeral chat | the snapshot is stamped into the meta it mints | — |
| `iota resume <id> -m …` | loaded, never changed | loaded |

The project column is a run's that has a project (§4.2); in any other it stays empty and
no file is read or written.

The store is ONE `Arc<Grants>` for the process, created at tool assembly
(`src/cmd/mod.rs:511-525`) and shared by `ToolEnv`, the walks and the chat state; the
swaps above are `replace_session` / `replace_project` on it, at the two places
`gate.reset()` is called today (`src/repl/commands/new.rs:107`,
`src/repl/commands/session.rs:236`) and where a start opens its session. No tool is
reassembled. The load is silent: a start, a resume, a `/session` switch and a `-m` run
print nothing about the grants they loaded. `/status`'s `Grants` row and the bare
`/allow` panel are where they are described.

> **Amended 2026-11-20 (no-grants-line, DIVERGENCES X-110).** As first shipped, a start
> or a resume that loaded anything said so once — `grants: 2 for this session, 3 for this
> project (/allow lists them)` in a chat, `grants: …` on stderr for `-m`. The owner wants
> nothing printed at startup about grants, so the line is gone.

## 8. What the model reads

- **`shell`, sandboxed** — one sentence appended to `SHELL_DESC_SANDBOXED`: `A write
  outside them fails with the OS's own permission error; the user can make a directory
  writable with /allow <dir>, so ask instead of working around it.` Nothing is appended
  to a command's result (§9 Q6).
- **An outside write, nobody to ask** — `path is outside the project root (<roots>):
  <arg> — writing there needs the user's approval, which this non-interactive run
  cannot ask for; pass --allow <dir>, or list its directory in the agent's `allow:`, to
  allow it`.
- **The roots clause** of every jail refusal (`the extra write roots (<e1>, <e2>)`)
  lists the static roots and the live grants, read at the call.
- **An MCP tool, nobody to ask** — `<name> was not executed: the tools of MCP server
  "<server>" need the user's approval, which this non-interactive run cannot ask for;
  set `mcp_servers.<server>.auto_run: true`, or allow it for the project from a chat
  (/allow <server>:<tool> project).` For a `--mcp` server the first clause stays
  `write it as an `mcp_servers` entry with `auto_run: true``. In a run without a project
  (§4.2) — where `/allow … project` is refused — the sentence ends after that first
  clause: `…; set `mcp_servers.<server>.auto_run: true`.`
- **The grants themselves** — `Why::Grants`: `<path> is protected: it holds the
  permissions the user granted — no tool may write it; the user grants with /allow`.

## 9. The open questions

**Q1. The project bucket's file: name and identity.** `grants.json`, a map keyed by the
canonical real project root; the bucket is NOT rekeyed. *I disagree with the PM on both
halves.* The name: `project.json` promises a project record and v1 has only grants to
put in it; a file is named for what it holds. The identity: "record the root, validate
on load" has no good answer for the mismatch it detects — the second of two colliding
projects could then never have project grants — while a map keyed by root makes the
collision harmless at the cost of one level of JSON. Rekeying the bucket by a hash
would orphan every existing bundle and break the bucket/bot-memory key equality
(bot-memory-scope.md §2.2) to fix a collision that this file can absorb by itself.

**Q2. What is a directory grant?** The real path, resolved once when granted; it
covers the subtree; matching compares real locations at call time; it does not survive
`D` becoming a symlink — the grant is for the location, and a stale grant (`physical(D)
!= D`) is ignored by both consumers and shown `(stale)`. §3.3.

**Q3. Session semantics.** Confirmed as the PM has them: `/new` inherits nothing at
session level, a resume restores, `/session` replaces (never merges — a merge would
leak one conversation's approvals into another). Project grants are reloaded at each of
the three, so they follow the project, not the conversation. Load and save: §7 — the
meta write is the writer's, under the bundle lock.

**Q4. May `--no-save` write project grants?** Yes, agreed. `--no-save` promises that
the conversation leaves no trace; a project grant is not conversation, and the user
typed or chose the word "project". Session-level grants of an ephemeral chat stay in
memory and are stamped by `/save`.

**Q5. The shell's door.** `/allow`, with `/revoke`; `project` as a keyword; one line
per outcome; listed by a bare `/allow`; one `Grants` row in `/status`. §5.2. Two
additions to the PM's sketch: it takes MCP keys too (one store, one command), and it
is the only door for `server:*`.

**Q6. The sandbox denial hint.** No line is appended to a failed command, fixed or
sniffed. Agreed that sniffing is out; *but a fixed line is worse than the PM's "maybe"*
— nearly every non-zero exit in a session is a failing test or a compile error, and a
line that says "a write may have been denied" on each of them teaches the model to
ignore it, at a token cost per failure. The static sentence in the tool's description
(§8) says the same thing once, where the model reads how the sandbox works.

**Q7. `--add-dir`.** Mirrors `--mcp`: repeatable, after the config's roots, in flag
order; on `run` and `resume` (it is a `RunArgs` flag, so both verbs take it and `-m`
honours it). It is in `<environment>`, the sandbox line and the writers' descriptions
because it is a static root like any other. **The self-call hole: yes, and wider than
the PM lists.** A line carrying `-c`/`--config` is a bug TODAY, and so is one carrying
`--mcp`: `iota run a --mcp 'sh -c "…"' -m hi` is an unsandboxed self-call that starts
an arbitrary command as an MCP server, with no file written at all. All three flags
(and `--add-dir` when it exists) keep the line in the sandbox, matched as `-c`,
`--config`, `--mcp`, `--add-dir` or their `=` forms among the words, the way the
writers' verbs are matched. It is exploitable under `auto_run: true` and after one
`Allow for this session` on any self-call, which covers every later one.

**Q8. MCP.** `auto_run` never asks; a grant only removes the question; both are
positive, so there is no precedence. No deny form in v1 (agreed: a deny list is a
policy engine, and nothing asks for one). A `--mcp` server's key is `Flag(<its spec>)`:
its name is `argv[0]`, and a persisted `npx:*` would approve whatever `npx` starts next
month — the command line is the only identity it has. `server:*` covers a tool exposed
later; that is what the wildcard says, as `auto_run` does, and a user who wants a
closed list grants tools by name.

**Q9. A session-level `refuse`.** *Neither the flag nor a command, in v1 — I disagree
with the PM.* In `-m` an outside write is already refused; in a chat the user can deny;
a standing policy is the config key. A `--outside-writes refuse` flag serves "a chat
nobody watches", which no one has asked for, and the rules forbid speculative
configuration. If the owner wants it, it is task 10: the flag only tightens
(`max(config, flag)`), exactly as one more file of the stack would.

**Q10. Where the grants sit.** `src/tool/grants.rs` — the lowest layer every reader can
name (`tool` is below `mcp`, `session`, `headless`, `repl`, `cmd`; `shell` and `agents`
are below it and receive plain `Vec<PathBuf>`s). The store does no I/O:

```rust
pub struct Grants { inner: Mutex<Inner> }          // shared as Arc<Grants>
struct Inner { session: GrantSet, project: GrantSet }

impl Grants {
    pub fn live_dirs(&self) -> Vec<PathBuf>;                 // both levels, each once, stale ones dropped
    pub fn roots(&self, statics: &[PathBuf]) -> Vec<PathBuf>; // statics, then live_dirs, each once
    pub fn allows(&self, key: &ToolKey) -> bool;             // GrantSet::allows at either level
    pub fn update<R>(&self, level: Level, f: impl FnOnce(&mut GrantSet) -> R) -> R; // the one mutator
    pub fn snapshot(&self, level: Level) -> GrantSet;
    pub fn replace_session(&self, set: GrantSet);
    pub fn replace_project(&self, set: GrantSet);
}

impl GrantSet {                                            // serde: { dirs, tools }, each omitted when empty
    pub fn allows(&self, key: &ToolKey) -> bool;             // by name, or an MCP key by its server's `*`
    pub fn add_dir(&mut self, dir: PathBuf) -> bool;
    pub fn add_tool(&mut self, key: ToolKey) -> bool;
}
```

`Grants::roots` is the one union the code jail, the shell sandbox and the harness's
`write roots:` line read; `GrantSet::allows` is the one `*` rule. `session::meta` embeds a
`GrantSet`; `session::store` reads and writes `grants.json`
(`SessionStore::{project_grants, update_project_grants}`). `ToolEnv` holds `grants:
Arc<Grants>` (`Default` is an empty store, so the `..ToolEnv::default()` literals stand)
and the STATIC roots in two lists, `write_roots` (the config's) and `added_dirs`
(`--add-dir`), joined by `static_roots()`. `CodeSet` and `ShellTool` clone the `Arc` at
assembly and read it per call; the MCP manager does not hold it (it only names its key,
`Dispatcher::grant_key`).

The chat's side is `src/repl/grants.rs`:

```rust
pub struct ProjectFile { pub store: SessionStore, pub root: PathBuf }
impl ProjectFile {
    fn load(&self, grants: &Grants) -> Option<String>;              // project level := the file; a warning if unreadable
    fn enter(&self, grants: &Grants, session: GrantSet) -> Entered; // session level replaced, then load
}
pub struct Footing { pub project: ProjectFile, pub write_roots: Vec<PathBuf>, pub added_dirs: Vec<PathBuf> }
struct Door { grants: Arc<Grants>, writer: WriterSlot, project: ProjectFile }
impl Door {
    fn apply(&self, level: Level, f: impl FnOnce(&mut GrantSet)) -> Result<(), Unsaved>;
}
```

`Door::apply` is what every door that changes a grant calls — the dialog, `/allow`,
`/revoke` and `/save`: the session level changes in memory and is then written through
`update_meta` (an ephemeral chat has no writer); the project level is written to the file
first and memory takes what the file then holds. `ProjectFile` is built once, in `cmd`, for
the root the tools read paths against — only for a run with a project (§4.2), `None`
otherwise; `load` runs at assembly, `enter` at a
start resume, `iota resume -m`, `/new` and `/session`. `Footing` is what `/allow` stands on:
that root, the project's file (`Option<ProjectFile>`) and the static roots kept apart by where
they were declared. The
harness is recomposed by the next turn (`refresh_harness`), not by the door. `/new`, a
resume and `/session` swap contents, never the handle (§7).

**Q11. Trust.** Nothing new is trusted. The trust gate (X-94) exists because a
repository can ship a config; `grants.json` and every `meta.json` live under `~/.iota`,
which no clone can write, so there is nothing to gate. Protection from the model:
`Reach::Entry` on `~/.iota` is NOT enough. It holds only while no writable root covers
the directory, and that fails as soon as the user runs iota in their home directory,
lists `~` in `write_roots:`, or types `/allow ~` — then a sandboxed `echo … >
grants.json` grants the model every tool of every server for the next start. So
`~/.iota/sessions` becomes a `Reach::Tree` row (`Why::Grants`), with no approval path,
like the trust record: the bundles and the buckets are written by iota in-process and
by nothing else. An unsandboxed self-call child still writes its own bundle; a
sandboxed command never could (it had no root there). The Linux gap of X-95 applies —
a missing path cannot be bound — so the store creates `~/.iota/sessions` when a run
starts.

**Q12. Divergences.** X-92 (grants), X-91 (the writable set is no longer fixed at
assembly), X-44 (self-calls), X-95 (the new protected tree) are amended; four rows are
new. X-94 is untouched. §10.

## 10. Divergences (drafts — the PM applies them)

Amendments, appended to the existing rows:

- **X-44** — `Amended <date>: a line that carries `-c`/`--config`, `--mcp` or
  `--add-dir` (the word, or its `=` form) is never a self-call either. Each one chooses
  what the child loads or starts — a config with `sandbox: off` and `auto_run: true`, a
  server command, a writable root — so, let out, a model wrote a scratch config inside
  the project and ran it unsandboxed. Pinned by
  `shell::selfcall::tests::a_line_that_chooses_a_config_a_server_or_a_root_stays_in`
  and `tests/tool/shell.rs::the_sandbox_keeps_an_iota_that_names_its_own_config_in`.`
- **X-91** — `Amended <date>: `ToolEnv::write_roots` is the STATIC part of the writable
  set (`write_roots:` then `--add-dir`, X-113). The jail and the sandbox read it
  together with the live directory grants of X-110 at every call, so a running session
  can gain a root; "resolved once at assembly" holds for the static part only.`
- **X-92** — `Amended <date> by X-110: the session grant for an outside write is no
  longer `(tool, exact directory)` held by the gate. It is a directory grant in the
  grant store — the real directory AND its subtree, for both writers and the shell
  sandbox — and a write there is thereafter INSIDE (asked or not by the writer's tool grant).
  `Approved::Dir` is gone; `Approved::Once` is unchanged. `/new` still starts with no
  session grants; a resume now RESTORES the bundle's. The dialog does not offer the
  home directory or an ancestor of it. Retired pins:
  `an_outside_session_grant_is_one_directory_through_one_tool`,
  `tests/repl/commands.rs::session_resume_forgets_the_session_grants`.`
- **X-95** — `Amended <date>: `~/.iota/sessions` joins the table as a tree
  (`Why::Grants`), with no approval path, like the trust record: it holds the session
  and project grants of X-110. A granted directory that is a repository gets the git
  rows of a `write_roots:` entry. Pinned by
  `app::protected::tests::the_sessions_tree_is_protected` and
  `tests/tool/shell.rs::the_sandbox_never_writes_the_sessions_tree`.`

New rows (`| id | area | before | now | reason |`):

| id | area | before | now | reason |
|---|---|---|---|---|
| X-110 | grants: one store, two persisted levels | three memories, none kept: the gate's `HashSet<(tool, Option<dir>)>`, cleared on `/new`, a `/session` resume and exit (X-92); `write_roots:` fixed at assembly (X-91); MCP approval all-or-nothing per server in the config | `tool::grants::Grants`, one `Arc` per process: directory grants (a real path and its subtree, a write root added at run time, read per call by the code jail and the shell sandbox; stale when `physical(D) != D`) and tool grants (`Builtin(name)`, session only; `Mcp{server, tool \| *}`), at session level (`meta.json` `grants`, schema still v2) and project level (`projects/<slug>/grants.json`, 0600, a map keyed by the real project root). Both walks decide through `tool::approval::gate`. `/new` empties the session level, a resume and `/session` replace it, each reloads the project level; `--no-save` keeps session grants in memory and still writes project ones; `-m` reads both and has no door | Decided <date> (docs/design/grants.md). A session finds its directories as it goes, and "allow this directory" that the shell cannot use is the Cursor split X-91 avoided. WHERE and WHETHER stay separate axes, as in Claude Code, Codex and Copilot, so a directory grant is a root and nothing more. The file is keyed by real root because the slug is lossy. Pinned by `tool::grants::tests::{a_directory_grant_covers_its_subtree, a_directory_replaced_by_a_link_is_stale, any_covers_a_tool_named_later, levels_only_add}`, `tool::approval::tests::a_tool_grant_never_answers_a_call_bound_to_a_location`, `repl::turn::tools::tests::a_directory_granted_in_the_dialog_is_a_root_for_both_writers`, `tests/tool/shell.rs::a_granted_directory_is_writable_in_the_sandbox_from_the_next_call`, `tests/repl/new_session.rs::new_forgets_the_session_grants`, `tests/repl/commands.rs::session_resume_restores_the_bundles_grants`, `session::store::tests::{project_grants_are_keyed_by_the_real_root, a_broken_grants_file_grants_nothing}`, `tests/headless/approval.rs::a_project_grant_runs_unattended` |
| X-111 | `/allow` and `/revoke` | no way to widen the boundary from a chat except a code write's dialog; a sandboxed command cannot be prompted | `/allow [project] <dir \| server:tool \| server:*>`, a bare `/allow` listing every level, `/revoke <…>`; one `Grants` row in `/status`; the harness `<environment>` lists the live roots and is recomposed when they change; typed input only (a socket message runs no command). No per-result hint on a failed command: the sandboxed `shell` description says once that the user can `/allow` a directory | Decided <date>. The runtime grant is the primary mechanism and this is its only door for the shell. Not configuration (X-42): it answers a question only a chat asks. A fixed hint on every non-zero exit is noise on every failing test; sniffing stderr is fragile. Pinned by `tests/repl/commands.rs::{allow_makes_a_directory_writable_and_says_where_it_really_is, allow_project_writes_the_bucket_file, allow_refuses_what_is_not_a_directory_or_a_connected_tool, revoke_removes_from_every_level, allow_and_revoke_are_in_the_table}`, `repl::commands::status::tests::the_grants_row_only_when_granted`, `tests/repl/messaging.rs::a_message_that_says_allow_grants_nothing` |
| X-112 | MCP tool grants | `requires_approval` is `!auto_run`: every tool of a server or none, in a config file; a `--mcp` server can never be allowed for longer than a session | a call is unasked under `auto_run`, or when the store holds `server:tool` or `server:*` — keyed by the CONFIGURED server name (never the wire segment) and the server's raw tool name; a `--mcp` server is keyed by its flag value (`ServerConfig::flag`). The dialog offers the tool at both levels; `server:*` is `/allow`'s. The wildcard covers tools the server exposes later. No deny form | Decided <date>. The wire segment is sanitized and suffixed by connect order (`mcp-tool-naming.md`), so it is not an identity; `argv[0]` is not one either. `auto_run` and a grant are both positive, so there is nothing to rank. Pinned by `mcp::manager::tests::{the_grant_key_is_the_configured_name_and_the_raw_tool, a_flag_server_is_keyed_by_its_spec}`, `repl::turn::tools::tests::an_mcp_tool_granted_for_the_session_is_not_asked_again`, `tests/repl/commands.rs::allow_server_star_covers_every_tool` |
| X-113 | `--add-dir` | the only way to name a writable directory up front is a config file | `iota run\|resume --add-dir <dir>` (repeatable): appended to `ToolEnv::write_roots` after the config's entries in flag order, relative to the working directory; a static root in every respect (`-m`, descriptions, `<environment>`); not recorded in the session. Never a self-call (X-44) | Decided <date>: every surveyed CLI with a boundary has the flag (Claude Code, Codex, Copilot `--add-dir`; Gemini `--include-directories`). It serves the known-up-front case and the headless one; the runtime grant is primary. Pinned by `tests/cmd/cli.rs::{add_dir_opens_a_directory_after_the_configs_roots, add_dir_is_relative_to_the_working_directory}`, `tests/headless/approval.rs::add_dir_admits_an_unattended_write` |

## 11. Tasks

Every task's gate is `ci.sh`'s:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1 cargo test
cargo build --release
```

plus `cargo test <name>` passing for each test it names. "Artifact" is the one module a
task is about; its tests, the paragraphs of `code-toolset.md` / `shell-toolset.md` /
`session-format.md` it makes true, its CHANGELOG line and its DIVERGENCES rows travel
with it. Each task leaves the product working; nothing is behind a flag.

| # | slug | kind | artifact | depends on |
|---|---|---|---|---|
| 1 | `selfcall-flags` | fix | `src/shell/selfcall.rs` | — |
| 2 | `grants-store` | feature | `src/tool/grants.rs` | — |
| 3 | `grants-shell` | feature | `src/tool/builtins/shell.rs` | 2 |
| 4 | `grants-allow` | feature | `src/repl/commands/allow.rs` | 3 |
| 5 | `grants-protect` | feature | `src/app/protected.rs` | — |
| 6 | `grants-session` | feature | `src/session/meta.rs` | 4, 5 |
| 7 | `grants-project` | feature | `src/session/store.rs` | 6 |
| 8 | `grants-mcp` | feature | `src/mcp/manager.rs` | 7 |
| 9 | `add-dir` | feature | `src/cmd/args.rs` | 1 |
| 10 | `outside-writes-flag` | feature, ONLY if the owner overrules Q9 | `src/cmd/args.rs` | 9 |

**1. `selfcall-flags`** — a line carrying `-c`, `--config`, `--mcp` (or an `=` form)
stays in the sandbox. Ships alone: it is a bug today. X-44 amendment (without
`--add-dir`, which task 9 adds). Done when the gate passes and these exist:
`shell::selfcall::tests::a_line_that_chooses_a_config_or_a_server_stays_in`,
`tests/tool/shell.rs::the_sandbox_keeps_an_iota_that_names_its_own_config_in`.

**2. `grants-store`** — `Grants`, `GrantSet`, `ToolKey` (`Mcp` defined, unused until
8), `tool::approval::gate`, `ToolEnv::grants`; the gate's `HashSet` and `reset` are
removed; both walks call `gate()`; the dialog's directory item becomes `Allow, and make
<dir> writable for this session` and writes a directory grant the code jail reads per
call; `Approved::Dir` is removed; no home-or-above offer; `/new` and `/session` empty
the session level. Memory only. X-110 (first half), X-92 amendment. Done when the gate
passes and these exist:
`tool::grants::tests::{a_directory_grant_covers_its_subtree, a_directory_replaced_by_a_link_is_stale, levels_only_add}`,
`tool::approval::tests::a_tool_grant_never_answers_a_call_bound_to_a_location`,
`repl::turn::tools::tests::{a_directory_granted_in_the_dialog_is_a_root_for_both_writers, the_dialog_never_offers_the_home_directory, a_link_swapped_after_a_grant_writes_nothing}`,
`tests/repl/new_session.rs::new_forgets_the_session_grants` (kept, rewritten on the
store).

**3. `grants-shell`** — the sandbox's writable roots and the protected table's `extra`
are the static roots plus `live_dirs()`, per call; the `<environment>` line lists the
same and the harness is recomposed when the set changes; the `/allow` sentence joins
`SHELL_DESC_SANDBOXED`. X-91 amendment. Done when the gate passes and these exist:
`tests/tool/shell.rs::{a_granted_directory_is_writable_in_the_sandbox_from_the_next_call, a_stale_grant_is_not_a_sandbox_root, a_granted_repository_keeps_its_hooks_and_config}`,
`agents::harness::tests::write_roots_line_lists_the_live_grants`.

**4. `grants-allow`** — `/allow [<dir>]`, `/revoke <dir>`, the `/status` row, the two
table rows; session level, directories only (`project` and `server:tool` arrive with 7
and 8). X-111. Done when the gate passes and these exist:
`tests/repl/commands.rs::{allow_makes_a_directory_writable_and_says_where_it_really_is, allow_refuses_what_is_not_a_directory, revoke_removes_a_grant, allow_and_revoke_are_in_the_table}`,
`repl::commands::status::tests::the_grants_row_only_when_granted`,
`tests/repl/messaging.rs::a_message_that_says_allow_grants_nothing`.

**5. `grants-protect`** — `~/.iota/sessions` as a `Reach::Tree` row with `Why::Grants`
and no approval path (the `why != Why::Trust` tests in the code set become one
predicate); the store directory is created at start. Must land before anything
persists. X-95 amendment. Done when the gate passes and these exist:
`app::protected::tests::the_sessions_tree_is_protected`,
`tests/tool/shell.rs::the_sandbox_never_writes_the_sessions_tree`,
`code::tools::tests::the_sessions_tree_has_no_approval_path`.

**6. `grants-session`** — `SessionMeta::grants`; every session-level change goes
through `update_meta`; a start resume, a `/session` resume and `iota resume -m` load
it; `/save` stamps it; the one-line `grants: …` notice (removed 2026-11-20, no-grants-line:
the load is silent, §7). X-110 (second half),
`session-format.md` §4.1. Done when the gate passes and these exist:
`session::meta::tests::grants_are_omitted_when_empty_and_round_trip`,
`tests/repl/commands.rs::{session_resume_restores_the_bundles_grants, session_switch_replaces_and_never_merges}`,
`tests/repl/commands.rs::save_keeps_the_grants_made_while_ephemeral`
(`session_resume_forgets_the_session_grants` is deleted).

**7. `grants-project`** — `grants.json` read/write in `SessionStore`; loaded at every
start, `/new` and resume, in every mode; the dialog's `…for this project` item and
`/allow project <dir>`; `--no-save` writes it. Done when the gate passes and these
exist:
`session::store::tests::{project_grants_are_keyed_by_the_real_root, two_roots_with_one_slug_share_a_file_and_nothing_else, a_broken_grants_file_grants_nothing, the_grants_file_is_private_and_written_whole}`,
`tests/repl/commands.rs::allow_project_writes_the_bucket_file`,
`tests/repl/new_session.rs::new_keeps_the_project_grants`,
`tests/headless/approval.rs::a_project_grant_runs_unattended`,
`tests/cmd/cli.rs::an_ephemeral_chat_still_writes_a_project_grant`.

**8. `grants-mcp`** — `Dispatcher::grant_key`, `ServerConfig::flag`, the MCP dialog
items, `/allow [project] <server>:<tool|*>`, `/revoke` for them, the unattended
refusal's new clause. X-112. Done when the gate passes and these exist:
`mcp::manager::tests::{the_grant_key_is_the_configured_name_and_the_raw_tool, a_flag_server_is_keyed_by_its_spec}`,
`tool::grants::tests::any_covers_a_tool_named_later`,
`repl::turn::tools::tests::an_mcp_tool_granted_for_the_session_is_not_asked_again`,
`tests/repl/commands.rs::{allow_server_star_covers_every_tool, allow_refuses_a_server_that_is_not_connected}`,
`tests/headless/approval.rs::a_project_mcp_grant_runs_unattended`.

**9. `add-dir`** — the flag on `RunArgs` (and in `first_given`), resolved into
`ToolEnv::write_roots` after the config's; `--add-dir` joins task 1's word list; the
unattended refusal names it. X-113. Independent of 2–8. Done when the gate passes and
these exist:
`tests/cmd/cli.rs::{add_dir_opens_a_directory_after_the_configs_roots, add_dir_is_relative_to_the_working_directory}`,
`tests/headless/approval.rs::add_dir_admits_an_unattended_write`,
`shell::selfcall::tests::a_line_that_adds_a_root_stays_in`.

**10. `outside-writes-flag`** — only if the owner overrules Q9: `--outside-writes
refuse`, `max(config, flag)`. Done when the gate passes and
`tests/cmd/cli.rs::the_outside_writes_flag_only_tightens` exists.

## 12. Deferred

A deny form for MCP tools; a grant that expires; a
picker for `/allow` (a list with checkboxes, as `/session` has) once the typed form
has been lived with; watching `grants.json` for another process's change (a running
chat sees it at its next `/new` or resume).
