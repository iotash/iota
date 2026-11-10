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

`edit_file` and `write_file` are **jailed to the project root plus the
top-level `write_roots:`** (below): the path's real location — symlinks
resolved, the nearest existing ancestor canonicalized — must lie inside the
real location of the project root or of an extra root. A link inside any
root that leads out of every root is refused; a path whose real location
cannot be worked out is refused too (`cannot verify <path> inside the
project root (…)`), never judged lexically. A path outside every root is
`path is outside the project root (<root>) and the extra write roots (<e1>,
<e2>): <path>` (the extras clause only when there are extras). The writers'
descriptions name the same roots.

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

## Approval — the first slice of the P2 permission framework

Mutating tools implement the package-private `approver` interface; the
`tool.ApprovalReporter` capability surfaces it through Registry and Merge
(parts without the capability — MCP servers — keep their old behavior).

- **Interactive (chat.Run)**: before an approval-requiring call, the tool
  header is already on screen; a selector asks Allow once / Allow for this
  session / Deny. Session allowance is per tool name and lives for the Run.
  A denial becomes an isError tool result ("The user declined this call."),
  so the provider still receives a result for every call id.
- **Non-interactive (-m / chat.Once)**: approval-requiring calls are rejected
  with a result explaining how to enable them.
- **`tools.code.auto_write: true`** waives approval entirely (the tools then
  report RequiresApproval=false), which also unlocks writes in -m runs.

**The jail is not the approval.** The jail governs writes only. The two are
separate checks, in order: approval is asked before the call runs, and the
jail is checked inside it.
Allowing a write whose path lies outside every root does not widen the jail —
the call still fails with `path is outside the project root (…)`; `auto_write`
does not widen it either. The one way to admit a path outside the project
root is to list its directory in `write_roots:` (Config, below). That list is
read when the tools are assembled at startup, so a running session cannot
gain a root: the user edits the config and starts a new session.

## Config

```yaml
write_roots:           # top level, every agent; shared with the shell sandbox
  - ~/scratch          # `~` is home
  - ../shared-lib      # relative = against the project root
tools:
  code:                # empty = writes jailed to the roots + confirmed
    # auto_write: true # skip write confirmations (and allow writes in -m)
```

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
the same ApprovalReporter seam.

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
and Merge routing (`src/tool/builtins/code/{mod,tools}.rs` tests,
`tests/tool/code.rs`; `write_roots:` end to end in
`tests/cmd/cli.rs::cli_write_roots_open_a_directory_outside_the_project`).
