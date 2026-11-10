# The code toolset — locate → read → edit, verified by the shell set's bash

The `code` built-in toolset gives the model the minimal coding loop used by
every mainstream coding agent (Claude Code, Codex CLI, Gemini CLI, Aider):
**locate** (glob/grep/list_dir) → **read** (read_file) → **edit**
(edit_file/write_file). The **verify** step is deliberately not part of this
set — builds and tests run through the `shell` set's sandboxed bash tool,
so execution policy stays in one place.

## Survey conclusions adopted

- **Edits are exact string replacements with a uniqueness constraint**
  (Claude Code Edit, Gemini CLI replace), not diff/patch application (Codex
  apply_patch, Aider): string matching is the most LLM-robust editing scheme;
  a patch format can come later as a separate tool if edit failure rates
  warrant it.
- **Read-before-edit is enforced**, with external-modification detection: the
  set keeps a session ledger of `path → mtime at last read`; edit_file and
  write_file (on existing files) refuse when the file was never read or
  changed on disk since. A successful write refreshes the ledger.
- **Reads are line-numbered** (`%6d\t`), all outputs are capped (64 KB per
  call) with continuation markers naming the next offset.

## Tools

| tool | args | notes |
|---|---|---|
| `glob` | pattern, path? | `**` via doublestar; bare patterns match any depth; newest first; caps 200 shown / 10000 examined |
| `grep` | pattern, path?, include?, context? | Go/RE2 regex; `path:line: text` (context lines `-`); 100-match cap; 10 MB/file cap; long lines truncated |
| `list_dir` | path? | one level; `dir/` + `file (size)`; 500-entry cap |
| `read_file` | path, offset?, limit? | numbered window; 20 MB read / 64 KB output caps; binary sniff (NUL) rejects |
| `edit_file` | path, old_string, new_string, replace_all? | unique-match string replace; snippet of the change echoed back |
| `write_file` | path, content | whole-file create/overwrite; parents auto-created |

**Reads are open; writes are jailed.** Every path argument is resolved the
same way: a relative path joins the project root (`ToolEnv::root`: the
configured project root, else the cwd), an absolute path is taken as given,
and either is cleaned lexically. For `read_file`, `list_dir`, `glob` and
`grep` (and their `path` base) that is all — no root is consulted and
nothing is refused. The jail would buy nothing: the sandboxed shell already
reads the whole machine without asking (the sandbox confines writes, and
the network unless `tools.shell.network: true`, but never reads), so a
jailed `read_file` only pushed the model into `cat`, which is less visible
and no safer.

`edit_file` and `write_file` **classify every path by its real location**
— symlinks resolved, the nearest existing ancestor canonicalized
(`CodeSet::write_path`):

- **INSIDE**: the real location of the project root or of a top-level
  `write_roots:` entry (below) holds it. Written at the path as given.
- **OUTSIDE**: the real location is known and lies outside every root — a
  link inside a root that leads out of every root included. Approvable
  (see Approval): the prompt names the real location, and the write lands
  there.
- **UNVERIFIABLE**: the real location cannot be worked out (`ENAMETOOLONG`,
  `ELOOP`, `EACCES`, too many hops). Refused, never approvable, never judged
  lexically: `cannot verify <path> inside the project root (…)`.
- **PROTECTED**: the real location is on the protected table, checked BEFORE
  inside/outside, so no root — `write_roots:` included — admits one (below).

### Protected paths

One table, `app::protected::table`, read by both the writers and the shell
sandbox (shell-toolset.md, Sandbox semantics), so the two cannot drift:

