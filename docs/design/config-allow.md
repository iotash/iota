# Config `allow:` — one grant vocabulary, in the config as in the chat

Status: **design** (2026-11-21). Nothing here is implemented. It revises DIVERGENCES X-91,
X-92, X-110, X-112 and X-113 and amends X-05, X-44 and X-94; §11 holds the rows as drafted.
§9 answers the ten questions one by one; §12 is the task list. Read
`docs/design/grants.md` first: this document changes its §3.1, §5.3, §5.4 and §6 and
keeps the rest.

## 1. The problem

"Do not ask for this" is written five ways, in four places, and only one of them is the
vocabulary the user types:

| what is allowed | in the chat | in the config | on the command line |
|---|---|---|---|
| a directory | `/allow <dir>` | top-level `write_roots: [<dir>]` | `--add-dir <dir>` |
| a built-in tool | `/allow shell`, `/allow edit_file` | `agents.<n>.tools.shell.auto_run`, `agents.<n>.tools.code.auto_write` | — |
| an MCP tool | `/allow github:search_issues` | — | — |
| an MCP server's tools | `/allow github:*` | top-level `mcp_servers.github.auto_run` | — |

and one key says the opposite: top-level `outside_writes: refuse`.

The costs are concrete:

- **Three scopes for one idea.** A directory is allowed for every agent (top level, and
  the key audit *refuses* it under `agents:`), a built-in per agent, a server for every
  agent that loads it. Nobody chose that; each key landed where its feature was.
- **Holes in the table.** One MCP tool cannot be allowed from a config at all. A `--mcp`
  server can never run unattended: it has no entry to write `auto_run` in, and the
  refusal text says so (`write it as an mcp_servers entry with auto_run: true`). A
  built-in cannot be allowed for one `-m` run without editing a file.
- **Two mechanisms for one answer.** A grant is a row in `tool::grants::Grants` that
  `tool::approval::gate` reads. A switch is a `bool` each tool reads for itself, in
  `requires_approval`, in `waives_approval` and — for `shell` — in `supports_parallel`.
  Every consumer written since the store shipped has had to learn both, and two have
  not (§4.4).
- **A silently ignored key.** `tools.<set>` values are decoded without the key audit
  (`yaml11::decode_mapping`, no `deny_unknown_fields`), so `tools.code.auto_wrtie: true`
  loads today and does nothing — the exact failure `config::strict` exists to prevent.

## 2. The decision

**`agents.<name>.allow: [<target>, …]`** — a list whose entries are `/allow`'s targets,
without the scope word. It, and `--allow <target>` on the command line, are the only
standing ways to say "do not ask". `write_roots:`, `outside_writes:`,
`tools.code.auto_write`, `tools.shell.auto_run`, `mcp_servers.<name>.auto_run` and
`--add-dir` are removed.

**Mechanically it is a third, read-only level of the grant store** — the *static* level,
filled once at assembly from the agent's `allow:` and the `--allow` flags, never
persisted, never revocable from the chat. Every enforcement point already reads the
store; after this they read nothing else. `ToolEnv::write_roots`, `added_dirs`,
`static_roots`, `CodeSet::extra`, `ShellTool::write_roots`, `Tool::waives_approval` and
the per-tool `auto_*` fields all go.

What does NOT change: the two axes (a directory is WHERE, a tool is WHETHER); a tool
grant never answers a call bound to a location; the protected table beats every root
and every grant; an unverifiable location is refused; the session and project levels,
their files and their doors; the trust gate, which still stands between a repository's
`.iota.yaml` and all of this.

**Where this departs from the owner's framing** (each argued where it is decided):

1. `outside_writes:` is not a spelling of "may this run" — it is the one key that says
   *no*. It goes, but for its own reason and with no dependency on `allow:` (Q5, task 3).
2. The switches have **two more readers** than the config: the inbound-message hold and
   the parallel batch. Replacing them is not a config change with a mechanical tail, and
   reading both turned up a hole each (§4.4). Those two fixes come first.
3. "Exactly the targets `/allow` takes" holds for the grammar, not for the checks: a
   config is read before any server is connected, so an MCP *tool* name cannot be
   verified where it is written, and a `--mcp` server cannot be named in a file at all
   (Q3).
4. The `Static` tab does not become redundant. "Every root is a grant" is true of what
   a root *does*; what the user can *do about it* still differs — a static grant is
   taken back by editing a file (Q1, Q4).
5. `grants.md` §5.4 said a config must not ship grants. It meant the *project level*;
   that still holds, and §6 says why `allow:` is not that.

## 3. The config surface

### 3.1 The key

```yaml
agents:
  coder:
    mode: agent
    tools: { shell: {}, code: {} }
    mcp_servers: [github]
    allow:
      - ~/scratch              # a directory: the writers and the sandbox may write there
      - ../shared-lib          # relative: against the project root, as /allow reads it
      - edit_file              # a built-in tool: an inside write is not asked
      - write_file
      - shell                  # …a call outside the sandbox is not asked
      - github:search_issues   # one tool of an mcp_servers: entry
      - github:*               # every tool of it, those it exposes later included
```

`allow:` is a sequence of strings (a single string is refused: a grant list that grows
by one must not change shape). Absent or empty = nothing is allowed up front. It is an
`agents.<name>` key only; §9 Q7 says why not top level.

### 3.2 The grammar of an entry

One entry is one `/allow` target, read in `/allow`'s order (`grants.md` §5.2) with the
config's own name list in place of the connected one:

1. **A server's whole name, the longest that matches, then `:` and the tool** — among the
   `mcp_servers:` entries this agent loads.
2. **A directory** — it starts with `/`, `~` or `.`. `~` is the home; a relative one hangs
   off the project root (`ToolEnv::root`), exactly as `/allow` and today's `write_roots:`
   read it.
3. **Holding a `:`** — `<server>:<tool>`, split at its last `:`; `<tool>` is the server's
   raw tool name or `*`.
4. **A bare word** — a built-in tool by its wire name.

There is **no scope word**. In a file the level is the file's; `project` as a last word
is part of the target (`~/my project` is a directory, with no trailing-slash rule).

