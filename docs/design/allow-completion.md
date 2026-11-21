# `/allow` completion — a list of what can be granted, and the scope at the end

Status: **proposed** (2026-11-21). Design only; nothing here is implemented. It amends
`docs/design/grants.md` §5.2 and DIVERGENCES X-111 (§10 holds the drafted rows). §9 answers
the brief's nine questions one by one, numbered as asked; §11 is the task list.

## 1. The problem

`/allow` is typed blind. The user must know that `shell` is a wire name, that a server's
tool is `github:search_issues`, that `github:*` exists, and which of those would still ask.
A wrong guess is a refusal, and every refusal answers a question the chat could have
answered before Enter: *what can I grant right now?*

`/skills ` already answers its own version of that: a space after the command opens a list
of the skills. `/allow ` opens nothing.

The scope makes it worse. `/allow project <target>` starts with a keyword, so a list opened
by `/allow ` would have to lead with a word that is not a target, and every target would be
listed twice (with and without it) or hidden behind it.

## 2. The decision

1. **The scope goes last**: `/allow <target>` grants for this session, `/allow <target>
   project` for this project. The old order is not read (§5).
2. **`/allow ` opens the completion list**, holding one row per line that would grant if
   submitted now: the built-in tools that ask, then one row per MCP server whose tools ask.
3. **Two levels, by prefix.** A server's row completes to `/allow github:` and the list
   then holds that server's rows: `*` first, then its tools. No picker surface.
4. **The scope completes too**: after `/allow <target> ` the list holds one row, `project`,
   when the project level can take that grant.
5. **Directories stay typed.** They are never rows.
6. **The list is pushed, not pulled**: computed by the chat, issued with the command table,
   re-issued when the answer changes (§7). The command's own refusals are the backstop for
   a row that outlived its truth.

The mechanism is the one the composer already has — whole values, matched by prefix — plus
one field and one key rule (§4.2). There is still no per-command argument engine.

### 2.1 What mature products do, and what is taken from them

- **Shell completion (fish, zsh)**: a candidate that is a prefix of more (`origin/`,
  a directory with its `/`) is inserted WITH its separator, and the list re-filters to the
  next level. That is the second level here: the server row's value ends in `:`.
- **Claude Code** keeps the two axes apart (`/add-dir` for WHERE, `/permissions` for
  WHETHER) and lists what is in force in a surface; iota has one store and one command
  (grants.md §2), and its surface is the bare `/allow`. Taken: nothing new — the split is
  already ours.
- **`kubectl`, `gh`, `git`** put the object first and the qualifier after it
  (`git push origin main --force`); a completion list that starts with the things
  themselves is the reason. Taken: the scope as a trailing word.

## 3. The list: content and shape

### 3.1 The one rule

> **Every row's value is a line that grants if it is submitted now.** (A server's row is
> the one exception: its value is the prefix of such lines.)

So the list is computed with the predicates the command refuses with — never a second
opinion. For a tool key `K`:

```
asks(K)        the run has the tool and a call bound to no location asks
session(K)     asks(K) && !grants.allows(K)                       // either level
project(K)     asks(K) && run has a project && !grants.snapshot(Project).allows(K)
```

`grants.allows` at either level is `grant_tool`'s own test for a session grant (a project
grant covers the session); the project snapshot alone is its test for a project grant (a
session grant does not stop a project one). `GrantSet::allows` already folds a server's `*`
into each of its tools.

`asks(K)`, by kind:

- **built-in** — `allow::asking(dispatch)`, unchanged: the dispatcher's tools whose
  `grant_key` is `Builtin` and whose `requires_approval(name, None)` is true. Today:
  `shell` without `auto_run`, `edit_file`/`write_file` without `auto_write`.