| path | reach | why (the refusal's reason) |
|---|---|---|
| `<root>/.iota.yaml`, `<root>/.iota.yml` | tree | the project config iota loads; a change takes effect next run |
| `<root>/.git` | the entry itself | replacing it (rename, link, `gitdir:` file) repoints git |
| `<root>/.git/hooks` | tree | git executes them on the user's next git command |
| `<root>/.git/config`, `<root>/.git/config.worktree` | tree | `core.fsmonitor`, `core.hooksPath` point git at a program |
| each `write_roots:` entry `<w>` that has a `.git`: `<w>/.git`, `hooks`, `config`, `config.worktree` (and its worktree rows) | as for the root | writable is not "run later in the user's terminal" |
| linked worktree: `<common>`, `<common>/hooks`, `<common>/config`, `<gitdir>/config.worktree` | entry, tree, tree, tree | as the three rows above |
| each submodule gitdir `<g>` under `.git/modules` (a directory with a `HEAD`, nested ones too): `<g>`, `hooks`, `config`, `config.worktree` | as for the root | a submodule's hooks run on git commands inside it |
| `~/.iota.yaml`, `~/.iota.yml` | tree | the user's own config, loaded without asking, takes effect next run |
| `~/.iota` | the entry itself | so the record below cannot be moved out from under its rule |
| `~/.iota/trusted.json` | tree | it records which project configs the user trusts |

In a linked worktree (`git worktree add`) `<root>/.git` is a `gitdir:` file,
and git's hooks and config are the main repository's: the table reads the
file (and the gitdir's `commondir`) when it is built and adds the common
dir's entry, `hooks` and `config` and the gitdir's `config.worktree` — not the
rest of either, so a commit still writes its objects and refs there. The
home config is on it because it is trusted by definition and decides which
code the next run starts (MCP commands, provider URLs, `sandbox: off`);
shell rc files are not, by the scoping above.

A tree entry holds the path and everything under it; an entry holds only the
name (its contents — `.git/objects`, `~/.iota/skills` — stay writable). Each
entry's real location is worked out per call, so a `.git/hooks` linked
elsewhere is matched where it points. The list is the intersection of what
Claude Code, Codex and Cursor protect, plus iota's own trust record — not
Claude Code's forty-item list, not all of `.git` (Codex's choice, which
pushes `git commit` out of the sandbox), not shell rc files.

A protected write is its own class (`Write::Protected`):

- **Never silent.** `auto_write` does not waive it; `write_roots:` cannot
  admit it (listing `<root>/.git` or `<root>/.git/hooks` there is accepted
  and opens the rest of `.git`, but not the table's paths); no session grant
  covers it — neither the tool-wide inside grant nor an outside-write grant
  for its directory.
- **Asked one file at a time** under `outside_writes: ask` with a user: the
  prompt names the real path, `(protected)` and why, e.g. `write_file
  /p/.git/hooks/pre-commit (protected): git executes the files in .git/hooks
  on the user's next git command — allow?`, and offers only `Allow once` /
  `Deny` (`ApprovalSubject::scope` is `None`, the gate's `Grant::Never`). The
  call lands only with `Approved::Once` naming exactly that real path.
- **Refused otherwise**: `<path> is protected: <why> — …`, ending, under
  `refuse`, ``outside_writes: refuse` refuses every write that needs
  approval; ask the user to make the change themselves`, with nobody to ask
  (`-m`, the SDK) `…which this non-interactive run cannot ask for; ask the
  user to make the change themselves`.
- **The trust record is never written**: no prompt, no approval path, `…
  no tool may write it; the user trusts a project config by running `iota
  config trust` themselves`.

Reads are untouched. The table is rebuilt for every classified write
(`CodeSet::protected_at`): a gitfile read, one `lstat` per extra root, one
`read_dir` of `.git/modules`, and a `physical` per row — a handful of
syscalls against a write that already resolves its own path, so it is not
cached (a cache would go stale when a `.git/hooks` is relinked). A hardlink
to a protected file made OUTSIDE iota (by the user, or an approved
unsandboxed command) is not seen by this path check; the sandbox refuses to
make one (shell-toolset.md), and the writers cannot make links.

A refused outside path is `path is outside the project root (<root>) and the
extra write roots (<e1>, <e2>): <path>` (the extras clause only when there
are extras). The writers' descriptions name the same roots, and under
`outside_writes: ask` in a chat they add that a write outside them asks the
user first, so the model does not take a refusal as final.

Results and headers stay anchored on the project root: a path outside it —
a read anywhere, a write under an extra root — is shown absolute (`~/…` in a
header when it sits under home). glob/grep never descend into a symlink below
the walk's start, but list a link and read through a file link wherever it
leads. A `path:` base is taken at its real location (`CodeSet::walk_base`),
spelled under the root that really holds it, else by that real location, so
a link to a directory, a path through one and the absolute path all walk
the same tree under the same rules. glob/grep skip `.git` and the matches
of the containing root's `.gitignore` — the project root's, or the extra
root's for a walk under one, whose patterns also match relative to that
root; a walk from a directory under no root is anchored on that directory
and its own `.gitignore` —
(re-read per call; nested .gitignore files are a later refinement), plus
binary files. A hit outside the project root is listed by its absolute path.

Because a walk may now start at `/`, every glob/grep walk is bounded by the
entries it VISITS, not only by its hits: it stops after 100 000 entries of
any kind (`CODE_WALK_MAX_ENTRIES`, per call) and ends its result with
`[stopped after visiting 100000 entries; narrow "path" to search further]`.
A grep's cost is the bytes it reads, not the entries it visits (a profile of
a grep over `/` spent three quarters of its time in `read`), so grep also
stops once it has read 256 MB of files in one call
(`CODE_GREP_MAX_TOTAL_BYTES`, `[stopped after reading 256 MB of files; …]`),
and it sniffs a file's first 8000 bytes for a NUL before reading the rest,
so a binary file costs one small read (ripgrep's order). Measured on a
laptop, a never-matching walk over `~` or `/` returns in about 1 s (glob)
and 7–11 s (grep, release build).
It also reads the run's cancellation token at every entry: nothing races the
call's await against ESC, so the token is the only thing that stops a walk on
the blocking pool. A cancelled walk returns what it found and ends with
`[search cancelled]`, as a cancelled shell command ends with `[command
cancelled]`: ESC cancels the call, not the turn, so the model reads the
result and must not take a partial answer for the whole one. Each stop has
its own line, so a cancel never reads as a cap. grep charges a binary
file's sniff to its read budget too.

## Approval

Approval is asked before a call runs (`requires_approval`, per call); the
jail is checked inside it. For the writers the two are tied by the path's
class:

| class | chat (a user to ask) | `-m`, the SDK path (nobody to ask) |
|---|---|---|
| INSIDE | asked unless `tools.code.auto_write` | refused unless `auto_write`, saying to set it |
| OUTSIDE, `outside_writes: ask` | asked ALWAYS — `auto_write` does not waive it | refused, saying to list the directory in `write_roots:` |
| OUTSIDE, `outside_writes: refuse` | refused, no prompt | refused |
| PROTECTED (not the trust record), `ask` | asked ALWAYS, "allow once" only | refused, saying the user can make the change |
| PROTECTED, `refuse`; the trust record | refused, no prompt | refused |
| UNVERIFIABLE | refused, no prompt | refused |

- **The prompt** for an outside write names the RESOLVED real path,
  absolute, marked `(outside the project root)` — the way the shell marks
  `(outside the sandbox)` — not the argument as typed. It is the writer's
  `header_summary`, so the call header says the same.
- **One resolution per call.** `Tool::approval_subject` resolves an
  outside write's real location ONCE and answers `ApprovalSubject { detail,
  target, scope }`: the prompt's text, the exact path "allow once" admits,
  and the directory "allow for this session" grants (the target's parent,
  or the target itself when it has none — `/` — so it never collapses into
  the tool-wide key). The walk takes the prompt's detail and the grant's key
  from that one answer, so what is shown and what is keyed cannot disagree
  (review B1: two resolutions let a link flipped between them pair one
  directory's prompt with another's grant).
- **"Allow for this session"** for an outside write grants that real
  DIRECTORY through that tool only, and the option says so (`Allow in
  <dir> for this session`). The gate keys its grants by wire tool name and
  scope (`None` = the whole tool, as for an inside write or a shell call): a
  grant for one directory does not cover another, nor the same directory
  through the other writer, and an inside grant never covers an outside
  write. Only the exact directory is granted, not its subtree. The grants
  belong to the conversation: `/new` and a `/session` resume clear them
  (`ApprovalGate::reset`).
- **The decision travels with the call, bound to what was approved.** The
  interactive walk sets `RunCtx::approved` from the gate's answer and the
  subject: `Approved::Once(target)` for "allow once",
  `Approved::Dir(scope)` for a session grant (given now or earlier).
  `write_target` re-resolves the real location at call time and admits an
  outside path only if "allow once" names exactly it, or the grant's
  directory is its directory — compared as paths, never as rendered text. A
  caller that skips the gate — the parallel batches, the headless loop, any
  future one — hands over `None`, and the call refuses; a link swapped after
  the gate moves the real location away from what was approved, and the call
  refuses too. The alternative, "`*_call` admits OUTSIDE because every
  caller gates", would hang the guarantee on a property of the callers;
  `an_outside_write_needs_this_calls_approval` fails under it, and
  `a_link_swapped_between_detail_and_scope_writes_nothing` /
  `a_grant_for_one_directory_never_admits_a_write_the_link_moved_into_another`
  replay B1.
- **The read ledger** stays keyed on the path as given (what `read_file`
  recorded); the bytes go to the real location, checked to be the same file
  at call time.
- A denial becomes an isError tool result ("The user declined this call."),
  so the provider still receives a result for every call id.
- **`outside_writes: refuse`** restores the jail as it was before writes
  could be approved: the existing refusal text, no prompt, `auto_write`
  unchanged. The other way to admit a directory — silently, in every mode — is
  to list it in `write_roots:`, read when the tools are assembled, so a
  running session cannot gain a root.
- **The hold** (`waives_approval`, docs/design/agent-messaging.md §4) still
  reads `auto_write`: it waives writes inside the roots, which then run
  unasked; an outside write is asked or refused, never waived.

## Config

```yaml
write_roots:           # top level, every agent; shared with the shell sandbox
  - ~/scratch          # `~` is home
  - ../shared-lib      # relative = against the project root
outside_writes: ask    # top level, every agent: ask (default) | refuse
tools:
  code:                # empty = writes inside the roots confirmed
    # auto_write: true # skip confirmations inside the roots (and allow them in -m)
```

`outside_writes:` is top-level like `write_roots:` and refused under
`agents:`/`models:` by the key audit. It can only tighten: once a file of the
stack says `refuse`, a later `ask` changes nothing.

`write_roots:` is ONE list for both questions "where may the code tools
write" and "where may a sandboxed shell write" — the split Cursor has
between its file tools and its terminal is exactly what it avoids. It is a
top-level key (beside `providers:`/`models:`/`agents:`), not a `tools:` key:
`tools:` keys are set names. `agents.<name>.write_roots` is refused by the
key audit. Every config file of the stack adds its entries. Each entry is
resolved once when the tools are assembled (`ToolEnv::write_roots`); an
extra root whose real location cannot be worked out refuses the set
(`cannot resolve the write root <path>`) instead of being dropped. Earlier,
at config load, a blank entry or a `~` entry with no home directory refuses
the load (`write_roots[i]: …`); an entry that names a directory that does not
exist yet is accepted. Two entries that clean to the same path are kept
once. An empty
or absent list is the project root alone, and the tools' descriptions are
then byte-identical to a build without the key.

## Deferred (v2+)

apply_patch-style multi-file edits, background/persistent processes (a
command-set evolution), LSP diagnostics, LLM edit self-correction (Gemini
editCorrector), nested .gitignore semantics, a diff preview inside the approval prompt, and the `web`
set. The rest of P2 (workspace trust, broader permission policies) builds on
the same approval seam (`requires_approval`, `approval_subject`, `RunCtx::approved`).

## Testing

Open reads (absolute/`..` and a link out of the root read, a `path:` base
outside the project walked and listed — every spelling of it, links
included, under its own `.gitignore` —, walks reporting escaping links, a
walk that never matches stopping at the entry cap, a cancelled token
stopping a walk, a cancelled glob/grep marked `[search cancelled]` with
hits and without, a finished one unmarked);
write jail (absolute/`..`; real locations: a symlink that leaves every root
refused, one that stays inside admitted, a root reached through a link, an
unverifiable path refused; extra write roots admitted, an unresolvable one
refusing the set; walks anchored on an extra root); a never-matching grep
stopping at the read budget; descriptions (no reader
claims a jail, the writers name the roots); glob depth default, gitignore exclusion, mtime
order; grep format, include filter, context, binary skip, bad regex;
list_dir; read_file numbering/window/binary/empty; edit_file read-gate,
uniqueness, replace_all, staleness, snippet; write_file create/parents,
overwrite gate, ledger refresh by writes; approval matrix incl. auto_write
and Merge routing; outside writes: the three classes
(`write_path_classifies_inside_outside_and_unverifiable`, unverifiable kept
refused under `ask` in `write_path_refuses_what_it_cannot_resolve`), who
asks (`the_writers_ask_matrix`), the approval invariant and the link swap
(`an_outside_write_needs_this_calls_approval`, `a_batch_never_writes_outside`),
the chat matrix and the per-directory grant
(`repl::turn::tools::tests::an_outside_write_is_asked_always_and_lands_where_the_prompt_said`,
`an_outside_session_grant_is_one_directory_through_one_tool`), the headless
matrix (`tests/headless/approval.rs::the_quiet_loop_never_writes_outside_the_roots`),
the key (`outside_writes_is_a_top_level_key_only`,
`tests/cmd/config.rs::outside_writes_defaults_to_ask_and_only_tightens_across_the_stack`)
(`src/tool/builtins/code/{mod,tools}.rs` tests,
`tests/tool/code.rs`; `write_roots:` end to end in
`tests/cmd/cli.rs::cli_write_roots_open_a_directory_outside_the_project`).