### 3.3 What is refused, and where

At the **load**, for every agent of every file (the key audit and `Config::validate`,
so a mistake in an agent nobody ran today is still found), as
`agents.<name>.allow[<i>]: …`:

```
expected a string (a directory, a built-in tool or <server>:<tool>)
is blank
"~" needs a home directory, and this run has none
expected a directory (starting with /, ~ or .), a built-in tool or <server>:<tool>
no built-in tool named "<t>" (tools that ask: shell, edit_file, write_file)
<t> never asks (tools that ask: shell, edit_file, write_file)
<t> is not one of this agent's tools (tools.<set> is not enabled)
<t> is not one of this agent's tools (tools.code.read_only leaves the writers out)
no mcp_servers entry named "<s>" (entries: a, b)
"<s>" is not one of this agent's mcp_servers (it loads: a, b)
"<s>:" names no tool: write <s>:<tool> or <s>:*
```

`agents.<name>.allow: expected a list` for a scalar or a map. The built-ins that ask are
a constant of `tool::builtins` (`ASKING: [&str; 3]`), pinned by a test against the
argument-less `requires_approval` probe over a registry with every set enabled, so the
list cannot drift from the tools.

At **assembly**, for the run's agent only (these need the run's root and filesystem), as
a startup error with the same coordinate — `--allow: …` for a flag value:

```
<entry> is not a directory                         (it exists and is something else)
cannot work out where <entry> is                   (fs::physical fails: replaces SetError::WriteRoot)
<real> is protected: <why>                         (the directory itself is on the protected table)
```

A directory that does not exist yet is accepted, as a `write_roots:` entry is today: a
file line is not an act at a moment, and `out/` is routinely created by the first build.

Not checked, deliberately: a repeat (dropped, first spelling wins, as `GrantSet::add_*`
already does), and a directory that lies inside the project root or another entry
(harmless, and true or false depending on where the agent is run).

**An MCP tool name cannot be checked at the load** — the server is not connected. When
the server's tool list resolves (the moment `/allow`'s completion rows are recomputed)
a named tool it does not expose warns once, on the MCP status path:
`⚠ agents.<name>.allow: server "github" has no tool "serch_issues"`. It grants nothing
meanwhile, which is the safe direction.

### 3.4 `--allow <target>`

`iota run|resume … --allow <target>` (repeatable; replaces `--add-dir`, Q6). Same
grammar; the server names are the agent's entries **and the run's `--mcp` specs**, so

```
iota run coder --mcp 'npx -y @acme/mcp' --allow 'npx -y @acme/mcp:*' -m "…"
```

is how a `--mcp` server runs unattended (keyed `ServerId::Flag(<spec>)`, as a chat grant
of it is). A relative directory is against the **working directory** — it is typed in a
shell — as `--add-dir` is today. The same refusals, prefixed `--allow: `. Not stored in
the session; a resume that wants it passes it again. Never a self-call (§4.5).

## 4. The mechanism

### 4.1 The static level

```rust
// src/tool/grants.rs
pub struct Grants { fixed: GrantSet, inner: Mutex<Inner> }   // Inner { session, project } unchanged

impl Grants {
    pub fn with_static(fixed: GrantSet) -> Self;           // the ONE time the static level is written
    pub fn statics(&self) -> &GrantSet;                    // for descriptions and the Static tab
    pub fn roots(&self) -> Vec<PathBuf>;                   // static dirs, then session, then project; each once; stale dropped
    pub fn allows(&self, key: &ToolKey) -> bool;           // at any of the three
    pub fn waives(&self) -> bool;                          // any tool key at any level — the hold's trigger (§4.4)
    // update / snapshot / replace_session / replace_project: unchanged, and still take `Level`
}
```

`Level` stays `{ Session, Project }`: it names what a door may **write**. The static
level is not a `Level` because nothing writes it after construction — a third variant
would put an unwritable arm in every `match` of every door. It sits outside the mutex
for the same reason: it never changes.

`roots()` loses its `statics: &[PathBuf]` parameter; that parameter *was* the second
list. A static directory is stored as a grant is — its real location, from
`fs::physical` at assembly — and is **live** by the grant's rule (`physical(D) == D`), so
a static root later replaced by a symlink drops out of the jail and the sandbox
together. Today the jail compares against the real location it resolved at assembly
while the sandbox is handed the configured path, which is the two-answers trap X-91
names; one rule closes it.

`config::allow` (new, `src/config/allow.rs`) owns the entry type and the parse:

```rust
pub enum Entry { Dir(String), Builtin(String), Mcp { server: String, tool: McpTool } }
pub fn parse(entry: &str, servers: &[String]) -> Result<Entry, String>;   // §3.2; the load and the flag call it
```

`cmd::assemble_tools` resolves the agent's entries and the flags into one `GrantSet`
(`Dir` → `physical`, `Builtin` → `ToolKey::Builtin`, `Mcp` → `ToolKey::Mcp` with
`ServerId::Config` or `Flag`) and builds the store with it, before any tool is built.
`repl::commands::allow` keeps its own target parse for the chat — it has a scope word,
connected names and a door — but the directory/built-in/server classification becomes
`config::allow::parse` there too: one grammar, one function.

### 4.2 The walk

`tool::approval::gate` is unchanged in shape and loses one thing to read:

```rust
pub fn gate(d: &dyn Dispatcher, grants: &Grants, name: &str, args: &JsonObject) -> Gate {
    if !d.requires_approval(name, Some(args)) { return Gate::Run; }   // now: "this call is of a kind that asks"
    let subject = d.approval_subject(name, args);                     // the ONE resolution (X-92)
    let key = d.grant_key(name);
    if subject.is_none() && grants.allows(&key) { return Gate::Run; } // static, session or project
    Gate::Ask { subject, key }
}
```

`requires_approval` stops being "the config gates this call" and becomes a fact about
the call alone:

| tool | `requires_approval(Some(args))` today | after |
|---|---|---|
| `edit_file`, `write_file` | inside: `!auto_write`; outside/protected: `outside == Ask` | inside: `true`; outside/protected: attended |
| `shell` | `!auto_run && (!sandboxed \|\| leaves_sandbox)` | `!sandboxed \|\| leaves_sandbox` |
| an MCP tool | `!server.auto_run` | `true` |