- **MCP** — for each name of `Manager::connected_names()`, `connected_named(name)` must be
  exactly ONE server and not `auto_run` (the command's two refusals); then every tool it
  lists, and `*`. A server that is connecting, failed, down or waiting for a login has no
  rows: `/allow` refuses it too.

### 3.2 The rows

`Suggestion` gains one field, `after` (§4.2): the row is offered once the line starts with
it. In table order, after the `/allow` row:

| row | `value` | `label` | `desc` | `after` | present when |
|---|---|---|---|---|---|
| built-in | `/allow shell` | `shell` | `stop asking before it runs` | `/allow ` | `session(K)` |
| server | `/allow github:` | `github:` | `MCP server · 12 tools` | `/allow ` | any tool row or the `*` row below it is present |
| every tool | `/allow github:*` | `*` | `every tool of github, those it exposes later included` | `/allow github:` | `session(github:*)` |
| one tool | `/allow github:search` | `search` | the tool's own description | `/allow github:` | `session(K)` |
| scope | `/allow <target> project` | `project` | `for this project (<real root>), in every session` | `/allow <target> ` | `project(K)` |

- **Order**: built-ins in the dispatcher's order; servers in config order; under a server
  `*`, then its tools in the server's order. A scope row follows its target.
- **The count** on a server row is the number of its tool rows (not counting `*`).
- **The level is not repeated on each row.** Every level-1 and level-2 row is a session
  grant — that is what the line says with no trailing word, and the notice confirms it
  (`✓ shell runs without asking in this session`). The level is said where it is chosen:
  the `project` row names the project root.
- **Server-written text is cleaned.** A tool's description becomes a row only after
  `text::ansi::strip_ansi` and with control characters dropped (the list draws raw ANSI
  rows); the list already flattens whitespace and cuts at two lines. A server or tool whose
  NAME holds whitespace or a control character gets no row — it cannot be completed into a
  line the parse reads back — and stays grantable from the dialog.
- `ConnectedServer::tools` carries each tool's description beside its raw name; nothing
  else of the manager changes.

A scope row does not need its target's row. `shell` granted for this session has no
`shell` row (that line would be refused), but `/allow shell ` still offers `project`,
because `/allow shell project` would grant.

## 4. The second level

### 4.1 Why prefix matching is enough

The composer matches whole values by whole-line prefix. A server's row is the value
`/allow github:`; choosing it writes that into the composer; every row of that server
starts with it. What prefix matching does NOT give for free is hiding the 80 tools of a
server while the line is still `/allow g` — every one of them starts with `/allow g`.

### 4.2 The two changes to the composer

1. **`Suggestion::after`** — "offered once the line starts with this" (`""`: always).
   `match_suggestions` becomes `value.starts_with(line) && line.starts_with(after)`.
   This REPLACES today's special case (`typed_arg || !value.contains(' ')`, which hides
   `/skills <name>` until the line has a space): a skill row carries `after: "/skills "`
   and behaves exactly as it does now. One rule for every level of every command.
2. **Enter on a branch row descends.** A branch is a row whose `value` is some row's
   `after` — only a server row is one. While the completion cycle highlights a branch,
   Enter ends the cycle and keeps the line (`/allow github:`) instead of submitting it; the
   list re-filters to the next level. Everywhere else Enter is untouched: it submits.

Nothing else moves: `Tab`/`↓`/`↑` cycle and write the value, `Esc` puts back what was
typed, any other key ends the cycle, a click highlights.

Typing is never forced through Enter: after `Tab` lands on `github:`, the next character
(`s`, `*`) ends the cycle and filters level 2 by itself. Enter is only how one SEES level 2
unfiltered.

### 4.3 `/allow github:search project`, key by key

| keys | the line | the list |
|---|---|---|
| `/allow␣` | `/allow ` | `shell` · `edit_file` · `write_file` · `github:` · `linear:` |
| `g` | `/allow g` | `github:` |
| `Tab` | `/allow github:` | `github:` highlighted (the cycle; its base is `/allow g`) |
| `Enter` | `/allow github:` | `*` · `create_issue` · `search` · … (the cycle ended; nothing submitted) |
| `s` | `/allow github:s` | `search` |
| `Tab` | `/allow github:search` | `search` highlighted |
| `␣` | `/allow github:search ` | `project` |
| `Tab` | `/allow github:search project` | `project` highlighted |
| `Enter` | — | `✓ github:search runs without asking in this project (/Users/x/proj)` |

Fourteen keys after `/allow`. Shorter roads: skip the first `Enter` (type `s` straight
after `Tab`); the whole server is `/allow g` `Tab` `*` `Enter`.

