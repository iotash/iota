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
| UNVERIFIABLE | refused, no prompt | refused |

- **The prompt** for an outside write names the RESOLVED real path,
  absolute, marked `(outside the project root)` — the way the shell marks
  `(outside the sandbox)` — not the argument as typed. It is the writer's
  `header_summary`, so the call header says the same.
- **"Allow for this session"** for an outside write grants its real
  DIRECTORY through that tool only, and the option says so (`Allow in
  <dir> for this session`). The gate keys its grants by wire tool name and
  `Dispatcher::grant_scope` (`None` = the whole tool, as for an inside write
  or a shell call): a grant for one directory does not cover another, nor the
  same directory through the other writer, and an inside grant never covers
  an outside write. Only the exact directory is granted, not its subtree.
- **The decision travels with the call.** The interactive walk puts the
  detail the user allowed into `RunCtx::approved` (prompted or granted for
  the session); `write_target` admits an outside path only when that equals
  the summary it computes NOW. A caller that skips the gate — the parallel
  batches, the headless loop, any future one — hands over `None`, and the
  call refuses; a link swapped between the prompt and the call changes the
  real location, so the summary no longer matches and the call refuses too.
  The alternative, "`*_call` admits OUTSIDE because every caller gates",
  would hang the guarantee on a property of the callers;
  `an_outside_write_needs_this_calls_approval` fails under it.
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
the same approval seam (`requires_approval`, `grant_scope`, `RunCtx::approved`).

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