and the argument-less probe is `true` for exactly those tools. "A caller that forgets
`gate()` asks too often, never too rarely" still holds.

### 4.3 Enforcement points

| point | today | becomes |
|---|---|---|
| the code jail, `code/mod.rs` | `extra: Vec<Root>` (static) + `grants.roots(&statics)` | `grants.roots()`; `Root` is the project root's alone; `anchor`/`walk_base` read the same list |
| the sandbox, `shell.rs` `writable` | `grants.roots(&self.write_roots)` | `grants.roots()` |
| the protected table's `extra` | the static roots + live grants | `grants.roots()` |
| the harness `write roots:` line | `grants.roots(&env.write_roots)` | `grants.roots()`; `Environment::write_roots` goes |
| the writers' and the shell's descriptions | the static roots, at assembly | `grants.statics().dirs`, at assembly |
| `/allow`'s `Static` tab, "already writable" | `Footing::{write_roots, added_dirs}` | `Footing::{config, flag}: GrantSet` — the two sources, kept apart for the label |
| the hold, `repl/inbound.rs` | `dispatch.waives_approval()`, once at start | `grants.waives()`, at each message (§4.4) |
| the batch, `headless/batch.rs` | `supports_parallel` (which reads `auto_run`) | `supports_parallel` and `gate() == Run` (§4.4) |

### 4.4 The two readers the framing missed

**The hold.** A chat whose tools act without asking holds a message from the socket
until the user admits it (`agent-messaging.md` §4). Its trigger is
`Dispatcher::waives_approval`, which reads the three switches, once, when the loop
starts (`src/repl/run.rs:693-699`). So today a session that typed `/allow shell`, or
runs in a project that granted `github:*`, acts unasked and is **not** held: the hold
predates the store and never learned it. The policy sentence — *hold where the user
explicitly gave up approval* — describes a tool grant exactly. The trigger becomes
`Grants::waives()` (a tool key at any level), read when a message arrives, since a grant
can be made mid-session. A directory grant does not hold: a sandbox writing one more
directory is "the sandbox is not a waiver" (the open policy of §7.1 there, untouched).

**The batch.** A run of consecutive parallel-capable calls goes out as one batch, and
"the parallel batch is the one path without an approval gate" (`src/tool/mod.rs:216`).
`ShellTool::supports_parallel` is `!(sandboxed && !auto_run && leaves_sandbox)` — `true`
for every call of an **unsandboxed** set whatever `auto_run` says, and
`tests/tool/shell.rs::shell_calls_batch` pins that with `sandbox: off` alone. As the
code reads, two `shell` calls in one round on a machine without a sandbox (every
Windows machine) run unasked; the dialog only ever sees a lone call. *I read this; I did
not run it* — task 1 starts by pinning it with a failing test. Either way the fix is
the design: the walk batches a call only when it is parallel-capable **and**
`gate() == Run`, so the invariant is the walk's and no tool reads a switch to keep it.

### 4.5 Self-calls

`--allow` joins `shell::selfcall`'s choosing flags in `--add-dir`'s place (X-44): a line
that carries it stays in the sandbox. It matters more than `--add-dir` did —
`iota run a --allow shell -m …` let out would be a child with an unconfined, unasked
shell.

## 5. What goes away, and what each removal costs

| removed | replaced by | what is lost or changes |
|---|---|---|
| top-level `write_roots:` | `allow: [<dir>]` per agent | a directory for *every* agent is written once per agent (Q7). A root reached through a link is named by its real path in messages, not by its spelling. A root replaced by a link stops being writable (a fix). A granted directory's `.gitignore` now anchors a walk under it, as a static root's did |
| `--add-dir <dir>` | `--allow <dir>` | scripts break with clap's `unexpected argument '--add-dir'`; the flag every other CLI spells this way is gone (Q6) |
| `tools.code.auto_write: true` | `allow: [edit_file, write_file]` | two words for one; in exchange one writer can be allowed without the other. `SetError::CodeContradiction` (`read_only` + `auto_write`) becomes the load's "not one of this agent's tools" |
| `tools.shell.auto_run: true` | `allow: [shell]` | nothing |
| `mcp_servers.<n>.auto_run: true` | `allow: ["<n>:*"]` per agent | the same per-agent repetition; in exchange one agent can be trusted with a server another must ask for |
| top-level `outside_writes: refuse` | — | a chat can no longer be configured to refuse an outside write without showing the dialog (Q5) |
| `Tool::waives_approval`, `Dispatcher::waives_approval` | `Grants::waives` | the hold now also covers session and project tool grants (a fix, §4.4) |
| `ToolEnv::{write_roots, added_dirs, static_roots, with_write_roots, outside_writes}`, `OutsideWrites`, `CodeSet::extra`, `Outside::Refuse`, `SetError::{WriteRoot, CodeContradiction}`, `check_write_root` | the store; `config::allow` | — |

`tools.shell` keeps `sandbox` and `network`; `tools.code` keeps `read_only`. With the
switches gone the audit finally learns the keys under `tools.<set>` (§7), so a typo
there fails the load like any other.

## 6. The trust summary

A project `.iota.yaml` may carry `agents.<name>.allow`. That is no new power: such a
file can already set `auto_run: true` and `sandbox: off`, and nothing in it takes
effect until the user has trusted these exact bytes (X-94). `grants.md` §5.4's "a
repository's `.iota.yaml` would then ship approvals" is about the **project level** —
`grants.json`, which lives under `~/.iota` precisely so that no clone can write it —
and stays true: `allow:` fills the static level, behind the gate, and writes nothing.