`github:*` is ONE row, on level 2, first. A level-1 twin would double every server in the
list to save the one key `*` costs after `Tab`.

## 5. The parse

```
/allow                          list and take back: Grants (· Static)        — unchanged
/allow <target>                 grant for this session
/allow <target> project         grant for this project, in every session
<target> = <dir> | <server>:<tool> | <server>:* | <tool>                     — unchanged
```

Read in this order, on the trimmed argument:

1. **`project` first is refused.** When the first word is `project` — alone, or followed
   by anything — nothing is read further: `/allow: the scope goes last: /allow shell
   project` (echoing what followed; with nothing after it, `/allow: the scope goes last:
   /allow <target> project`). This is one rule of the new grammar, not a reading of the old
   one: `project` is the scope word, and a target never starts with it.
2. **The scope.** If the last whitespace-separated word is exactly `project`, the level is
   the project and the target is what precedes it; otherwise the session, and the target is
   everything.
3. **No project, no project level.** `…project` in a run without one is refused before the
   target is read, with today's three sentences (`no_project`).
4. **The target**, exactly as today: a connected server's whole name, a directory (a
   leading `/`, `~` or `.`), `<server>:<tool>` at its last `:`, else a built-in's wire name.
5. **A directory is never guessed.** When step 2 took the scope off a target spelled as a
   directory, and the WHOLE argument is itself an existing directory (`~/my project` beside
   `~/my`), the line is refused: `/allow: "~/my project" is a directory too: write "~/my
   project/" for it, or "~/my/ project" for ~/my in this project`.

What each case means:

- **`project` alone** — refused by step 1. It was never a directory (one starts with `/`,
  `~` or `.`) and no built-in is called that.
- **A directory literally named `project`** — `./project`, as it always was; for the
  project, `/allow ./project project`.
- **A directory whose name ENDS in ` project`** — a trailing `/` keeps it whole:
  `/allow ~/my project/` (the last word is `project/`, not the scope). Step 5 is what stops
  the unslashed spelling from silently granting `~/my` to the project.
- **The old form, `/allow project <target>`** — refused loudly by step 1, naming the line
  to type. It is not a shim: nothing is granted through it and no second grammar is parsed.
  Without step 1 the old form would still never grant (a target starting with `project `
  is no path, no connected server and no built-in), but it would be refused as `no built-in
  tool named "project shell"` or `no MCP server named "project github" is connected` — true
  and useless.

The refusal list of grants.md §5.2 gains steps 1 and 5 and loses nothing. The bare
`/allow`'s hint and every text that names the command follow the new order
(`No grants. /allow <dir> makes a directory writable for this session; /allow <dir> project
for this project.`; the unattended MCP refusal's `(/allow <server>:<tool> project)`).

## 6. What is refused, and how

Unchanged from grants.md §5.2 unless listed:

```
the scope goes last: /allow <target> project                       — new (§5 step 1)
"<arg>" is a directory too: write "<arg>/" for it, or "<dir>/ project" for <dir> in this project
                                                                     — new (§5 step 5)
<what> already runs without asking (this session | this project)   — the list's backstop (§7)
no MCP server named "<s>" is connected (connected: a, b)           — a row for a server that has since gone
server "<s>" has no tool "<t>"                                     — a row for a tool a reconnect dropped
```

`/allow github:` submitted with no tool stays `expected a directory (starting with /, ~
or .) or <server>:<tool>`.

## 7. Dynamics

The table is built once today and re-issued for two reasons (`/jobs` appearing, the skill
catalog changing). The `/allow` rows are a third: `CommandTable::set_allow(rows) -> bool`
(true when they changed, as `set_jobs`), then `ui.set_slash_commands(table.active())` under
the table's lock, as every re-issue is.

One function computes the rows (`allow::grantable`, beside `asking` and `grant_tool`, whose
predicates it shares). It is called at three kinds of moment:

| moment | covers |
|---|---|
| the chat starts | the built-ins; servers are still connecting |
| the loop returns to the prompt (before each read of the next input) | a grant or a revoke by `/allow`, a grant from the dialog during the turn, `/new` and `/session` (the session level replaced, the project's reloaded), `/save` |
| a server's status resolves | a background connect finishing, a server going down, a `/tools` reconnect ending |

**Cost.** Two `GrantSet` clones, one read lock on the manager's state, one walk of the
dispatcher's tool list — per prompt, not per key — and a channel send of a few hundred
short strings only when the rows changed. The UI keeps doing what it does: filter a `Vec`
by prefix per draw.

**What breaks if a re-issue is missed.** A row for something no longer grantable (granted
from the dialog mid-turn while the user was already typing; a server that died a moment
ago). Submitting it is refused by the command's own check, in words that say why (§6).
Nothing is granted that should not be, because the list decides nothing — it only writes a
line the parse reads like any typed one. A missing row (a server that connected a moment
ago) costs nothing either: the typed form works.

**Why not pull** — have the UI ask for the rows when the line starts with `/allow `: it is
always fresh, and it is the argument engine this codebase does not have, entered by the
back door: a callback from the UI thread into the chat's locks, called per draw, for one
command. Three push moments and a refusal that is already written cost less.

## 8. Tests

- **The list** — pure, in `repl::commands::allow::tests` over the existing `Chat` fixture
  and a `Manager` with merged fake servers: which rows exist, their four fields, their
  order; a session grant removes the tool's row and keeps its scope row; a project grant
  removes both; `srv:*` removes every session row of the server; an `auto_run` server, a
  name two servers share and a run with no project (no scope rows) have none.
- **The table** — `repl::commands::tests`: `active()` places the `/allow` rows after the
  command's row; `set_allow` reports a change only when there is one.
- **The parse** — `repl::commands::allow::tests`: the four target forms at both levels with
  the scope last; `project` first; the directory that ends in ` project`.
- **The matcher and the key** — need the UI: `ui::input::suggest::tests` for `after`
  (levels 1, 2 and the scope; the skills behaviour unchanged) and `ui::input::keys::tests`
  for Enter on a branch.
- **The re-issue** — `tests/repl/commands.rs` on the scripted UI, which records every
  `set_slash_commands`: the rows after a start, after a grant, after `/new`.
- **End to end** — L4 scenario `55-allow-completion.sh` against the mock MCP server of
  `53-mcp-grant.sh`: the §4.3 walk in a real terminal.

## 9. The nine questions

**1. The list's content.** The lines that would grant if submitted now (§3.1): per tool key,
`session(K)` for its row and `project(K)` for its scope row, computed with the command's own
predicates over `asking(dispatch)` and the manager's connected, single, non-`auto_run`
servers. A row is value / label / description / `after` (§3.2). It does not say the level:
a row with no trailing word is a session grant, and the `project` row is where the level is
named, with the root. A tool granted for the session but not the project has NO row of its
own and still gets its `project` row after `/allow <tool> ` — the scope is typed after the
target, so that is the only place "at the level the command would grant" can be answered.

**2. The second level.** Prefix rows, no picker (§4): the server's row leaves `/allow
github:` typed and the list re-filters. It needs `Suggestion::after` so a server's tools
wait for their prefix, and Enter on a branch row descending instead of submitting.
`github:*` is one row, on level 2, first. Keystrokes and the list at each step: §4.3.

**3. The scope at the end.** `<target>` then an optional last word `project` (§5).
`project` alone, and the old prefix form, are refused loudly by one rule — a target never
starts with the scope word — naming the line to type; no second grammar is read. A
directory named `project` is `./project`; one ending in ` project` takes a trailing `/`,
and the ambiguous unslashed spelling is refused, never guessed. The scope word completes:
one `project` row after `/allow <target> `.

**4. Dynamics.** Yes, rebuilt — at the start, at every return to the prompt, and when a
server's status resolves (§7). The cost is a few clones per prompt and a send when the rows
changed. If a re-issue is missed, a stale row's line is refused by `already runs without
asking` or `no MCP server named …`; the list never decides a grant.

**5. `defer:` servers.** *The brief's premise does not hold here, and I checked it.* The
manager knows every tool of a connected server from its `tools/list`
(`ServerStatus::tools`, filled in `merge_result`); `defer:` only keeps them out of the
MODEL's tool array (`DeferDispatcher::tools`). `/allow srv:tool` already resolves against
the manager's list, and the gate keys a deferred call the same way (`grant_key` passes
through the wrapper). So a deferred server gets the same rows as any other, loaded or not,
and the model's `search_tools` (or an implicit load) changes nothing in the list. What IS
unknown is a server that has not finished connecting: it has no row until its status
resolves, which is a re-issue moment (§7).

**6. Directories.** Typed only; never rows. At `/allow /Us` no value starts with the line,
so the list closes — as it does for `/file some/path` — and says nothing. (A `--mcp` server
named by a path keeps its row, `/allow ./srv.py:`: its name wins over the directory
spelling in the parse too.) That directories exist is said where the command is chosen: the
`/allow` row's description becomes `Stop asking for a tool or make a directory writable
(type its path); bare, list and revoke grants`.

**7. The empty list.** No rows, so no list: `/allow ` looks like any command with nothing
to complete, and the composer says nothing. A placeholder row is not possible — a row is
selectable and writes its value — and a non-selectable line would be a new kind of list
content for one case. The answer to "what is granted, what can I do" stays the bare
`/allow`: its surface, or its one-line hint.

**8. Tests and the pin.** §8: the list and the parse are pure and pinned in
`allow::tests`; the table in `commands::tests`; `after` and the Enter rule need the UI
(`suggest::tests`, `keys::tests`); the re-issue is pinned on the scripted UI; one L4
scenario walks it in a terminal.

**9. Tasks.** Four, §11: the parse, the composer's levels, the built-in rows with the
re-issue, the MCP rows — each leaving the product working.

## 10. Divergences (drafts — the PM applies them)

- **X-111**, appended — `Amended <date> (allow-completion): the scope is the LAST word,
  `/allow <target> [project]`; a line whose first word is `project` is refused (`the scope
  goes last: /allow <target> project`) and grants nothing, and a directory spelling that
  could be read both ways is refused naming both (`"<arg>" is a directory too: …`); a
  directory whose name ends in ` project` is typed with a trailing `/`. The hint and the
  unattended MCP refusal name the new order. `/allow ` opens the completion list: one row
  per line that would grant now — the built-ins that ask, one row per connected,
  non-`auto_run` server (`github:`), and under it `*` and its tools; after `/allow <target>
  ` one `project` row when the project level can take the grant. Directories are never
  rows. The rows are issued with the command table and re-issued at the start, at every
  return to the prompt and when a server's status resolves; a stale row is refused by the
  command's own checks. Decided because a list that starts with a keyword is worse to type
  into, and because every refusal of a blind `/allow` answered a question the chat knew.
  Pinned by `repl::commands::allow::tests::{the_scope_is_the_last_word,
  project_first_is_refused_and_grants_nothing, a_directory_ending_in_project_is_never_guessed,
  the_rows_are_the_lines_that_would_grant, a_session_grant_keeps_the_scope_row,
  a_servers_rows_wait_behind_its_prefix}`,
  `tests/repl/commands.rs::the_allow_rows_follow_the_grants` and L4 scenario
  `55-allow-completion.sh`.`

| id | area | before | now | reason |
|---|---|---|---|---|
| X-116 | completion levels | `match_suggestions` hid a value holding a space until the line had one (suggest.go), the one two-level case (`/skills <name>`); Enter always submitted | `Suggestion::after`: a row is offered once the line starts with it (`""`: always), for any depth — a skill row carries `/skills `, an MCP tool row `/allow <server>:`. A row whose value is another row's `after` is a branch: Enter on it while it is highlighted ends the cycle and keeps the line, so the list shows the next level; every other Enter submits | Decided <date> (docs/design/allow-completion.md §4.2). One rule in place of a special case, and the smallest thing that hides a server's tools behind its row; no per-command argument engine. Pinned by `ui::input::suggest::tests::a_row_waits_for_its_after_prefix`, `ui::input::keys::tests::enter_on_a_branch_row_descends_instead_of_submitting` |

## 11. Tasks

Every task's gate is `ci.sh`'s, plus the host suite on this machine (`tmux` is in
`/usr/local/bin` and `hc` in `~/.local/bin`, appended to `PATH`, never prepended):

```
export PATH="$PATH:/usr/local/bin:$HOME/.local/bin"
cargo fmt --check
./scripts/check-deps.sh
./scripts/check-stubs.sh
./scripts/check-poison.sh
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
IOTA_TMUX_REQUIRED=1 IOTA_SANDBOX_REQUIRED=1 cargo test
cargo build --release
IOTA_HC_REQUIRED=1 cargo test --test host
```

plus `cargo test <name>` passing for each test a task names. "Artifact" is the one module a
task is about; its tests, the paragraphs of grants.md it makes true, its CHANGELOG line and
its DIVERGENCES text travel with it. Nothing is behind a flag.

| # | slug | kind | artifact | depends on |
|---|---|---|---|---|
| 1 | `allow-scope-last` | change | `src/repl/commands/allow.rs` | — |
| 2 | `suggest-levels` | feature | `src/ui/input/suggest.rs` | — |
| 3 | `allow-rows-builtin` | feature | `src/repl/commands/allow.rs` | 1, 2 |
| 4 | `allow-rows-mcp` | feature | `src/repl/commands/allow.rs` | 3 |

Order: 1, 2 (either first; they do not touch each other), 3, 4.

**1. `allow-scope-last`** — the parse of §5: the trailing scope, `project` first refused,
the directory guard; the prefix form's branch deleted. Every text that names the command
follows: the two hints, the module doc, `Manager::unattended_refusal`'s clause, grants.md
§5.2 and §8, the X-111 amendment (its first half), CHANGELOG. Existing tests and the L4
scenarios that type `/allow project …` are rewritten to the new order. After it the
product works as before, typed the other way round. Done when the gate passes and these
exist: `repl::commands::allow::tests::{the_scope_is_the_last_word,
project_first_is_refused_and_grants_nothing, a_directory_ending_in_project_is_never_guessed}`,
`tests/repl/commands.rs::allow_project_writes_the_bucket_file` (rewritten),
`tests/headless/approval.rs` naming `/allow gh:echo project`.

**2. `suggest-levels`** — `Suggestion::after`, the matcher's one rule in place of the
space special case, the skill rows carrying `/skills `, Enter on a branch (§4.2). No
command's list changes: there is no branch row yet. X-116. Done when the gate passes and
these exist: `ui::input::suggest::tests::a_row_waits_for_its_after_prefix`,
`ui::input::keys::tests::enter_on_a_branch_row_descends_instead_of_submitting`, and the
existing skills completion tests pass unchanged in what they assert.

**3. `allow-rows-builtin`** — `allow::grantable` for the built-ins and their scope rows,
`CommandTable::set_allow`, the issue at the start and at every return to the prompt, the
`/allow` row's new description. `/allow ` now opens a list. Done when the gate passes and
these exist: `repl::commands::allow::tests::{the_rows_are_the_lines_that_would_grant,
a_session_grant_keeps_the_scope_row}`,
`repl::commands::tests::the_allow_rows_follow_the_command_and_report_a_change`,
`tests/repl/commands.rs::the_allow_rows_follow_the_grants`.

**4. `allow-rows-mcp`** — the server rows, `*` and the tool rows; `ConnectedServer::tools`
with descriptions, cleaned; the re-issue when a server's status resolves (the connect
reporter's drain, a `/tools` reconnect ending). The X-111 amendment's second half. Done
when the gate passes and these exist:
`repl::commands::allow::tests::{a_servers_rows_wait_behind_its_prefix,
an_auto_run_or_ambiguous_server_has_no_rows, a_servers_text_is_cleaned_before_it_is_a_row}`,
`tests/repl/commands.rs::the_allow_rows_arrive_when_a_server_connects`, and L4 scenario
`55-allow-completion.sh` (PASS, FAIL=0).

## 12. Not done, and why

- **A row that promotes** (`shell project` on level 1 for a tool granted for the session
  only). It would make level 1 mix two levels; the scope row after `/allow shell ` already
  grants it. The owner may want it (the report asks).
- **Directory completion.** The owner removed the browser; a path completer in the list is
  the same feature in another place.
- **A list for the bare `/allow`'s revocations.** That is the `Grants` tab.