`config::trust::summary` drops its `auto_run`, `auto_write`, `write_roots` and
`outside_writes` lines and gains one line **per entry**, as written, with what it does
(never shortened, never joined — a long list is the file's):

```
agents.coder.allow: ../shared-lib (a directory its tools may write)
agents.coder.allow: ~ (a directory its tools may write)
agents.coder.allow: shell (runs without asking, outside the sandbox too)
agents.coder.allow: edit_file (runs without asking)
agents.coder.allow: github:* (every tool of github runs without asking)
agents.coder.allow: github:push (runs without asking)
```

The summary has the raw document and nothing else, so it classifies by spelling (§3.2
with the file's own `mcp_servers:` names); an entry that is not a string is shown as
`agents.coder.allow: (not a string: the file would be refused)`.

There is nothing to say about merging, and that is the point (Q8): an `agents:` entry
is merged **whole, by name** (`merge_document`: `self.agents.insert(name, …)`), so a
project file that defines `agents.coder` replaces the home's `coder` — `allow:`
included — and the summary of that file shows everything the agent will be allowed.

## 7. Migration

No compatibility: an old key **fails the load**, in the key audit, before the typed
decode, naming what to write. `RETIRED_KEYS` gains the rows and learns a `{entry}`
placeholder; the audit checks retired top-level keys before `unknown top-level key`,
and — new — checks the keys of `agents.<n>.tools.shell` (`sandbox`, `network`) and
`agents.<n>.tools.code` (`read_only`). The exact texts (`<coordinate>: <message>`):

```
write_roots: `write_roots` is now `allow:` on an agent — list each directory in `agents.<name>.allow`, or pass `--allow <dir>` for one run

outside_writes: `outside_writes` was removed — a write outside every root asks the user, and a run with nobody to ask refuses it; delete the key

agents.coder.write_roots: `write_roots` is now `allow:` — list each directory in `agents.coder.allow`

agents.coder.outside_writes: `outside_writes` was removed — a write outside every root asks the user, and a run with nobody to ask refuses it; delete the key

agents.coder.tools.code.auto_write: `auto_write` is now `allow:` — write `allow: [edit_file, write_file]` in `agents.coder`

agents.coder.tools.shell.auto_run: `auto_run` is now `allow:` — write `allow: [shell]` in `agents.coder`

mcp_servers.github.auto_run: `auto_run` is now `allow:` — write `"github:*"` in the `allow:` of each agent that may run its tools without asking

agents.coder.tools.code.auto_wrtie: unknown key (want read_only)
agents.coder.tools.shell.netwrok: unknown key (want sandbox, network)
```

A retired key is refused **whatever its value** (`auto_run: false` too): a key that is
accepted when false and refused when true is a compatibility layer with a condition.

`--add-dir` gets no hidden flag and no message of its own: clap answers
`error: unexpected argument '--add-dir' found`. A hidden argument whose only job is to
refuse is the "compatibility layer" the rules forbid, and unlike a config key a flag is
in the command the user is looking at.

`iota config init`'s template (`src/cmd/config_cmd.rs:60-66`) is rewritten to the new
spelling in the task that removes the switches.

## 8. What the model and the user read

- **An outside write, nobody to ask** — `path is outside the project root (<roots>):
  <arg> — writing there needs the user's approval, which this non-interactive run cannot
  ask for; pass --allow <dir>, or list its directory in the agent's `allow:`, to allow
  it`.
- **An outside write, attended, this call unapproved** — unchanged.
- **The bare `path is outside the project root (<roots>): <arg>`** — `refuse`'s text —
  is no longer produced; `protected_refusal`'s `outside_writes: refuse` arm goes with it.
- **A built-in, nobody to ask** (`headless::refusal_text`) — `<name> was not executed:
  it needs the user's approval, which this non-interactive run cannot ask for; pass
  --allow <name>, or list it in the agent's `allow:`, to permit it here.`
- **An MCP tool, nobody to ask** — `<name> was not executed: the tools of MCP server
  "<server>" need the user's approval, which this non-interactive run cannot ask for;
  pass --allow <server>:<tool>, list it in the agent's `allow:`, or allow it for the
  project from a chat (/allow <server>:<tool> project).` The last clause only in a run
  with a project, as today. A `--mcp` server takes the same sentence with its spec
  quoted (`--allow '<spec>:<tool>'`) and without the `allow:` clause — it finally has an
  answer that is not "rewrite it as an entry".
- **`/allow`'s refusals** — `<dir> is already writable (the project root | allow: |
  --allow | this project | this session)`; `<what> already runs without asking (allow: |
  --allow | this session | this project)`, which absorbs `<s>'s tools already run
  without asking (auto_run)`. `<t> never asks` stays for a tool that is not in `ASKING`.
- **`/allow`'s completion rows** — a built-in's row is absent when *any* level holds it,
  the static one included; a server's row is absent when the static level holds its
  `*` (today: when it is `auto_run`).
- **The `Static` tab** — prompt `From the config and the command line: not grants made
  here, so none can be revoked here — edit agents.<name>.allow, or drop the flag.` Rows,
  config first: `allow:    /Users/x/scratch`, `allow:    shell`, `allow:    github:*`,
  `--allow   /data/out`. It now lists tools as well as directories.
- **`/status`** — `Grants   3 static, 2 for this session, 1 for this project (/allow
  lists them)`, each part present only when non-zero.

## 9. The ten questions

### Q1. What is a config grant, mechanically?

**(b), seeded into the store — as a read-only level, not as a third persisted one.** The
static-list option (a) is what `write_roots:` is today, and its cost is on the page:
`Grants::roots(statics)` takes the second list as a parameter at five call sites, and
the switches are read by three trait methods on every tool. With the entries in the
store the jail, the sandbox, the harness, the table, the gate, the hold and the batch
have one thing to ask.

`/revoke` (there is no such command; the `Grants` tab) does **not** apply to a static
grant, and there is no session-level shadow-deny. A deny is a second sign in a store
whose one law is "levels only add" (`grants.md` Q8 already declined a deny form: it is a
policy engine, and nothing asks for one). The honest answer to "take back what the
config allows" is the one for every other config line: edit the file. So the `Static`
tab **stays**, read-only, now fed by `Grants::statics()` with `Footing` keeping which
source declared what; it gains the tool rows.

### Q2. The level.

Not a `Level`. `Level { Session, Project }` is the set of levels a door can write and
the persisted shape's two homes; the static level has neither property. It is a field
of `Grants` set at construction (§4.1). The same target at two levels is what it is
today — nothing: levels only add, a revoke at one leaves the others, and `/allow`
refuses to add at a level what the static level already gives, naming where
(`… (allow:)`), as it refuses a directory `write_roots:` covers now.

### Q3. Does `allow:` replace `auto_write` / `auto_run`?

**Yes, and they are removed — not kept, not sugar.** Verified against the walk:

- An inside write: `requires_approval` is `!auto_write`, the subject is `None`
  (`CodeSet::approvable` returns `Some` only for an outside or protected path), so
  `gate` runs it when the store holds `Builtin("edit_file")`. `allow: [edit_file]` ≡
  `auto_write` for `edit_file`. An outside or protected write has a subject and is asked
  under both. Pinned today by
  `tool::approval::tests::a_tool_grant_never_answers_a_call_bound_to_a_location`.
- `shell`: `requires_approval` is `!auto_run && (!sandboxed || leaves_sandbox)` and the
  subject is always `None`. `allow: [shell]` ≡ `auto_run`.
- MCP: `!cfg.auto_run`, subject `None`, key `Mcp{server, Named}` which `GrantSet::allows`
  matches by the server's `*`. `allow: ["s:*"]` ≡ `auto_run`.

The equivalence fails in exactly two places, and both are consumers that read the
switch instead of the store — the hold and the batch (§4.4). They are fixed first
(tasks 1, 2), after which the switches have no reader the store does not serve.

Sugar is refused on the owner's own ground: a config layer that expands
`auto_write: true` into `allow:` is the second spelling, kept. Three smaller facts the
replacement has to own: `auto_write` is one word for two tools and `allow:` needs both;
the MCP switch was per server and becomes per agent; and a config cannot verify a tool
name or name a `--mcp` server (§3.3, §3.4).

### Q4. `write_roots:`'s other jobs.

All five read `Grants::roots()` (§4.3) — the jail, the sandbox, the `write roots:` line,
the table's `extra` — and the fifth, the `Static` tab, reads `Grants::statics()`. None
keeps a list of its own. The tab survives (Q1): not because a static root behaves
differently from a grant — after this it does not — but because Enter on a row means
"revoke", and that is not available.

### Q5. `outside_writes:`.

**It goes.** With it, the residual policy is one bit the run already knows: is there a
user to ask. The three candidates for a standing `refuse`:

- *A CI.* It runs `-m`; nobody can be asked; an outside write is already refused, with
  a text that says how to allow it.
- *A bot.* A bot is a chat with a terminal; its user can deny. A bot nobody watches is
  `grants.md` Q9's "chat nobody watches", for which no one has asked.
- *A repository hardening itself.* A project file can only tighten what happens on the
  **user's** machine outside the repository — which is not the repository's to decide,
  and the user is asked each time anyway.

`Outside` collapses to its two real states (`Ask`, `Unattended`); `CodeSet::new` takes
`attended: bool`. The texts are §8: the bare refusal and the `outside_writes: refuse`
arm of `protected_refusal` disappear; the other two are unchanged but for `--allow`.

This removal has nothing to do with `allow:` and depends on none of it.

### Q6. `--add-dir`.

**Rename to `--allow <target>`, and remove `--add-dir`.** Not for symmetry: a
directory-only flag cannot say `--allow shell` or `--allow 'spec:*'`, and those are the
two things an unattended run cannot get today (§1). Once `--allow` exists and takes a
directory, `--add-dir` is a second spelling of one of its cases.

This goes against the survey that justified X-113 — Claude Code, Codex and Copilot all
spell the directory flag `--add-dir` — and the owner should weigh that familiarity
(§13). The same products spell tool approval as a *separate* flag (`--allowedTools`,
`--allow-tool`); iota would be choosing one flag over the convention of two.

It resolves a relative directory against the working directory, as today.

### Q7. Per-agent, not top level.

**Per agent only; the repetition is the price, and it is worth paying.**

- Two of the three kinds are per-agent by nature: a tool grant names a tool of *this*
  agent's `tools:`, a server grant a server of *this* agent's `mcp_servers:`. A
  top-level `allow: [shell]` would have to mean "for every agent that has a shell,
  including the one with `sandbox: off` written next month" — and the load could no
  longer refuse a tool an agent lacks, because at top level that is not a mistake.
- A top-level list needs a merge rule across the stack (union, as `write_roots:`
  extends today); the per-agent key needs none (Q8).
- The per-repository case — the common reason to share directories across agents — is
  already served without a config: `/allow ../lib project` is one line and holds for
  every agent run in that project.

What is left is "the same home directories for all my agents". YAML answers the exact
repeat — `allow: &w [~/scratch, ~/notes]` on one agent, `allow: *w` on the others (the
decoder resolves an alias before the audit sees the document; task 4 pins it) — but an
alias cannot be extended, so an agent that wants the shared list *plus* its own repeats
it. If that proves to hurt, a top-level `allow:` is an additive task later (§14);
adding it now is configuration nobody has asked for.

### Q8. The stack.

**Replace, by the rule that already exists.** `merge_document` merges `agents:` a whole
entry at a time, by name. A project `agents.coder` therefore replaces the home's
`coder` entirely, `tools:` and `allow:` alike; there is no list-level merge to define,
and "only tightens" needs no analogue because nothing accumulates. An agent the project
file does not mention keeps the home's `allow:` untouched — a repository cannot widen
an agent it does not redefine, which a top-level list could not promise.

The trust dialog shows one line per entry of every agent the file defines (§6).

### Q9. Migration.

The strict audit, before the typed decode; the texts are §7. Two things beyond the
brief: the audit must start checking the keys **under** `tools.shell` and `tools.code`,
because today an unknown key there is silently ignored and a removed `auto_write`
would be too; and a retired key is refused at every value.

### Q10. Layers.

§12: eight tasks, two of them fixes that ship before anything is renamed. About 3,300
changed lines.

## 10. Tests

New pins, by the module that owns them (each named in its task):

- `config::allow::tests` — `an_entry_is_read_in_allows_order`,
  `a_server_name_wins_over_the_directory_spelling`, `project_is_not_a_scope_word_in_a_file`.
- `config::strict::tests` — `the_retired_grant_keys_name_allow` (every text of §7,
  byte for byte), `a_retired_key_is_refused_at_false_too`,
  `a_key_under_a_toolset_is_audited`.
- `tests/cmd/config.rs` — `allow_refuses_a_tool_the_agent_lacks`,
  `allow_refuses_a_server_the_agent_does_not_load`,
  `an_aliased_allow_list_is_each_agents_own`,
  `a_project_agent_replaces_the_homes_allow`.
- `tool::grants::tests` — `the_static_level_is_read_and_never_written`,
  `a_static_directory_replaced_by_a_link_is_stale`,
  `any_tool_grant_waives_and_a_directory_does_not`.
- `tool::builtins::tests` — `the_asking_list_is_what_the_probe_says`.
- `headless::batch::tests` — `a_call_that_would_ask_ends_the_run`;
  `repl::turn::tools::tests::two_unwaived_shell_calls_are_each_asked`.
- `repl::inbound` — `tests/repl/messaging.rs::a_message_is_held_after_a_tool_grant`,
  `a_directory_grant_does_not_hold`.
- `config::trust::tests::the_summary_shows_what_the_file_does` — rewritten on the lines
  of §6.
- `tests/cmd/cli.rs` — `allow_flag_takes_a_directory_a_tool_and_a_flag_server`,
  `allow_flag_is_relative_to_the_working_directory`, `add_dir_is_an_unknown_flag`.
- `tests/headless/approval.rs` — `a_config_allow_runs_unattended`,
  `the_allow_flag_runs_a_flag_server_unattended`, `an_outside_write_names_the_allow_flag`.
- `shell::selfcall::tests::a_line_that_allows_stays_in`.
- `repl::commands::allow::tests` — `the_static_tab_lists_tools_and_directories_by_source`,
  `allow_refuses_what_the_config_already_allows`.

Retired pins: `tests/tool/shell.rs::shell_calls_batch` (rewritten: capability, not
permission), `tool::sets::tests::the_dispatcher_says_whether_approval_is_waived`,
`code::tests::{write_path_admits_the_extra_write_roots, an_unresolvable_write_root_refuses_the_set}`
(moved onto the store and the assembly), `tests/cmd/cli.rs::{add_dir_opens_a_directory_after_the_configs_roots, add_dir_is_relative_to_the_working_directory}`,
`tests/headless/approval.rs::add_dir_admits_an_unattended_write`.

## 11. Divergences (drafts — the PM applies them)

Amendments, appended to the existing rows:

- **X-05** — `Amended <date>: a call joins a parallel batch only when it is
  parallel-capable AND `tool::approval::gate` would run it unasked. `supports_parallel`
  is a capability again and reads no approval setting. Before, an unsandboxed `shell`
  without `auto_run` answered true and two calls in one round ran without the dialog.
  Pinned by `headless::batch::tests::a_call_that_would_ask_ends_the_run`,
  `repl::turn::tools::tests::two_unwaived_shell_calls_are_each_asked`.`
- **X-44** — `Amended <date>: `--allow` replaces `--add-dir` among the choosing flags.
  Pinned by `shell::selfcall::tests::a_line_that_allows_stays_in`.`
- **X-91** — `Amended <date> by X-117: there is no top-level `write_roots:` and no static
  list beside the store. The one list the jail and the sandbox share is
  `Grants::roots()`; a directory an agent's `allow:` or `--allow` names is its static
  level, stored as a real location and live by the grant's rule. `write_roots:` fails
  the load naming `allow:`.`
- **X-92** — `Amended <date> by X-117: `outside_writes:` is removed. A write outside
  every root asks; a run with nobody to ask refuses, naming `--allow`. The bare
  refusal of `refuse` is no longer produced.`
- **X-94** — `Amended <date>: the summary shows one line per `agents.<name>.allow`
  entry; the `auto_run`, `auto_write`, `write_roots` and `outside_writes` lines are gone
  with their keys.`
- **X-110** — `Amended <date> by X-117: the store has a third, read-only level — static
  — filled once at assembly, never persisted, not a `Level`. The permanent forms named
  here (`write_roots:`, `auto_run`, `auto_write`) are that level's entries now. The
  inbound hold reads `Grants::waives()`.`
- **X-112** — `Amended <date> by X-117: `mcp_servers.<name>.auto_run` is removed; an MCP
  tool is asked unless the store holds it at any level. A `--mcp` server is allowed for
  a run with `--allow '<spec>:<tool|*>'`.`
- **X-113** — `Replaced <date> by X-117: `--add-dir` is `--allow <target>`.`

New row (`| id | area | before | now | reason |`):

| id | area | before | now | reason |
|---|---|---|---|---|
| X-117 | one grant vocabulary: `agents.<name>.allow` and `--allow` | "do not ask" is spelled five ways — top-level `write_roots:`, `--add-dir`, `tools.code.auto_write`, `tools.shell.auto_run`, `mcp_servers.<name>.auto_run` — at three scopes, each switch a `bool` its tool reads in `requires_approval`, `waives_approval` and (shell) `supports_parallel`; `outside_writes: refuse` says the opposite; one MCP tool cannot be allowed from a config and a `--mcp` server can never run unattended; a key under `tools.<set>` is not audited | `agents.<name>.allow: [<target>, …]` and `iota run\|resume --allow <target>` (repeatable), a target being `/allow`'s without the scope word: a directory (`/`, `~`, `.`; a config's relative to the project root, a flag's to the working directory), a built-in tool that asks (`shell`, `edit_file`, `write_file`), `<server>:<tool>` or `<server>:*`. They fill the grant store's static level (`Grants::with_static`, read-only, not a `Level`, never persisted), which the gate, the jail, the sandbox, the harness, the protected table, the hold (`Grants::waives`) and the batch read with the session and project levels. `requires_approval` is a fact of the call; `waives_approval` is gone. An `agents:` entry merges whole, so `allow:` has no merge rule. The six old spellings fail the load (the flag: clap's error) naming the new one; the audit checks `tools.shell` and `tools.code` keys. `/allow`'s `Static` tab lists the level, tools included | Decided <date> (docs/design/config-allow.md). `/allow` already had the vocabulary; a second spelling per kind was the redundancy, and each switch was a second mechanism every consumer had to learn — two had not (the hold ignored grants; the batch ignored an unsandboxed shell's approval). Per agent because a tool and a server are an agent's; not top level, which would need a union across the stack and could not refuse a tool an agent lacks. No shadow-deny: levels only add. `outside_writes: refuse` had no user — `-m` already refuses and a chat can deny. Pinned by `config::strict::tests::the_retired_grant_keys_name_allow`, `tool::grants::tests::{the_static_level_is_read_and_never_written, any_tool_grant_waives_and_a_directory_does_not}`, `tests/cmd/config.rs::a_project_agent_replaces_the_homes_allow`, `tests/headless/approval.rs::{a_config_allow_runs_unattended, the_allow_flag_runs_a_flag_server_unattended}`, `tests/repl/messaging.rs::a_message_is_held_after_a_tool_grant` |

## 12. Tasks

Every task's gate is `ci.sh`'s, run from the worktree:

```
cargo fmt --check
./scripts/check-deps.sh && ./scripts/check-stubs.sh && ./scripts/check-poison.sh
cargo clippy --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings
cargo clippy --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1 cargo test --no-fail-fast
IOTA_HC_REQUIRED=1 cargo test --test host
cargo build --release
```

plus `cargo test <name>` passing for each test the task names. "Artifact" is the one
module a task is about; its tests, the paragraphs of `README.md`, `ARCHITECTURE.md`,
`grants.md`, `code-toolset.md`, `shell-toolset.md` and `agent-messaging.md` it makes
true, its CHANGELOG line and its DIVERGENCES rows travel with it. Each task leaves the
product working and removes what it replaces in the same commit; nothing is behind a
flag, and no task leaves two spellings of one thing behind it *that it introduced* —
between task 4 and tasks 6–8 the new key coexists with an old one still awaiting its
removal, which is the layering, not a compatibility layer.

| # | slug | kind | artifact | depends on | size (changed lines) |
|---|---|---|---|---|---|
| 1 | `batch-gate` | fix | `src/headless/batch.rs` | — | ~150 |
| 2 | `hold-grants` | fix | `src/repl/inbound.rs` | — | ~150 |
| 3 | `outside-ask` | removal | `src/tool/builtins/code/mod.rs` | — | ~350 |
| 4 | `allow-key` | feature | `src/config/allow.rs` | 1, 2 | ~900 |
| 5 | `allow-flag` | feature + removal | `src/cmd/args.rs` | 4 | ~300 |
| 6 | `allow-roots` | removal | `src/tool/mod.rs` | 4, 5 | ~550 |
| 7 | `allow-builtins` | removal | `src/tool/builtins/shell.rs` | 4 | ~600 |
| 8 | `allow-mcp` | removal | `src/mcp/manager.rs` | 4 | ~300 |

Order: 1, 2, 3 in any order (each stands alone and is worth shipping if the rest is
never done); then 4; then 5; then 6, 7, 8 in any order.

**1. `batch-gate`** — first a failing test that drives two `shell` calls of a
`sandbox: off` set without `auto_run` through the chat walk and expects two dialogs.
Then `parallel_run` takes the gate's verdict: a call extends a run only when
`supports_parallel` and `gate() == Run`; both walks pass their store.
`ShellTool::supports_parallel` returns `true`. X-05 amendment. If the test passes
before the change, the hole is not there: the task then is the refactor alone and the
amendment loses its "before". Done when the gate passes and these exist:
`headless::batch::tests::a_call_that_would_ask_ends_the_run`,
`repl::turn::tools::tests::two_unwaived_shell_calls_are_each_asked`,
`tests/tool/shell.rs::shell_calls_batch` (rewritten).

**2. `hold-grants`** — `Grants::waives()`; `Served` holds the `Arc<Grants>` and its
`holds()` is `waives_approval() || grants.waives()`, read per message (the switch half
leaves in tasks 7 and 8). `agent-messaging.md` §4. Done when the gate passes and these
exist: `tool::grants::tests::any_tool_grant_waives_and_a_directory_does_not`,
`tests/repl/messaging.rs::{a_message_is_held_after_a_tool_grant, a_directory_grant_does_not_hold}`.

**3. `outside-ask`** — `outside_writes:` retired in the audit (both coordinates of §7);
`OutsideWrites`, `ToolEnv::outside_writes`, `Config::outside_writes`, `Outside::Refuse`
and its texts removed; `CodeSet::new` takes `attended`; the trust summary loses its
line. 44 references to the key and 67 to the two enums in `src/`, 33 in `tests/`. X-92
amendment. Done when the gate passes and these exist:
`config::strict::tests::outside_writes_is_retired_at_both_coordinates`,
`code::tests::an_outside_write_asks_or_says_how_to_allow_it`.

**4. `allow-key`** — the whole feature, beside the old keys: `config::allow::{Entry,
parse}`; `AgentConfig::allow`; `"allow"` in `AGENT_KEYS` and the load's refusals
(§3.3); `tool::builtins::ASKING`; `Grants::{with_static, statics}` and `roots()`
reading the static directories (its `statics` parameter stays until task 6);
`cmd::assemble_tools` resolving the agent's entries, with the assembly refusals;
`Footing::config`; the `Static` tab's `allow:` rows and tool rows; `/allow`'s
`(allow:)` refusals and the completion rows; `/status`'s `static` count; the trust
summary's per-entry lines; the late warning for an unknown MCP tool name; the README's
config section. X-117 (first half), X-94 and X-110 amendments. Done when the gate
passes and these exist: `config::allow::tests::{an_entry_is_read_in_allows_order, a_server_name_wins_over_the_directory_spelling, project_is_not_a_scope_word_in_a_file}`,
`tests/cmd/config.rs::{allow_refuses_a_tool_the_agent_lacks, allow_refuses_a_server_the_agent_does_not_load, an_aliased_allow_list_is_each_agents_own, a_project_agent_replaces_the_homes_allow}`,
`tool::grants::tests::{the_static_level_is_read_and_never_written, a_static_directory_replaced_by_a_link_is_stale}`,
`tool::builtins::tests::the_asking_list_is_what_the_probe_says`,
`tests/headless/approval.rs::a_config_allow_runs_unattended`,
`repl::commands::allow::tests::{the_static_tab_lists_tools_and_directories_by_source, allow_refuses_what_the_config_already_allows}`,
`config::trust::tests::the_summary_shows_what_the_file_does` (extended).

**5. `allow-flag`** — `RunArgs::allow` (and `first_given`), parsed with the agent's
servers and the `--mcp` specs, directories against the working directory;
`Footing::flag`; `--add-dir`, `ToolEnv::added_dirs` and the `--add-dir` row removed;
`--allow` in `selfcall::CHOOSING_LONG`; the unattended refusals of §8 name it. 70
references in `src/`, 20 in `tests/`. X-113 replaced, X-44 amended. Done when the gate
passes and these exist:
`tests/cmd/cli.rs::{allow_flag_takes_a_directory_a_tool_and_a_flag_server, allow_flag_is_relative_to_the_working_directory, add_dir_is_an_unknown_flag}`,
`tests/headless/approval.rs::{the_allow_flag_runs_a_flag_server_unattended, an_outside_write_names_the_allow_flag}`,
`shell::selfcall::tests::a_line_that_allows_stays_in`.

**6. `allow-roots`** — `write_roots:` retired in the audit (§7); `Config::write_roots`,
`check_write_root`, `ToolEnv::{write_roots, static_roots, with_write_roots}`,
`CodeSet::extra`, `ShellTool::write_roots`, `Environment::write_roots`,
`Footing::write_roots`, `SetError::WriteRoot` and `roots()`'s parameter removed; the
descriptions read `statics().dirs`; the trust summary loses its line. 110 references in
`src/`, 66 in `tests/`. X-91 amendment. Done when the gate passes and these exist:
`config::strict::tests::write_roots_is_retired_at_both_coordinates`,
`code::tests::the_jail_admits_a_static_directory_from_the_store`,
`tests/tool/shell.rs::a_static_directory_is_writable_in_the_sandbox`,
`agents::harness::tests::write_roots_line_lists_the_static_and_the_live_grants`.

**7. `allow-builtins`** — `tools.code.auto_write` and `tools.shell.auto_run` retired in
the audit, which now checks `tools.shell` and `tools.code` keys; `CodeConfig::auto_write`,
`ShellConfig::auto_run`, `CodeSet::{auto_write, waives}`, `SetError::CodeContradiction`
and `Tool::waives_approval` for the two sets removed; `requires_approval` per §4.2;
`headless::refusal_text` per §8; `iota config init`'s template; every fixture that
writes a switch (76 `auto_write` and ~60 `auto_run` references in `src/`, ~60 in
`tests/` including nine tmux scenarios and `tests/host/hc.rs`). Done when the gate
passes and these exist:
`config::strict::tests::{the_retired_grant_keys_name_allow, a_retired_key_is_refused_at_false_too, a_key_under_a_toolset_is_audited}`,
`tests/headless/approval.rs::a_builtin_refusal_names_the_allow_flag`,
`cmd::config_cmd::tests::the_template_allows_instead_of_switching`.

**8. `allow-mcp`** — `mcp_servers.<name>.auto_run` retired (the last text of §7);
`McpServerConfig::auto_run`, `ServerConfig::auto_run`, the manager's
`requires_approval` test and `waives_approval` removed — and with them
`Dispatcher::waives_approval`, the trait method, and the `waives_approval() ||` half of
task 2's `holds()`; the unattended refusal per §8; the completion and `/allow` texts
that say `auto_run`. ~55 references in `src/`, ~20 in `tests/`. X-112 amendment, X-117
(second half). Done when the gate passes and these exist:
`config::strict::tests::a_servers_auto_run_names_each_agents_allow`,
`mcp::manager::tests::every_tool_of_a_server_asks_until_the_store_holds_it`,
`tests/headless/approval.rs::a_flag_servers_refusal_names_the_allow_flag`.

**Size.** The brief's counts, re-measured on `af49a16` (lines matching, `src/` /
`tests/` / docs): `write_roots` 110 / 66 / 64; `outside_writes` 44 / 20 / 18, with
another 67 / 13 for the `OutsideWrites` and `Outside::` enums; `auto_write|auto_run`
183 / 75 / 85; `add_dir|add-dir|added_dirs` 70 / 20 / 36, which the brief did not count.
That is about 475 lines in `src/` across ~45 files and ~195 in `tests/` across ~20 that
must change or go, plus ~900 new. **Total ≈ 3,300 changed lines over eight tasks**,
roughly 1,400 added and 1,900 removed or rewritten; the tree ends smaller than it
starts. Tasks 1–3 are ~650 of it and deliver without the rest.

## 13. What the owner must decide

1. **Per-agent only (recommended), or a top-level `allow:` as well** (Q7). The cost of
   the recommendation is a list repeated per agent.
2. **`--allow` replacing `--add-dir` (recommended), or `--add-dir` kept** for
   directories with `--allow` for tools and servers — the convention of every surveyed
   CLI, at the price of two flags (Q6).
3. **The hold covers session and project tool grants** (recommended, §4.4) — a
   behaviour change for anyone who granted a tool in a chat that also takes socket
   messages — or only the static level, which is exact parity with today.
4. **`outside_writes: refuse` has no user** (Q5). If one exists, task 3 is dropped and
   nothing else changes.
5. **Tool names for the writers.** `allow: [edit_file, write_file]` is `/allow`'s
   vocabulary exactly. A set-level word (`code`) would be shorter and would be a target
   `/allow` does not take; not recommended.

## 14. Deferred

A deny form at any level; a top-level `allow:`; a glob over MCP tool names
(`github:search_*`); `allow:` on an `mcp_servers:` entry as a shorthand for "every agent
that loads it"; verifying a config's MCP tool names by connecting at `iota config`
time.
